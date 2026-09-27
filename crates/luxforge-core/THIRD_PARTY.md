# Bundled JPEG encoder native source

JPEG export encodes with libjpeg-turbo's compressor as bundled and built by the `mozjpeg-sys` crate, through the `mozjpeg` crate's safe API (`crates/luxforge-core/src/export/encode.rs`). The C source comes from the pinned crate in the Cargo registry and is compiled at build time with the `cc` crate and the platform's C compiler and linked statically. Nothing is downloaded at run time, no system libjpeg is loaded, and no CMake or NASM is used.

This software is based in part on the work of the Independent JPEG Group.

| Component | Exact upstream | Built | License / notices |
| --- | --- | --- | --- |
| `mozjpeg-sys` 2.2.3 (libjpeg-turbo with Mozilla's mozjpeg additions) | crates.io package, checksum `7f0dc668bf9bf888c88e2fb1ab16a406d2c380f1d082b20d51dd540ab2aa70c1`; source commit `93e9c78d0e9afb018a224e1105f06e48aec77766` of `kornelski/mozjpeg-sys` | The library's compress and decompress core. Features `with_simd` and `unwinding` only: NEON on aarch64 from its C intrinsics and GNU assembler source through the C compiler; on x86_64 its SIMD needs NASM, which is not enabled, so the portable C path builds. No arithmetic coding, `jpegtran`, TurboJPEG API or ICC I/O sources | IJG AND BSD-3-Clause AND Zlib; the crate's `LICENSE` is collected by `cargo xtask inventory` under `licenses/mozjpeg-sys-2.2.3`, and the source files keep their upstream headers |
| `mozjpeg` 0.10.13 | crates.io package, checksum `b7891b80aaa86097d38d276eb98b3805d6280708c4e0a1e6f6aed9380c51fec9` | Rust bindings only | IJG; `licenses/mozjpeg-0.10.13` |

Export uses libjpeg's fastest profile (plain libjpeg-turbo behaviour: baseline, standard Huffman tables, no trellis quantization or progressive scans). Its errors unwind into Rust and are caught around every call. The crate's own `write_icc_profile` numbers ICC chunks from 0, which the ICC specification does not allow, so Luxforge writes its one ICC chunk with `write_marker` instead. Manual license and native review remains deferred; this file records provenance, not a completed audit.
