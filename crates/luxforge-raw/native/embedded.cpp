// Embedded previews: list and extract the images a RAW file carries beside its mosaic, without
// unpacking the mosaic. Private C ABI, compiled into the same native library as adapter.cpp.
//
// LibRaw reads the source through LfStream, whose every fetch is a positional read back into
// Rust (LfReadAt), so a preview never needs the whole file in memory. Rust owns the reader the
// callback's context points at and the handle: lf_preview_open creates the handle, Rust closes it
// exactly once through lf_preview_close, and the reader outlives it. A cancel callback and its
// context are used only during the synchronous call they are passed to and are cleared before it
// returns. No C++ exception crosses the ABI. This file never calls LibRaw's unpack.
#include "libraw/libraw.h"
#include "native_limits.h"
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <memory>
#include <new>
#include <vector>

extern "C" {
typedef int (*LfCancel)(void *);
// Fill exactly `length` bytes at `offset` of the source into `dest`: 0 on success, nonzero when
// the reader failed, the read budget would be exceeded or the source ended early. The Rust side
// records which; this side only stops.
typedef int (*LfReadAt)(void *context, uint64_t offset, uint8_t *dest, size_t length);

// One entry of LibRaw's thumbnail list, as identify left it. `format` is LibRaw's internal
// thumbnail format; `head` holds the first eight stored bytes of an item LibRaw declares a JPEG
// (zero otherwise), so Rust can tell a JPEG from what LibRaw only calls one.
struct LfPreviewItem {
  uint64_t offset;
  uint32_t length, format, width, height, flip, misc;
  uint8_t head[8];
};
static_assert(sizeof(LfPreviewItem) == 40, "LfPreviewItem layout differs from Rust");

// What identify read that a preview is judged against, and the thumbnail list.
struct LfPreviewList {
  char make[64], model[64];
  uint32_t width, height, raw_width, raw_height, flip, count;
  LfPreviewItem items[LIBRAW_THUMBNAIL_MAXCOUNT];
};
static_assert(sizeof(LfPreviewList) == 472, "LfPreviewList layout differs from Rust");

// One extracted image, borrowed from LibRaw's thumbnail buffer until lf_preview_release, the next
// extraction or lf_preview_close.
enum LfPreviewKind : uint32_t { LF_PREVIEW_JPEG = 1, LF_PREVIEW_BITMAP = 2 };
struct LfPreviewImage {
  const uint8_t *data;
  uint64_t length;
  uint32_t kind, width, height, colors;
};
static_assert(sizeof(LfPreviewImage) == 32, "LfPreviewImage layout differs from Rust");
}

namespace {
// Blocks start on this boundary, so a run of small reads near one offset fetches aligned blocks.
constexpr uint64_t BLOCK_ALIGN = 4096;
// The largest block, and the most blocks, a caller may ask for.
constexpr uint64_t MAX_BLOCK = 1u << 20;
constexpr uint32_t MAX_BLOCKS = 64;
// A read at least one block long bypasses the block buffer and fetches into the caller's buffer
// in pieces of at most this size, so cancellation is checked between them.
constexpr size_t DIRECT_PIECE = 1u << 20;

void error(char *dst, size_t len, const char *message) noexcept {
  if (!dst || !len) return;
  std::strncpy(dst, message, len - 1);
  dst[len - 1] = 0;
}
void copy_name(char *dst, size_t len, const char *src) noexcept {
  std::memset(dst, 0, len);
  if (src) std::strncpy(dst, src, len - 1);
}

// Why a stream stopped reading. Once set it stays set: every later fetch stops the same way.
enum class StreamFailure { None, Cancelled, Reader };

// LibRaw's datastream over the Rust reader. The semantics of every call follow LibRaw's own
// LibRaw_buffer_datastream (the stream open_buffer uses): reads and seeks clamp to [0, size],
// read returns whole or partial items, get_char returns -1 at the end, and scanf_one reads a
// token from the next 24 bytes. gets follows fgets, where the buffer stream writes its terminator
// one byte late at the end of the data and skips one byte after a line longer than the buffer.
//
// Bytes come from a cache of `blocks` blocks of `block` bytes, the least recently read replaced
// first: a small read fetches the aligned block that holds it, so identify's many 1–4-byte reads
// cost one callback per block, and its jumps between a directory and the values it points to
// elsewhere find both blocks still held. A read of at least a block goes straight to the caller's
// buffer. Each fetch checks the cancel callback first. A reader failure or cancellation throws a
// LibRaw exception, which LibRaw turns into an error code; failure() tells the caller why even
// where LibRaw swallows the exception.
class LfStream final : public LibRaw_abstract_datastream {
public:
  LfStream(LfReadAt read, void *context, uint64_t size, size_t block, size_t blocks)
      : read_(read), context_(context), size_(INT64(size)), block_size_(block), cache_(blocks) {}
  LfStream(const LfStream &) = delete;
  LfStream &operator=(const LfStream &) = delete;

  // The cancel callback for the synchronous call in progress; cleared by the call's scope.
  void set_cancel(LfCancel cancel, void *context) noexcept {
    cancel_ = cancel;
    cancel_context_ = context;
  }
  StreamFailure failure() const noexcept { return failure_; }

  int valid() override { return read_ ? 1 : 0; }

  int read(void *ptr, size_t size, size_t nmemb) override {
    if (!size || !nmemb) return 0;
    size_t to_read = nmemb > SIZE_MAX / size ? SIZE_MAX : size * nmemb;
    const uint64_t remaining = pos_ < size_ ? uint64_t(size_ - pos_) : 0;
    if (to_read > remaining) to_read = size_t(remaining);
    if (to_read < 1) return 0;
    copy_out(static_cast<uint8_t *>(ptr), to_read);
    return int((to_read + size - 1) / size);
  }

  int seek(INT64 o, int whence) override {
    switch (whence) {
    case SEEK_SET:
      pos_ = o < 0 ? 0 : (o > size_ ? size_ : o);
      return 0;
    case SEEK_CUR:
      if (o < 0)
        pos_ = o <= -pos_ ? 0 : pos_ + o;
      else if (o > 0)
        pos_ = o > size_ - pos_ ? size_ : pos_ + o;
      return 0;
    case SEEK_END:
      if (o > 0)
        pos_ = size_;
      else if (o < -size_)
        pos_ = 0;
      else
        pos_ = size_ + o;
      return 0;
    default:
      return 0;
    }
  }

  INT64 tell() override { return pos_; }
  INT64 size() override { return size_; }
  int eof() override { return pos_ >= size_; }

  int get_char() override {
    if (pos_ >= size_) return -1;
    return byte_at(pos_++);
  }

  char *gets(char *s, int sz) override {
    if (sz < 1 || pos_ >= size_) return nullptr;
    int n = 0;
    while (n < sz - 1 && pos_ < size_) {
      const uint8_t c = byte_at(pos_++);
      s[n++] = char(c);
      if (c == '\n') break;
    }
    s[n] = 0;
    return s;
  }

  int scanf_one(const char *fmt, void *val) override {
    // As the buffer stream: the token must start within the last 24 bytes' reach.
    if (size_ < 24 || pos_ > size_ - 24) return 0;
    char token[25];
    for (int i = 0; i < 24; ++i) token[i] = char(byte_at(pos_ + i));
    token[24] = 0;
    const int result = std::sscanf(token, fmt, val);
    if (result > 0) {
      int count = 0;
      while (pos_ < size_ - 1) {
        ++pos_;
        ++count;
        const uint8_t c = byte_at(pos_);
        if (c == 0 || c == ' ' || c == '\t' || c == '\n' || count > 24) break;
      }
    }
    return result;
  }

  // Copy `length` bytes at `offset` without moving the position: from a block when one holds
  // them, otherwise with one exact fetch that leaves the blocks alone. False past the end.
  bool peek(uint64_t offset, uint8_t *dest, size_t length) {
    if (offset > uint64_t(size_) || length > uint64_t(size_) - offset) return false;
    if (length == 0) return true;
    const Block *block = find(INT64(offset));
    if (block && INT64(offset + length) <= block->end()) {
      std::memcpy(dest, block->data.data() + (INT64(offset) - block->start), length);
      return true;
    }
    fetch(offset, dest, length);
    return true;
  }

private:
  struct Block {
    std::vector<uint8_t> data;
    INT64 start = 0;
    size_t length = 0;
    // When it was last read, for replacement; 0 for a block never filled.
    uint64_t used = 0;
    bool holds(INT64 at) const noexcept { return length && at >= start && at < end(); }
    INT64 end() const noexcept { return start + INT64(length); }
  };

  // The block that holds `at`, or null; the block read last is tried first.
  Block *find(INT64 at) noexcept {
    if (cache_[last_].holds(at)) return &cache_[last_];
    for (size_t i = 0; i < cache_.size(); ++i)
      if (cache_[i].holds(at)) {
        last_ = i;
        return &cache_[i];
      }
    return nullptr;
  }

  // The block that holds `at`, which must be before the end, fetched if none does.
  Block &block_at(INT64 at) {
    Block *block = find(at);
    if (!block) block = &fill(at);
    block->used = ++clock_;
    return *block;
  }

  uint8_t byte_at(INT64 at) {
    const Block &block = block_at(at);
    return block.data[size_t(at - block.start)];
  }

  // Load the aligned block that holds `at` in place of the least recently read one.
  Block &fill(INT64 at) {
    size_t victim = 0;
    for (size_t i = 1; i < cache_.size(); ++i)
      if (cache_[i].used < cache_[victim].used) victim = i;
    Block &block = cache_[victim];
    if (block.data.empty()) block.data.resize(block_size_);
    const uint64_t start = uint64_t(at) & ~(BLOCK_ALIGN - 1);
    const uint64_t available = uint64_t(size_) - start;
    const size_t length = size_t(available < block_size_ ? available : block_size_);
    block.length = 0;
    fetch(start, block.data.data(), length);
    block.start = INT64(start);
    block.length = length;
    last_ = victim;
    return block;
  }

  // Copy `length` bytes from the position, which the caller has clamped to the end, and advance.
  void copy_out(uint8_t *dest, size_t length) {
    while (length) {
      if (Block *block = find(pos_)) {
        const size_t available = size_t(block->end() - pos_);
        const size_t n = available < length ? available : length;
        std::memcpy(dest, block->data.data() + (pos_ - block->start), n);
        block->used = ++clock_;
        pos_ += INT64(n);
        dest += n;
        length -= n;
      } else if (length >= block_size_) {
        const size_t n = length < DIRECT_PIECE ? length : DIRECT_PIECE;
        fetch(uint64_t(pos_), dest, n);
        pos_ += INT64(n);
        dest += n;
        length -= n;
      } else {
        fill(pos_);
      }
    }
  }

  // One positional read through the Rust reader, after the cancel check. Throws, and stays
  // failed, when either stops.
  void fetch(uint64_t offset, uint8_t *dest, size_t length) {
    if (failure_ == StreamFailure::Cancelled) throw LIBRAW_EXCEPTION_CANCELLED_BY_CALLBACK;
    if (failure_ == StreamFailure::Reader) throw LIBRAW_EXCEPTION_IO_CORRUPT;
    if (cancel_ && cancel_(cancel_context_)) {
      failure_ = StreamFailure::Cancelled;
      throw LIBRAW_EXCEPTION_CANCELLED_BY_CALLBACK;
    }
    if (read_(context_, offset, dest, length) != 0) {
      failure_ = StreamFailure::Reader;
      throw LIBRAW_EXCEPTION_IO_CORRUPT;
    }
  }

  const LfReadAt read_;
  void *const context_;
  const INT64 size_;
  const size_t block_size_;
  LfCancel cancel_ = nullptr;
  void *cancel_context_ = nullptr;
  INT64 pos_ = 0;
  std::vector<Block> cache_;
  size_t last_ = 0;
  uint64_t clock_ = 0;
  StreamFailure failure_ = StreamFailure::None;
};

struct CancelData {
  LfCancel callback;
  void *context;
};
int progress(void *data, enum LibRaw_progress, int, int) noexcept {
  auto *cancel = static_cast<CancelData *>(data);
  return cancel->callback && cancel->callback(cancel->context) ? 1 : 0;
}

// Test observability, per thread because each call runs synchronously on its caller's thread:
// how many preview handles are alive.
thread_local long live_handles = 0;

// LibRaw with a way to free its thumbnail buffer early. LibRaw allocates the buffer through its
// own memory manager, which tracks it and frees whatever is left when the decoder closes, so it
// must be freed through that manager (LibRaw's protected free), never with free() directly.
class PreviewLibRaw final : public LibRaw {
public:
  void release_thumbnail() noexcept {
    free(imgdata.thumbnail.thumb);
    imgdata.thumbnail.thumb = nullptr;
  }
};

// One source's LibRaw identify and thumbnail list. The stream is declared before the decoder, so
// it is destroyed after it: LibRaw keeps a pointer to the stream it opened.
struct PreviewHandle {
  LfStream stream;
  PreviewLibRaw decoder;
  // False once an extraction failed inside LibRaw, which may have recycled the decoder's state.
  bool usable = true;
  PreviewHandle(LfReadAt read, void *context, uint64_t size, size_t block, size_t blocks)
      : stream(read, context, size, block, blocks) {
    ++live_handles;
  }
  ~PreviewHandle() { --live_handles; }
  PreviewHandle(const PreviewHandle &) = delete;
  PreviewHandle &operator=(const PreviewHandle &) = delete;

  // Free the last extracted image, if any.
  void release() noexcept { decoder.release_thumbnail(); }
};

// The caller's cancel callback on the stream's fetches and on LibRaw's progress callback, for one
// synchronous call. Both point to this scope's own data and are cleared when the scope ends, on
// every return path, so the handle never keeps a pointer to a finished call's cancel token.
class CallScope {
public:
  CallScope(LfStream &stream, LibRaw &decoder, LfCancel callback, void *context) noexcept
      : stream_(stream), decoder_(decoder), data_{callback, context} {
    stream_.set_cancel(callback, context);
    decoder_.set_progress_handler(progress, &data_);
  }
  ~CallScope() {
    stream_.set_cancel(nullptr, nullptr);
    decoder_.set_progress_handler(nullptr, nullptr);
  }
  CallScope(const CallScope &) = delete;
  CallScope &operator=(const CallScope &) = delete;

private:
  LfStream &stream_;
  LibRaw &decoder_;
  CancelData data_;
};

// The status for a stream that stopped, or OK.
int stream_status(const LfStream &stream, char *err, size_t err_len) noexcept {
  switch (stream.failure()) {
  case StreamFailure::Cancelled:
    error(err, err_len, "cancelled");
    return LF_STATUS_CANCELLED;
  case StreamFailure::Reader:
    error(err, err_len, "source read failed");
    return LF_STATUS_FAILED;
  case StreamFailure::None:
    return LF_STATUS_OK;
  }
  return LF_STATUS_FAILED;
}
int libraw_status(int code, char *err, size_t err_len) noexcept {
  error(err, err_len, libraw_strerror(code));
  return code == LIBRAW_CANCELLED_BY_CALLBACK ? LF_STATUS_CANCELLED : LF_STATUS_FAILED;
}

// Fill `list` from an identified decoder, peeking the first bytes of each item LibRaw declares a
// JPEG through `peek`.
template <typename Peek>
void fill_list(const LibRaw &decoder, LfPreviewList *list, Peek peek) {
  const auto &d = decoder.imgdata;
  copy_name(list->make, sizeof(list->make), d.idata.make);
  copy_name(list->model, sizeof(list->model), d.idata.model);
  list->width = d.sizes.width;
  list->height = d.sizes.height;
  list->raw_width = d.sizes.raw_width;
  list->raw_height = d.sizes.raw_height;
  list->flip = uint32_t(d.sizes.flip);
  const int count = d.thumbs_list.thumbcount;
  list->count = count < 0 ? 0 : count > LIBRAW_THUMBNAIL_MAXCOUNT ? LIBRAW_THUMBNAIL_MAXCOUNT : uint32_t(count);
  for (uint32_t i = 0; i < list->count; ++i) {
    const auto &from = d.thumbs_list.thumblist[i];
    auto &to = list->items[i];
    to.offset = from.toffset < 0 ? 0 : uint64_t(from.toffset);
    to.length = from.tlength;
    to.format = uint32_t(from.tformat);
    to.width = from.twidth;
    to.height = from.theight;
    to.flip = from.tflip;
    to.misc = from.tmisc;
    if (from.tformat == LIBRAW_INTERNAL_THUMBNAIL_JPEG && to.offset) peek(to.offset, to.head);
  }
}

// What an extraction of `item` reads from the source and LibRaw's largest allocation for it, by
// LibRaw 0.22.2's unpack_thumb; false for a format this adapter does not extract.
bool extraction_bytes(const libraw_thumbnail_item_t &item, uint64_t &stored, uint64_t &allocated,
                      uint32_t &colors) noexcept {
  const uint64_t pixels = uint64_t(item.twidth) * item.theight;
  colors = item.tmisc >> 5 & 7;
  switch (item.tformat) {
  case LIBRAW_INTERNAL_THUMBNAIL_JPEG:
    colors = 3;
    stored = allocated = item.tlength;
    return true;
  case LIBRAW_INTERNAL_THUMBNAIL_LAYER:
    // The planes and the interleaved copy.
    stored = colors * pixels;
    allocated = 2 * stored;
    return colors == 1 || colors == 3;
  case LIBRAW_INTERNAL_THUMBNAIL_ROLLEI:
    colors = 3;
    stored = 2 * pixels;
    allocated = 5 * pixels;
    return true;
  case LIBRAW_INTERNAL_THUMBNAIL_PPM:
    // LibRaw reads the declared length, or the pixels when none is declared, as 8-bit samples
    // whatever their depth. A declared length below the pixels sends it to a multi-strip path
    // that can leave strips unread; refused, as is any other depth.
    if ((item.tmisc & 31) != 8 || (colors != 1 && colors != 3)) return false;
    stored = item.tlength ? item.tlength : colors * pixels;
    allocated = stored;
    return stored == colors * pixels;
  case LIBRAW_INTERNAL_THUMBNAIL_PPM16:
    // The 16-bit samples and their 8-bit high bytes.
    if ((item.tmisc & 31) > 16 || (colors != 1 && colors != 3)) return false;
    stored = 2 * colors * pixels;
    allocated = 3 * colors * pixels;
    return true;
  default:
    return false;
  }
}
} // namespace

// Identify the source through the Rust reader without unpacking it and list its embedded images.
// The stream caches `blocks` blocks of `block` bytes. On success the handle belongs to the caller,
// who passes it to lf_preview_extract and always to lf_preview_close; the reader context must
// stay valid until then. On failure no handle is returned and `list` stays zeroed.
extern "C" int lf_preview_open(LfReadAt read, void *read_context, uint64_t size, uint32_t block,
                               uint32_t blocks, LfCancel cancel, void *cancel_context,
                               void **handle_out, LfPreviewList *list, char *err,
                               size_t err_len) noexcept {
  if (!handle_out || !list) {
    error(err, err_len, "invalid embedded-preview arguments");
    return LF_STATUS_INVALID_INPUT;
  }
  *handle_out = nullptr;
  std::memset(list, 0, sizeof(*list));
  if (!read || !size || size > uint64_t(INT64_MAX) || block < BLOCK_ALIGN || block > MAX_BLOCK ||
      block % BLOCK_ALIGN || !blocks || blocks > MAX_BLOCKS) {
    error(err, err_len, "invalid embedded-preview source");
    return LF_STATUS_INVALID_INPUT;
  }
  if (cancel && cancel(cancel_context)) {
    error(err, err_len, "cancelled");
    return LF_STATUS_CANCELLED;
  }
  std::unique_ptr<PreviewHandle> h;
  try {
    h.reset(new PreviewHandle(read, read_context, size, block, blocks));
    LfPreviewList filled{};
    {
      CallScope scope(h->stream, h->decoder, cancel, cancel_context);
      const int code = h->decoder.open_datastream(&h->stream);
      if (const int status = stream_status(h->stream, err, err_len)) return status;
      if (code != LIBRAW_SUCCESS) return libraw_status(code, err, err_len);
      fill_list(h->decoder, &filled, [&](uint64_t offset, uint8_t head[8]) {
        h->stream.peek(offset, head, 8);
      });
    }
    if (cancel && cancel(cancel_context)) {
      error(err, err_len, "cancelled");
      return LF_STATUS_CANCELLED;
    }
    *list = filled;
    *handle_out = h.release();
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) {
    error(err, err_len, "native allocation failed");
    return LF_STATUS_ALLOCATION;
  } catch (const LibRaw_exceptions &) {
    // Only the stream throws these outside LibRaw's own handlers: a peek that stopped.
    if (h)
      if (const int status = stream_status(h->stream, err, err_len)) return status;
    error(err, err_len, "source read failed");
    return LF_STATUS_FAILED;
  } catch (const std::exception &e) {
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    error(err, err_len, "unknown native embedded-preview failure");
    return LF_STATUS_FAILED;
  }
}

// Extract thumbnail-list item `index` with LibRaw's unpack_thumb_ex, refusing before LibRaw
// allocates when its allocation would pass `max_bytes`, when the item's stored bytes run past
// the end of the source, or when the item is a format this adapter does not extract (LibRaw
// converts 16-bit bitmaps to 8 bits itself, keeping each sample's high byte, because the adapter
// never sets LIBRAW_RAWOPTIONS_USE_PPM16_THUMBS). A JPEG's stored bytes must begin with SOI:
// LibRaw writes FF D8 over the first two bytes it returns whatever they were. On success `image`
// borrows LibRaw's buffer until lf_preview_release, the next extraction or lf_preview_close.
// After a failure inside LibRaw the handle refuses further extractions.
extern "C" int lf_preview_extract(void *handle, uint32_t index, uint64_t max_bytes,
                                  LfCancel cancel, void *cancel_context, LfPreviewImage *image,
                                  char *err, size_t err_len) noexcept {
  if (!handle || !image) {
    error(err, err_len, "invalid embedded-preview arguments");
    return LF_STATUS_INVALID_INPUT;
  }
  std::memset(image, 0, sizeof(*image));
  auto *h = static_cast<PreviewHandle *>(handle);
  h->release();
  if (!h->usable) {
    error(err, err_len, "an earlier extraction failed; open the source again");
    return LF_STATUS_INVALID_INPUT;
  }
  const auto &list = h->decoder.imgdata.thumbs_list;
  if (index >= uint32_t(list.thumbcount) || index >= LIBRAW_THUMBNAIL_MAXCOUNT) {
    error(err, err_len, "no embedded image at this index");
    return LF_STATUS_INVALID_INPUT;
  }
  const libraw_thumbnail_item_t item = list.thumblist[index];
  uint64_t stored = 0, allocated = 0;
  uint32_t colors = 0;
  if (!extraction_bytes(item, stored, allocated, colors)) {
    error(err, err_len, "embedded image format is not extracted");
    return LF_STATUS_INVALID_INPUT;
  }
  const uint64_t size = uint64_t(h->stream.size());
  if (item.toffset <= 0 || uint64_t(item.toffset) > size || stored > size - uint64_t(item.toffset)) {
    error(err, err_len, "embedded image runs past the end of the source");
    return LF_STATUS_INVALID_INPUT;
  }
  if (allocated > max_bytes) {
    error(err, err_len, "embedded image exceeds the byte limit");
    return LF_STATUS_GEOMETRY;
  }
  if (cancel && cancel(cancel_context)) {
    error(err, err_len, "cancelled");
    return LF_STATUS_CANCELLED;
  }
  try {
    int code;
    {
      CallScope scope(h->stream, h->decoder, cancel, cancel_context);
      if (item.tformat == LIBRAW_INTERNAL_THUMBNAIL_JPEG) {
        uint8_t head[2]{};
        h->stream.peek(uint64_t(item.toffset), head, 2);
        if (head[0] != 0xff || head[1] != 0xd8) {
          error(err, err_len, "embedded image declared a JPEG does not begin with SOI");
          return LF_STATUS_INVALID_INPUT;
        }
      }
      // LibRaw keeps the colour count of the previous extraction for a PPM that declares none.
      h->decoder.imgdata.thumbnail.tcolors = 0;
      code = h->decoder.unpack_thumb_ex(int(index));
      if (const int status = stream_status(h->stream, err, err_len)) {
        h->usable = false;
        h->release();
        return status;
      }
    }
    if (code != LIBRAW_SUCCESS) {
      if (!(h->decoder.imgdata.progress_flags & LIBRAW_PROGRESS_IDENTIFY)) h->usable = false;
      h->release();
      return libraw_status(code, err, err_len);
    }
    const auto &t = h->decoder.imgdata.thumbnail;
    bool valid = t.thumb != nullptr;
    LfPreviewImage out{};
    out.data = reinterpret_cast<const uint8_t *>(t.thumb);
    out.length = t.tlength;
    out.width = t.twidth;
    out.height = t.theight;
    if (item.tformat == LIBRAW_INTERNAL_THUMBNAIL_JPEG) {
      out.kind = LF_PREVIEW_JPEG;
      out.colors = 3;
      valid = valid && t.tformat == LIBRAW_THUMBNAIL_JPEG && t.tlength == item.tlength;
    } else {
      out.kind = LF_PREVIEW_BITMAP;
      // LibRaw sets the colour count of every 8-bit kind; PPM16 keeps the item's.
      out.colors = item.tformat == LIBRAW_INTERNAL_THUMBNAIL_PPM16 ? colors : uint32_t(t.tcolors);
      valid = valid && t.tformat == LIBRAW_THUMBNAIL_BITMAP && out.colors == colors &&
              uint64_t(t.tlength) == uint64_t(t.twidth) * t.theight * colors;
    }
    if (!valid) {
      h->release();
      error(err, err_len, "LibRaw returned an embedded image other than the one listed");
      return LF_STATUS_FAILED;
    }
    if (out.length > max_bytes) {
      h->release();
      error(err, err_len, "embedded image exceeds the byte limit");
      return LF_STATUS_GEOMETRY;
    }
    *image = out;
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) {
    h->usable = false;
    h->release();
    error(err, err_len, "native allocation failed");
    return LF_STATUS_ALLOCATION;
  } catch (const LibRaw_exceptions &) {
    // Only the stream throws these outside LibRaw's own handlers: the SOI peek that stopped.
    h->usable = false;
    if (const int status = stream_status(h->stream, err, err_len)) return status;
    error(err, err_len, "source read failed");
    return LF_STATUS_FAILED;
  } catch (const std::exception &e) {
    h->usable = false;
    h->release();
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    h->usable = false;
    h->release();
    error(err, err_len, "unknown native embedded-preview failure");
    return LF_STATUS_FAILED;
  }
}

// Free the image the last extraction returned.
extern "C" void lf_preview_release(void *handle) noexcept {
  if (handle) static_cast<PreviewHandle *>(handle)->release();
}

extern "C" void lf_preview_close(void *handle) noexcept {
  delete static_cast<PreviewHandle *>(handle);
}

// Test observability for this thread: live preview handles.
extern "C" long lf_preview_live_handles(void) noexcept { return live_handles; }

// Test observability: whether LibRaw holds any raw image allocation, which only an unpack makes.
extern "C" int lf_preview_raw_allocated(void *handle) noexcept {
  if (!handle) return 0;
  const auto &raw = static_cast<PreviewHandle *>(handle)->decoder.imgdata.rawdata;
  return raw.raw_alloc || raw.raw_image || raw.color4_image || raw.color3_image || raw.float_image;
}

// The listing LibRaw's own buffer datastream gives for the same bytes, for tests that compare
// the two streams; the JPEG heads are read from the bytes. Production never calls it.
extern "C" int lf_preview_list_buffer(const uint8_t *bytes, size_t length, LfPreviewList *list,
                                      char *err, size_t err_len) noexcept {
  if (!bytes || !length || !list) {
    error(err, err_len, "invalid embedded-preview arguments");
    return LF_STATUS_INVALID_INPUT;
  }
  std::memset(list, 0, sizeof(*list));
  try {
    std::unique_ptr<LibRaw> decoder(new LibRaw());
    const int code = decoder->open_buffer(bytes, length);
    if (code != LIBRAW_SUCCESS) return libraw_status(code, err, err_len);
    fill_list(*decoder, list, [&](uint64_t offset, uint8_t head[8]) {
      if (offset <= length && length - offset >= 8) std::memcpy(head, bytes + offset, 8);
    });
    return LF_STATUS_OK;
  } catch (const std::exception &e) {
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    error(err, err_len, "unknown native embedded-preview failure");
    return LF_STATUS_FAILED;
  }
}

namespace {
struct MemoryReader {
  const uint8_t *bytes;
  size_t length;
};
int read_memory(void *context, uint64_t offset, uint8_t *dest, size_t length) noexcept {
  const auto *memory = static_cast<const MemoryReader *>(context);
  if (offset > memory->length || length > memory->length - offset) return 1;
  std::memcpy(dest, memory->bytes + offset, length);
  return 0;
}
} // namespace

// Drive LfStream and LibRaw's buffer datastream over the same bytes with one sequence of
// `operations` pseudo-random calls from `seed` and report the first call whose result or
// position differs, for tests of the stream's semantics. gets is compared only where both write
// the same bytes: a line that ends within the buffer, before the end of the data. Production
// never calls it.
extern "C" int lf_preview_stream_matches_buffer(const uint8_t *bytes, size_t length,
                                                uint32_t block, uint32_t blocks, uint32_t seed,
                                                uint32_t operations, char *err,
                                                size_t err_len) noexcept {
  if (!bytes || !length || block < BLOCK_ALIGN || block % BLOCK_ALIGN || !blocks ||
      blocks > MAX_BLOCKS) {
    error(err, err_len, "invalid stream comparison");
    return LF_STATUS_INVALID_INPUT;
  }
  try {
    MemoryReader memory{bytes, length};
    LfStream stream(read_memory, &memory, length, block, blocks);
    LibRaw_buffer_datastream buffer(bytes, length);
    uint32_t state = seed ? seed : 1;
    auto next = [&]() {
      state = state * 1664525u + 1013904223u;
      return state >> 8;
    };
    const INT64 span = INT64(length) + 64;
    char message[160];
    for (uint32_t op = 0; op < operations; ++op) {
      const uint32_t kind = next() % 7;
      int ours = 0, theirs = 0;
      switch (kind) {
      case 0: { // seek
        const int whence = int(next() % 3);
        const INT64 offset = INT64(next() % uint32_t(2 * span)) - span;
        ours = stream.seek(offset, whence);
        theirs = buffer.seek(offset, whence);
        break;
      }
      case 1: { // read
        const size_t item = 1 + next() % 4;
        const size_t items = next() % 3 == 0 ? next() % (2 * block / item + 2) : next() % 9;
        std::vector<uint8_t> a(item * items + 1, 0xa5), b(item * items + 1, 0xa5);
        ours = stream.read(a.data(), item, items);
        theirs = buffer.read(b.data(), item, items);
        if (a != b) ours = theirs + 1;
        break;
      }
      case 2: // get_char
        ours = stream.get_char();
        theirs = buffer.get_char();
        break;
      case 3: { // gets, where the line fits and ends before the data does
        const int sz = int(2 + next() % 40);
        const INT64 at = stream.tell();
        const void *newline =
            at < INT64(length)
                ? std::memchr(bytes + at, '\n', size_t(std::min<INT64>(sz - 1, INT64(length) - at)))
                : nullptr;
        if (!newline || static_cast<const uint8_t *>(newline) + 1 >= bytes + length) continue;
        char a[48] = {}, b[48] = {};
        const char *ra = stream.gets(a, sz);
        const char *rb = buffer.gets(b, sz);
        ours = ra ? 1 : 0;
        theirs = rb ? 1 : 0;
        if (std::strcmp(a, b)) ours = theirs + 1;
        break;
      }
      case 4: { // scanf_one
        int a = 0, b = 0;
        ours = stream.scanf_one("%d", &a);
        theirs = buffer.scanf_one("%d", &b);
        if (a != b) ours = theirs + 1;
        break;
      }
      case 5:
        ours = stream.eof();
        theirs = buffer.eof();
        break;
      default:
        ours = int(stream.size() != buffer.size());
        theirs = 0;
        break;
      }
      if (ours != theirs || stream.tell() != buffer.tell()) {
        std::snprintf(message, sizeof(message),
                      "operation %u (kind %u): result %d against %d, position %lld against %lld",
                      op, kind, ours, theirs, (long long)stream.tell(), (long long)buffer.tell());
        error(err, err_len, message);
        return LF_STATUS_FAILED;
      }
    }
    return LF_STATUS_OK;
  } catch (const std::exception &e) {
    error(err, err_len, e.what());
    return LF_STATUS_FAILED;
  } catch (...) {
    error(err, err_len, "stream comparison threw");
    return LF_STATUS_FAILED;
  }
}
