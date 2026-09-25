// Private C ABI. Rust owns every input and output buffer; no C++ pointer escapes
// beyond the temporary decoder handle, which is destroyed before decode returns.
#include "libraw/libraw.h"
#include "librtprocess.h"
#include "camera_allowlist.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <new>
#include <vector>

extern "C" {
typedef int (*LwCancel)(void *);
struct LwCancelState { LwCancel callback; void *context; };

struct LwMosaicCorrection { uint32_t index; uint16_t value; };
struct LwDevelopDiagnostics {
  uint64_t normalization_ns;
  uint64_t demosaic_ns;
  float *normalized_mosaic_capture;
};

struct LwMetadata {
  char make[64], model[64], decoder[80];
  uint32_t width, height, raw_pitch, raw_bps, dng_version, decoder_flags;
  uint32_t active_x, active_y, active_width, active_height;
  uint32_t inset_x, inset_y, inset_width, inset_height;
  uint32_t cfa_width, cfa_height, flip, raw_count;
  uint8_t cfa[36];
  // LibRaw's four Bayer sites are retained for black calibration. The public
  // CFA below merges both green sites to channel 1 for demosaicing.
  uint8_t black_cfa[36];
  float black_base, black_channels[4];
  uint32_t black_repeat_width, black_repeat_height;
  float black_repeat[4096];
  float white, as_shot[3], rgb_cam[12], cam_xyz[12];
};
}

extern "C" bool lw_tile_cancel(void *context) noexcept {
  const auto *state = static_cast<const LwCancelState *>(context);
  return state->callback && state->callback(state->context);
}

namespace {
class CalibratingLibRaw final : public LibRaw {
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
};
struct Handle { CalibratingLibRaw decoder; };
void error(char *dst, size_t len, const char *message) noexcept {
  if (!dst || !len) return;
  std::strncpy(dst, message, len - 1);
  dst[len - 1] = 0;
}
void copy_name(char *dst, size_t len, const char *src) noexcept {
  std::memset(dst, 0, len);
  if (src) std::strncpy(dst, src, len - 1);
}
struct CancelData { LwCancel callback; void *context; };
int progress(void *data, enum LibRaw_progress, int, int) noexcept {
  auto *cancel = static_cast<CancelData *>(data);
  return cancel->callback && cancel->callback(cancel->context) ? 1 : 0;
}
struct NormalizeRows {
  const uint16_t *samples;
  const LwMetadata *meta;
  const LwMosaicCorrection *patches;
  size_t patch_count;
  const float *gains;
  float *mosaic;
  float *red;
  float *green;
  float *blue;
  const unsigned (*bayer)[2];
  const unsigned (*xtrans)[6];
  const LwCancelState *cancel;
  size_t width;
  size_t height;
  std::vector<const float *> *input_rows;
  std::vector<float *> *red_rows;
  std::vector<float *> *green_rows;
  std::vector<float *> *blue_rows;
  std::atomic<int> status{0};
};
constexpr size_t kNormalizeRowBatch = 16;
constexpr size_t kNormalizeParallelPixelThreshold = 1024 * 1024;

void normalize_rows(void *opaque, size_t batch) noexcept {
  auto *state = static_cast<NormalizeRows *>(opaque);
  const size_t first_row = batch * kNormalizeRowBatch;
  const size_t end_row = std::min(first_row + kNormalizeRowBatch, state->height);
  const auto &meta = *state->meta;
  for (size_t y = first_row; y < end_row; ++y) {
    if (state->status.load(std::memory_order_relaxed) != 0) return;
    if (state->cancel->callback && state->cancel->callback(state->cancel->context)) {
      int expected = 0;
      state->status.compare_exchange_strong(expected, 2, std::memory_order_relaxed);
      return;
    }
    (*state->input_rows)[y] = state->mosaic + y * state->width;
    (*state->red_rows)[y] = state->red + y * state->width;
    (*state->green_rows)[y] = state->green + y * state->width;
    (*state->blue_rows)[y] = state->blue + y * state->width;
    const size_t row_start = y * state->width;
    const LwMosaicCorrection *patch = nullptr;
    const LwMosaicCorrection *patch_end = nullptr;
    if (state->patch_count) {
      patch = std::lower_bound(
          state->patches, state->patches + state->patch_count, row_start,
          [](const LwMosaicCorrection &correction, size_t index) { return correction.index < index; });
      patch_end = state->patches + state->patch_count;
    }
    const size_t row_end = row_start + state->width;
    for (size_t x = 0; x < state->width; ++x) {
      const size_t index = row_start + x;
      const unsigned channel = meta.cfa_width == 2
          ? state->bayer[y % 2][x % 2]
          : state->xtrans[y % 6][x % 6];
      const unsigned black_channel = meta.cfa_width == 2
          ? meta.black_cfa[(y % 2) * 2 + (x % 2)]
          : meta.black_cfa[(y % 6) * 6 + (x % 6)];
      if (channel > 2 || black_channel > 3) {
        int expected = 0;
        state->status.compare_exchange_strong(expected, 5, std::memory_order_relaxed);
        return;
      }
      float black = meta.black_base + meta.black_channels[black_channel];
      if (meta.black_repeat_width && meta.black_repeat_height) {
        const size_t ix = (y % meta.black_repeat_height) * meta.black_repeat_width +
                          (x % meta.black_repeat_width);
        black += meta.black_repeat[ix];
      }
      const float denominator = meta.white - black;
      if (!std::isfinite(denominator) || denominator <= 0.f) {
        int expected = 0;
        state->status.compare_exchange_strong(expected, 5, std::memory_order_relaxed);
        return;
      }
      uint16_t sample = state->samples[index];
      if (patch && patch != patch_end && patch->index == index && patch->index < row_end) {
        sample = patch->value;
        ++patch;
      }
      const float normalized = (float(sample) - black) * (65535.f / denominator) * state->gains[channel];
      state->mosaic[index] = normalized;
    }
  }
}
// LibRaw's callback context is needed only for the synchronous open/unpack.
// It points to a stack value in lw_raw_open and is cleared before return.
}

extern "C" int lw_raw_open(const uint8_t *bytes, size_t length,
                            LwCancel cancel, void *cancel_context,
                            void **handle_out, LwMetadata *meta,
                            char *err, size_t err_len) noexcept {
  if (!bytes || !length || !handle_out || !meta || length > LW_MAX_SOURCE_BYTES) {
    error(err, err_len, "invalid or oversized RAW input"); return 1;
  }
  *handle_out = nullptr;
  std::memset(meta, 0, sizeof(*meta));
  if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return 2; }
  try {
    CancelData cd{cancel, cancel_context};
    std::unique_ptr<Handle> h(new Handle());
    h->decoder.imgdata.rawparams.max_raw_memory_mb = 512;
    // Primary sensor image only. The exact frame count is also a profile mode
    // selector; a second Dual Pixel image is never allocated or blended.
    h->decoder.imgdata.rawparams.shot_select = 0;
    h->decoder.set_progress_handler(progress, &cd);
    int code=h->decoder.open_buffer(bytes, length);
    if (code != LIBRAW_SUCCESS) { error(err, err_len, libraw_strerror(code)); return code == LIBRAW_CANCELLED_BY_CALLBACK ? 2 : 3; }
    const auto &identity=h->decoder.imgdata.idata;
    const auto *profile = std::find_if(std::begin(lw_cameras),std::end(lw_cameras),[&](const auto &camera){
      return std::strcmp(identity.make,camera.make)==0 && std::strcmp(identity.model,camera.model)==0;
    });
    if(profile == std::end(lw_cameras)){error(err,err_len,"camera model is outside the RAW catalog");return 7;}
    if(identity.raw_count<1||identity.raw_count>2){error(err,err_len,"RAW frame count exceeds primary-frame mode bounds");return 7;}
    const auto &s=h->decoder.imgdata.sizes;
    const uint64_t n=uint64_t(s.raw_width)*s.raw_height;
    if (!s.raw_width || !s.raw_height || s.raw_width>LW_MAX_SIDE || s.raw_height>LW_MAX_SIDE || n>LW_MAX_PIXELS) {
      error(err, err_len, "RAW dimensions or stride exceed adapter limits"); return 4;
    }
    const unsigned before_width=s.raw_width,before_height=s.raw_height,before_pitch=s.raw_pitch;
    code=h->decoder.unpack();
    h->decoder.set_progress_handler(nullptr, nullptr);
    if (code != LIBRAW_SUCCESS) { error(err, err_len, libraw_strerror(code)); return code == LIBRAW_CANCELLED_BY_CALLBACK ? 2 : 3; }
    if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return 2; }
    const auto &d=h->decoder.imgdata;
    const auto &after=d.sizes;
    if(after.raw_width!=before_width||after.raw_height!=before_height||
       (before_pitch!=0&&after.raw_pitch!=before_pitch)||
       after.raw_pitch<unsigned(after.raw_width)*2||after.raw_pitch%2){
      error(err,err_len,"decoder changed mosaic geometry during unpack");return 4;
    }
    if (!d.rawdata.raw_image || d.rawdata.float_image || d.rawdata.color4_image || d.rawdata.color3_image) {
      error(err, err_len, "decoder did not return a single-channel integer mosaic"); return 5;
    }
    if (profile->calibrated && !h->decoder.apply_xyz_to_camera(profile->xyz_to_camera)) {
      error(err, err_len, "configured calibration requires three camera colours"); return 5;
    }
    libraw_decoder_info_t decoder_info{};
    code=h->decoder.get_decoder_info(&decoder_info);
    if (code != LIBRAW_SUCCESS) { error(err,err_len,libraw_strerror(code)); return 3; }
    copy_name(meta->make,sizeof(meta->make),d.idata.make);
    copy_name(meta->model,sizeof(meta->model),d.idata.model);
    copy_name(meta->decoder,sizeof(meta->decoder),decoder_info.decoder_name);
    meta->width=s.raw_width;meta->height=s.raw_height;meta->raw_pitch=s.raw_pitch;
    meta->raw_bps=d.color.raw_bps;meta->dng_version=d.idata.dng_version;
    meta->decoder_flags=decoder_info.decoder_flags;meta->raw_count=d.idata.raw_count;
    meta->active_x=s.left_margin;meta->active_y=s.top_margin;
    meta->active_width=s.width;meta->active_height=s.height;
    const auto &inset=s.raw_inset_crops[0];
    meta->inset_x=inset.cleft;meta->inset_y=inset.ctop;
    meta->inset_width=inset.cwidth;meta->inset_height=inset.cheight;
    meta->flip=s.flip;
    if (d.idata.filters==9) {
      meta->cfa_width=6;meta->cfa_height=6;
      for(unsigned y=0;y<6;++y)for(unsigned x=0;x<6;++x){
        meta->cfa[y*6+x]=d.idata.xtrans_abs[y][x];
        meta->black_cfa[y*6+x]=meta->cfa[y*6+x];
      }
    } else {
      meta->cfa_width=2;meta->cfa_height=2;
      for(unsigned y=0;y<2;++y)for(unsigned x=0;x<2;++x){
        int col=h->decoder.COLOR(y,x);meta->cfa[y*2+x]=col==3?1:col;
        if(col<0||col>3){error(err,err_len,"invalid Bayer CFA channel");return 5;}
        meta->black_cfa[y*2+x]=static_cast<uint8_t>(col);
      }
    }
    meta->black_base=d.color.black;
    for(unsigned i=0;i<4;++i)meta->black_channels[i]=d.color.cblack[i];
    meta->black_repeat_height=d.color.cblack[4];
    meta->black_repeat_width=d.color.cblack[5];
    const uint64_t repeat=uint64_t(meta->black_repeat_width)*meta->black_repeat_height;
    if (repeat>4096) {error(err,err_len,"black pattern exceeds bound");return 4;}
    for(size_t i=0;i<repeat;++i)meta->black_repeat[i]=d.color.cblack[6+i];
    meta->white=d.color.maximum;
    for(unsigned i=0;i<3;++i)meta->as_shot[i]=d.color.cam_mul[i];
    for(unsigned y=0;y<3;++y)for(unsigned x=0;x<4;++x)meta->rgb_cam[y*4+x]=d.color.rgb_cam[y][x];
    for(unsigned y=0;y<4;++y)for(unsigned x=0;x<3;++x)meta->cam_xyz[y*3+x]=d.color.cam_xyz[y][x];
    *handle_out=h.release();
    return 0;
  } catch (const std::bad_alloc &) { error(err,err_len,"native allocation failed");return 6; }
    catch (const std::exception &e) { error(err,err_len,e.what());return 3; }
    catch (...) { error(err,err_len,"unknown native decoder failure");return 3; }
}

extern "C" int lw_raw_copy(void *handle, uint16_t *dest, size_t length,
                            char *err,size_t err_len) noexcept {
  if (!handle || !dest) {error(err,err_len,"invalid mosaic copy arguments");return 1;}
  try {
    auto &d=static_cast<Handle*>(handle)->decoder.imgdata;
    const auto&s=d.sizes;
    if(!d.rawdata.raw_image || length!=uint64_t(s.raw_width)*s.raw_height){error(err,err_len,"mosaic length mismatch");return 1;}
    for(unsigned y=0;y<s.raw_height;++y)
      std::memcpy(dest+size_t(y)*s.raw_width,d.rawdata.raw_image+size_t(y)*(s.raw_pitch/2),size_t(s.raw_width)*2);
    return 0;
  } catch (const std::exception &e) {error(err,err_len,e.what());return 3;}
    catch (...) {error(err,err_len,"unknown native copy failure");return 3;}
}

extern "C" void lw_raw_close(void *handle) noexcept { delete static_cast<Handle*>(handle); }

extern "C" int lw_raw_develop(const uint16_t *samples,size_t count,
                               const LwMetadata *meta,const LwMosaicCorrection *patches,size_t patch_count,
                               const float gains[3],
                               float *red,float *green,float *blue,
                               unsigned test_fault,
                               rpTileExecutor executor,void *executor_context,
                               LwCancel cancel,void *cancel_context,
                               char *err,size_t err_len,
                               LwDevelopDiagnostics *diagnostics) noexcept {
  using Clock = std::chrono::steady_clock;
  if (diagnostics) {
    diagnostics->normalization_ns = 0;
    diagnostics->demosaic_ns = 0;
  }
  if(!samples||!meta||!gains||!red||!green||!blue||!meta->width||!meta->height||
     count!=uint64_t(meta->width)*meta->height||count>LW_MAX_PIXELS||
     count>LW_MAX_RGB_BYTES/(3*sizeof(float))) {
    error(err,err_len,"invalid or oversized develop buffers");return 1;
  }
  if (patch_count>65536 || (patch_count && !patches)) {
    error(err,err_len,"invalid sparse mosaic corrections"); return 1;
  }
  // The pinned X-Trans tile code requires one full 114px tile to initialize
  // scratch before its final-edge calculation. Qualified sensors exceed this.
  if (meta->cfa_width==6 && (meta->width<120 || meta->height<120)) {
    error(err,err_len,"X-Trans sensor below native tile minimum"); return 4;
  }
  // The pinned Bayer border pass fills a 9 px band from each pixel's 3x3
  // neighbourhood without bounding the band's far side, so a side below
  // 10 px would index outside its rows. Qualified sensors exceed this.
  if (meta->cfa_width==2 && (meta->width<10 || meta->height<10)) {
    error(err,err_len,"Bayer sensor below native border minimum"); return 4;
  }
  for(size_t i=0;i<patch_count;++i) {
    if(patches[i].index>=count || (i && patches[i-1].index>=patches[i].index)) {
      error(err,err_len,"invalid sparse mosaic correction ordering"); return 1;
    }
  }
  if(cancel&&cancel(cancel_context)){error(err,err_len,"cancelled");return 2;}
  try{
    const size_t w=meta->width,h=meta->height;
    std::vector<float> mosaic(count);
    std::vector<const float*> input_rows(h);
    std::vector<float*> rrows(h),grows(h),brows(h);
    unsigned bayer[2][2]{},xtrans[6][6]{};
    if(meta->cfa_width==2&&meta->cfa_height==2){
      for(size_t y=0;y<2;++y)for(size_t x=0;x<2;++x)bayer[y][x]=meta->cfa[y*2+x];
    }else if(meta->cfa_width==6&&meta->cfa_height==6){
      for(size_t y=0;y<6;++y)for(size_t x=0;x<6;++x)xtrans[y][x]=meta->cfa[y*6+x];
    }else{error(err,err_len,"unsupported CFA");return 5;}
    const Clock::time_point normalization_start = diagnostics ? Clock::now() : Clock::time_point{};
    LwCancelState cancel_state{cancel,cancel_context};
    NormalizeRows normalization{
      samples, meta, patches, patch_count, gains, mosaic.data(), red, green, blue,
      bayer, xtrans, &cancel_state, w, h,
      &input_rows, &rrows, &grows, &brows
    };
    const size_t batches = (h + kNormalizeRowBatch - 1) / kNormalizeRowBatch;
    const bool parallel_bayer_normalization =
        executor && meta->cfa_width == 2 && count >= kNormalizeParallelPixelThreshold && batches > 1;
    int executor_status = 0;
    if (parallel_bayer_normalization) {
      executor_status = executor(executor_context, batches, normalize_rows, &normalization);
    } else {
      size_t patch_index = 0;
      for (size_t y = 0; y < h; ++y) {
        input_rows[y] = mosaic.data() + y * w;
        rrows[y] = red + y * w;
        grows[y] = green + y * w;
        brows[y] = blue + y * w;
        if (cancel && y % 128 == 0 && cancel(cancel_context)) {
          error(err, err_len, "cancelled");
          return 2;
        }
        for (size_t x = 0; x < w; ++x) {
          const unsigned channel = meta->cfa_width == 2 ? bayer[y % 2][x % 2] : xtrans[y % 6][x % 6];
          const unsigned black_channel = meta->cfa_width == 2
              ? meta->black_cfa[(y % 2) * 2 + (x % 2)]
              : meta->black_cfa[(y % 6) * 6 + (x % 6)];
          if (channel > 2) { error(err, err_len, "invalid CFA channel"); return 5; }
          if (black_channel > 3) { error(err, err_len, "invalid black CFA channel"); return 5; }
          float black = meta->black_base + meta->black_channels[black_channel];
          if (meta->black_repeat_width && meta->black_repeat_height) {
            const size_t ix = (y % meta->black_repeat_height) * meta->black_repeat_width +
                              (x % meta->black_repeat_width);
            black += meta->black_repeat[ix];
          }
          // Sensor scale 65535 is the established numerical contract.
          // No pre-demosaic clamp: sampled under-black and over-white latitude is retained.
          const float denominator = meta->white - black;
          if (!std::isfinite(denominator) || denominator <= 0.f) {
            error(err, err_len, "invalid black/white denominator");
            return 5;
          }
          uint16_t sample = samples[y * w + x];
          if (patch_index < patch_count && patches[patch_index].index == y * w + x) {
            sample = patches[patch_index++].value;
          }
          mosaic[y * w + x] = (float(sample) - black) * (65535.f / denominator) * gains[channel];
        }
      }
    }
    if (diagnostics) {
      diagnostics->normalization_ns = static_cast<uint64_t>(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() - normalization_start).count());
    }
    const int normalization_status = normalization.status.load(std::memory_order_relaxed);
    if (executor_status == 2 || normalization_status == 2) { error(err,err_len,"cancelled"); return 2; }
    if (executor_status != 0) { error(err,err_len,"normalization worker failed"); return 3; }
    if (normalization_status == 5) { error(err,err_len,"invalid CFA or black/white denominator"); return 5; }
    if (normalization_status != 0) { error(err,err_len,"normalization failed"); return 3; }
    if (diagnostics && diagnostics->normalized_mosaic_capture)
      std::memcpy(diagnostics->normalized_mosaic_capture, mosaic.data(), count * sizeof(float));
    const Clock::time_point demosaic_start = diagnostics ? Clock::now() : Clock::time_point{};
    // librtprocess ignores this progress return; both demosaics check
    // cancel_state between tiles instead.
    auto no_cancel=[](double){return false;};
    rpError code=RP_WRONG_CFA;
    if(meta->cfa_width==2)
      code=rcd_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),bayer,no_cancel,2,false,false,executor,executor_context,lw_tile_cancel,&cancel_state,test_fault);
    else{
      float cam[3][4]{};for(size_t i=0;i<12;++i)cam[i/4][i%4]=meta->rgb_cam[i];
      code=markesteijn_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),xtrans,cam,no_cancel,1,false,2,false,executor,executor_context,lw_tile_cancel,&cancel_state,test_fault);
    }
    if (diagnostics) {
      diagnostics->demosaic_ns = static_cast<uint64_t>(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() - demosaic_start).count());
    }
    if(code!=RP_NO_ERROR){
      error(err,err_len,"float demosaic failed");
      return code==RP_MEMORY_ERROR?6:code==RP_CANCELLED?2:code==RP_WORKER_ERROR?3:5;
    }
    if(cancel&&cancel(cancel_context)){error(err,err_len,"cancelled after demosaic");return 2;}
    // No finiteness scan here: the caller checks every value once, where it
    // adopts the planes after the camera conversion, and a non-finite camera
    // value makes each converted channel of its pixel non-finite.
    for(size_t i=0;i<count;++i){red[i]/=65535.f;green[i]/=65535.f;blue[i]/=65535.f;}
    return 0;
  }catch(const std::bad_alloc&){error(err,err_len,"native allocation failed");return 6;}
   catch(const std::exception&e){error(err,err_len,e.what());return 3;}
   catch(...){error(err,err_len,"unknown native develop failure");return 3;}
}
