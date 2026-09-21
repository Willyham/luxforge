// Compare full-float as-shot channel gains before vs after established demosaicers.
#include "libraw/libraw.h"
#include "librtprocess.h"
#include <algorithm>
#include <cmath>
#include <fstream>
#include <filesystem>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

int main(int argc,char **argv){
 try{
  if(argc!=4)throw std::runtime_error("usage: wb_probe SOURCE {rcd|markesteijn1|xtransfast} NEW_RESULT_JSON");
  std::string alg(argv[2]);LibRaw decoder;
  int status=decoder.open_file(argv[1]);if(status)throw std::runtime_error(libraw_strerror(status));
  status=decoder.unpack();if(status)throw std::runtime_error(libraw_strerror(status));
  const auto &d=decoder.imgdata;const auto&s=d.sizes;
  if(!d.rawdata.raw_image||s.raw_width<2040||s.raw_height<2040)throw std::runtime_error("mosaic too small");
  constexpr int w=2040,h=2040;const size_t n=size_t(w)*h;
  const int phase=alg=="rcd"?2:6;
  const int x0=((int(s.raw_width)-w)/2/phase)*phase,y0=((int(s.raw_height)-h)/2/phase)*phase;
  float gain[3]={d.color.cam_mul[0],d.color.cam_mul[1],d.color.cam_mul[2]};
  if(gain[1]<=0)throw std::runtime_error("no green WB");
  for(float &v:gain)v/=d.color.cam_mul[1];
  unsigned cfa2[2][2];for(int y=0;y<2;++y)for(int x=0;x<2;++x){int c=decoder.COLOR(y,x);cfa2[y][x]=c==3?1:c;}
  unsigned cfa6[6][6];for(int y=0;y<6;++y)for(int x=0;x<6;++x)cfa6[y][x]=d.idata.xtrans[y][x];
  std::vector<float> input(n),r(n),g(n),b(n),r0(n),g0(n),b0(n);
  std::vector<const float*>inrows(h);std::vector<float*>rr(h),gr(h),br(h);
  for(int y=0;y<h;++y){inrows[y]=input.data()+size_t(y)*w;rr[y]=r.data()+size_t(y)*w;gr[y]=g.data()+size_t(y)*w;br[y]=b.data()+size_t(y)*w;
   const auto*row=d.rawdata.raw_image+size_t(y+y0)*(s.raw_pitch/2)+x0;
   for(int x=0;x<w;++x){
    unsigned channel=alg=="rcd"?cfa2[y%2][x%2]:cfa6[y%6][x%6];
    float black=d.color.black+d.color.cblack[channel];
    if(d.color.cblack[4]&&d.color.cblack[5]){
     unsigned rh=d.color.cblack[4],rw=d.color.cblack[5];
     black+=d.color.cblack[6+((y+y0)%rh)*rw+((x+x0)%rw)];
    }
    input[size_t(y)*w+x]=float(row[x])-black;
   }}
  auto progress=[](double){return false;};
  auto demosaic=[&](){
   if(alg=="rcd")return rcd_demosaic(w,h,inrows.data(),rr.data(),gr.data(),br.data(),cfa2,progress,2,false,false);
   if(alg=="markesteijn1")return markesteijn_demosaic(w,h,inrows.data(),rr.data(),gr.data(),br.data(),cfa6,d.color.rgb_cam,progress,1,false,2,false);
   if(alg=="xtransfast")return xtransfast_demosaic(w,h,inrows.data(),rr.data(),gr.data(),br.data(),cfa6,progress);
   throw std::runtime_error("algorithm");
  };
  if(demosaic()!=RP_NO_ERROR)throw std::runtime_error("baseline demosaic error");
  r0=r;g0=g;b0=b;
  for(int y=0;y<h;++y)for(int x=0;x<w;++x){size_t i=size_t(y)*w+x;
   int c=alg=="rcd"?cfa2[y%2][x%2]:cfa6[y%6][x%6];input[i]*=gain[c];}
  if(demosaic()!=RP_NO_ERROR)throw std::runtime_error("pre-gain demosaic error");
  std::vector<float> differences;differences.reserve(3*n);
  double sum=0,sum2=0,edge_sum=0;size_t edge_count=0,cfa_mismatch=0;
  float maxdiff=0;
  for(int y=0;y<h;++y)for(int x=0;x<w;++x){size_t i=size_t(y)*w+x;
   int c=alg=="rcd"?cfa2[y%2][x%2]:cfa6[y%6][x%6];
   const float before[3]={r[i],g[i],b[i]},after[3]={r0[i]*gain[0],g0[i]*gain[1],b0[i]*gain[2]};
   if(before[c]!=input[i])++cfa_mismatch;
   bool edge=x>0&&std::abs(input[i]-input[i-1])>500;
   for(int k=0;k<3;++k){float delta=std::abs(before[k]-after[k]);differences.push_back(delta);
    maxdiff=std::max(maxdiff,delta);sum+=delta;sum2+=double(delta)*delta;
    if(edge){edge_sum+=delta;++edge_count;}}
  }
  size_t ix=95*differences.size()/100;std::nth_element(differences.begin(),differences.begin()+ix,differences.end());
  if(std::filesystem::exists(argv[3]))throw std::runtime_error("result path exists");
  std::ofstream out(argv[3]);if(!out)throw std::runtime_error("result open error");
  out<<"{\"algorithm\":\""<<alg<<"\",\"width\":"<<w<<",\"height\":"<<h
     <<",\"origin\":["<<x0<<','<<y0<<"],\"gains\":["<<gain[0]<<','<<gain[1]<<','<<gain[2]
     <<"],\"mean_abs\":"<<sum/differences.size()<<",\"rmse\":"<<std::sqrt(sum2/differences.size())
     <<",\"p95_abs\":"<<differences[ix]<<",\"max_abs\":"<<maxdiff
     <<",\"edge_mean_abs\":"<<(edge_count?edge_sum/edge_count:0)
     <<",\"edge_channel_count\":"<<edge_count<<",\"cfa_site_mismatches\":"<<cfa_mismatch<<"}\n";
  std::cout<<alg<<" mean "<<sum/differences.size()<<" p95 "<<differences[ix]<<" max "<<maxdiff<<'\n';
  return 0;
 }catch(const std::exception&e){std::cerr<<e.what()<<'\n';return 1;}
}
