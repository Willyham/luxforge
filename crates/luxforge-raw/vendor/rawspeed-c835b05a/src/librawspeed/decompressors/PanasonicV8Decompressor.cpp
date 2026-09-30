/*
    RawSpeed - RAW file decoder.

    Copyright (C) 2022-2024 LibRaw LLC (info@libraw.org)
    Copyright (C) 2024 Daniel Vogelbacher
    Copyright (C) 2025 Kolton Yager

    This library is free software; you can redistribute it and/or
    modify it under the terms of the GNU Lesser General Public
    License as published by the Free Software Foundation; either
    version 2.1 of the License, or (at your option) any later version.

    This library is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
    Lesser General Public License for more details.

    You should have received a copy of the GNU Lesser General Public
    License along with this library; if not, write to the Free Software
    Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA
*/

#include "rawspeedconfig.h"
#include "decompressors/PanasonicV8Decompressor.h"
#include "adt/Array1DRef.h"
#include "adt/Array1DRefExtras.h"
#include "adt/Array2DRef.h"
#include "adt/Bit.h"
#include "adt/Casts.h"
#include "adt/CroppedArray2DRef.h"
#include "adt/Invariant.h"
#include "adt/Optional.h"
#include "adt/Point.h"
#include "adt/TiledArray2DRef.h"
#include "bitstreams/BitStream.h"
#include "bitstreams/BitStreamer.h"
#include "bitstreams/BitStreamerMSB.h" // IWYU pragma: keep
#include "bitstreams/BitStreams.h"
#include "codes/AbstractPrefixCode.h"
#include "codes/AbstractPrefixCodeDecoder.h"
#include "common/Common.h"
#include "common/RawImage.h"
#include "common/RawspeedException.h"
#include "decoders/RawDecoderException.h"
#include "io/ByteStream.h"
#include "io/IOException.h"
#include <algorithm>
#include <array>
#include <cassert>
#include <climits>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <numeric>
#include <utility>
#include <vector>

namespace rawspeed {

// The set of templates and classes below define a specialized bit streamer
// which works mostly the same as BitStreamerMSB, but which reverses the
// ordering of the bits within each byte.
template <typename Tag>
struct BitStreamerReversedSequentialReplenisher
    : public BitStreamerForwardSequentialReplenisher<Tag> {
  using Base = BitStreamerForwardSequentialReplenisher<Tag>;
  using Traits = BitStreamerTraits<Tag>;
  using StreamTraits = BitStreamTraits<Traits::Tag>;

  using Base::BitStreamerForwardSequentialReplenisher;

  // Almost an exact copy of
  // BitStreamerForwardSequentialReplenisher::getInput(), but here we flip the
  // order of the bits within each byte, as they get loaded.
  std::array<std::byte, BitStreamerTraits<Tag>::MaxProcessBytes> getInput() {
    Base::establishClassInvariants();

    std::array<std::byte, BitStreamerTraits<Tag>::MaxProcessBytes> tmpStorage;
    auto tmp = Array1DRef<std::byte>(tmpStorage.data(),
                                     implicit_cast<int>(tmpStorage.size()));

    if (Base::getPos() + BitStreamerTraits<Tag>::MaxProcessBytes <=
        Base::input.size()) [[likely]] {
      auto currInput =
          Base::input
              .getCrop(Base::getPos(), BitStreamerTraits<Tag>::MaxProcessBytes)
              .getAsArray1DRef();
      invariant(currInput.size() == tmp.size());
      std::copy_n(currInput.begin(), BitStreamerTraits<Tag>::MaxProcessBytes,
                  tmp.begin());

      std::array<uint8_t, 4> ints;
      for (int i = 0; i != 4; ++i)
        ints[i] = uint8_t(tmp(i));
      ints = bitreverse_each(ints);
      for (int i = 0; i != 4; ++i)
        tmp(i) = std::byte{ints[i]};

      return tmpStorage;
    }

    if (Base::getPos() >
        Base::input.size() + 2 * BitStreamerTraits<Tag>::MaxProcessBytes)
        [[unlikely]]
      ThrowIOE("Buffer overflow read in BitStreamer");

    variableLengthLoadNaiveViaMemcpy(tmp, Base::input, Base::getPos());

    return tmpStorage;
  }
};

class BitStreamerRevMSB;

template <> struct BitStreamerTraits<BitStreamerRevMSB> final {
  static constexpr BitOrder Tag = BitOrder::MSB;

  static constexpr bool canUseWithPrefixCodeDecoder = true;

  static constexpr int MaxProcessBytes = 4;
  static_assert(MaxProcessBytes == sizeof(uint32_t));
};

/// Variation of standard MSB bit streamer. Bits are processed in reverse order
/// (least significant bits consume first).
class BitStreamerRevMSB final
    : public BitStreamer<
          BitStreamerRevMSB,
          BitStreamerReversedSequentialReplenisher<BitStreamerRevMSB>> {
  using Base =
      BitStreamer<BitStreamerRevMSB,
                  BitStreamerReversedSequentialReplenisher<BitStreamerRevMSB>>;

public:
  using Base::Base;
};

/// Utility class for Panasonic V8 entropy decoding
class PanasonicV8Decompressor::InternalDecoder {
private:
  // Reference to PanasonicV8Decompressor::mDecoderLUT
  const Array1DRef<const DecoderLUTEntry> mLUT;
  BitStreamerRevMSB mBitPump;

public:
  InternalDecoder(const Array1DRef<const DecoderLUTEntry>& LUT,
                  Array1DRef<const uint8_t> bitStream)
      : mLUT(LUT), mBitPump(bitStream) {}

  int32_t decodeNextDiffValue();
};

namespace {

enum class TileSequenceStatus : uint8_t { ContinuesRow, BeginsNewRow, Invalid };

inline TileSequenceStatus
evaluateConsecutiveTiles(const iRectangle2D rect, const iRectangle2D nextRect) {
  using enum TileSequenceStatus;
  // Are these two are horizontally-adjacent rectangles of same height?
  if (rect.getTopRight() == nextRect.getTopLeft() &&
      rect.getBottomRight() == nextRect.getBottomLeft())
    return ContinuesRow;
  // Otherwise, the next rectangle should be the first row of next Row.
  if (nextRect.getTopLeft() == iPoint2D(0, rect.getBottom()))
    return BeginsNewRow;
  return Invalid;
}

void isValidImageGrid(iPoint2D imgSize, Array1DRef<const iRectangle2D> rects) {
  auto outPos = iPoint2D(0, 0);
  const auto imgDim = iRectangle2D(outPos, imgSize);

  iRectangle2D rect = rects(0);
  if (rect.pos != outPos)
    ThrowRDE("First tile is out-of-order");
  invariant(rect.isThisInside(imgDim));
  invariant(rect.hasPositiveArea());
  outPos.x += rect.getWidth();
  for (int tileIdx = 1; tileIdx != rects.size(); ++tileIdx) {
    iRectangle2D nextRect = rects(tileIdx);
    invariant(nextRect.isThisInside(imgDim));
    invariant(nextRect.hasPositiveArea());
    switch (evaluateConsecutiveTiles(rect, nextRect)) {
    case TileSequenceStatus::ContinuesRow:
      outPos.x += nextRect.getWidth();
      rect = nextRect;
      continue;
    case TileSequenceStatus::BeginsNewRow:
      if (outPos.x != imgDim.getRight())
        ThrowRDE("Previous row has not been fully filled yet");
      outPos.x = 0;
      outPos.y += nextRect.getHeight();
      rect = nextRect;
      continue;
    case TileSequenceStatus::Invalid:
      ThrowRDE("Invalid tiling config");
    }
  }
  if (rect.getBottomRight() != imgDim.getBottomRight())
    ThrowRDE("Tiles do not cover whole output image");
}

template <typename T>
int bitsPerPixelNeeded(
    Array1DRef<const PanasonicV8Decompressor::DecoderLUTEntry> mDecoderLUT,
    T cb) {
  invariant(mDecoderLUT.size() > 0);
  const auto r = std::accumulate(
      mDecoderLUT.begin(), mDecoderLUT.end(), Optional<int>(),
      [cb](auto init, const PanasonicV8Decompressor::DecoderLUTEntry& e) {
        if (e.isSentinel())
          return init;
        invariant(e.bitcount > 0);
        const auto total = e.bitcount + e.diffCat;
        invariant(total > 0);
        init = init.has_value() ? cb(*init, total) : total;
        return init;
      });
  const auto bit = *r;
  invariant(bit > 0);
  return bit;
}

int minBitsPerPixelNeeded(
    Array1DRef<const PanasonicV8Decompressor::DecoderLUTEntry> mDecoderLUT) {
  return bitsPerPixelNeeded(mDecoderLUT,
                            [](auto a, auto b) { return std::min(a, b); });
}

int maxBitsPerPixelNeeded(
    Array1DRef<const PanasonicV8Decompressor::DecoderLUTEntry> mDecoderLUT) {
  return bitsPerPixelNeeded(mDecoderLUT,
                            [](auto a, auto b) { return std::max(a, b); });
}

} // namespace

std::vector<PanasonicV8Decompressor::DecoderLUTEntry>
PanasonicV8Decompressor::DecompressorParamsBuilder::getDecoderLUT(
    ByteStream stream) {
  std::vector<PanasonicV8Decompressor::DecoderLUTEntry> mDecoderLUT;

  const auto numSymbols = stream.getU16();
  if (numSymbols < 1 || numSymbols > 17)
    ThrowRDE("Unexpected number of symbols: %u", numSymbols);

  struct Entry {
    uint8_t bitcount;
    uint16_t symbol, mask;
    uint8_t codeValue;
  };
  std::vector<Entry> table;
  table.reserve(numSymbols);

  for (unsigned symbolIndex = 0; symbolIndex != numSymbols; ++symbolIndex) {
    const auto len = stream.getU16(); // Number of bits in symbol
    if (len < 1 || len > 16)
      ThrowRDE("Unexpected symbol length");
    const auto code = stream.getU16();
    if (!isIntN<uint32_t>(code, len))
      ThrowRDE("Bad symbol code");
    Entry entry;
    entry.bitcount = implicit_cast<uint8_t>(len);
    entry.symbol = uint16_t(code << (16U - entry.bitcount));
    entry.codeValue = implicit_cast<uint8_t>(symbolIndex);
    entry.mask = uint16_t(
        0xffffU << (16U -
                    entry.bitcount)); // mask of the bits overlapping symbol
    if (entry.bitcount == PanasonicV8Decompressor::DecoderLUTEntry().bitcount &&
        entry.codeValue == PanasonicV8Decompressor::DecoderLUTEntry().diffCat)
      ThrowRDE("Sentinel symbol encountered");
    table.emplace_back(entry);
  }
  assert(table.size() == numSymbols);

  // Cache of decoding results for all possible 16-bit values.
  mDecoderLUT.resize(1 + UINT16_MAX);

  // Populates LUT by checking for a bitwise match between each value and the
  // codes recorded in the table.
  for (unsigned li = 0; li < mDecoderLUT.size(); ++li) {
    PanasonicV8Decompressor::DecoderLUTEntry& lutVal = mDecoderLUT[li];
    for (const auto& ti : table) {
      if ((uint16_t(li) & ti.mask) == ti.symbol) {
        lutVal.bitcount = ti.bitcount;
        lutVal.diffCat = ti.codeValue;
        break; // NOTE: not a prefix code!
      }
    }
  }

  return mDecoderLUT;
}

std::vector<iRectangle2D>
PanasonicV8Decompressor::DecompressorParamsBuilder::getOutRects(
    iPoint2D imgSize, Array1DRef<const uint32_t> stripLineOffsets,
    Array1DRef<const uint16_t> stripWidths,
    Array1DRef<const uint16_t> stripHeights) {
  if (!imgSize.hasPositiveArea())
    ThrowRDE("Empty image requested");
  if (imgSize.x % 2 != 0 || imgSize.y % 2 != 0)
    ThrowRDE("Image size is not multiple of 2");
  const int totalStrips = stripLineOffsets.size();
  if (stripWidths.size() != totalStrips || stripHeights.size() != totalStrips)
    ThrowRDE("Inputs have mismatched length");
  if (totalStrips <= 0)
    ThrowRDE("No strips provided");

  std::vector<iRectangle2D> mOutRects;

  for (int stripIdx = 0; stripIdx < totalStrips; ++stripIdx) {
    const uint32_t stripWidth = stripWidths(stripIdx);
    const uint32_t stripHeight = stripHeights(stripIdx);
    const uint32_t stripOutputX = stripLineOffsets(stripIdx) & 0xFFFF;
    const uint32_t stripOutputY = stripLineOffsets(stripIdx) >> 16;

    const auto rect = iRectangle2D(iPoint2D(stripOutputX, stripOutputY),
                                   iPoint2D(stripWidth, stripHeight));
    const auto imgDim = iRectangle2D({0, 0}, imgSize);

    if (!rect.isThisInside(imgDim))
      ThrowRDE("Tile isn't fully within the output image");
    if (!rect.hasPositiveArea())
      ThrowRDE("The tile is empty");

    if (rect.pos.x % 2 != 0 || rect.pos.y % 2 != 0)
      ThrowRDE("Tile position is not multiple of 2");
    if (rect.dim.x % 2 != 0 || rect.dim.y % 2 != 0)
      ThrowRDE("Tile size is not multiple of 2");

    mOutRects.emplace_back(rect);
  }

  isValidImageGrid(imgSize, getAsArray1DRef(mOutRects));
  return mOutRects;
}

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

PanasonicV8Decompressor::PanasonicV8Decompressor(RawImage outputImg,
                                                 DecompressorParams mParams_)
    : mRawOutput(std::move(outputImg)), mParams(std::move(mParams_)) {
  if (mRawOutput->getCpp() != 1 ||
      mRawOutput->getDataType() != RawImageType::UINT16 ||
      mRawOutput->getBpp() != sizeof(uint16_t)) {
    ThrowRDE("Unexpected component count / data type");
  }
  if (mRawOutput->dim != mParams.imgSize)
    ThrowRDE("Unexpected image dimensions");
  const auto maxBpp = maxBitsPerPixelNeeded(mParams.mDecoderLUT);
  if (maxBpp > 32) {
    ThrowRDE("Single pixel decode may consume more than 32 bits");
  }
  const auto minBpp = minBitsPerPixelNeeded(mParams.mDecoderLUT);
  for (int stripIdx = 0; stripIdx < mParams.mStrips.size(); ++stripIdx) {
    const auto strip = mParams.mStrips(stripIdx);
    const auto maxPixelsInStrip = (uint64_t{CHAR_BIT} * strip.size()) / minBpp;
    const auto outRect = mParams.mOutRect(stripIdx);
    if (outRect.dim.area() > maxPixelsInStrip)
      ThrowRDE("Input strip is unsufficient to produce requested tile");
  }
}

void PanasonicV8Decompressor::decompress() const {
  const int numStrips = mParams.mStrips.size();
#ifdef HAVE_OPENMP
  unsigned threadCount =
      std::min(numStrips, rawspeed_get_number_of_processor_cores());
#pragma omp parallel for num_threads(threadCount)                              \
    schedule(static) default(none) firstprivate(numStrips)
#endif
  for (int stripIdx = 0; stripIdx < numStrips; ++stripIdx) {
    try {
      Array1DRef<const uint8_t> strip = mParams.mStrips(stripIdx);

      const auto outRect = mParams.mOutRect(stripIdx);

      const auto out = CroppedArray2DRef<uint16_t>(
                           mRawOutput->getU16DataAsUncroppedArray2DRef(),
                           /*offsetCols=*/outRect.pos.x,
                           /*offsetRows=*/outRect.pos.y,
                           /*croppedWidth=*/outRect.dim.x,
                           /*croppedHeight=*/outRect.dim.y)
                           .getAsArray2DRef();

      InternalDecoder decoder(mParams.mDecoderLUT, strip);

      decompressStrip(out, decoder);
    } catch (const RawspeedException& err) {
      // Propagate the exception out of OpenMP magic.
      mRawOutput->setError(err.what());
    } catch (...) {
      // We should not get any other exception type here.
      __builtin_unreachable();
    }
  }
}

void PanasonicV8Decompressor::decompressStrip(const Array2DRef<uint16_t> out,
                                              InternalDecoder decoder) const {
  Bayer2x2 predictedStorage = mParams.initialPrediction;
  const auto pred = Array2DRef(predictedStorage.data(), 2, 2);

  invariant(out.height() % 2 == 0);
  invariant(out.width() % 2 == 0);

  for (int j = 0; j != 2; ++j)
    for (int i = 0; i != 2; ++i)
      pred(i, j) = pred(j, i);

  const auto rowGroups = TiledArray2DRef(out,
                                         /*tileWidth=*/out.width(),
                                         /*tileHeight_=*/2);

  invariant(rowGroups.numCols() == 1);
  for (int rowGroup = 0; rowGroup != rowGroups.numRows(); ++rowGroup) {
    const auto outRow = rowGroups(rowGroup, 0).getAsArray2DRef();

    const auto outBlocks = TiledArray2DRef(outRow,
                                           /*tileWidth=*/2,
                                           /*tileHeight=*/2);

    // Each decoded 'row' is actually two rows of pixels in the raw image
    // because the image is encoded in rows of 2x2 CFA tiles. Likewise the
    // effective width here is 2x the strip width.
    invariant(outBlocks.numRows() == 1);
    for (int blockIdx = 0; blockIdx < outBlocks.numCols(); ++blockIdx) {
      const auto outBlock = outBlocks(0, blockIdx).getAsArray2DRef();

      for (int j = 0; j != 2; ++j) {
        for (int i = 0; i != 2; ++i) {
          const int32_t diff = decoder.decodeNextDiffValue();
          const int32_t decodedValue = pred(i, j) + diff;
          pred(i, j) = uint16_t(std::clamp(
              decodedValue, 0, int32_t(std::numeric_limits<uint16_t>::max())));
          outBlock(i, j) = pred(i, j);
        }
      }
    }

    // At the end of the line, reset predicted value to the first tile of the
    // prior line.
    const auto tmp = outBlocks(0, 0).getAsArray2DRef();
    for (int j = 0; j != 2; ++j)
      for (int i = 0; i != 2; ++i)
        pred(i, j) = tmp(i, j);
  }
}

int32_t inline PanasonicV8Decompressor::InternalDecoder::decodeNextDiffValue() {
  // Retrieve the difference category, which indicates magnitude of the
  // difference between the predicted and actual value.
  mBitPump.fill(32);
  const auto next16 = uint16_t(mBitPump.peekBitsNoFill(16));
  invariant(mLUT.size() == 1 + UINT16_MAX);
  const auto& [codeLen, codeValue] = mLUT(next16);
  if (codeValue == 0 && codeLen == 7)
    ThrowRDE("Decoding encountered an invalid value!");
  // Skip the bits that encoded the difference category
  mBitPump.skipBitsNoFill(codeLen);
  int diffLen = codeValue;

  if (diffLen == 0)
    return 0;

  const uint32_t diff = mBitPump.getBitsNoFill(diffLen);
  return AbstractPrefixCodeDecoder<BaselineCodeTag>::extend(diff, diffLen);
}

} // namespace rawspeed
