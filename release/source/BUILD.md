# Building Luxforge {{version}} from source

This archive is the complete corresponding source of Luxforge {{version}}, release tag `{{tag}}`, commit `{{revision}}`. It builds the editor (`luxforge`) and both command lines (`luxforge-json`, `luxforge-ctl`) without network access and without a Git checkout or a Cargo source cache.{{rehearsal}}

Luxforge is free software under the GNU General Public License, version 3 or later (`LICENSE`). The bundled components keep their own licences, listed in `notices/` and described in each crate's `THIRD_PARTY.md`. The distribution licence and notice review of this release has not been completed; nothing in this archive states that it has.

## What the archive holds

| Path | Contents |
| --- | --- |
| The project tree | Every file tracked at commit `{{revision}}`, as `git archive` writes it. The one changed file is `.cargo/config.toml`: its original settings are kept, and it adds the offline source configuration and `ORT_CACHE_DIR` described below. |
| `vendor/` | The {{packages}} crates `Cargo.lock` names, as `cargo vendor --locked --versioned-dirs` writes them, each with its own licence files and Cargo's checksums. This includes the C sources the crates bundle: libjpeg-turbo in `mozjpeg-sys`, libwebp in `libwebp-sys`, SQLite in `libsqlite3-sys` and the C and assembly of `ring`. |
| `crates/luxforge-raw/vendor/`, `crates/luxforge-raw/patches/` | LibRaw 0.22.2, librtprocess, RawSpeed and pugixml, and the patches the build applies to staged copies of them (`crates/luxforge-raw/THIRD_PARTY.md`). |
| `crates/luxforge-diffusion/vendor/`, `crates/luxforge-diffusion/native/` | stable-diffusion.cpp, ggml and the sources they bundle, with Luxforge's replacement and staged changes (`crates/luxforge-diffusion/THIRD_PARTY.md`). |
| `third_party/onnxruntime-1.28.0/` | ONNX Runtime 1.28.0 as prebuilt static libraries, for the three shipping targets (below). |
| Compiled-in data | The Inter font (`crates/luxforge-ui/assets/fonts`), the Omarchy themes (`crates/luxforge-core/src/theme/omarchy/bundled`), the GeoNames place names (`crates/luxforge-core/assets/gazetteer`), the Lensfun profile index (`crates/luxforge-core/data/lensfun`) and RawSpeed's `cameras.xml`, each with its notice. |
| `notices/macos-arm64/`, `notices/linux-x86_64/`, `notices/linux-arm64/` | Each shipping target's licence texts, native and asset notices and `dependencies.json`, the component inventory `cargo xtask inventory` writes for that target's dependency graph. An inventory is not a licence review. |
| `rust-toolchain.toml`, `Cargo.lock` | The pinned Rust toolchain ({{rust}}) and the lockfile (SHA-256 `{{lock_sha256}}`). |
| `SOURCE.json` | This archive's identity and contents, in machine-readable form. |
| `BUILD.md`, `INSTALL.md`, `RELINK.md` | These instructions. |

### ONNX Runtime

The local model runtime links ONNX Runtime statically through the `ort` crate. ONNX Runtime is not built from source: the `ort-sys` build script links pyke's prebuilt static library for the target and, when it has no copy, downloads it. This archive carries the extracted libraries in the layout that build script reads, under `third_party/onnxruntime-1.28.0/ort-cache/`, and `.cargo/config.toml` points `ORT_CACHE_DIR` at it, so no download happens. Each library was extracted, when this archive was made, from the pinned archive below after its length and SHA-256 were checked:

{{onnxruntime}}

ONNX Runtime is MIT-licensed (`notices/*/native/onnxruntime-1.28.0/`); its source is Microsoft's `microsoft/onnxruntime` at tag `v1.28.0`, and `crates/luxforge-onnx/THIRD_PARTY.md` records how pyke built the archives. Windows is not a shipping target: a Windows build from this archive has no prebuilt library and needs network access for `ort-sys` or a library of your own (`RELINK.md`).

## Requirements

- **Rust {{rust}}**, through rustup: `rust-toolchain.toml` selects it. Install it before going offline (`rustup toolchain install {{rust}} --profile minimal`).
- **macOS (Apple silicon):** macOS 14 or later and the Xcode Command Line Tools (`xcode-select --install`), for clang, the C++ standard library and the macOS SDK.
- **Linux (x86-64 or ARM64):** built and tested on Ubuntu 24.04 (glibc 2.39) with `build-essential pkg-config libssl-dev libx11-dev libxkbcommon-dev libwayland-dev libvulkan-dev`. Running the editor needs a Vulkan driver and an XDG desktop portal with a file-chooser backend.

No CMake, NASM, Python or network access is needed to build.

## Build

From the directory holding this file:

```sh
cargo build --release --locked --offline -p luxforge-app -p luxforge-cli
```

The executables are `target/release/luxforge`, `target/release/luxforge-json` and `target/release/luxforge-ctl`. The configuration in `.cargo/config.toml` replaces crates.io with `vendor/`, so `--offline` works with an empty Cargo home.

Use the `--offline` flag rather than `CARGO_NET_OFFLINE`, `ORT_OFFLINE` or `ORT_SKIP_DOWNLOAD`: `ort-sys` reads those variables as an instruction not to use its prebuilt library, and the link then fails.

The release builds compile in the release version and the commit. To build the same identity:

```sh
LUXFORGE_RELEASE_VERSION={{version}} LUXFORGE_SOURCE_REVISION={{revision}} \
  cargo build --release --locked --offline -p luxforge-app -p luxforge-cli
```

Without them the executables report the development version `0.0.0`. Rebuilt executables are not expected to be bit-identical to the published ones.

## Tests

`cargo test --locked --offline -p CRATE` runs a crate's tests. Some inputs are not in this archive because they are private or licensed for local verification only, and the tests and checks that need them are left out or report the input as missing:

- the owner's RAW originals (`fixtures/raw/`), the licensed sample corpus and the manifests that name them (`--manifest`), which the RAW, GPU qualification and corpus checks read;
- JPEG fixtures generated locally (`fixtures/generated/`, `fixtures/jpg/`), which `cargo xtask generate-fixtures` recreates;
- catalogs, evidence and verification output (`artifacts/`), which runs create.

This archive is not a Git checkout. The `xtask` commands that record or check the commit, such as `package`, `release-build`, `release-source` and the GPU qualification and measurement commands, need the Git repository at the tag above. The separate packages under `services/` and `probes/` have their own lockfiles; they are not part of the shipped executables and their dependencies are not vendored.

See `INSTALL.md` to install what you built and `RELINK.md` to change a bundled library.
