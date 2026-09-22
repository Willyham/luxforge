// Developer-only LibRaw unpack comparison. Never linked into Lightwell.
#include "libraw/libraw.h"
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <string>

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;
static double ms(Clock::time_point a, Clock::time_point b) {
  return std::chrono::duration<double, std::milli>(b - a).count();
}
static void check(int status, const char *where) {
  if (status != LIBRAW_SUCCESS) throw std::runtime_error(std::string(where) + ": " + libraw_strerror(status));
}
static void q(std::ostream &o, const std::string &v) {
  o << '"';
  for (unsigned char c : v) {
    if (c == '"' || c == '\\') o << '\\' << c;
    else if (c >= 32 && c < 127) o << c;
    else o << '?';
  }
  o << '"';
}
template<typename T> static void array(std::ostream &o, const T *v, size_t n) {
  o << '[';
  for (size_t i = 0; i < n; ++i) { if (i) o << ','; o << +v[i]; }
  o << ']';
}
int main(int argc, char **argv) {
  try {
    if (argc != 3 && argc != 4) throw std::runtime_error("usage: libraw_probe SOURCE NEW_OUTPUT_DIRECTORY [--metadata-only]");
    const bool metadata_only = argc == 4 && std::string(argv[3]) == "--metadata-only";
    if (argc == 4 && !metadata_only) throw std::runtime_error("unknown option");
    const fs::path source(argv[1]), out(argv[2]);
    // Diagnostic corpus bounds are independent of the application's admission
    // limits: inspect large modes before deciding whether they can be supported.
    if (!fs::is_regular_file(source) || fs::file_size(source) > 512ull * 1024 * 1024)
      throw std::runtime_error("input bound");
    if (!fs::create_directory(out)) throw std::runtime_error("output directory must be new");
    LibRaw decoder;
    auto a = Clock::now();
    check(decoder.open_file(source.c_str()), "identify");
    const auto &identified = decoder.imgdata.sizes;
    if (!identified.raw_width || !identified.raw_height || identified.raw_width > 16384 ||
        identified.raw_height > 16384 || uint64_t(identified.raw_width) * identified.raw_height > 128000000)
      throw std::runtime_error("probe sensor bound");
    auto b = Clock::now();
    check(decoder.unpack(), "unpack");
    auto c = Clock::now();
    const auto &d = decoder.imgdata;
    const auto &s = d.sizes;
    libraw_decoder_info_t decoder_info{};
    check(decoder.get_decoder_info(&decoder_info), "decoder info");
    if (!d.rawdata.raw_image || !s.raw_width || !s.raw_height || s.raw_width > 16384 || s.raw_height > 16384 ||
        uint64_t(s.raw_width) * s.raw_height > 128000000 || s.raw_pitch < unsigned(s.raw_width) * 2 ||
        s.raw_pitch % 2) throw std::runtime_error("unsupported probe mosaic/stride");
    if (!metadata_only) {
    std::ofstream pixels(out / "samples.u16le", std::ios::binary | std::ios::out);
    if (!pixels) throw std::runtime_error("cannot create samples");
    for (unsigned y = 0; y < s.raw_height; ++y) {
      const auto *row = d.rawdata.raw_image + size_t(y) * (s.raw_pitch / 2);
      for (unsigned x = 0; x < s.raw_width; ++x) {
        const uint16_t v = row[x];
        const char le[2] = {char(v & 255), char(v >> 8)};
        pixels.write(le, 2);
      }
    }
    pixels.close();
    if (!pixels) throw std::runtime_error("samples write failed");
    }
    std::ofstream j(out / "result.json", std::ios::out);
    if (!j) throw std::runtime_error("cannot create result");
    j << "{\n  \"format\":1,\"backend\":\"LibRaw 0.22.2\",\"scope\":\"unpack experiment\",\n";
    j << "  \"make\":"; q(j,d.idata.make); j << ",\"model\":"; q(j,d.idata.model);
    j << ",\"raw_width\":" << s.raw_width << ",\"raw_height\":" << s.raw_height
      << ",\"width\":" << s.width << ",\"height\":" << s.height
      << ",\"top_margin\":" << s.top_margin << ",\"left_margin\":" << s.left_margin
      << ",\"raw_pitch\":" << s.raw_pitch << ",\"flip\":" << s.flip
      << ",\"filters\":" << d.idata.filters << ",\"colors\":" << d.idata.colors
      << ",\"cdesc\":"; q(j,d.idata.cdesc); j << ",\"decoder_name\":";q(j,decoder_info.decoder_name?decoder_info.decoder_name:"");j<<",\"decoder_flags\":"<<decoder_info.decoder_flags<<",\"dng_version\":" << d.idata.dng_version;
    j << ",\"raw_count\":" << d.idata.raw_count;
    j << ",\"raw_inset_crops\":[";
    for(int i=0;i<2;++i){if(i)j<<',';auto &r=s.raw_inset_crops[i];j<<'['<<r.cleft<<','<<r.ctop<<','<<r.cwidth<<','<<r.cheight<<']';}
    j << ']';
    j << ",\"cfa_6x6\":[";
    for (int y=0;y<6;++y) { if(y)j<<','; array(j,d.idata.xtrans[y],6); }
    j << "],\"cfa_2x2\":[";
    for (int y=0;y<2;++y) { if(y)j<<','; j<<'['; for(int x=0;x<2;++x){if(x)j<<',';j<<decoder.COLOR(y,x);}j<<']'; }
    j << "],\"black\":" << d.color.black << ",\"maximum\":" << d.color.maximum
      << ",\"raw_bps\":" << d.color.raw_bps
      << ",\"data_maximum\":" << d.color.data_maximum << ",\"cblack_first4\":";
    array(j,d.color.cblack,4);
    j << ",\"cblack_repeat_dimensions\":[" << d.color.cblack[4] << ',' << d.color.cblack[5] << ']';
    j << ",\"cblack_repeat_first16\":"; array(j,d.color.cblack+6,16);
    j << ",\"dng_black\":" << d.color.dng_levels.dng_black
      << ",\"dng_cblack_first4\":"; array(j,d.color.dng_levels.dng_cblack,4);
    j << ",\"dng_fblack\":" << d.color.dng_levels.dng_fblack
      << ",\"dng_fcblack_first4\":"; array(j,d.color.dng_levels.dng_fcblack,4);
    j << ",\"white_8x8\":[";
    for (int y=0;y<8;++y) { if(y)j<<',';array(j,d.color.white[y],8); }
    j << "],\"cam_mul\":"; array(j,d.color.cam_mul,4);
    j << ",\"pre_mul\":"; array(j,d.color.pre_mul,4);
    j << ",\"cam_xyz\":[";
    for(int y=0;y<4;++y){if(y)j<<',';array(j,d.color.cam_xyz[y],3);}
    j << "],\"cmatrix\":[";
    for(int y=0;y<3;++y){if(y)j<<',';array(j,d.color.cmatrix[y],4);}
    j << "],\"identify_ms\":" << ms(a,b) << ",\"unpack_ms\":" << ms(b,c) << "}\n";
    j.close();
    if (!j) throw std::runtime_error("metadata write failed");
    std::cout << d.idata.make << ' ' << d.idata.model << ": " << s.raw_width << 'x' << s.raw_height
              << "; unpack " << ms(b,c) << " ms\n";
    return 0;
  } catch (const std::exception &e) { std::cerr << e.what() << '\n'; return 1; }
}
