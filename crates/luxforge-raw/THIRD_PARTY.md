# Bundled RAW native source

This crate builds its native code from the source files in `vendor/` at compile time. It does not download code, load a system LibRaw, use OpenMP, or call LibRaw's `dcraw_process` output converter. The `build.rs` selects LibRaw source and librtprocess's RCD, Markesteijn, and border routines. Compiler/runtime toolchains are still required to build the application; clean Windows and Linux packages are not yet verified.

| Component | Exact upstream | Bundled files | License selected / notices |
| --- | --- | --- | --- |
| LibRaw 0.22.2 | Release tag commit `b93f6e45c194f5df9b02a43b1af9a54b4f41f33f`; official archive SHA-256 `627928088300ecde6ca91ffd202e189203f04ad61ad12f0fe9dc57b9a7a0fb3c` | `src`, `libraw`, `internal`, README and both license files | LGPL-2.1 (LibRaw also offers CDDL-1.0); preserve `LICENSE.LGPL`, `LICENSE.CDDL`, file headers and notices |
| librtprocess | Commit `9a858270acb2096e2e403d932760ee688fcac425`; CMake project version `0.11.0` at this commit | `src`, README, `LICENSE.txt` | GPL-3.0-or-later; retain upstream copyright/file headers and license |

The source selection is pinned in the directory names and `build.rs`; changing a bundled file must be reviewed as a dependency update. The build deliberately excludes LibRaw's `postprocessing_ph.cpp` alternative stub, because it is a postprocessing placeholder and must not be linked beside the selected decompressor sources; no Adobe DNG SDK source is copied. For an independent local integrity check, serialize the sorted file list as JSON with each relative path and SHA-256 digest (`json.dumps(entries,sort_keys=True,separators=(',',':'))` as in the probe inventory). SHA-256 of those serialized inventories at bundling time: LibRaw `4186672bc6edcde428f2097064bb0c851e2b9b76620c7a1ab5f821e0650c080e` (104 files); librtprocess `6cf59b4c87d170e62f8475075360027f0fd9e118186e777255ddc168a3591aa9` (33 files). These inventories are provenance evidence, not a completed manual license or native security audit.

The build does not define `USE_ZLIB`, `USE_JPEG`, `USE_RAWSPEED`, `USE_DNGSDK`, `USE_LCMS`, or an OpenMP flag. The enabled recording modes and required processing capabilities are declared in `data/cameras.json` and backed by the modern-camera evidence manifest; upstream model recognition alone does not qualify rendering. GainMap and WarpRectilinear are parsed and applied by Luxforge's Rust adapter, with interpolation semantics checked against the Adobe DNG specification and SDK source; Adobe SDK code is not bundled. The adapter intentionally retains float headroom outside the specification's per-opcode clipped range, as recorded in its interpretation metadata.

The C ABI in `native/adapter.cpp` is private to this crate. It consumes borrowed Rust-owned source/mosaic/output buffers synchronously, catches C++ exceptions, and returns a status and message. `RawSource::decode` closes LibRaw and drops the encoded byte buffer after copying one bounded u16 mosaic. `develop` writes directly to one Rust-owned planar float allocation. The adapter's one-pass Markesteijn path uses synchronous row-group callbacks on Rust's shared Rayon pool, dispatched in joined batches of at most eight; RCD remains serial. The pinned Markesteijn source and private header are locally changed only for that scheduling boundary, joined cancellation/error propagation and once-safe CIELab table initialization. Full tile rows retain their left-to-right scratch history, and the final partial row stays with its predecessor in one callback. Small frames run serially through the same admitted callback; X-Trans dimensions below 120 px fail explicitly because the pinned native edge code needs one full tile to initialize its scratch. The final border pass runs only after all worker entries join.

One-pass Markesteijn allocates `(114² × 19 + 128) × 4 = 988,208` explicit scratch bytes per active callback. A process-wide cap of eight active scratch slots therefore bounds these allocations at 7,905,664 bytes across independent `RawSource` callers. Each row-group callback allocates and releases one scratch set; a full Fuji frame has about 52 such groups, so total allocation traffic is larger than the peak. Each callback also uses stack locals, including `dcolor[3][6]` (72 bytes), plus thread-stack and allocator overhead; the explicit scratch figure is not an RSS bound. The callback holds one slot only while native tile work runs and releases it before any Rayon scope join. Cancellation is checked between X-Trans tiles and suppresses partial output; RCD still uses its existing post-demosaic cancellation check. The surrounding source worker continues to cap active/pending full-frame work and memory reservations. The local fault mode in the private C ABI is used only by crate tests; production calls pass zero.

## Camera calibration data

The OM-3 `xyz_to_camera` matrix in `data/cameras.json` is attributed to the
RawSpeed contributors' [camera data](https://github.com/darktable-org/rawspeed/blob/0a4a6fdc2da228c71c39025a2a5647b081042299/data/cameras.xml),
licensed [CC-BY-SA 3.0 Unported](https://creativecommons.org/licenses/by-sa/3.0/).
The nine values are transcribed from the OM Digital Solutions OM-3 entry and
scaled by 1/10000; no other camera's calibration is substituted. That matrix
data retains the upstream license. The complete source XML is not bundled;
its SHA-256 is `d67d32beb3acf073a4ecc521daae29545f90bf79270749e9041031dedb89d166`.
The configured matrix uses the pinned LibRaw coefficient conversion; all other
non-DNG profiles currently use pinned LibRaw calibration, and DNG profiles use
validated source matrices. Manual license review remains deferred.
