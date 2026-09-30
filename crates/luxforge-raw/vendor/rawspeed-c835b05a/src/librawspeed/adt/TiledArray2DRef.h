/*
    RawSpeed - RAW file decoder.

    Copyright (C) 2025 Roman Lebedev

    This library is free software; you can redistribute it and/or
    modify it under the terms of the GNU Lesser General Public
    License as published by the Free Software Foundation; either
    version 2 of the License, or (at your option) any later version.

    This library is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
    Lesser General Public License for more details.

    You should have received a copy of the GNU Lesser General Public
    License along with this library; if not, write to the Free Software
    Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA
*/

#pragma once

#include "rawspeedconfig.h"
#include "adt/Array2DRef.h"
#include "adt/CroppedArray2DRef.h"
#include "adt/Invariant.h"
#include <cstddef>
#include <type_traits>

namespace rawspeed {

template <class T> class TiledArray2DRef final {
  Array2DRef<T> data;

  int tileWidth;
  int tileHeight;

  friend TiledArray2DRef<const T>; // We need to be able to convert to const
                                   // version.

  // We need to be able to convert to std::byte.
  friend TiledArray2DRef<std::byte>;
  friend TiledArray2DRef<const std::byte>;

public:
  void establishClassInvariants() const noexcept;

  TiledArray2DRef(Array2DRef<T> data, int tileWidth, int tileHeight);

  using value_type = T;
  using cvless_value_type = std::remove_cv_t<value_type>;

  [[nodiscard]] int RAWSPEED_READONLY numRows() const;
  [[nodiscard]] int RAWSPEED_READONLY numCols() const;

  TiledArray2DRef() = delete;

  // Can not cast away constness.
  template <typename T2>
    requires(std::is_const_v<T2> && !std::is_const_v<T>)
  TiledArray2DRef(TiledArray2DRef<T2> RHS) = delete;

  // Can not change type to non-byte.
  template <typename T2>
    requires(!(std::is_const_v<T2> && !std::is_const_v<T>) &&
             !std::is_same_v<std::remove_const_t<T>, std::remove_const_t<T2>> &&
             !std::is_same_v<std::remove_const_t<T>, std::byte>)
  TiledArray2DRef(TiledArray2DRef<T2> RHS) = delete;

  // Conversion from TiledArray2DRef<T> to TiledArray2DRef<const T>.
  template <typename T2>
    requires(!std::is_const_v<T2> && std::is_const_v<T> &&
             std::is_same_v<std::remove_const_t<T>, std::remove_const_t<T2>>)
  TiledArray2DRef( // NOLINT(google-explicit-constructor)
      TiledArray2DRef<T2> RHS)
      : TiledArray2DRef(RHS.data, RHS._width, RHS._height, RHS._pitch) {}

  // Const-preserving conversion from TiledArray2DRef<T> to
  // TiledArray2DRef<std::byte>.
  template <typename T2>
    requires(
        !(std::is_const_v<T2> && !std::is_const_v<T>) &&
        !(std::is_same_v<std::remove_const_t<T>, std::remove_const_t<T2>>) &&
        std::is_same_v<std::remove_const_t<T>, std::byte>)
  TiledArray2DRef( // NOLINT(google-explicit-constructor)
      TiledArray2DRef<T2> RHS)
      : TiledArray2DRef(RHS.data, sizeof(T2) * RHS._width, RHS._height,
                        sizeof(T2) * RHS._pitch) {}

  CroppedArray2DRef<T> operator()(int row, int col) const;
};

// CTAD deduction guide
template <typename T>
explicit TiledArray2DRef(Array2DRef<T> data, int tileWidth,
                         int tileHeight) -> TiledArray2DRef<T>;

template <class T>
__attribute__((always_inline)) inline void
TiledArray2DRef<T>::establishClassInvariants() const noexcept {
  data.establishClassInvariants();
  invariant(tileWidth > 0);
  invariant(tileHeight > 0);
  invariant(tileWidth <= data.width());
  invariant(tileHeight <= data.height());
  invariant(data.width() % tileWidth == 0);
  invariant(data.height() % tileHeight == 0);
}

template <class T>
inline TiledArray2DRef<T>::TiledArray2DRef(Array2DRef<T> data_,
                                           const int tileWidth_,
                                           const int tileHeight_)
    : data(data_), tileWidth(tileWidth_), tileHeight(tileHeight_) {
  establishClassInvariants();
}

template <class T>
__attribute__((always_inline)) inline int TiledArray2DRef<T>::numCols() const {
  establishClassInvariants();
  return data.width() / tileWidth;
}

template <class T>
__attribute__((always_inline)) inline int TiledArray2DRef<T>::numRows() const {
  establishClassInvariants();
  return data.height() / tileHeight;
}

template <class T>
__attribute__((always_inline)) inline CroppedArray2DRef<T>
TiledArray2DRef<T>::operator()(const int row, const int col) const {
  establishClassInvariants();
  invariant(col >= 0);
  invariant(col < numCols());
  invariant(row >= 0);
  invariant(row < numRows());
  return CroppedArray2DRef(data,
                           /*offsetCols=*/tileWidth * col,
                           /*offsetRows=*/tileHeight * row,
                           /*croppedWidth=*/tileWidth,
                           /*croppedHeight=*/tileHeight);
}

} // namespace rawspeed
