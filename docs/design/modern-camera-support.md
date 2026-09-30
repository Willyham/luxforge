# Modern camera and drone support

## Scope

Expand the data-driven RAW path to a practical set of 100 widely used modern
camera/drone models across brands, including the three existing models. This is
a coverage selection, not a measured sales ranking. The owner selected broad
usage across brands rather than newest-first coverage and requested Luna
subagents. Maintain exact make/model identities and recording-mode evidence.

## Behavior and constraints

Profiles select bounded processing capabilities; no model-name processing
branches. Source-dependent calibration remains authoritative in each file.
LibRaw's camera list alone is not Luxforge qualification: its published list
assumes optional features which this build may not enable. Unknown modes,
unsupported mandatory corrections and incompatible metadata fail explicitly.
Do not invent full-sensor dimensions from advertised megapixels.

Use a selected subset of CC0 authentic files from raw.pixls.us, with URL,
SHA-256, license and observed decoder metadata recorded. Downloads and rendered
photos stay ignored, bounded and local. Do not mirror the whole corpus. Validate
source preservation, mosaic/float correctness, calibrated dimensions and crop,
then representative background editor/history/reopen workflows. Separate each
mode's demonstrated evidence from outstanding controlled color/scene coverage.

The [owner-approved RAW resource contract](architecture.md#limits) is separate from JPEG, whose limits are unchanged. High-resolution models, container compression, missing crop
metadata, per-green black levels and DNG correction layouts may require shared
capabilities before a profile can be enabled. Never inflate the support count
with guessed entries or bypass required corrections to admit a model.

## Current evidence

The catalog contains 107 camera profiles and 126 recording modes, including the
original modes and the [popular camera modes](popular-camera-support.md). The
authentic adapter evidence has 130 samples covering 123 modes: 129 CC0 public
sources and the existing owner Air 2S source. The three modes without an entry,
the Z6's 12-bit and 14-bit lossless NEF and the X100VI's lossless RAF, are
covered by the crate's authentic owner and public-fixture tests. Each
qualification checks source hashes before/after, full mosaic retention, usable
calibration, and finite as-shot and perturbed-white-balance development.
Per-source metadata and hashes, including each full mosaic's SHA-256 from the
LibRaw-only path, are in
[the evidence manifest](../../fixtures/modern-camera-evidence.json);
[the selection](../../fixtures/modern-camera-selection.json) lists each model's
samples with the mode each evidences, its source URL, hash and license. Float
hashes record this build's output; they are not independent color-reference
ground truth.

### Popular recording modes

Each mode below was identified from what the adapter reads (decoder, bit depth,
stored frame, frame count and container marker), not from the sample's label,
and qualified from the listed raw.pixls.us sample. C-RAW samples on bodies whose
RAW mode was already admitted share that mode: Canon RAW and C-RAW use one
decoder, bit depth and size.

| Camera | Sample | Mode | Selector | Mosaic SHA-256 |
| --- | --- | --- | --- | --- |
| Sony A7 III | 2414, 2415 | `SonyILCE7M3Compressed14` | `sony_arw2_load_raw()` 14-bit | `89ceacf8cfa2…`, `edf760740d0c…` |
| Sony A7 IV | 6932 | `SonyILCE7M4Compressed14` | `sony_arw2_load_raw()` 14-bit | `76c01cc0e91d…` |
| Sony A7 IV | 6929 | `SonyILCE7M4Lossless14` | `sony_ljpeg_load_raw()` 14-bit, 7168×5120 frame | `1ae99c2cf427…` |
| Sony A7C II | 6868 | `SonyILCE7CM2Compressed14` | `sony_arw2_load_raw()` 14-bit | `d3bc1d9d7a5f…` |
| Sony A7C II | 6867 | `SonyILCE7CM2Lossless14` | `sony_ljpeg_load_raw()` 14-bit, 7168×5120 frame | `f4c9f3858edd…` |
| Sony A7R V | 6235 | `SonyILCE7RM5Compressed14` | `sony_arw2_load_raw()` 14-bit | `5a4e900224fd…` |
| Sony A7R V | 6232 | `SonyILCE7RM5Lossless14` | `sony_ljpeg_load_raw()` 14-bit, 9728×6656 frame | `70d841fc2c5c…` |
| Sony a6700 | 6736 | `SonyILCE6700Lossless14` | `sony_ljpeg_load_raw()` 14-bit, 6656×4608 frame | `ad5a4455d20d…` |
| Sony ZV-E10 | 4855 | `SonyZVE10Compressed14` | `sony_arw2_load_raw()` 14-bit | `d708a48b3170…` |
| Fujifilm X-T5 | 6124 | `FujifilmXT5Uncompressed14` | `unpacked_load_raw()` 14-bit, RAF header 0 | `7ee1af041d36…` |
| Fujifilm X-T5 | 6123 | `FujifilmXT5Lossy14` | `fuji_compressed_load_raw()` 14-bit, RAF header 3 | `0b19a4f1fb56…` |
| Fujifilm X-M5 | 7747 | `FujifilmXM5Uncompressed14` | `unpacked_load_raw()` 14-bit, RAF header 0 | `34e4347b54ab…` |
| Fujifilm X-M5 | 7748 | `FujifilmXM5Lossless14` | `fuji_compressed_load_raw()` 14-bit, RAF header 2 | `809fbafb9c12…` |
| Nikon D850 | 1840 | `NikonD850Lossless14` | `nikon_load_raw()` 14-bit, maker-note compression 3 | `614933e6f550…` |
| Nikon D850 | 1841 | `NikonD850Lossy14` | `nikon_load_raw()` 14-bit, maker-note compression 4 | `bc9372e0e11e…` |
| Nikon Z6III | 7819 | `NikonZ63Lossless14` | `nikon_load_raw()` 14-bit, maker-note compression 3 | `ddf95d32217f…` |
| Nikon Z50II | 7762 | `NikonZ502Lossless14` | `nikon_load_raw()` 14-bit, maker-note compression 3 | `bd2399b51421…` |
| Nikon Z5II | 7745 | `NikonZ52Lossless14` | `nikon_load_raw()` 14-bit, maker-note compression 3 | `202d872a9f11…` |
| Canon EOS 5D Mark IV | 983 | `CanonEOS5DMarkIVRaw14` | `lossless_jpeg_load_raw()` 14-bit, one frame | `910c0aa81939…` |
| Canon EOS 90D | 4649 (RAW), 4650 (C-RAW) | `CanonEOS90DRaw14` | `crxLoadRaw()` 14-bit | `184ac5daa56c…`, `3898269dc57d…` |
| Canon EOS 5D Mark III | 771 | `CanonEOS5DMarkIIIRaw14` | `lossless_jpeg_load_raw()` 14-bit | `d4f894f5e341…` |
| Canon EOS R6 Mark II | 6409 (C-RAW) | `CanonEOSR6MarkIIRaw14` | `crxLoadRaw()` 14-bit | `edb3745dec39…` |
| Canon EOS R6 Mark II | 6405 (Dual Pixel RAW) | `CanonEOSR6MarkIIRaw14DualPixel` | `crxLoadRaw()` 14-bit, two frames | `caf3351c989d…` |
| Canon EOS R6 | 4660 (C-RAW) | `CanonEOSR6Raw14` | `crxLoadRaw()` 14-bit | `d179b086225d…` |
| Canon EOS R5 | 4698 (C-RAW) | `CanonEOSR5Raw14` | `crxLoadRaw()` 14-bit | `d9420d23104f…` |
| Canon EOS R5 | 4697 (C-RAW Dual Pixel) | `CanonEOSR5Raw14DualPixel` | `crxLoadRaw()` 14-bit, two frames | `034083098e20…` |
| Canon EOS R5 Mark II | 7882 (C-RAW) | `CanonEOSR5MarkIIRaw14` | `crxLoadRaw()` 14-bit | `3b7b168da7b7…` |
| Canon EOS R7 | 5634 (C-RAW) | `CanonEOSR7Raw14` | `crxLoadRaw()` 14-bit | `600b934b935d…` |

Sony's lossless compressed ARW is stored in 512-pixel tiles, so LibRaw reports a
padded frame larger than the uncompressed sensor; the mode declares that frame
(`frame_size`) and LibRaw's inset crops to the same visible area as the
uncompressed mode. The new Nikon bodies' lossless modes require maker-note
compression 3, so a High Efficiency file (compression 13 or 14) matches no mode
even without the earlier High Efficiency refusal. The R6 Mark II 6405 and R5 4697
samples are Dual Pixel RAW: a second full-size CRX track beside the primary one.
The adapter decodes only the primary frame, as it does for the 5D Mark IV's Dual
Pixel CR2; that frame averages about twice the second frame's signal, as the
combined image should. The Z50II and Z5II use the model's own matrix from
RawSpeed's camera data (below), since pinned LibRaw has none; the Z6III uses
LibRaw's.

The EOS R10 has no C-RAW sample in the raw.pixls.us index: its four samples
(5871, 5873, 5907, 8185) all record Canon quality 4, RAW.

### Refused recording modes

These stay refused, each for a concrete reason:

| Files | Refusal | Reason |
| --- | --- | --- |
| Nikon High Efficiency and High Efficiency★ NEF (Z9 5147, Z8 6618, Zf 6886, Z6III 7815, Z50II 7763, Z5II 7743) | Unsupported compression, before unpack | JPEG XS payload that neither LibRaw 0.22.2 nor RawSpeed decodes; the owner waits for upstream LibRaw support. The message says to record Lossless compressed RAW |
| Sony A7 V compressed ARW (8846) | LibRaw open fails | A new compressed format neither library decodes |
| APS-C crop sizes on full-frame bodies (R6 4661 at 3584×2386, R6 Mark II 6404 at 3936×2612, A7R V 6239 at 6304×4180, A7C II 6869 and A7 IV 6936 at 4736×3132) | Unsupported recording mode | The stored frame is smaller than the catalogued sensor. A mode's frame may only pad the sensor, never crop it, and no crop mode has its own qualified geometry |
| Small and medium RAW sizes | Unsupported recording mode | As for crop sizes: they differ from the catalogued sensor and are not qualified |

The high-resolution profiles use the approved RAW-only resource contract in
[the resource ledger](modern-camera-resource-ledger.md). For DNGs whose decoder
reports a trimmed active bottom, a bounded profile value reconciles that exact
delta while preserving the source ActiveArea for correction coordinates. The
SL2 profile records LibRaw's 19-row trim; the full raw mosaic is retained.
A demonstrated mode does not qualify every recording setting or, for drones,
every camera module. The Mavic 3 Pro Cine fixture covers its 4032×3024 module.

The native M4 Pro editor passed 39 edit/history/reopen trials across ten
representative modern models and the three original owner fixtures. The checks
cover temperature/tint, individual channel WB, neutral picking, exposure,
rotation/crop/undo, historical previews and exact reopened displayed pixels.
The final build additionally passed 27 edit/reopen trials across six larger/Leica
models and the three owner originals. Full verification also passed 19 general
rendered scenarios and the independent RAW numerical reference. The resource ledger records sampled process memory
and its limits. For the popular modes, the background `raw-editor` journey
passed on Sony A7 IV compressed (6932), Fujifilm X-T5 uncompressed (6124),
Nikon Z6III lossless (7819), Nikon Z50II lossless (7762) and Canon 90D RAW
(4649). The Z6III sample's As shot white lies within half a tint unit beyond
+100, so its controls show the limit (4877 K, +100), as the white-balance
inversion does for any such white. The
A7R V lossless mode's padded 9728×6656 frame has no editor or resource-ledger
measurement yet. Controlled
color/detail, other recording modes, and native Windows/Linux package
qualification remain separate work.

### RawSpeed-routed modes

64 of the 67 catalog modes whose LibRaw decoder is in the [replaceable table](rawspeed-unpack.md#routed-modes) have `"unpacker": "rawspeed"`: RawSpeed fills their mosaic inside LibRaw's unpack, and `backend` reads `LibRaw 0.22.2 + RawSpeed c835b05a + librtprocess 9a858270`. The current rule, pending the speed measurement, routes every candidate that is exact on every local authentic sample of it; the measurement applies the 1.3× gate and moves any mode below it back to LibRaw. Each sample was decoded through both unpackers by the crate's `replaceable_catalog_modes_match_libraw_on_every_local_sample`: the mosaic, every metadata field but `backend`, and the as-shot and perturbed-white-balance developments are identical, and LibRaw's results equal what was recorded before routing: the evidence manifest's mosaic and development hashes, or for a sample marked (pin), the mosaic hash in `tests/real_files.rs` (the modes with no evidence entry). A sample marked (no record) has neither and is compared with LibRaw only. After routing, the qualifier reproduced all 130 evidence entries exactly.

| Mode | Decoder | Samples (raw.pixls.us id) | Routed |
| --- | --- | --- | --- |
| `CanonEOS5DMarkIIIRaw14` | `lossless_jpeg_load_raw()` | 771 | yes |
| `CanonEOS5DMarkIVRaw14` | `lossless_jpeg_load_raw()` | 983 | yes |
| `CanonEOS5DMarkIVRaw14DualPixel` | `lossless_jpeg_load_raw()` | 980 | yes |
| `CanonEOS6DMarkIIRaw14` | `lossless_jpeg_load_raw()` | 1625 | yes |
| `CanonEOS7DMarkIIRaw14` | `lossless_jpeg_load_raw()` | 1064 | yes |
| `CanonEOS80DRaw14` | `lossless_jpeg_load_raw()` | 1294 | yes |
| `CanonEOSM6Raw14` | `lossless_jpeg_load_raw()` | 1382 | yes |
| `DJIFC220Dng16` | `packed_dng_load_raw()` | 1052 | yes |
| `DJIFC4382Dng16` | `lossless_dng_load_raw()` | 7823 | no |
| `DJIFC6310Dng16` | `packed_dng_load_raw()` | 2155 | yes |
| `DJIFC7303Dng16` | `packed_dng_load_raw()` | 4785 | yes |
| `DjiAir2sDng16` | `packed_dng_load_raw()` | owner Air 2S | yes |
| `FujifilmX100FRaw14` | `fuji_compressed_load_raw()` | 1936 | yes |
| `FujifilmX100VRaw14` | `fuji_compressed_load_raw()` | 3812 | yes |
| `FujifilmX100ViLossless14` | `fuji_compressed_load_raw()` | 7301 (pin) | yes |
| `FujifilmXE4Raw14` | `fuji_compressed_load_raw()` | 4446 | yes |
| `FujifilmXH2Raw14` | `fuji_compressed_load_raw()` | 6001 | no |
| `FujifilmXH2SRaw14` | `fuji_compressed_load_raw()` | 6007 | yes |
| `FujifilmXM5Lossless14` | `fuji_compressed_load_raw()` | 7748 | yes |
| `FujifilmXS20Raw14` | `fuji_compressed_load_raw()` | 6666 | yes |
| `FujifilmXT3Raw14` | `fuji_compressed_load_raw()` | 2783 | yes |
| `FujifilmXT5Lossy14` | `fuji_compressed_load_raw()` | 6123 | no |
| `FujifilmXT5Raw14` | `fuji_compressed_load_raw()` | 6122 | yes |
| `LeicaCLDng14` | `packed_dng_load_raw()` | 2489 | yes |
| `LeicaM10Dng16` | `lossless_dng_load_raw()` | 1603 | yes |
| `LeicaM10RDng16` | `lossless_dng_load_raw()` | 7853 | yes |
| `LeicaQ2Dng14` | `packed_dng_load_raw()` | 3204 | yes |
| `LeicaSL2Dng14` | `packed_dng_load_raw()` | 7872 | yes |
| `NikonD5600Raw14` | `nikon_load_raw()` | 1416 | yes |
| `NikonD7500Raw14` | `nikon_load_raw()` | 1534 | yes |
| `NikonD750Raw14` | `nikon_load_raw()` | 898, 896 (no record) | yes |
| `NikonD780Raw14` | `nikon_load_raw()` | 3828 | yes |
| `NikonD850Lossless14` | `nikon_load_raw()` | 1840 | yes |
| `NikonD850Lossy14` | `nikon_load_raw()` | 1841 | yes |
| `NikonZ30Raw14` | `nikon_load_raw()` | 5813 | yes |
| `NikonZ502Lossless14` | `nikon_load_raw()` | 7762 | yes |
| `NikonZ50Raw14` | `nikon_load_raw()` | 3647 | yes |
| `NikonZ52Lossless14` | `nikon_load_raw()` | 7745 | yes |
| `NikonZ5Raw14` | `nikon_load_raw()` | 4136 | yes |
| `NikonZ62Raw14` | `nikon_load_raw()` | 4160, 4161 (no record) | yes |
| `NikonZ63Lossless14` | `nikon_load_raw()` | 7819 | yes |
| `NikonZ6Lossless12` | `nikon_load_raw()` | 3585 (pin) | yes |
| `NikonZ6Lossless14` | `nikon_load_raw()` | 3582 (pin), owner Z6 (pin) | yes |
| `NikonZ8Raw14` | `nikon_load_raw()` | 6617 | yes |
| `NikonZ9Raw14` | `nikon_load_raw()` | 5146 | yes |
| `NikonZfRaw14` | `nikon_load_raw()` | 6885 | yes |
| `NikonZfcRaw14` | `nikon_load_raw()` | 4812 | yes |
| `OMDigitalOM1MarkIIRaw12` | `olympus_load_raw()` | 7262 | yes |
| `OMDigitalOM1Raw12` | `olympus_load_raw()` | 5283 | yes |
| `OMDigitalOM3Raw12` | `olympus_load_raw()` | 7796 | yes |
| `OMDigitalOM5Raw12` | `olympus_load_raw()` | 6343 | yes |
| `OlympusEM10MarkIVRaw12` | `olympus_load_raw()` | 4126 | yes |
| `OlympusEM1MarkIIIRaw12` | `olympus_load_raw()` | 3800 | yes |
| `OlympusEM1XRaw12` | `olympus_load_raw()` | 3041 | yes |
| `PanasonicDCG9M2Raw16` | `panasonicC8_load_raw()` | 6999 | yes |
| `PanasonicDCGH5Raw12` | `panasonic_load_raw()` | 1516 | yes |
| `PanasonicDCGH6Raw16` | `panasonicC8_load_raw()` | 5876 | yes |
| `PanasonicDCGH7Raw16` | `panasonicC8_load_raw()` | 8062 | yes |
| `PanasonicDCGX7MK3Raw12` | `panasonic_load_raw()` | 5967 | yes |
| `PanasonicDCS5M2Raw14` | `panasonicC8_load_raw()` | 7790 | yes |
| `PanasonicDCS5Raw14` | `panasonicC6_load_raw()` | 4096 | yes |
| `PentaxK1MarkIIDng14` | `lossless_dng_load_raw()` | 3345 | yes |
| `PentaxK3MarkIIIRaw14` | `pentax_load_raw()` | 4677 | yes |
| `PentaxK70Raw14` | `pentax_load_raw()` | 1141 | yes |
| `PentaxKPDng14` | `lossless_dng_load_raw()` | 1824 | yes |
| `RicohGRIIIDng14` | `lossless_dng_load_raw()` | 3115 | yes |
| `RicohGRIIIxDng14` | `lossless_dng_load_raw()` | 5818 | yes |

The background `raw-editor` journey passed on routed modes from every routed family: the owner Z6 (`NikonZ6Lossless14`) and Air 2S (`DjiAir2sDng16`), Nikon Z6III lossless (7819), Canon 5D Mark IV (983), Fujifilm X-T5 lossless compressed (6122), OM-1 (5283), Panasonic S5II (7790), Pentax K-3 Mark III (4677) and the Ricoh GR III lossless DNG (3115), which applies no DNG opcodes.

The three candidates left on LibRaw, each refused by RawSpeed on its sample and structurally on every file of the mode:

| Mode | Sample | Reason |
| --- | --- | --- |
| `FujifilmXH2Raw14`, `FujifilmXT5Lossy14` | 6001, 6123 | Lossy compressed RAF (RAF header 3). The compressed stream's header stores 0 in its third byte, LibRaw's lossless flag; RawSpeed's `FujiDecompressor` reads the byte as a version that must be 1 and refuses the file ("compressed RAF header check"). Lossless compressed files store 1 and are routed |
| `DJIFC4382Dng16` | 7823 | The lossless JPEG DNG tiles use predictor 6; RawSpeed's `LJpegDecoder::decodeScan` implements only predictor 1 and refuses every tile ("Unsupported predictor mode: 6"). LibRaw's decoder handles every predictor |

## Reproducing adapter qualification

Download the selected public sources with the bounded
`cargo xtask raw-camera-metadata --index FILE --ids ID[,ID...] --output NEW_DIR`,
passing the raw.pixls.us repository index (`json/getrepository.php?set=all`)
and the selected numeric sample IDs. Pass `--max-source-mib 512` for the
complete selection; its conservative default download cap is 128 MiB. It
verifies the repository's CC0 license and SHA-256, downloads one sample at a
time through `curl` over HTTPS and refuses to reuse an output directory. It then
decodes each file once with the RAW adapter and records its metadata, or the
adapter's refusal (an unrecognised mode names LibRaw's make, model, decoder, bit
depth and size), and hashes it again; a decode alone does not establish
application support. Keep images and generated results in an ignored evidence
directory.

Create a qualifier manifest containing `{"samples": [{"id": "sample-id",
"path": "/absolute/source/path", "sha256": "expected-source-sha256"}]}` for
only the enabled selection entries. The owner fixture needs local access; it
is not downloadable from the public repository. Run:

```sh
cargo run --release --locked -p luxforge-raw --example qualify_profiles -- \
  /path/to/manifest.json /path/to/new-results.json
```

The qualifier runs sources sequentially, refuses an existing output file, and
returns failure if any source changes, metadata is unsupported, or development
fails. Its maximum source read is the RAW adapter's [encoded-source bound](architecture.md#limits). Public
fixture redistributions and source photographs are not committed.

OM-3, Nikon Z50II and Nikon Z5II calibration comes from each model's exact entry
in pinned RawSpeed camera data, with its attribution and data license in the RAW
adapter's notices.
Unpacking with LibRaw's identity fallback is insufficient: the adapter rejects
missing or singular XYZ-to-camera calibration before publishing the source.

## Work and acceptance

Luna agents research disjoint model groups and prepare evidence-backed candidate
records. Root owns shared decoder/profile changes, selection integration and
verification. A profile can be marked enabled only with its required processing
implemented and authentic decode/development evidence. Track concrete blocked
modes and missing fixtures; distinguish implementation from broad image-quality
qualification.

The finished catalog has 100 distinct selected model identities, source-grounded
mode rules and capability choices, reproducible qualification evidence, current
support documentation, and passing quick/rendered/timing checks. Full verification
is required before claiming the broad support checkpoint. Existing three-camera
metadata, corrections, source preservation and history remain regressions.

## Coverage and resource scope

- Exact model set follows the agreed broadly used modern mix and available
  decoder evidence; selection must not hide popular high-resolution models simply
  because they require more memory.
- The [approved RAW-specific allocation budget](architecture.md#limits) applies; JPEG limits stay independent. A
  process-wide RSS claim still requires measured editor liveness evidence.

## Performance checklist

- Source reads, hashes, unpacking and development stay on the existing verified
  source worker. Cache hits, retained mosaics and source exposure/WB reuse keep
  the existing source-signature path.
- Sparse sensor repairs add at most 65,536 index/value pairs, shared with the
  source. Their sorted coordinate list is also bounded. List membership uses
  binary search; constant-marker scans are linear in the bounded sensor size.
  No new full RGB copy is introduced. Ordered DNG warps reuse one active-area
  float plane after native demosaic scratch is released, as in the original
  Air 2S path, within the [approved RAW limits](architecture.md#limits); aggregate editor RSS is measured separately and
  high-resolution editor evidence is recorded in the resource ledger.
- The neutral picker checks at most 65,536 sparse replacements and samples its
  fixed patch with binary-search lookup. It does not develop or rasterize a
  frame. No decoding, color conversion or hashing moves onto the catalog owner.
- The only desktop admission change is the set of RAW filename extensions in
  Open. Preview, history, upload and cached source messages retain their existing
  triggers. No timers, polls or subscriptions are added.
- Native and full editor measurements are scoped in the resource ledger and
  verification evidence. These changes make no speedup or process-memory-bound
  claim; loaded-host timings are not a baseline.
- Exact tests compare stage-ordered gains/warps to separate reference stages,
  sparse correction to an undamaged mosaic, and physical green-site calibration
  to an independently calculated result. Existing original-source hashes,
  independent Air 2S numerical samples and editor history/reopen checks remain
  required. Crop/orientation continue sharing the developed float planes.
