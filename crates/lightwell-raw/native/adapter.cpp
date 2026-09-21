// Private C ABI. Rust owns every input and output buffer; no C++ pointer escapes
// beyond the temporary decoder handle, which is destroyed before decode returns.
#include "libraw/libraw.h"
#include "librtprocess.h"
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <new>
#include <vector>

extern "C" {
typedef int (*LwCancel)(void *);

struct LwMetadata {
  char make[64], model[64], decoder[80];
  uint32_t width, height, raw_pitch, raw_bps, dng_version, decoder_flags;
  uint32_t active_x, active_y, active_width, active_height;
  uint32_t inset_x, inset_y, inset_width, inset_height;
  uint32_t cfa_width, cfa_height, flip, raw_count;
  uint8_t cfa[36];
  float black_base, black_channels[4];
  uint32_t black_repeat_width, black_repeat_height;
  float black_repeat[4096];
  float white, as_shot[3], rgb_cam[12], cam_xyz[12];
};
}

namespace {
struct Handle { LibRaw decoder; };
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
// LibRaw's callback context is needed only for the synchronous open/unpack.
// It points to a stack value in lw_raw_open and is cleared before return.
}

extern "C" int lw_raw_open(const uint8_t *bytes, size_t length,
                            LwCancel cancel, void *cancel_context,
                            void **handle_out, LwMetadata *meta,
                            char *err, size_t err_len) noexcept {
  if (!bytes || !length || !handle_out || !meta || length > 128ull*1024*1024) {
    error(err, err_len, "invalid or oversized RAW input"); return 1;
  }
  *handle_out = nullptr;
  std::memset(meta, 0, sizeof(*meta));
  if (cancel && cancel(cancel_context)) { error(err, err_len, "cancelled"); return 2; }
  try {
    CancelData cd{cancel, cancel_context};
    std::unique_ptr<Handle> h(new Handle());
    h->decoder.imgdata.rawparams.max_raw_memory_mb = 512;
    h->decoder.set_progress_handler(progress, &cd);
    int code=h->decoder.open_buffer(bytes, length);
    if (code != LIBRAW_SUCCESS) { error(err, err_len, libraw_strerror(code)); return code == LIBRAW_CANCELLED_BY_CALLBACK ? 2 : 3; }
    const auto &identity=h->decoder.imgdata.idata;
    const bool nikon=std::strcmp(identity.make,"Nikon")==0 && std::strcmp(identity.model,"Z 6")==0;
    const bool fuji=std::strcmp(identity.make,"Fujifilm")==0 && std::strcmp(identity.model,"X100VI")==0;
    const bool dji=std::strcmp(identity.make,"DJI")==0 && std::strcmp(identity.model,"FC3411")==0;
    if(!(nikon||fuji||dji)){error(err,err_len,"camera model is outside the RAW allowlist");return 7;}
    if(identity.raw_count!=1){error(err,err_len,"multi-frame RAW containers are outside the supported mode");return 7;}
    const auto &s=h->decoder.imgdata.sizes;
    const uint64_t n=uint64_t(s.raw_width)*s.raw_height;
    if (!s.raw_width || !s.raw_height || s.raw_width>16384 || s.raw_height>16384 || n>64000000) {
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
      for(unsigned y=0;y<6;++y)for(unsigned x=0;x<6;++x)meta->cfa[y*6+x]=d.idata.xtrans_abs[y][x];
    } else {
      meta->cfa_width=2;meta->cfa_height=2;
      for(unsigned y=0;y<2;++y)for(unsigned x=0;x<2;++x){
        int col=h->decoder.COLOR(y,x);meta->cfa[y*2+x]=col==3?1:col;
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
                               const LwMetadata *meta,const float gains[3],
                               float *red,float *green,float *blue,
                               LwCancel cancel,void *cancel_context,
                               char *err,size_t err_len) noexcept {
  if(!samples||!meta||!gains||!red||!green||!blue||!meta->width||!meta->height||
     count!=uint64_t(meta->width)*meta->height||count>64000000||
     count>512ull*1024*1024/(3*sizeof(float))) {
    error(err,err_len,"invalid or oversized develop buffers");return 1;
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
    for(size_t y=0;y<h;++y){
      input_rows[y]=mosaic.data()+y*w;rrows[y]=red+y*w;grows[y]=green+y*w;brows[y]=blue+y*w;
      if(cancel&&y%128==0&&cancel(cancel_context)){error(err,err_len,"cancelled");return 2;}
      for(size_t x=0;x<w;++x){
        unsigned channel=meta->cfa_width==2?bayer[y%2][x%2]:xtrans[y%6][x%6];
        if(channel>2){error(err,err_len,"invalid CFA channel");return 5;}
        float black=meta->black_base+meta->black_channels[channel];
        if(meta->black_repeat_width&&meta->black_repeat_height){
          size_t ix=(y%meta->black_repeat_height)*meta->black_repeat_width+(x%meta->black_repeat_width);
          black+=meta->black_repeat[ix];
        }
        // Sensor scale 65535 is the established library's numerical contract.
        // No pre-demosaic clamp: sampled under-black and over-white latitude is retained.
        mosaic[y*w+x]=(float(samples[y*w+x])-black)*(65535.f/(meta->white-black))*gains[channel];
      }
    }
    auto no_cancel=[](double){return false;}; // librtprocess ignores this return.
    rpError code=RP_WRONG_CFA;
    if(meta->cfa_width==2)
      code=rcd_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),bayer,no_cancel,2,false,false);
    else{
      float cam[3][4]{};for(size_t i=0;i<12;++i)cam[i/4][i%4]=meta->rgb_cam[i];
      code=markesteijn_demosaic(w,h,input_rows.data(),rrows.data(),grows.data(),brows.data(),xtrans,cam,no_cancel,1,false,2,false);
    }
    if(code!=RP_NO_ERROR){error(err,err_len,"float demosaic failed");return code==RP_MEMORY_ERROR?6:5;}
    if(cancel&&cancel(cancel_context)){error(err,err_len,"cancelled after demosaic");return 2;}
    for(size_t i=0;i<count;++i){
      red[i]/=65535.f;green[i]/=65535.f;blue[i]/=65535.f;
      if(!std::isfinite(red[i])||!std::isfinite(green[i])||!std::isfinite(blue[i])){
        error(err,err_len,"non-finite demosaic result");return 5;
      }
    }
    return 0;
  }catch(const std::bad_alloc&){error(err,err_len,"native allocation failed");return 6;}
   catch(const std::exception&e){error(err,err_len,e.what());return 3;}
   catch(...){error(err,err_len,"unknown native develop failure");return 3;}
}
