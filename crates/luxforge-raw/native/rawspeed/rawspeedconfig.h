/*
    RawSpeed - RAW file decoder.

    Copyright (C) 2016-2018 Roman Lebedev

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

// Luxforge: this file replaces the `rawspeedconfig.h` that RawSpeed's CMake
// build generates from `src/config.h.in` at commit
// c835b05aecfacb7343f7c424abd620aa12116c3f. It is that template configured by
// hand for luxforge-raw's `cc` build, with the values a CMake
// `BINARY_PACKAGE_BUILD=ON` configuration chooses (generic CPU, hardcoded cache
// line and page sizes), pugixml enabled, and OpenMP, zlib and libjpeg
// disabled: deflate and lossy JPEG DNG are unsupported. RawSpeed at this
// commit generates no other configuration or version header. Keep the
// directives in the template's order when updating the pin.

#pragma once

#if defined(__SSE2__)
#define WITH_SSE2
#else
/* #undef WITH_SSE2 */
#endif

// BINARY_PACKAGE_BUILD's hardcoded L1d cache line size.
// NOLINTNEXTLINE(google-runtime-int)
static constexpr unsigned long long RAWSPEED_CACHELINESIZE = 64;
static_assert(RAWSPEED_CACHELINESIZE > 0 &&
                  ((RAWSPEED_CACHELINESIZE & (RAWSPEED_CACHELINESIZE - 1)) ==
                   0),
              "Expected to know CPU L1d cache line size.");

// BINARY_PACKAGE_BUILD's hardcoded (minimal) page size.
// NOLINTNEXTLINE(google-runtime-int)
static constexpr unsigned long long RAWSPEED_PAGESIZE = 4096;
static_assert(RAWSPEED_PAGESIZE > 0 &&
                  ((RAWSPEED_PAGESIZE & (RAWSPEED_PAGESIZE - 1)) == 0),
              "Expected to know (minimal) CPU page size.");
static_assert(RAWSPEED_PAGESIZE >= RAWSPEED_CACHELINESIZE,
              "Expected that (minimal) CPU page size is not smaller than "
              "(minimal) CPU page size.");

// BINARY_PACKAGE_BUILD uses the page size as the large page size.
// NOLINTNEXTLINE(google-runtime-int)
static constexpr unsigned long long RAWSPEED_LARGEPAGESIZE = 4096;
static_assert(RAWSPEED_LARGEPAGESIZE > 0 &&
                  ((RAWSPEED_LARGEPAGESIZE & (RAWSPEED_LARGEPAGESIZE - 1)) ==
                   0),
              "Expected to know CPU page size.");
static_assert(RAWSPEED_LARGEPAGESIZE >= RAWSPEED_PAGESIZE,
              "Expected that CPU large page size is not smaller than (minimal) "
              "CPU page size.");

// The bundled pugixml reads the embedded cameras.xml.
#define HAVE_PUGIXML

/* #undef HAVE_OPENMP */

/* #undef HAVE_ZLIB */

/* #undef HAVE_JPEG */
/* #undef HAVE_JPEG_MEM_SRC */

// C++11 thread_local, which every supported compiler has.
#define HAVE_CXX_THREAD_LOCAL
/* #undef HAVE_GCC_THREAD_LOCAL */

// Built as part of luxforge-raw, never standalone, so RAWSPEED_SOURCE_DIR is
// not recorded (upstream omits it for reproducible embedded builds).
/* #undef RAWSPEED_STANDALONE_BUILD */
#ifdef RAWSPEED_STANDALONE_BUILD
#define RAWSPEED_SOURCE_DIR "@RAWSPEED_SOURCE_DIR@"
#else
// If rawspeed is being built as part of some larger build, we can not retain
// the RAWSPEED_SOURCE_DIR, because that would affect the reproducible builds.
#endif

// see http://clang.llvm.org/docs/LanguageExtensions.html
#ifndef __has_feature      // Optional of course.
#define __has_feature(x) 0 // Compatibility with non-clang compilers.
#endif
#ifndef __has_extension
#define __has_extension __has_feature // Compatibility with pre-3.0 compilers.
#endif

#define RAWSPEED_UNLIKELY_FUNCTION __attribute__((cold))
#define RAWSPEED_NOINLINE __attribute__((noinline))
#define RAWSPEED_READONLY __attribute__((pure))
#define RAWSPEED_READNONE __attribute__((const))
