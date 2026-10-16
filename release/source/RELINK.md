# Changing and relinking a bundled library

Every native library in Luxforge {{version}} is linked statically into the executables. This archive holds the complete source of the program and of each bundled library (ONNX Runtime excepted, below), so you modify a library by changing its source here and rebuilding: Cargo recompiles the changed library and links new executables. There are no separate object files to relink.

After any change, rebuild as `BUILD.md` describes:

```sh
cargo build --release --locked --offline -p luxforge-app -p luxforge-cli
```

## The LGPL libraries: LibRaw and RawSpeed

LibRaw 0.22.2 (LGPL-2.1, also offered under CDDL-1.0) and RawSpeed (LGPL-2.0-or-later code; `data/cameras.xml` under CC-BY-SA-3.0) are compiled by `crates/luxforge-raw/build.rs` from the pristine trees in `crates/luxforge-raw/vendor/`:

- `vendor/libraw-0.22.2/` is staged into Cargo's build directory, where `patches/libraw-arw6.patch` is applied and `patches/sony_arw6.cpp` is added;
- `vendor/rawspeed-c835b05a/` and `vendor/pugixml-1.16/` are compiled into one static library, without `src/librawspeed/common/Common.cpp`, whose log function `native/rawspeed_adapter.cpp` replaces;
- `native/adapter.cpp` and `native/rawspeed_adapter.cpp` are Luxforge's private C interfaces to them.

To use a modified LibRaw or RawSpeed, edit the files in its vendored tree, or replace the tree with another release under the same directory name. A staged patch that no longer applies stops the build with an error naming it: rebase the patch in `crates/luxforge-raw/patches/` or remove it from `build.rs`. `crates/luxforge-raw/THIRD_PARTY.md` lists every build flag and which files are compiled.

librtprocess (GPL-3.0-or-later) is built the same way from `vendor/librtprocess-9a858270/`, with `patches/librtprocess-local.patch` and `patches/librtprocess-mosaic.patch`.

## Libraries inside vendored crates

libjpeg-turbo (`vendor/mozjpeg-sys-2.2.3/`), libwebp (`vendor/libwebp-sys-0.14.4/`), SQLite (`vendor/libsqlite3-sys-*/`) and the other C code in `vendor/` are compiled by their crates' build scripts. Cargo checks each vendored crate's files against its `.cargo-checksum.json`, so after editing a file in `vendor/NAME-VERSION/`, remove that file's entry from the `"files"` object of `vendor/NAME-VERSION/.cargo-checksum.json` (or empty the object) before rebuilding.

## The generative runtime

stable-diffusion.cpp and ggml (MIT) are compiled by `crates/luxforge-diffusion/build.rs` from `crates/luxforge-diffusion/vendor/`, with one staged file changed by exact-anchor replacement and Luxforge's own `native/` sources (`crates/luxforge-diffusion/THIRD_PARTY.md`).

## ONNX Runtime

ONNX Runtime 1.28.0 (MIT) is linked from the prebuilt static libraries in `third_party/onnxruntime-1.28.0/`, not built from this archive. To link your own build of ONNX Runtime instead, build it as a static library and point `ort-sys` at the directory holding it:

```sh
ORT_LIB_LOCATION=/path/to/onnxruntime/lib cargo build --release --locked --offline -p luxforge-app -p luxforge-cli
```

`ORT_LIB_LOCATION` takes precedence over the bundled libraries. The `ort` 2.0.0-rc.13 crate expects the ONNX Runtime 1.28 API; its documentation at <https://ort.pyke.io/setup/linking> describes the libraries it links.
