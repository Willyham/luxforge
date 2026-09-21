# RAW backend experiments

These probes are development tools, not Lightwell's renderer. They read local sources, write only into new output paths, and never alter an original. `run.py` verifies each input SHA-256 before and after both decoders. The full-size u16 buffers under `artifacts/raw-backend-probe/` are ignored local evidence; do not commit owner files or those buffers.

## Pinned inputs and reproduction

- Rawler `=0.8.0` is pinned by this probe's `Cargo.lock`. The Rust probe uses `raw_image(..., false)` and writes little-endian row-major u16 samples.
- LibRaw 0.22.2 source tarball SHA-256: `627928088300ecde6ca91ffd202e189203f04ad61ad12f0fe9dc57b9a7a0fb3c`; release tag commit `b93f6e45c194f5df9b02a43b1af9a54b4f41f33f`. Local archive/extraction is ignored under `private/raw-backends/`. It was built with `make -f Makefile.dist -j4` as `libraw_r.a`, without a system LibRaw runtime. Its source carries LGPL-2.1 and CDDL license files; native/license review remains outstanding.
- librtprocess source commit `9a858270acb2096e2e403d932760ee688fcac425` (project CMake version 0.11.0), GPL-3.0-or-later. It was built static with `OPTION_OMP=OFF` and no OpenMP runtime. This is a commit pin because its upstream README version and CMake project version differ; no release equivalence is assumed.
- RawSpeed was inspected through the pinned LibRaw 0.22.2 release's `RawSpeed3/README.md` and C adapter. That integration requires a separately pinned RawSpeed commit, LibRaw-specific patches and an additional native library; LibRaw documents increased memory because the whole input is buffered. It offers decode only, so it would still require the same color and demosaic pipeline. We did not build it for this gate: Rawler already supplied an independent u16 comparator, while the selected LibRaw path correctly unpacks all qualified NEF/RAF modes without a second decoder dependency. A future speed study can measure that pinned RawSpeed3 path separately.
- Host: macOS Darwin 25.5, arm64 Apple M4 Pro, 48 GiB RAM. All listed timings are one warm-filesystem release trial, not distributions or UI end-to-end timing. `/usr/bin/time -l` files record process wall and maximum RSS. The machine was also used for unrelated work; no cold OS cache or uncontended claim is made.

Build the Rust probe with `cargo build --release --locked --manifest-path probes/raw/Cargo.toml`. Build the native probes using the local static sources:

```sh
make -C private/raw-backends/LibRaw-0.22.2 -f Makefile.dist -j4
cmake -S private/raw-backends/librtprocess -B private/raw-backends/librtprocess-build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DOPTION_OMP=OFF
cmake --build private/raw-backends/librtprocess-build --parallel 4
c++ -std=c++17 -O2 -Iprivate/raw-backends/LibRaw-0.22.2 probes/raw/libraw_probe.cpp private/raw-backends/LibRaw-0.22.2/lib/libraw_r.a -lz -o private/raw-backends/libraw_probe
c++ -std=c++17 -O2 -Iprivate/raw-backends/LibRaw-0.22.2 -Iprivate/raw-backends/librtprocess/src/include probes/raw/float_probe.cpp private/raw-backends/LibRaw-0.22.2/lib/libraw_r.a private/raw-backends/librtprocess-build/src/librtprocess.a -lz -o private/raw-backends/float_probe
c++ -std=c++17 -O2 -Iprivate/raw-backends/LibRaw-0.22.2 -Iprivate/raw-backends/librtprocess/src/include probes/raw/wb_probe.cpp private/raw-backends/LibRaw-0.22.2/lib/libraw_r.a private/raw-backends/librtprocess-build/src/librtprocess.a -lz -o private/raw-backends/wb_probe
python3 probes/raw/run.py /path/to/owner/raw/directory artifacts/new-raw-probe
python3 probes/raw/compare.py artifacts/new-raw-probe
```

`compare.py` needs NumPy; the host used NumPy 2.4.3. The owner directory needs `nikon_z6.NEF`, `fujifilm_x100vi.RAF`, and `mavic_air_2s.DNG`. The four CC0 mode samples and hashes are in ignored `private/raw/manifest.json`; `run.py` picks them up automatically. The archive can be obtained from the pinned LibRaw release; the source commit can be cloned from [librtprocess](https://github.com/CarVac/librtprocess). Choose a fresh evidence directory for each run. Native float probes take `SOURCE ALGORITHM NEW_RESULT_JSON`, where algorithms are `rcd`, `ahd`, `xtransfast`, `markesteijn1`, or `markesteijn3` for `float_probe`; `wb_probe` accepts `rcd`, `xtransfast`, or `markesteijn1`.

## Unpack result

All seven sources decoded in both backends and retained identical pre/post source hashes. Their sensor-size u16 buffers have these differences:

| Source group | Whole-sensor comparison | Rawler unpack | LibRaw unpack |
| --- | --- | ---: | ---: |
| Owner X100VI RAF, public uncompressed and lossless RAF | All 40,902,912 sensor samples equal in each file | 11–12 ms uncompressed; 82 ms lossless | 22–23 ms uncompressed; 867 ms lossless |
| Owner Mavic Air 2S DNG | Every 20,312,064 sample equal | 4 ms | 16 ms |
| Owner Z6 NEF, public 12/14-bit lossless NEF | LibRaw exceeds Rawler by exactly one code on ~75% of the 24,498,560 samples; all other samples equal | 128–133 ms | 213–234 ms |

The locally generated `artifacts/raw-backend-probe/comparison.json` contains exact counts and first differing coordinates in the full, active, and crop domains. It is ignored evidence and is regenerated by `compare.py`, not a tracked repository file. This is a sensor-domain comparison, not a claim that active/default crops or output colors match. The production adapter uses the RAF camera crop `[12,21,7728,5152]` for X100VI, preserves LibRaw's differing inset as separate metadata, and applies the Nikon IFD orientation to its default crop.

The Nikon one-code difference is explained by pinned code. Rawler `decoders/nef.rs` sends even lossless NEF predictors through `LookupTable::dither` (`bits.rs`); for an identity curve its base is one below the decoded integer and its pseudorandom rounding restores that code about one quarter of the time. LibRaw `nikon_load_raw` uses the curve value directly. The observed 75% direction and exact magnitude match that arithmetic. Rawler's default `raw_image` output is therefore unsuitable as an exact lossless-NEF reference without a targeted fix. This does not establish that every LibRaw mode is correct; mode qualification remains file-specific.

Black levels agree when LibRaw's complete representation is used: Nikon scalar 1008; Fuji repeated 6×6 values all 1023; DJI repeated 2×2 values all 4096. Reading only `color.black` incorrectly reports zero for Fuji/DJI. White metadata differs: Rawler Nikon 15520/3880 versus LibRaw `maximum` 16383/4095, and those fields represent different calibration/saturation choices. The owner Z6 source contains 11 integer samples above Rawler's white 15520. The owner NEF is rotated according to independent IFD metadata and LibRaw `flip=5`; Rawler reports `Normal`, so orientation cannot be taken from that Rawler field alone. The owner DJI DNG has two mandatory OpcodeList3 operations (GainMap ID 9 and WarpRectilinear ID 1, flags zero), so the adapter explicitly rejects it before unpack rather than claiming incomplete DNG support.

## Float feasibility and WB placement

With LibRaw unpacked u16 data cast to float, established librtprocess algorithms ran at full sensor resolution without a u16 RGB terminal conversion. Each output is three float planes; all tested samples were finite and the original CFA sites were preserved in the ungained runs. These are algorithm-stage probes, with no color transform, display conversion, image-quality acceptance, or editor integration:

| Owner file / algorithm | Demosaic | Process peak RSS | Relevant output values |
| --- | ---: | ---: | --- |
| Z6 RCD | 295 ms | 425 MiB | max 17187, five channel values > LibRaw 16383 |
| Z6 AHD | 538 ms | 425 MiB | max 16584 |
| X100VI Markesteijn 1 pass | 1.62 s | 707 MiB | min -6.14; no nonfinite values |
| X100VI Markesteijn 3 pass | 3.85 s | 707 MiB | min -618.37; no nonfinite values |
| X100VI XTransFast | 256 ms | 705 MiB | fast quality comparator only |
| DJI RCD | 218 ms | 353 MiB | max 91308.6; 2197 channel values > 65535 |

RCD explicitly clamps its output at zero; Markesteijn can return negative interpolated values. Their float outputs retain highlight values above the u16 sensor ceiling where interpolation creates them, but this experiment does not prove color-pipeline headroom or acceptable visual quality. The callback return is ignored by both routines in this revision; a production job must bound native work and discard cancelled results on return, or use a separately controlled helper. `OPTION_OMP=OFF` prevents independent OpenMP oversubscription, though the full editor's worker strategy still needs measurement.

`wb_probe` compared as-shot gains before demosaic with gains after demosaic on black-subtracted, phase-aligned 2040×2040 real sensor crops. The corrected outputs are `*.wb-black.json`. Z6 RCD differs by mean 3.84/p95 16.7/max 1922 sensor-scale float codes; Fuji Markesteijn 1 pass by 5.51/21.35/17583; DJI RCD by 193.8/678.5/89504. These nonlinear algorithms cannot generally reuse one neutral demosaic for arbitrary WB while claiming identical processing. Retain a bounded unpacked mosaic, re-demosaic for WB changes, and reuse a cached camera-linear float result for downstream exposure/matrix/geometry changes. One XTransFast crop was almost linear to gain placement, with an isolated large outlier, and its speed alone does not qualify its appearance.

## Selected adapter and remaining gates

The selected, implemented path is [`lightwell-raw`](../../crates/lightwell-raw/README.md): bundled pinned LibRaw 0.22.2 unpack plus librtprocess RCD for Z6 Bayer and Markesteijn one-pass for Fuji X-Trans. The adapter copies one bounded u16 sensor mosaic into `Arc<Vec<u16>>`, closes LibRaw and drops encoded source bytes, then develops WB before demosaic into one Rust-owned contiguous planar float buffer. It does not retain an opaque decoder handle between edits or pass through LibRaw `dcraw_process()`'s 8/16-bit RGB conversion. Repeated WB edits use the retained mosaic; exposure and later recipe layers reuse the developed float frame. The C ABI catches exceptions and validates dimensions, modes, calibration and CFA; cancellation during librtprocess's non-interruptible demosaic suppresses the result on return. Authentic source/hash, float-headroom and error tests live in that crate.

The owner's DJI DNG is not supported by this adapter because its mandatory GainMap and WarpRectilinear corrections are unimplemented. The full background editor acceptance harness is `cargo xtask raw-editor --manifest FILE --output NEW --samples 30 --binary RELEASE_BINARY`; its local owner NEF/RAF run is recorded under ignored `artifacts/raw-editor-owner-final-30-01/`. All 30 trials per owner source passed the edit/history/reopen checks. Fuji's first-process sampled RSS p95 was 1992 MiB and one unexplained trial peaked at 2436 MiB; a separate 24-edit live API run without screenshots plateaued near 1583 MiB after edit three. These are measured process observations, not a 1.5 GiB bound. Rendered color/detail quality across the requested scene/ISO matrix, process-wide RSS behavior, cancellation latency, and clean Windows/Linux native package builds remain qualification gates. The backend probe timings above are single-trial stage evidence, separate from the editor distributions.
