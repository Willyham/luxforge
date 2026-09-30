// Private C ABI to the vendored RawSpeed. Rust and the LibRaw adapter own every
// input and output buffer. The encoded bytes are borrowed for one synchronous
// call and never copied; no C++ pointer or exception crosses the boundary.
// RawSpeed's camera data is the cameras.xml embedded at build time, parsed once
// per process.
#include "RawSpeed-API.h"
#include "decoders/RawDecoderException.h"
#include "native_limits.h"
#include <pugixml.hpp>
#include <atomic>
#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <mutex>
#include <new>
#include <stdexcept>
#include <string>

extern "C" {
typedef int (*LfCancel)(void *);
// What RawSpeed decoded, reported whether or not it matched the caller's
// expectation. Rust's RawSpeedImage has the same layout.
struct LfRawSpeedImage {
  uint32_t width, height, components, u16;
};
static_assert(sizeof(LfRawSpeedImage) == 16, "LfRawSpeedImage layout differs from Rust");
}

// RawSpeed's only log sink. Upstream defines it alone in common/Common.cpp,
// which build.rs leaves out of the RawSpeed build, and writes it to standard
// output, where the luxforge-json CLI writes its JSON. This definition keeps
// upstream's filtering (errors and warnings only) and format, and writes each
// message as one line to standard error.
namespace rawspeed {
void writeLog(DEBUG_PRIO priority, const char *format, ...) {
  if (priority >= DEBUG_PRIO::INFO) return;
  char line[1024];
  va_list args;
  va_start(args, format);
  std::vsnprintf(line, sizeof(line), format, args);
  va_end(args);
  std::fprintf(stderr, "RawSpeed:%s\n", line);
}
} // namespace rawspeed

namespace {
// The pinned data/cameras.xml, generated into OUT_DIR by build.rs.
constexpr unsigned char cameras_xml[] = {
#include "rawspeed_cameras_xml.inc"
};

std::atomic<uint32_t> camera_parses{0};

// RawSpeed's own CameraMetaData(const char *) constructor reads a file, and its
// addCamera is private. This subclass fills the public camera tables from the
// embedded bytes exactly as that constructor and addCamera do: one Camera per
// <Camera> node, then one per alias; a duplicate make/model/mode keeps the
// first entry; a CHDK mode is indexed by its filesize hint. Upstream's
// duplicate and missing-hint warnings are not printed.
class EmbeddedCameraMetaData final : public rawspeed::CameraMetaData {
public:
  EmbeddedCameraMetaData() {
    camera_parses.fetch_add(1, std::memory_order_relaxed);
    pugi::xml_document doc;
    const pugi::xml_parse_result result = doc.load_buffer(cameras_xml, sizeof(cameras_xml));
    if (!result)
      throw std::runtime_error(std::string("embedded cameras.xml could not be parsed: ") +
                               result.description());
    for (const pugi::xml_node node : doc.child("Cameras").children("Camera")) {
      const rawspeed::Camera *camera = add(std::make_unique<rawspeed::Camera>(node));
      if (!camera) continue;
      for (uint32_t alias = 0; alias < camera->aliases.size(); ++alias)
        add(std::make_unique<rawspeed::Camera>(camera, alias));
    }
    if (cameras.empty()) throw std::runtime_error("embedded cameras.xml has no cameras");
  }

private:
  const rawspeed::Camera *add(std::unique_ptr<rawspeed::Camera> camera) {
    rawspeed::CameraId id{rawspeed::trimSpaces(camera->make), rawspeed::trimSpaces(camera->model),
                          rawspeed::trimSpaces(camera->mode)};
    const auto [entry, inserted] = cameras.try_emplace(std::move(id), std::move(camera));
    if (!inserted) return nullptr;
    rawspeed::Camera *added = entry->second.get();
    if (added->mode.find("chdk") != std::string::npos) {
      const auto filesize = added->hints.get("filesize", std::string());
      if (!filesize.empty()) chdkCameras[static_cast<uint32_t>(std::stoi(filesize))] = added;
    }
    return added;
  }
};

std::once_flag camera_once;
// Written once inside call_once and never changed or freed; call_once orders
// that write before every caller that returns from it.
const EmbeddedCameraMetaData *camera_data = nullptr;

const rawspeed::CameraMetaData &camera_metadata() {
  std::call_once(camera_once, [] { camera_data = new EmbeddedCameraMetaData(); });
  return *camera_data;
}

void error(char *dst, size_t len, const char *message) noexcept {
  if (!dst || !len) return;
  std::strncpy(dst, message, len - 1);
  dst[len - 1] = 0;
}

// Test observability, per thread because each decode runs synchronously on its
// caller's thread: how many times decodeRaw has started, and a one-shot request
// that the next decode fail as RawSpeed would, in place of decodeRaw.
thread_local unsigned long long decode_calls = 0;
thread_local bool fail_next_decode = false;

// Decode `bytes` with RawSpeed and write its one-component u16 image of exactly
// `width` x `height` into `dest`, row r at dest + r * stride, each value v
// written as map[v] when `map` (65536 entries) is given and unchanged when not.
// RawSpeed returns uncorrected values (no linearization curve), uncropped, with
// no bad-pixel interpolation and no stage-1 DNG opcodes, and refuses unknown
// cameras. Cancellation is checked immediately before and immediately after
// decodeRaw, which cannot be interrupted; a cancelled or failed decode writes
// nothing to `dest`. `image` always receives what RawSpeed produced, so a
// mismatch can be reported, and RawSpeed's image is released before this
// returns.
int decode_into(const uint8_t *bytes, size_t byte_length, uint16_t *dest, size_t stride,
                uint32_t width, uint32_t height, const uint16_t *map, LfCancel cancel,
                void *cancel_context, LfRawSpeedImage *image, char *err,
                size_t err_len) noexcept {
  if (!bytes || !byte_length || !dest || !image || byte_length > LF_MAX_SOURCE_BYTES ||
      byte_length > size_t(std::numeric_limits<int32_t>::max())) {
    error(err, err_len, "invalid or oversized RAW input");
    return LF_STATUS_INVALID_INPUT;
  }
  std::memset(image, 0, sizeof(*image));
  if (!width || !height || width > LF_MAX_SIDE || height > LF_MAX_SIDE ||
      uint64_t(width) * height > LF_MAX_PIXELS || stride < width) {
    error(err, err_len, "RawSpeed mosaic size is outside the adapter limits or the buffer");
    return LF_STATUS_GEOMETRY;
  }
  try {
    const rawspeed::CameraMetaData &meta = camera_metadata();
    const rawspeed::Buffer buffer(bytes, static_cast<rawspeed::Buffer::size_type>(byte_length));
    rawspeed::RawParser parser(buffer);
    const std::unique_ptr<rawspeed::RawDecoder> decoder = parser.getDecoder(&meta);
    if (!decoder) {
      error(err, err_len, "RawSpeed found no decoder");
      return LF_STATUS_FAILED;
    }
    decoder->uncorrectedRawValues = true;
    decoder->applyCrop = false;
    decoder->interpolateBadPixels = false;
    decoder->applyStage1DngOpcodes = false;
    decoder->failOnUnknown = true;
    decoder->checkSupport(&meta);
    if (cancel && cancel(cancel_context)) {
      error(err, err_len, "cancelled");
      return LF_STATUS_CANCELLED;
    }
    ++decode_calls;
    if (fail_next_decode) {
      fail_next_decode = false;
      ThrowRDE("injected test failure in place of decodeRaw");
    }
    const rawspeed::RawImage raw = decoder->decodeRaw();
    if (cancel && cancel(cancel_context)) {
      error(err, err_len, "cancelled");
      return LF_STATUS_CANCELLED;
    }
    const rawspeed::iPoint2D size = raw->getUncroppedDim();
    image->width = size.x > 0 ? uint32_t(size.x) : 0;
    image->height = size.y > 0 ? uint32_t(size.y) : 0;
    image->components = raw->getCpp();
    image->u16 = raw->getDataType() == rawspeed::RawImageType::UINT16 ? 1 : 0;
    std::string first;
    if (raw->isTooManyErrors(1, &first)) {
      error(err, err_len, ("RawSpeed recorded a decode error: " + first).c_str());
      return LF_STATUS_FAILED;
    }
    if (!image->u16 || image->components != 1) {
      error(err, err_len, "RawSpeed did not decode a one-component u16 mosaic");
      return LF_STATUS_UNSUPPORTED_CFA;
    }
    if (image->width != width || image->height != height) {
      error(err, err_len, "RawSpeed mosaic size differs from the expected size");
      return LF_STATUS_GEOMETRY;
    }
    const rawspeed::Array2DRef<uint16_t> samples = raw->getU16DataAsUncroppedArray2DRef();
    for (uint32_t row = 0; row < height; ++row) {
      const uint16_t *source = &samples(int(row), 0);
      uint16_t *target = dest + size_t(row) * stride;
      if (map)
        for (uint32_t column = 0; column < width; ++column) target[column] = map[source[column]];
      else
        std::memcpy(target, source, size_t(width) * 2);
    }
    // Returning destroys RawSpeed's image, then its decoder and parser.
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) {
    error(err, err_len, "RawSpeed allocation failed");
    return LF_STATUS_ALLOCATION;
  } catch (const std::exception &e) {
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    error(err, err_len, "unknown RawSpeed failure");
    return LF_STATUS_FAILED;
  }
}
} // namespace

// Parse the embedded camera data if no call has yet, and report how many
// camera entries it holds and how many times this process has parsed it.
extern "C" int lf_rawspeed_cameras(uint32_t *cameras, uint32_t *parses, char *err,
                                   size_t err_len) noexcept {
  if (!cameras || !parses) {
    error(err, err_len, "invalid camera data arguments");
    return LF_STATUS_INVALID_INPUT;
  }
  try {
    *cameras = static_cast<uint32_t>(camera_metadata().cameras.size());
    *parses = camera_parses.load(std::memory_order_relaxed);
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) {
    error(err, err_len, "native allocation failed");
    return LF_STATUS_ALLOCATION;
  } catch (const std::exception &e) {
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    error(err, err_len, "unknown camera data failure");
    return LF_STATUS_FAILED;
  }
}

// Decode the sensor samples of `bytes` with RawSpeed: uncorrected values (no
// linearization curve), uncropped, no bad-pixel interpolation, no stage-1 DNG
// opcodes, unknown cameras refused. The result must be a one-component u16
// image of exactly `width` x `height`, which is copied row by row into `dest`
// (`length` = width x height samples). `image` always receives what RawSpeed
// produced, so a mismatch can be reported.
extern "C" int lf_rawspeed_decode(const uint8_t *bytes, size_t byte_length, uint16_t *dest,
                                  size_t length, uint32_t width, uint32_t height,
                                  LfRawSpeedImage *image, char *err, size_t err_len) noexcept {
  if (image) std::memset(image, 0, sizeof(*image));
  if (length != uint64_t(width) * height) {
    error(err, err_len, "RawSpeed mosaic size is outside the adapter limits or the buffer");
    return LF_STATUS_GEOMETRY;
  }
  return decode_into(bytes, byte_length, dest, width, width, height, nullptr, nullptr, nullptr,
                     image, err, err_len);
}

// The LibRaw adapter's substitute for a replaceable LibRaw decoder: decode with
// RawSpeed into LibRaw's raw buffer, `stride` samples per row, each value
// through `map` (the replaced decoder's curve rule) when it is given. See
// decode_into.
extern "C" int lf_rawspeed_unpack(const uint8_t *bytes, size_t byte_length, uint16_t *dest,
                                  size_t stride, uint32_t width, uint32_t height,
                                  const uint16_t *map, LfCancel cancel, void *cancel_context,
                                  LfRawSpeedImage *image, char *err, size_t err_len) noexcept {
  return decode_into(bytes, byte_length, dest, stride, width, height, map, cancel,
                     cancel_context, image, err, err_len);
}

// Test observability for this thread: decodeRaw calls started, and a request
// that the next decode on this thread fail in place of decodeRaw.
extern "C" unsigned long long lf_rawspeed_decode_calls(void) noexcept { return decode_calls; }
extern "C" void lf_rawspeed_fail_next_decode(void) noexcept { fail_next_decode = true; }

// Test access to the log sink through the RawSpeed library's own call site:
// RawImageData::subFrame (common/RawImage.cpp) logs one warning when asked for
// a crop larger than its image, and returns without changing it.
extern "C" int lf_rawspeed_log_probe(void) noexcept {
  try {
    const rawspeed::RawImage raw = rawspeed::RawImage::create(rawspeed::iPoint2D(4, 4));
    raw->subFrame(rawspeed::iRectangle2D(0, 0, 8, 8));
    return LF_STATUS_OK;
  } catch (...) {
    return LF_STATUS_FAILED;
  }
}

