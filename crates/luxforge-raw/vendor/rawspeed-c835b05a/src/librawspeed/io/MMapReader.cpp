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

#if !defined(_WIN32)

#include "io/MMapReader.h"
#include "adt/Array1DRef.h"
#include "adt/Casts.h"
#include "io/Buffer.h"
#include "io/FileIOException.h"
#include <cstdint>
#include <fcntl.h>
#include <limits>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

namespace rawspeed {

MMapReader::MMapReader(const std::string& fname)
    : fd(open(fname.c_str(), O_RDONLY)) {

  if (fd == -1)
    ThrowFIE("Could not open file \"%s\".", fname.c_str());

  struct stat sb;
  if (fstat(fd, &sb) == -1)
    ThrowFIE("Could not obtain the file size");

  length = sb.st_size;

  addr = mmap(nullptr, length, PROT_READ, MAP_PRIVATE, fd, 0);
  if (addr == MAP_FAILED)
    ThrowFIE("Could not mmap the file");
}

Buffer MMapReader::getAsBuffer() const {
  if (static_cast<int64_t>(length) >
      std::numeric_limits<Buffer::size_type>::max())
    ThrowFIE("File is too big (%zu bytes).", length);
  return Array1DRef(static_cast<const uint8_t*>(addr),
                    implicit_cast<Buffer::size_type>(length));
}

MMapReader::~MMapReader() {
  munmap(addr, length);
  close(fd);
}

} // namespace rawspeed

#endif // !defined(_WIN32)
