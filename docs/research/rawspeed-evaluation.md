# RawSpeed as the RAW unpacker

Question: can [RawSpeed](https://github.com/darktable-org/rawspeed) replace LibRaw 0.22.2 in `luxforge-raw` for faster parsing with the same functionality?

Answer, adopted by the owner on 2026-09-30: **do not replace LibRaw. Keep LibRaw for identify, metadata and CR3, and use RawSpeed only to fill the sensor mosaic for the recording modes where it is exact and faster.** The implementation contract is [RawSpeed unpacking](../design/rawspeed-unpack.md). A full replacement cannot keep current behaviour: it loses CR3, changes samples and changes white levels, crops and black levels.

## What was measured

The probes were C++ programs linking the vendored LibRaw and a local RawSpeed build in one process. They and their CC0 corpora stay outside the repository.

- **RawSpeed.** Commit `c835b05aecfacb7343f7c424abd620aa12116c3f` (2026-07-28), still the tip of upstream `develop` on 2026-09-30. A release CMake build with `BINARY_PACKAGE_BUILD=ON` (generic CPU, no `-march=native`), serial, and separately with OpenMP for comparison only.
- **LibRaw.** The vendored 0.22.2 source, compiled `-O3` with the adapter's defines and no zlib, JPEG or OpenMP.
- **Timed work.** The source bytes are already in memory, as the adapter receives them. Each timing covers identify, unpack and one copy of the full u16 mosaic into a caller buffer. Develop is excluded.
- **Inputs.** Two corpora with verified SHA-256 values: one CC0 sample per LibRaw decoder family in the camera catalog plus the three owner originals, and 51 CC0 samples covering the [most-used cameras](popular-cameras.md) in their default and common recording modes.
- **Host.** Apple M4 Pro, macOS 25.5, shared. The family corpus ran at load 15–20 on 14 cores, with medians of 6–8 alternating runs; the popular-camera corpus ran at load 2.5–3.7 with medians of 7 alternating runs. Ratios are more reliable than absolute times; these are not editor latency distributions.

## Hybrid result: LibRaw metadata, RawSpeed pixels

LibRaw still identifies the file, reads all metadata and runs every step of `unpack` after the pixels are read, including masked-pixel black (`crop_masked_pixels`) and black normalization. Only the sensor samples come from RawSpeed.

For every routed file, all compared LibRaw fields were identical to native LibRaw: `black`, the full `cblack` table, `maximum`, CFA, `flip`, raw size, margins, inset crop, `cam_mul`, `pre_mul`, `rgb_cam` and `cam_xyz`. Every sample was identical except in the one case below.

| Decoder family (sample) | Share of popular-camera files | LibRaw | Hybrid | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Nikon lossless and lossy NEF (D750, Z6II, Zf, D850, owner Z6, Z8) | ~15% | 196–497 ms | 58–183 ms | 2.5–3.4× |
| Canon lossless JPEG CR2 (5D Mark III, 5D Mark IV, 6D Mark II) | ~11% | 229–363 ms | 65–98 ms | 3.1–3.7× |
| Fujifilm lossless compressed RAF (X-T5) | part of ~4% | 858 ms | 305 ms | 2.8× |
| Olympus ORF (OM-1, OM-5) | ~3% | 301–322 ms | 126–131 ms | 2.3–2.6× |
| Lossless DNG (Ricoh GR III, Leica M10-R) | ~1% | 255–489 ms | 77–170 ms | 2.9–3.3× |
| Packed DNG (Leica Q2, owner Air 2S) | ~1% | 14–335 ms | 6–35 ms | 2.5–9.6× |
| Panasonic RW2 (S5II, GH7, GX7 Mark III, S5) | ~1% | 46–214 ms | 34–100 ms | 1.3–2.1× |
| Pentax PEF (K-3 Mark III) | <1% | 258 ms | 78 ms | 3.3× |
| Sony compressed ARW (A7 III, A7 IV, A7C II, A6400, A6700, ZV-E10, A7R V) | ~23% | 46–116 ms | 43–112 ms | 1.0× |

Shares are from the [popularity study](popular-cameras.md); they are upload shares, not measured photographer counts.

These are not routed at all:

- **Canon CR3**, about 38% of popular-camera files. RawSpeed has no CR3 decoder. LibRaw takes 48–278 ms (R6 Mark II 148 ms, R5 273 ms, R5 Mark II 273 ms).
- **Uncompressed RAF and NEF.** RawSpeed is as fast or slower (Z 7 uncompressed 33 ms against LibRaw's 16 ms).
- **Fujifilm lossy compressed RAF.** RawSpeed rejects it; LibRaw decodes it.
- **Nikon High Efficiency NEF** and **Sony A7 V compressed ARW.** Neither library decodes them; see [popular cameras](popular-cameras.md).

About a third of popular-camera files would open 130–550 ms faster. CR3 and Sony ARW, together about 60%, gain nothing.

With OpenMP, RawSpeed alone decodes Fujifilm compressed in about 68 ms, ARW in about 17 ms and Panasonic 2–3× faster again. Luxforge does not use OpenMP, and a Rayon executor for RawSpeed's parallel regions is not part of the adopted work.

## How the pixels must be taken

LibRaw can call RawSpeed itself when built with `USE_RAWSPEED3`, and the first probe used that hook. It has three defects that rule it out:

- **Curves.** RawSpeed applies its own linearization curves, dithered by default, which moves about 72% of lossless NEF samples down by one code and ARW samples by up to ±8. With dithering disabled by a source patch, RawSpeed's own curve still differs from LibRaw's on Nikon lossy NEF: 11,181 of 24,498,560 Z6II samples.
- **Silent fallback.** When RawSpeed fails, the hook quietly decodes with LibRaw instead. It did so on the Z50II and Z5II samples. A routed mode must fail explicitly.
- **Patches.** Routing Fujifilm compressed and Panasonic C6/C8 needs a local LibRaw `decoder_info.cpp` patch, and the hook copies the whole encoded input into a new buffer first.

Taking RawSpeed's **uncorrected** values (`uncorrectedRawValues`) and applying LibRaw's own `curve` table reproduced LibRaw exactly on every compared file, the Z6II lossy NEF included, wherever the replaced LibRaw decoder applies that curve. Where the replaced decoder does not apply it, the uncorrected values already equal LibRaw's. Which rule applies is a property of the replaced LibRaw decoder, not of the camera. So the adapter substitutes its own decode for LibRaw's `load_raw`, applies that rule, and needs no patch to either library.

## Why a full replacement does not keep current behaviour

Standalone RawSpeed decoded every sample except CR3, High Efficiency NEF, Sony A7 V and Fujifilm lossy compressed. Its uncropped mosaics matched LibRaw exactly for uncompressed and lossless NEF (lossless only after removing dithering), RAF, CR2, RW2, PEF, ORF and DNG. Everything around the samples is different:

| Area | RawSpeed alone | Current behaviour it would change |
| --- | --- | --- |
| Canon CR3 | No decoder | About 38% of popular-camera files stop opening |
| Curves | Dithered by default (NEF lossless, ARW) | Stored samples change; exact mosaic tests fail |
| White level | From `cameras.xml` or DNG tags; differs on 8 of 16 files (for example Z6/Z7 15520 against 16383, A6700 15360 against 16383, 5D IV 14008 against 16383) | Normalization, clipping and exposure appearance change |
| Crop and active area | From `cameras.xml` `Crop` entries; differs for PEF, CR2, RW2, RAF and DNG | Every `libraw_inset` and `active_area` profile needs new framing |
| Black level | Masked-area or XML values; OM-5 256 against 254, S5 509 against 510 | Small shifts in shadows |
| Orientation | Not provided | Needs a new EXIF reader for each container, including RAF and CR3 |
| Colour matrices | `cameras.xml`; of the 52 cameras matched by name in both tables, 50 are identical and 2 differ (K-70, X100V) | Colour for those cameras changes |
| Cancellation | No progress callback; `decodeRaw` cannot be interrupted | Cancellation only between stages |
| Threading | OpenMP only; serial without it | Needs a Rayon executor patch |
| Camera data | `cameras.xml` (733 KB, CC-BY-SA 3.0) parsed in about 4 ms | Must ship with the binary |

## What the hybrid costs

- **Build.** RawSpeed has about 36,000 lines in 247 files of C++20, against LibRaw's C++17. It needs pugixml (MIT). zlib and libjpeg can be disabled, because they only serve deflate and lossy DNG, and no routed mode uses either. RawSpeed has no releases and changes often; each update is a dependency review.
- **Windows.** Unverified. LibRaw's own RawSpeed notes needed a clang-cl compatibility patch for an older commit; the MSVC toolchain may need clang-cl for this crate.
- **Licence.** RawSpeed's code is LGPL-2.0-or-later and `cameras.xml` is CC-BY-SA 3.0, both compatible with GPL-3.0-or-later. Manual review stays deferred.
- **Liveness.** RawSpeed's decode cannot be interrupted. The measured worst case is 305 ms serial (X-T5 lossless compressed), down from 858 ms of cancellable LibRaw work.
- **Memory.** RawSpeed decodes into its own image before the copy into LibRaw's buffer, so a routed unpack briefly holds one extra u16 mosaic (91 MiB at 45.7 MP). After unpack the retained memory is the same single mosaic.
- **Attack surface.** Both parsers read every routed file. RawSpeed is continuously fuzzed upstream, but the hybrid adds its bugs to LibRaw's.

## What it buys the editor

Unpacking runs on source open and cache misses only. White-balance and exposure edits develop from the retained mosaic, so they do not change.

- **Owner Z6.** The 546 ms cold saved-WB preparation p50 includes about 235 ms of unpack, so about 160 ms could be saved. This is an estimate until the editor is measured.
- **Popular compressed NEF, CR2, lossless RAF, ORF and DNG sources.** Open time falls by 130–550 ms per photo.
- **Owner X100VI and CR3, ARW and uncompressed sources.** No change.
