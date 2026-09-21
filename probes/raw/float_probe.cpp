// Developer-only native float demosaic feasibility probe. No display conversion.
#include "libraw/libraw.h"
#include "librtprocess.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <fstream>
#include <filesystem>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

using Clock=std::chrono::steady_clock;
static double ms(Clock::time_point a,Clock::time_point b){return std::chrono::duration<double,std::milli>(b-a).count();}
int main(int argc,char**argv){
  try {
    if(argc!=4)throw std::runtime_error("usage: float_probe SOURCE ALGORITHM NEW_RESULT_JSON");
    const std::string alg(argv[2]);
    LibRaw decoder;
    auto a=Clock::now();
    int rc=decoder.open_file(argv[1]);if(rc)throw std::runtime_error(libraw_strerror(rc));
    auto b=Clock::now();
    rc=decoder.unpack();if(rc)throw std::runtime_error(libraw_strerror(rc));
    auto c=Clock::now();
    const auto &d=decoder.imgdata;const auto &s=d.sizes;
    const size_t w=s.raw_width,h=s.raw_height,n=w*h;
    if(!d.rawdata.raw_image||w==0||h==0||n>64000000||s.raw_pitch<w*2||s.raw_pitch%2)throw std::runtime_error("unsupported mosaic");
    std::vector<float> raw(n),red(n),green(n),blue(n);
    std::vector<const float*> rawrows(h);
    std::vector<float*> rrows(h),grows(h),brows(h);
    for(size_t y=0;y<h;++y){
      rawrows[y]=raw.data()+y*w;rrows[y]=red.data()+y*w;grows[y]=green.data()+y*w;brows[y]=blue.data()+y*w;
      auto row=d.rawdata.raw_image+y*(s.raw_pitch/2);
      for(size_t x=0;x<w;++x)raw[y*w+x]=row[x];
    }
    unsigned bayer[2][2];for(int y=0;y<2;++y)for(int x=0;x<2;++x){int col=decoder.COLOR(y,x);bayer[y][x]=(col==3?1:col);}
    unsigned xtrans[6][6];for(int y=0;y<6;++y)for(int x=0;x<6;++x)xtrans[y][x]=d.idata.xtrans[y][x];
    // LibRaw cam_xyz can be zero for DNG; Markesteijn's optional CIELab path is disabled.
    float cam[3][4]{};for(int y=0;y<3;++y)for(int x=0;x<4;++x)cam[y][x]=d.color.rgb_cam[y][x];
    auto e=Clock::now();
    auto progress=[](double){return false;};
    rpError err=RP_WRONG_CFA;
    if(alg=="rcd")err=rcd_demosaic(w,h,rawrows.data(),rrows.data(),grows.data(),brows.data(),bayer,progress,2,false,false);
    else if(alg=="ahd")err=ahd_demosaic(w,h,rawrows.data(),rrows.data(),grows.data(),brows.data(),bayer,cam,progress);
    else if(alg=="xtransfast")err=xtransfast_demosaic(w,h,rawrows.data(),rrows.data(),grows.data(),brows.data(),xtrans,progress);
    else if(alg=="markesteijn1")err=markesteijn_demosaic(w,h,rawrows.data(),rrows.data(),grows.data(),brows.data(),xtrans,cam,progress,1,false,2,false);
    else if(alg=="markesteijn3")err=markesteijn_demosaic(w,h,rawrows.data(),rrows.data(),grows.data(),brows.data(),xtrans,cam,progress,3,false,2,false);
    else throw std::runtime_error("unknown algorithm");
    auto f=Clock::now();
    if(err!=RP_NO_ERROR)throw std::runtime_error("demosaic error "+std::to_string(err));
    double minv=INFINITY,maxv=-INFINITY;size_t negative=0,above_white=0,nonfinite=0,cfa_mismatch=0;
    double white = d.color.maximum ? d.color.maximum : 65535;
    for(size_t y=0;y<h;++y)for(size_t x=0;x<w;++x){
      const size_t i=y*w+x;
      const int channel=(alg[0]=='x'||alg[0]=='m')?xtrans[y%6][x%6]:bayer[y%2][x%2];
      const float at_cfa=(channel==0?red[i]:channel==1?green[i]:blue[i]);
      if(at_cfa!=raw[i])++cfa_mismatch;
      for(float v:{red[i],green[i],blue[i]}){
        if(!std::isfinite(v)){++nonfinite;continue;}
        minv=std::min(minv,double(v));maxv=std::max(maxv,double(v));
        negative+=(v<0);above_white+=(v>white);
      }
    }
    if(std::filesystem::exists(argv[3]))throw std::runtime_error("result path exists");
    std::ofstream j(argv[3]);if(!j)throw std::runtime_error("result open failed");
    j<<"{\"algorithm\":\""<<alg<<"\",\"width\":"<<w<<",\"height\":"<<h
     <<",\"identify_ms\":"<<ms(a,b)<<",\"unpack_ms\":"<<ms(b,c)<<",\"float_allocation_copy_ms\":"<<ms(c,e)
     <<",\"demosaic_ms\":"<<ms(e,f)<<",\"min\":"<<minv<<",\"max\":"<<maxv
     <<",\"libraw_maximum\":"<<white<<",\"negative_channels\":"<<negative
     <<",\"channels_above_libraw_maximum\":"<<above_white<<",\"nonfinite_channels\":"<<nonfinite
     <<",\"original_cfa_sample_mismatches\":"<<cfa_mismatch<<"}\n";
    std::cout<<alg<<' '<<w<<'x'<<h<<" demosaic "<<ms(e,f)<<" ms\n";
    return 0;
  }catch(const std::exception&e){std::cerr<<e.what()<<'\n';return 1;}
}
