# Bundled RAW native source

This crate builds its native code from the source files in `vendor/` at compile time. It does not download code, load a system LibRaw, use OpenMP, or call LibRaw's `dcraw_process` output converter. The `build.rs` selects LibRaw source and librtprocess's RCD, Markesteijn, and border routines. Compiler/runtime toolchains are still required to build the application; clean Windows and Linux packages are not yet verified.

| Component | Exact upstream | Bundled files | License selected / notices |
| --- | --- | --- | --- |
| LibRaw 0.22.2 | Release tag commit `b93f6e45c194f5df9b02a43b1af9a54b4f41f33f`; official archive SHA-256 `627928088300ecde6ca91ffd202e189203f04ad61ad12f0fe9dc57b9a7a0fb3c` | `src`, `libraw`, `internal`, README and both license files | LGPL-2.1 (LibRaw also offers CDDL-1.0); preserve `LICENSE.LGPL`, `LICENSE.CDDL`, file headers and notices |
| librtprocess | Commit `9a858270acb2096e2e403d932760ee688fcac425`; CMake project version `0.11.0` at this commit | `src`, README, `LICENSE.txt` | GPL-3.0-or-later; retain upstream copyright/file headers and license |

The source selection is pinned in the directory names and `build.rs`; changing a bundled file must be reviewed as a dependency update. For an independent local integrity check, serialize the sorted file list as JSON with each relative path and SHA-256 digest (`json.dumps(entries,sort_keys=True,separators=(',',':'))` as in the probe inventory). SHA-256 of those serialized inventories at bundling time: LibRaw `4186672bc6edcde428f2097064bb0c851e2b9b76620c7a1ab5f821e0650c080e` (104 files); librtprocess `6cf59b4c87d170e62f8475075360027f0fd9e118186e777255ddc168a3591aa9` (33 files). These inventories are provenance evidence, not a completed manual license or native security audit.

The build does not define `USE_ZLIB`, `USE_JPEG`, `USE_RAWSPEED`, `USE_DNGSDK`, `USE_LCMS`, or an OpenMP flag. It is scoped to the six qualified Nikon/Fujifilm recording-mode samples. The DJI DNG carries mandatory GainMap and WarpRectilinear opcodes that are not implemented; it is rejected before unpack and is not advertised as supported.

The C ABI in `native/adapter.cpp` is private to this crate. It consumes borrowed Rust-owned source/mosaic/output buffers synchronously, catches all C++ exceptions, and returns a status and message. `RawSource::decode` closes LibRaw and drops the encoded byte buffer after copying one bounded u16 mosaic. `develop` writes directly to one Rust-owned planar float allocation. The supported librtprocess routines ignore the cancellation callback's return value; cancellation during demosaic suppresses its result after that bounded native stage finishes. The surrounding worker must cap active/pending work and memory reservations.
