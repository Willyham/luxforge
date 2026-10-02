// Private C ABI. Rust owns every input and output buffer and the decoder handle: lf_raw_open
// creates it, Rust's drop guard destroys it through lf_raw_close on every path, and no C++
// pointer escapes beyond it. The caller's encoded bytes stay borrowed until the handle closes.
#include "libraw/libraw.h"
#include "librtprocess.h"
#include "camera_allowlist.h"
#include "rawspeed_decoders.h"
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <memory>
#include <new>
#include <type_traits>
#include <vector>

extern "C" {
typedef int (*LfCancel)(void *);
struct LfCancelState { LfCancel callback; void *context; };

// What LibRaw's identify decides, returned by lf_raw_open before any unpack work so Rust can
// classify the recording mode. Rust's NativeIdentity has the same layout; its documentation
// records why each field is final at identify, and Rust re-validates them after unpack.
struct LfIdentity {
  char make[64], model[64], decoder[80];
  uint32_t width, height, raw_bps, dng_version, decoder_flags, raw_count;
  uint32_t cfa_width, cfa_height;
  uint8_t cfa[36], black_cfa[36];
  uint32_t channels;
};
static_assert(sizeof(LfIdentity) == 316, "LfIdentity layout differs from Rust");

struct LfMetadata {
  char make[64], model[64], decoder[80];
  uint32_t width, height, raw_pitch, raw_bps, dng_version, decoder_flags;
  uint32_t active_x, active_y, active_width, active_height;
  uint32_t inset_x, inset_y, inset_width, inset_height;
  uint32_t cfa_width, cfa_height, flip, raw_count, channels;
  uint8_t cfa[36];
  // LibRaw's four Bayer sites are retained for black calibration. The public
  // CFA below merges both green sites to channel 1 for demosaicing.
  uint8_t black_cfa[36];
  float black_base, black_channels[4];
  uint32_t black_repeat_width, black_repeat_height;
  float black_repeat[4096];
  float white, as_shot[3], rgb_cam[12], cam_xyz[12];
};

// What the demosaic reads of a frame; Rust's DemosaicShape has the same layout.
struct LfDemosaicShape {
  uint32_t width, height, cfa_width, cfa_height;
  uint8_t cfa[36];
  float rgb_cam[12];
};
static_assert(sizeof(LfDemosaicShape) == 100, "LfDemosaicShape layout differs from Rust");

// The decoder that fills the mosaic in lf_raw_unpack; Rust's NativeUnpacker has the same values.
// Any other value is refused before unpack.
enum LfUnpacker : uint32_t { LF_UNPACKER_LIBRAW = 0, LF_UNPACKER_RAWSPEED = 1, LF_UNPACKER_EXTERNAL_JXL = 2 };

// What RawSpeed decoded (rawspeed_adapter.cpp).
struct LfRawSpeedImage { uint32_t width, height, components, u16; };
// RawSpeed's decode of the borrowed bytes into LibRaw's raw buffer (rawspeed_adapter.cpp).
int lf_rawspeed_unpack(const uint8_t *bytes, size_t byte_length, uint16_t *dest, size_t stride,
                       uint32_t width, uint32_t height, const uint16_t *map, LfCancel cancel,
                       void *cancel_context, LfRawSpeedImage *image, char *err,
                       size_t err_len) noexcept;
}

extern "C" bool lf_tile_cancel(void *context) noexcept {
  const auto *state = static_cast<const LfCancelState *>(context);
  return state->callback && state->callback(state->context);
}

namespace {
void error(char *dst, size_t len, const char *message) noexcept;

// The LibRaw object each handle owns: LibRaw with the catalog's configured calibration and the
// adapter's RawSpeed substitute for a replaceable LibRaw decoder.
class AdapterLibRaw final : public LibRaw {
public:
  bool apply_xyz_to_camera(const double configured[9]) {
    if (imgdata.idata.colors != 3) return false;
    double cam_xyz[4][3]{};
    for (unsigned row = 0; row < 3; ++row)
      for (unsigned column = 0; column < 3; ++column)
        cam_xyz[row][column] = configured[row * 3 + column];
    for (unsigned row = 0; row < 4; ++row)
      for (unsigned column = 0; column < 3; ++column)
        imgdata.color.cam_xyz[row][column] = static_cast<float>(cam_xyz[row][column]);
    cam_xyz_coeff(imgdata.color.rgb_cam, cam_xyz);
    return true;
  }

  // Why RawSpeed cannot replace `row`'s LibRaw decoder exactly on this identified file, or null.
  // Every row needs a one-channel colour filter array mosaic (so LibRaw's unpack allocates its
  // raw buffer the way it does for the replaced decoder, whatever decoder flags the substitute
  // reports) and no Fujifilm SuperCCD rotation; the row's guard adds what its decoder needs.
  const char *rawspeed_refusal(const LfReplaceable &row) const noexcept {
    const auto &sizes = imgdata.sizes;
    const auto &unpacker = libraw_internal_data.unpacker_data;
    if (!imgdata.idata.filters)
      return "RawSpeed replaces LibRaw's decoder only for a colour filter array mosaic";
    if (libraw_internal_data.internal_output_params.fuji_width)
      return "RawSpeed does not replace LibRaw's decoder for a rotated Fujifilm layout";
    switch (row.guard) {
    case LF_GUARD_NONE:
      return nullptr;
    case LF_GUARD_NIKON_ROWS:
      return sizes.height == sizes.raw_height
                 ? nullptr
                 : "LibRaw's Nikon decoder leaves the rows below the image height unwritten";
    case LF_GUARD_LOSSLESS_JPEG_PLACEMENT:
      return (unpacker.load_flags & 1) || sizes.raw_width == 3984
                 ? "LibRaw's lossless JPEG decoder interleaves or shifts this file's samples"
                 : nullptr;
    case LF_GUARD_DNG_SINGLE_FRAME:
      return unpacker.tiff_samples == 1 && imgdata.idata.raw_count == 1
                 ? nullptr
                 : "LibRaw's DNG decoder selects one of several frames or samples";
    case LF_GUARD_PANASONIC_C6_BLOCKS: {
      const unsigned block = unpacker.pana_bpp == 12 ? 14 : 11;
      return sizes.raw_height % 16 == 0 && sizes.raw_width % block == 0
                 ? nullptr
                 : "LibRaw's Panasonic C6 decoder leaves a partial strip or block unwritten";
    }
    }
    return "unknown replaceable-decoder guard";
  }

  // LibRaw's unpack with RawSpeed in place of `row`'s decoder. LibRaw allocates its raw buffer,
  // calls the substitute where it would call its decoder, and runs every later step unchanged.
  // `bytes` are the encoded bytes this object opened, borrowed for the call. A RawSpeed failure
  // is a LibRaw decode failure whose status and message rawspeed_status and rawspeed_message
  // then report; there is no fallback to LibRaw's decoder.
  int unpack_with_rawspeed(const LfReplaceable &row, const uint8_t *bytes, size_t length,
                           LfCancel cancel, void *cancel_context) {
    Substitution substitution{&row, bytes, length, cancel, cancel_context, load_raw};
    // Restores LibRaw's decoder and forgets the substitution on every path, including an unpack
    // that fails before it reaches the substitute.
    struct Restore {
      AdapterLibRaw &self;
      ~Restore() {
        if (self.substitution_) self.load_raw = self.substitution_->replaced;
        self.substitution_ = nullptr;
      }
    } restore{*this};
    substitution_ = &substitution;
    load_raw = static_cast<void (LibRaw::*)()>(&AdapterLibRaw::rawspeed_load_raw);
    return unpack();
  }

  int rawspeed_status() const noexcept { return rawspeed_status_; }
  const char *rawspeed_message() const noexcept { return rawspeed_message_; }

private:
  struct Substitution {
    const LfReplaceable *row;
    const uint8_t *bytes;
    size_t length;
    LfCancel cancel;
    void *cancel_context;
    void (LibRaw::*replaced)();
  };

  // Runs inside LibRaw's unpack, in place of the replaced decoder, on the object's own raw
  // buffer (raw_width x raw_height at raw_pitch, allocated by unpack).
  void rawspeed_load_raw() {
    // LibRaw's steps after its decoder compare load_raw with its own decoders
    // (crop_masked_pixels' masked areas among them), so they must see the replaced one.
    load_raw = substitution_->replaced;
    const Substitution &substitution = *substitution_;
    const auto &sizes = imgdata.sizes;
    uint16_t *raw = imgdata.rawdata.raw_image;
    if (!raw || sizes.raw_pitch != unsigned(sizes.raw_width) * 2) {
      fail(LF_STATUS_FAILED, "RawSpeed: LibRaw's raw buffer is not one row of raw_width samples "
                             "per raw_pitch");
    }
    // The replaced decoder's curve rule as one value map; null writes RawSpeed's values.
    std::vector<uint16_t> clamped;
    const uint16_t *map = nullptr;
    switch (substitution.row->curve) {
    case LF_CURVE_UNCHANGED:
      break;
    case LF_CURVE_LIBRAW:
      map = imgdata.color.curve;
      break;
    case LF_CURVE_LIBRAW_CLAMPED_14:
      clamped.resize(0x10000);
      for (unsigned value = 0; value < 0x10000; ++value)
        clamped[value] = imgdata.color.curve[std::min(value, 0x3fffu)];
      map = clamped.data();
      break;
    default:
      fail(LF_STATUS_FAILED, "RawSpeed: unknown curve rule");
    }
    LfRawSpeedImage image{};
    char text[512] = {};
    const int status = lf_rawspeed_unpack(
        substitution.bytes, substitution.length, raw, sizes.raw_pitch / 2, sizes.raw_width,
        sizes.raw_height, map, substitution.cancel, substitution.cancel_context, &image, text,
        sizeof(text));
    if (status == LF_STATUS_OK) return;
    if (status == LF_STATUS_CANCELLED) throw LIBRAW_EXCEPTION_CANCELLED_BY_CALLBACK;
    char decoded[96] = "nothing decoded";
    if (image.width)
      std::snprintf(decoded, sizeof(decoded), "decoded %ux%u with %u component(s) of %s",
                    image.width, image.height, image.components, image.u16 ? "u16" : "float");
    // RawSpeed prefixes its exception text with the throwing function's signature and line;
    // the message keeps the text after them.
    const char *what = text;
    if (const char *line = std::strstr(text, ", line "))
      if (const char *colon = std::strstr(line, ": ")) what = colon + 2;
    char message[sizeof(rawspeed_message_)];
    std::snprintf(message, sizeof(message), "RawSpeed: %s (%s; expected %ux%u u16)", what,
                  decoded, unsigned(sizes.raw_width), unsigned(sizes.raw_height));
    fail(status == LF_STATUS_ALLOCATION ? LF_STATUS_ALLOCATION : LF_STATUS_FAILED, message);
  }

  // Record the failure for lf_raw_unpack and end LibRaw's unpack with a decode failure.
  [[noreturn]] void fail(int status, const char *message) {
    rawspeed_status_ = status;
    error(rawspeed_message_, sizeof(rawspeed_message_), message);
    if (status == LF_STATUS_ALLOCATION) throw LIBRAW_EXCEPTION_ALLOC;
    throw LIBRAW_EXCEPTION_DECODE_RAW;
  }

  Substitution *substitution_ = nullptr;
  int rawspeed_status_ = LF_STATUS_OK;
  char rawspeed_message_[256] = {};
};
using CameraEntry = std::remove_reference_t<decltype(lf_cameras[0])>;

// Test observability, per thread because each decode runs synchronously on its caller's thread:
// how many handles are alive and how many LibRaw unpacks have started.
thread_local long live_handles = 0;
thread_local unsigned long long unpack_calls = 0;

struct Handle {
  AdapterLibRaw decoder;
  // Set by lf_raw_open: the caller's encoded bytes, borrowed until the handle closes; LibRaw's
  // identified decoder name (static text); the catalog entry (static data, null only for the
  // test-only uncatalogued open); and the identify-time geometry that unpack must keep.
  const uint8_t *bytes = nullptr;
  size_t length = 0;
  const char *decoder_name = nullptr;
  const CameraEntry *profile = nullptr;
  bool opened = false;
  unsigned width = 0, height = 0, pitch = 0;
  // One unpack attempt per handle, successful or not.
  bool unpack_started = false, unpacked = false;
  Handle() noexcept { ++live_handles; }
  ~Handle() { --live_handles; }
  Handle(const Handle &) = delete;
  Handle &operator=(const Handle &) = delete;
};
void error(char *dst, size_t len, const char *message) noexcept {
  if (!dst || !len) return;
  std::strncpy(dst, message, len - 1);
  dst[len - 1] = 0;
}
void copy_name(char *dst, size_t len, const char *src) noexcept {
  std::memset(dst, 0, len);
  if (src) std::strncpy(dst, src, len - 1);
}
struct CancelData { LfCancel callback; void *context; };
int progress(void *data, enum LibRaw_progress, int, int) noexcept {
  auto *cancel = static_cast<CancelData *>(data);
  return cancel->callback && cancel->callback(cancel->context) ? 1 : 0;
}
// LibRaw's progress callback context is needed only during one synchronous open or unpack call.
// It points to this scope's own value and is cleared when the scope ends, on every return path,
// so the handle never keeps a pointer to a finished call's stack or cancel token.
class ProgressScope {
public:
  ProgressScope(LibRaw &decoder, LfCancel callback, void *context) noexcept
      : decoder_(decoder), data_{callback, context} {
    decoder_.set_progress_handler(progress, &data_);
  }
  ~ProgressScope() { decoder_.set_progress_handler(nullptr, nullptr); }
  ProgressScope(const ProgressScope &) = delete;
  ProgressScope &operator=(const ProgressScope &) = delete;
private:
  LibRaw &decoder_;
  CancelData data_;
};
int status_of(int code) noexcept {
  return code == LIBRAW_CANCELLED_BY_CALLBACK ? LF_STATUS_CANCELLED : LF_STATUS_FAILED;
}
// The CFA dimensions and pattern, with LibRaw's four Bayer sites kept in black_cfa and both
// greens merged to channel 1 in cfa. False for a site outside LibRaw's four channels.
bool read_cfa(LibRaw &decoder, uint32_t &width, uint32_t &height, uint8_t cfa[36],
              uint8_t black_cfa[36], uint32_t &channels) noexcept {
  channels = 1;
  const auto &idata = decoder.imgdata.idata;
  if (!idata.filters) {
    width = height = 0;
    channels = idata.colors;
    return channels == 1 || (channels == 3 && idata.dng_version != 0);
  }
  if (idata.filters == 9) {
    width = 6; height = 6;
    for (unsigned y = 0; y < 6; ++y)
      for (unsigned x = 0; x < 6; ++x) {
        // DNG stores its full-frame CFAPattern in xtrans; xtrans_abs is RAF-only.
        cfa[y * 6 + x] = idata.dng_version ? idata.xtrans[y][x] : idata.xtrans_abs[y][x];
        black_cfa[y * 6 + x] = cfa[y * 6 + x];
      }
    return true;
  }
  width = 2; height = 2;
  for (unsigned y = 0; y < 2; ++y)
    for (unsigned x = 0; x < 2; ++x) {
      const int col = decoder.COLOR(y, x);
      if (col < 0 || col > 3) return false;
      cfa[y * 2 + x] = col == 3 ? 1 : col;
      black_cfa[y * 2 + x] = static_cast<uint8_t>(col);
    }
  return true;
}
}

namespace {
// lf_raw_open, with the catalog check only when `catalogued`.
int open_handle(const uint8_t *bytes, size_t length, LfCancel cancel, void *cancel_context,
                void **handle_out, LfIdentity *identity, char *err, size_t err_len,
                bool catalogued) noexcept {
  if (!bytes || !length || !handle_out || !identity || length > LF_MAX_SOURCE_BYTES) {
    error(err, err_len, "invalid or oversized RAW input"); return LF_STATUS_INVALID_INPUT;
  }
  *handle_out = nullptr;
  std::memset(identity, 0, sizeof(*identity));
  if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return LF_STATUS_CANCELLED; }
  try {
    std::unique_ptr<Handle> h(new Handle());
    h->decoder.imgdata.rawparams.max_raw_memory_mb = 512;
    // Primary sensor image only. The exact frame count is also a profile mode
    // selector; a second Dual Pixel image is never allocated or blended.
    h->decoder.imgdata.rawparams.shot_select = 0;
    {
      ProgressScope scope(h->decoder, cancel, cancel_context);
      const int code = h->decoder.open_buffer(bytes, length);
      if (code != LIBRAW_SUCCESS) { error(err, err_len, libraw_strerror(code)); return status_of(code); }
    }
    libraw_decoder_info_t opened{};
    if (h->decoder.get_decoder_info(&opened) != LIBRAW_SUCCESS || !opened.decoder_name) {
      error(err, err_len, "LibRaw selected no decoder"); return LF_STATUS_FAILED;
    }
    // Defence in depth behind Rust's container check: LibRaw's High Efficiency decoder reads
    // nothing, so refuse the file for any model before unpack.
    if (std::strcmp(opened.decoder_name, "nikon_he_load_raw()") == 0) {
      error(err, err_len, "LibRaw selected its Nikon High Efficiency decoder");
      return LF_STATUS_NIKON_HIGH_EFFICIENCY;
    }
    // This bounded upstream decoder has a larger, owner-approved working set.
    // Every other decoder retains 512 MiB; the file and pixel bounds remain shared.
    if (std::strcmp(opened.decoder_name, "sony_arw6_load_raw()") == 0)
      h->decoder.imgdata.rawparams.max_raw_memory_mb = 1024;
    const auto &d = h->decoder.imgdata;
    const auto *profile = std::find_if(std::begin(lf_cameras),std::end(lf_cameras),[&](const auto &camera){
      return std::strcmp(d.idata.make,camera.make)==0 && std::strcmp(d.idata.model,camera.model)==0;
    });
    if(catalogued && profile == std::end(lf_cameras)){error(err,err_len,"camera model is outside the RAW catalog");return LF_STATUS_UNSUPPORTED_MODE;}
    if(d.idata.raw_count<1||d.idata.raw_count>2){error(err,err_len,"RAW frame count exceeds primary-frame mode bounds");return LF_STATUS_UNSUPPORTED_MODE;}
    const auto &s = d.sizes;
    const uint64_t n=uint64_t(s.raw_width)*s.raw_height;
    if (!s.raw_width || !s.raw_height || s.raw_width>LF_MAX_SIDE || s.raw_height>LF_MAX_SIDE || n>LF_MAX_PIXELS) {
      error(err, err_len, "RAW dimensions or stride exceed adapter limits"); return LF_STATUS_GEOMETRY;
    }
    if (!read_cfa(h->decoder, identity->cfa_width, identity->cfa_height, identity->cfa, identity->black_cfa, identity->channels)) {
      std::memset(identity, 0, sizeof(*identity));
      error(err, err_len, "invalid Bayer CFA channel"); return LF_STATUS_UNSUPPORTED_CFA;
    }
    copy_name(identity->make, sizeof(identity->make), d.idata.make);
    copy_name(identity->model, sizeof(identity->model), d.idata.model);
    copy_name(identity->decoder, sizeof(identity->decoder), opened.decoder_name);
    identity->width = s.raw_width; identity->height = s.raw_height;
    identity->raw_bps = d.color.raw_bps; identity->dng_version = d.idata.dng_version;
    identity->decoder_flags = opened.decoder_flags; identity->raw_count = d.idata.raw_count;
    h->bytes = bytes; h->length = length; h->decoder_name = opened.decoder_name;
    h->profile = profile == std::end(lf_cameras) ? nullptr : profile;
    h->opened = true;
    h->width = s.raw_width; h->height = s.raw_height; h->pitch = s.raw_pitch;
    *handle_out = h.release();
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) { std::memset(identity,0,sizeof(*identity));error(err,err_len,"native allocation failed");return LF_STATUS_ALLOCATION; }
    catch (const std::exception &e) { std::memset(identity,0,sizeof(*identity));error(err,err_len,e.what());return LF_STATUS_FAILED; }
    catch (...) { std::memset(identity,0,sizeof(*identity));error(err,err_len,"unknown native decoder failure");return LF_STATUS_FAILED; }
}
}

// Identify without unpacking: open the caller's bytes, refuse what the adapter never unpacks
// (LibRaw's High Efficiency decoder, a camera outside the catalog, a frame count or size outside
// the bounds), and return what classification reads. On success the handle belongs to the caller,
// who passes it to lf_raw_unpack and always to lf_raw_close; on failure no handle is returned.
// The bytes stay borrowed until the handle closes.
extern "C" int lf_raw_open(const uint8_t *bytes, size_t length,
                            LfCancel cancel, void *cancel_context,
                            void **handle_out, LfIdentity *identity,
                            char *err, size_t err_len) noexcept {
  return open_handle(bytes, length, cancel, cancel_context, handle_out, identity, err, err_len,
                     true);
}

// lf_raw_open without the catalog check, for crate tests that compare the two unpackers on
// authentic files whose camera or mode the catalog does not yet hold. Production never calls it.
extern "C" int lf_raw_open_uncatalogued(const uint8_t *bytes, size_t length,
                                         LfCancel cancel, void *cancel_context,
                                         void **handle_out, LfIdentity *identity,
                                         char *err, size_t err_len) noexcept {
  return open_handle(bytes, length, cancel, cancel_context, handle_out, identity, err, err_len,
                     false);
}

// Unpack the opened file once with the selected decoder and fill the complete metadata. The
// identify-time geometry and stride must be unchanged; Rust then compares every identity field,
// the decoder among them, with the metadata filled here. On failure `meta` stays zeroed. Once
// LibRaw's unpack has started, successfully or not, the handle is never unpacked again.
//
// LF_UNPACKER_RAWSPEED runs LibRaw's unpack with the adapter's RawSpeed decode in place of the
// identified LibRaw decoder, which must be in the replaceable table (rawspeed_decoders.h) and
// whose guard must hold; otherwise it is refused before unpack and the handle stays unpackable.
// A RawSpeed failure fails the unpack with a message naming RawSpeed; LibRaw's own decoder is
// never tried instead.
extern "C" int lf_raw_unpack(void *handle, uint32_t unpacker,
                              LfCancel cancel, void *cancel_context,
                              LfMetadata *meta, char *err, size_t err_len) noexcept {
  if (!handle || !meta) { error(err, err_len, "invalid unpack arguments"); return LF_STATUS_INVALID_INPUT; }
  std::memset(meta, 0, sizeof(*meta));
  if (unpacker != LF_UNPACKER_LIBRAW && unpacker != LF_UNPACKER_RAWSPEED && unpacker != LF_UNPACKER_EXTERNAL_JXL) { error(err, err_len, "unknown RAW unpacker"); return LF_STATUS_INVALID_INPUT; }
  auto *h = static_cast<Handle *>(handle);
  if (!h->opened || h->unpack_started) { error(err, err_len, "RAW handle was already unpacked"); return LF_STATUS_INVALID_INPUT; }
  const bool external_jxl = unpacker == LF_UNPACKER_EXTERNAL_JXL;
  if (external_jxl && (!h->decoder_name || std::strcmp(h->decoder_name,"jxl_dng_load_raw_placeholder()") != 0 || h->decoder.imgdata.idata.colors != 3 || h->decoder.imgdata.idata.filters || !h->decoder.imgdata.idata.dng_version)) {
    error(err,err_len,"external JPEG XL metadata requires a three-channel DNG");return LF_STATUS_INVALID_INPUT;
  }
  const LfReplaceable *replaceable = nullptr;
  if (unpacker == LF_UNPACKER_RAWSPEED) {
    for (const auto &row : lf_replaceable)
      if (h->decoder_name && std::strcmp(row.decoder, h->decoder_name) == 0) replaceable = &row;
    if (!replaceable) {
      char message[160];
      std::snprintf(message, sizeof(message), "RawSpeed does not replace LibRaw's %s",
                    h->decoder_name ? h->decoder_name : "unnamed decoder");
      error(err, err_len, message);
      return LF_STATUS_INVALID_INPUT;
    }
    if (const char *refusal = h->decoder.rawspeed_refusal(*replaceable)) {
      error(err, err_len, refusal);
      return LF_STATUS_UNSUPPORTED_MODE;
    }
  }
  if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return LF_STATUS_CANCELLED; }
  h->unpack_started = true;
  try {
    if (!external_jxl) {
      ProgressScope scope(h->decoder, cancel, cancel_context);
      ++unpack_calls;
      const int code = replaceable
          ? h->decoder.unpack_with_rawspeed(*replaceable, h->bytes, h->length, cancel, cancel_context)
          : h->decoder.unpack();
      if (code != LIBRAW_SUCCESS) {
        if (h->decoder.rawspeed_status() != LF_STATUS_OK) {
          error(err, err_len, h->decoder.rawspeed_message());
          return h->decoder.rawspeed_status();
        }
        error(err, err_len, libraw_strerror(code));
        return status_of(code);
      }
    }
    if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return LF_STATUS_CANCELLED; }
    const auto &d=h->decoder.imgdata;
    const auto &s=d.sizes;
    const unsigned channels = !d.idata.filters ? d.idata.colors : 1;
    const unsigned storage_channels = channels == 3 && d.rawdata.color4_image ? 4 : channels;
    if(s.raw_width!=h->width||s.raw_height!=h->height||
       (channels == 1 && h->pitch!=0&&s.raw_pitch!=h->pitch)||
       (!external_jxl && (s.raw_pitch<unsigned(s.raw_width)*storage_channels*2||s.raw_pitch%2))){
      error(err,err_len,"decoder changed mosaic geometry during unpack");return LF_STATUS_GEOMETRY;
    }
    if (!external_jxl && (d.rawdata.float_image || d.rawdata.float3_image || d.rawdata.float4_image ||
        (channels == 1 ? (!d.rawdata.raw_image || d.rawdata.color3_image || d.rawdata.color4_image)
                       : ((!d.rawdata.color3_image && !d.rawdata.color4_image) || d.rawdata.raw_image)))) {
      error(err, err_len, "decoder did not return the identified integer sensor layout"); return LF_STATUS_UNSUPPORTED_CFA;
    }
    if (h->profile && h->profile->calibrated && !h->decoder.apply_xyz_to_camera(h->profile->xyz_to_camera)) {
      error(err, err_len, "configured calibration requires three camera colours"); return LF_STATUS_UNSUPPORTED_CFA;
    }
    libraw_decoder_info_t decoder_info{};
    const int code=h->decoder.get_decoder_info(&decoder_info);
    if (code != LIBRAW_SUCCESS) { error(err,err_len,libraw_strerror(code)); return LF_STATUS_FAILED; }
    LfMetadata filled{};
    if (!read_cfa(h->decoder, filled.cfa_width, filled.cfa_height, filled.cfa, filled.black_cfa, filled.channels)) {
      error(err,err_len,"invalid Bayer CFA channel");return LF_STATUS_UNSUPPORTED_CFA;
    }
    copy_name(filled.make,sizeof(filled.make),d.idata.make);
    copy_name(filled.model,sizeof(filled.model),d.idata.model);
    copy_name(filled.decoder,sizeof(filled.decoder),decoder_info.decoder_name);
    filled.width=s.raw_width;filled.height=s.raw_height;filled.raw_pitch=s.raw_pitch;
    filled.raw_bps=d.color.raw_bps;filled.dng_version=d.idata.dng_version;
    filled.decoder_flags=decoder_info.decoder_flags;filled.raw_count=d.idata.raw_count;
    filled.active_x=s.left_margin;filled.active_y=s.top_margin;
    filled.active_width=s.width;filled.active_height=s.height;
    const auto &inset=s.raw_inset_crops[0];
    filled.inset_x=inset.cleft;filled.inset_y=inset.ctop;
    filled.inset_width=inset.cwidth;filled.inset_height=inset.cheight;
    filled.flip=s.flip;
    filled.black_base=d.color.black;
    for(unsigned i=0;i<4;++i)filled.black_channels[i]=d.color.cblack[i];
    filled.black_repeat_height=d.color.cblack[4];
    filled.black_repeat_width=d.color.cblack[5];
    // LibRaw may retain one dimension after folding a uniform repeat into black.
    // A zero product means there are no repeat entries.
    if (!filled.black_repeat_width || !filled.black_repeat_height)
      filled.black_repeat_width = filled.black_repeat_height = 0;
    const uint64_t repeat=uint64_t(filled.black_repeat_width)*filled.black_repeat_height;
    if (repeat>4096) {error(err,err_len,"black pattern exceeds bound");return LF_STATUS_GEOMETRY;}
    for(size_t i=0;i<repeat;++i)filled.black_repeat[i]=d.color.cblack[6+i];
    filled.white=d.color.maximum;
    for(unsigned i=0;i<3;++i)filled.as_shot[i]=d.color.cam_mul[i];
    for(unsigned y=0;y<3;++y)for(unsigned x=0;x<4;++x)filled.rgb_cam[y*4+x]=d.color.rgb_cam[y][x];
    for(unsigned y=0;y<4;++y)for(unsigned x=0;x<3;++x)filled.cam_xyz[y*3+x]=d.color.cam_xyz[y][x];
    // Published only once every check has passed.
    *meta=filled;
    h->unpacked = true;
    return LF_STATUS_OK;
  } catch (const std::bad_alloc &) { error(err,err_len,"native allocation failed");return LF_STATUS_ALLOCATION; }
    catch (const std::exception &e) { error(err,err_len,e.what());return LF_STATUS_FAILED; }
    catch (...) { error(err,err_len,"unknown native decoder failure");return LF_STATUS_FAILED; }
}

extern "C" int lf_raw_copy(void *handle, uint16_t *dest, size_t length,
                            char *err,size_t err_len) noexcept {
  if (!handle || !dest) {error(err,err_len,"invalid mosaic copy arguments");return LF_STATUS_INVALID_INPUT;}
  try {
    const auto *h=static_cast<Handle*>(handle);
    const auto &d=h->decoder.imgdata;
    const auto&s=d.sizes;
    if(!h->unpacked){error(err,err_len,"RAW handle is not unpacked");return LF_STATUS_INVALID_INPUT;}
    const unsigned channels = !d.idata.filters ? d.idata.colors : 1;
    const unsigned storage_channels = channels == 3 && d.rawdata.color4_image ? 4 : channels;
    const uint16_t *source = channels == 3 ? reinterpret_cast<const uint16_t *>(d.rawdata.color4_image ? static_cast<void *>(d.rawdata.color4_image) : static_cast<void *>(d.rawdata.color3_image)) : d.rawdata.raw_image;
    if(!source || length!=uint64_t(s.raw_width)*s.raw_height*channels){error(err,err_len,"sensor sample length mismatch");return LF_STATUS_INVALID_INPUT;}
    for(unsigned y=0;y<s.raw_height;++y) {
      const uint16_t *row = source+size_t(y)*(s.raw_pitch/2);
      uint16_t *out = dest+size_t(y)*s.raw_width*channels;
      if(storage_channels == channels) std::memcpy(out,row,size_t(s.raw_width)*channels*2);
      else for(unsigned x=0;x<s.raw_width;++x) for(unsigned c=0;c<3;++c) out[size_t(x)*3+c]=row[size_t(x)*4+c];
    }
    return LF_STATUS_OK;
  } catch (const std::exception &e) {error(err,err_len,e.what());return LF_STATUS_FAILED;}
    catch (...) {error(err,err_len,"unknown native copy failure");return LF_STATUS_FAILED;}
}

extern "C" void lf_raw_close(void *handle) noexcept { delete static_cast<Handle*>(handle); }

// LibRaw's 65536-entry linearization table after unpack; the decoders that
// apply it wrote curve[stored value] into the mosaic. Crate tests compare it
// with RawSpeed's uncorrected values.
extern "C" int lf_raw_curve(void *handle, uint16_t *dest, size_t length) noexcept {
  if (!handle || !dest) return LF_STATUS_INVALID_INPUT;
  const auto &curve=static_cast<Handle*>(handle)->decoder.imgdata.color.curve;
  if (length != sizeof(curve)/sizeof(curve[0])) return LF_STATUS_INVALID_INPUT;
  std::memcpy(dest,curve,sizeof(curve));
  return LF_STATUS_OK;
}

// Test observability for this thread: live decoder handles, and LibRaw unpacks started.
extern "C" long lf_raw_live_handles(void) noexcept { return live_handles; }
extern "C" unsigned long long lf_raw_unpack_calls(void) noexcept { return unpack_calls; }

// Demosaic Rust's normalized float mosaic, in which sensor white is 65535, into
// three planes at the same scale. Rust owns the mosaic and planes, normalizes
// before this call and divides the planes by 65535 after it.
extern "C" int lf_raw_develop(const float *mosaic,size_t count,const LfDemosaicShape *shape,
                               float *red,float *green,float *blue,
                               rpTileExecutor executor,void *executor_context,
                               LfCancel cancel,void *cancel_context,
                               char *err,size_t err_len) noexcept {
  if(!mosaic||!shape||!red||!green||!blue||!shape->width||!shape->height||
     count!=uint64_t(shape->width)*shape->height||count>LF_MAX_PIXELS||
     count>LF_MAX_RGB_BYTES/(3*sizeof(float))) {
    error(err,err_len,"invalid or oversized develop buffers");return LF_STATUS_INVALID_INPUT;
  }
  // The pinned X-Trans tile code requires one full 114px tile to initialize
  // scratch before its final-edge calculation. Qualified sensors exceed this.
  if (shape->cfa_width==6 && (shape->width<120 || shape->height<120)) {
    error(err,err_len,"X-Trans sensor below native tile minimum"); return LF_STATUS_GEOMETRY;
  }
  // The pinned Bayer border pass fills a 9 px band from each pixel's 3x3
  // neighbourhood without bounding the band's far side, so a side below
  // 10 px would index outside its rows. Qualified sensors exceed this.
  if (shape->cfa_width==2 && (shape->width<10 || shape->height<10)) {
    error(err,err_len,"Bayer sensor below native border minimum"); return LF_STATUS_GEOMETRY;
  }
  if(cancel&&cancel(cancel_context)){error(err,err_len,"cancelled");return LF_STATUS_CANCELLED;}
  try{
    const size_t w=shape->width,h=shape->height;
    std::vector<const float*> input_rows(h);
    std::vector<float*> rrows(h),grows(h),brows(h);
    unsigned bayer[2][2]{},xtrans[6][6]{};
    if(shape->cfa_width==2&&shape->cfa_height==2){
      for(size_t y=0;y<2;++y)for(size_t x=0;x<2;++x)bayer[y][x]=shape->cfa[y*2+x];
    }else if(shape->cfa_width==6&&shape->cfa_height==6){
      for(size_t y=0;y<6;++y)for(size_t x=0;x<6;++x)xtrans[y][x]=shape->cfa[y*6+x];
    }else{error(err,err_len,"unsupported CFA");return LF_STATUS_UNSUPPORTED_CFA;}
    for(size_t y=0;y<h;++y){
      input_rows[y]=mosaic+y*w;rrows[y]=red+y*w;grows[y]=green+y*w;brows[y]=blue+y*w;
    }
    LfCancelState cancel_state{cancel,cancel_context};
    // librtprocess ignores this progress return; both demosaics check
    // cancel_state between tiles instead.
    auto no_cancel=[](double){return false;};
    rpError code=RP_WRONG_CFA;
    if(shape->cfa_width==2)
      code=rcd_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),bayer,no_cancel,2,false,false,executor,executor_context,lf_tile_cancel,&cancel_state);
    else{
      float cam[3][4]{};for(size_t i=0;i<12;++i)cam[i/4][i%4]=shape->rgb_cam[i];
      code=markesteijn_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),xtrans,cam,no_cancel,1,false,2,false,executor,executor_context,lf_tile_cancel,&cancel_state);
    }
    if(code!=RP_NO_ERROR){
      error(err,err_len,"float demosaic failed");
      return code==RP_MEMORY_ERROR?LF_STATUS_ALLOCATION:code==RP_CANCELLED?LF_STATUS_CANCELLED:code==RP_WORKER_ERROR?LF_STATUS_FAILED:LF_STATUS_UNSUPPORTED_CFA;
    }
    if(cancel&&cancel(cancel_context)){error(err,err_len,"cancelled after demosaic");return LF_STATUS_CANCELLED;}
    return LF_STATUS_OK;
  }catch(const std::bad_alloc&){error(err,err_len,"native allocation failed");return LF_STATUS_ALLOCATION;}
   catch(const std::exception&e){error(err,err_len,e.what());return LF_STATUS_FAILED;}
   catch(...){error(err,err_len,"unknown native develop failure");return LF_STATUS_FAILED;}
}
