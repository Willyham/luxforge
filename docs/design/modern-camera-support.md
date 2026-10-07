# Modern camera and drone support

## Scope

The data-driven RAW path covers the available files from the curated
250-camera selection and five flagship phone generations, including older
cameras still in use. This is a coverage selection, not a measured sales
ranking or a 99% active-device claim. Maintain exact make/model identities and
recording-mode evidence; see [corpus support](corpus-camera-support.md).

## Behavior and constraints

Profiles select bounded processing capabilities; no model-name processing
branches. Source-dependent calibration remains authoritative in each file.
LibRaw's camera list alone is not Luxforge qualification: its published list
assumes optional features which this build may not enable. Unknown modes,
unsupported mandatory corrections and incompatible metadata fail explicitly.
Do not invent full-sensor dimensions from advertised megapixels.

Use a selected subset of CC0 authentic files from raw.pixls.us, with URL,
SHA-256, license and observed decoder metadata recorded. Downloads and rendered
photos stay ignored and bounded. The curated CC0 originals are retained in
private R2 storage ([sample corpus](sample-corpus.md)). Validate
source preservation, mosaic/float correctness, calibrated dimensions and crop,
then representative background editor/history/reopen workflows. Separate each
mode's demonstrated evidence from outstanding controlled color/scene coverage.

The [owner-approved RAW resource contract](architecture.md#limits) is separate from JPEG, whose limits are unchanged. High-resolution models, container compression, missing crop
metadata, per-green black levels and DNG correction layouts may require shared
capabilities before a profile can be enabled. Never inflate the support count
with guessed entries or bypass required corrections to admit a model.

## Current evidence

The catalog contains 259 exact camera/phone profiles and 316 recording modes.
All 341 retained CC0 samples pass source preservation, full unpacked integer
hashes, metadata and finite as-shot/development checks. They cover 258 exact
identities and 315 modes; the owner Air 2S original supplies the remaining
profile/mode. Two monochrome sources develop identical R/G/B planes and refuse
colour-changing white balance. Their second development reference repeats unity gains.

The [corpus manifest](../../fixtures/sample-corpus.json) pins source URLs,
licenses, metadata and M4 development hashes. The
[independent unpack evidence](../../fixtures/corpus-camera-evidence.json)
records references for the 209 added sources, from the LibRaw-only path or,
for the Galaxy S22 JPEG XL DNG, libjxl 0.11.2. The existing focused
[evidence](../../fixtures/modern-camera-evidence.json) and
[selection](../../fixtures/modern-camera-selection.json) remain valid for their
130-source scope. Development hashes are regressions, not independent
photographic colour ground truth. The complete frozen repeat takes 670.067
seconds on the M4, including source hashing and two developments per source;
this is a functional trial, not an isolated performance distribution.

Sony A7 V Compressed HQ (8846) uses the staged upstream ARW6 backport and a
format-only 1 GiB decoder working-space budget. The native decode/two-development
trial takes 2.24 seconds and peaks at 1,233,829,888 bytes of process RSS (about
1.15 GiB). This is a single trial, not a total-process memory bound. Phone linear
RGB DNGs bypass demosaic; the S22 JPEG XL sample matches independent libjxl
integer output exactly. Selected phones without licensed files, including all
selected iPhones, remain acquisition gaps.

Eleven background Metal editor journeys cover new phone/layout/correction
families and both monochrome cameras, with exact reopen and source-preservation
checks; the [corpus design](corpus-camera-support.md#native-editor-evidence)
lists their scope.

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
| APS-C crop sizes on full-frame bodies (R6 4661 at 3584×2386, R6 Mark II 6404 at 3936×2612, A7R V 6239 at 6304×4180, A7C II 6869 and A7 IV 6936 at 4736×3132) | Unsupported recording mode | These particular shapes have no qualified catalog selector; exact sampled alternate frames can be admitted, but a listed camera does not admit every crop setting |
| Small and medium RAW sizes | Unsupported recording mode | Unsampled recording sizes remain unqualified; the retained corpus includes observed reduced/converted DNG modes with their own exact frame selectors |

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

64 previously measured catalog modes have `"unpacker": "rawspeed"`: RawSpeed fills their mosaic inside LibRaw's unpack, and `backend` reads `LibRaw 0.22.2 + RawSpeed c835b05a + librtprocess 9a858270`. A candidate is routed when it is exact on every local authentic sample of it and its adapter unpack is at least 1.3× faster through RawSpeed than through LibRaw. Every exact candidate passed the speed gate, so none was moved back to LibRaw: the lowest ratio is the Panasonic S5's 1.50×, and every other mode is 1.84× or more. The last column is each sample's median `RawSource::decode` time (identify, unpack and the mosaic copy, bytes in memory, nothing developed) with LibRaw forced and with RawSpeed, and their ratio, from 16 alternating observations per unpacker ([performance](../specs/performance.md#rawspeed-unpacking)). Each sample was decoded through both unpackers by the crate's `replaceable_catalog_modes_match_libraw_on_every_local_sample`: the mosaic, every metadata field but `backend`, and the as-shot and perturbed-white-balance developments are identical, and LibRaw's results equal what was recorded before routing: the evidence manifest's mosaic and development hashes, or for a sample marked (pin), the mosaic hash in `tests/real_files.rs` (the modes with no evidence entry). A sample marked (no record) has neither and is compared with LibRaw only. The full corpus regression preserves those modes' frozen references. Newly added modes retain LibRaw unless separately shown exact and faster through RawSpeed; membership of the decoder table alone does not route them.

| Mode | Decoder | Samples (raw.pixls.us id) | Routed | Unpack p50, LibRaw → RawSpeed |
| --- | --- | --- | --- | --- |
| `CanonEOS5DMarkIIIRaw14` | `lossless_jpeg_load_raw()` | 771 | yes | 234.1 → 66.3 ms, 3.53× |
| `CanonEOS5DMarkIVRaw14` | `lossless_jpeg_load_raw()` | 983 | yes | 314.9 → 88.7 ms, 3.55× |
| `CanonEOS5DMarkIVRaw14DualPixel` | `lossless_jpeg_load_raw()` | 980 | yes | 312.9 → 86.8 ms, 3.60× |
| `CanonEOS6DMarkIIRaw14` | `lossless_jpeg_load_raw()` | 1625 | yes | 291.0 → 94.5 ms, 3.08× |
| `CanonEOS7DMarkIIRaw14` | `lossless_jpeg_load_raw()` | 1064 | yes | 205.1 → 56.4 ms, 3.64× |
| `CanonEOS80DRaw14` | `lossless_jpeg_load_raw()` | 1294 | yes | 285.2 → 91.0 ms, 3.13× |
| `CanonEOSM6Raw14` | `lossless_jpeg_load_raw()` | 1382 | yes | 262.5 → 81.4 ms, 3.23× |
| `DJIFC220Dng16` | `packed_dng_load_raw()` | 1052 | yes | 7.9 → 3.7 ms, 2.16× |
| `DJIFC4382Dng16` | `lossless_dng_load_raw()` | 7823 | no | not routed |
| `DJIFC6310Dng16` | `packed_dng_load_raw()` | 2155 | yes | 12.8 → 5.8 ms, 2.20× |
| `DJIFC7303Dng16` | `packed_dng_load_raw()` | 4785 | yes | 7.7 → 3.5 ms, 2.17× |
| `DjiAir2sDng16` | `packed_dng_load_raw()` | owner Air 2S | yes | 12.9 → 5.9 ms, 2.18× |
| `FujifilmX100FRaw14` | `fuji_compressed_load_raw()` | 1936 | yes | 545.3 → 180.3 ms, 3.02× |
| `FujifilmX100VRaw14` | `fuji_compressed_load_raw()` | 3812 | yes | 580.9 → 203.2 ms, 2.86× |
| `FujifilmX100ViLossless14` | `fuji_compressed_load_raw()` | 7301 (pin) | yes | 843.7 → 303.8 ms, 2.78× |
| `FujifilmXE4Raw14` | `fuji_compressed_load_raw()` | 4446 | yes | 535.5 → 197.5 ms, 2.71× |
| `FujifilmXH2Raw14` | `fuji_compressed_load_raw()` | 6001 | no | not routed |
| `FujifilmXH2SRaw14` | `fuji_compressed_load_raw()` | 6007 | yes | 550.7 → 200.1 ms, 2.75× |
| `FujifilmXM5Lossless14` | `fuji_compressed_load_raw()` | 7748 | yes | 551.3 → 200.1 ms, 2.76× |
| `FujifilmXS20Raw14` | `fuji_compressed_load_raw()` | 6666 | yes | 584.4 → 202.7 ms, 2.88× |
| `FujifilmXT3Raw14` | `fuji_compressed_load_raw()` | 2783 | yes | 551.5 → 199.7 ms, 2.76× |
| `FujifilmXT5Lossy14` | `fuji_compressed_load_raw()` | 6123 | no | not routed |
| `FujifilmXT5Raw14` | `fuji_compressed_load_raw()` | 6122 | yes | 843.6 → 303.0 ms, 2.78× |
| `LeicaCLDng14` | `packed_dng_load_raw()` | 2489 | yes | 151.7 → 25.1 ms, 6.03× |
| `LeicaM10Dng16` | `lossless_dng_load_raw()` | 1603 | yes | 250.5 → 84.9 ms, 2.95× |
| `LeicaM10RDng16` | `lossless_dng_load_raw()` | 7853 | yes | 422.6 → 150.9 ms, 2.80× |
| `LeicaQ2Dng14` | `packed_dng_load_raw()` | 3204 | yes | 291.8 → 47.0 ms, 6.21× |
| `LeicaSL2Dng14` | `packed_dng_load_raw()` | 7872 | yes | 291.1 → 47.4 ms, 6.14× |
| `NikonD5600Raw14` | `nikon_load_raw()` | 1416 | yes | 216.5 → 76.3 ms, 2.84× |
| `NikonD7500Raw14` | `nikon_load_raw()` | 1534 | yes | 180.8 → 61.3 ms, 2.95× |
| `NikonD750Raw14` | `nikon_load_raw()` | 898, 896 (no record) | yes | 200.6 → 60.8 ms, 3.30× (898); 203.9 → 64.7 ms, 3.15× (896) |
| `NikonD780Raw14` | `nikon_load_raw()` | 3828 | yes | 233.8 → 88.7 ms, 2.64× |
| `NikonD850Lossless14` | `nikon_load_raw()` | 1840 | yes | 383.3 → 126.0 ms, 3.04× |
| `NikonD850Lossy14` | `nikon_load_raw()` | 1841 | yes | 384.6 → 126.3 ms, 3.04× |
| `NikonZ30Raw14` | `nikon_load_raw()` | 5813 | yes | 176.9 → 58.6 ms, 3.02× |
| `NikonZ502Lossless14` | `nikon_load_raw()` | 7762 | yes | 180.1 → 61.8 ms, 2.91× |
| `NikonZ50Raw14` | `nikon_load_raw()` | 3647 | yes | 194.7 → 70.0 ms, 2.78× |
| `NikonZ52Lossless14` | `nikon_load_raw()` | 7745 | yes | 204.6 → 66.6 ms, 3.07× |
| `NikonZ5Raw14` | `nikon_load_raw()` | 4136 | yes | 213.3 → 72.7 ms, 2.93× |
| `NikonZ62Raw14` | `nikon_load_raw()` | 4160, 4161 (no record) | yes | 253.6 → 101.6 ms, 2.50× (4160); 205.0 → 86.1 ms, 2.38× (4161) |
| `NikonZ63Lossless14` | `nikon_load_raw()` | 7819 | yes | 222.9 → 80.0 ms, 2.79× |
| `NikonZ6Lossless12` | `nikon_load_raw()` | 3585 (pin) | yes | 206.4 → 64.4 ms, 3.21× |
| `NikonZ6Lossless14` | `nikon_load_raw()` | 3582 (pin), owner Z6 (pin) | yes | 206.3 → 65.4 ms, 3.15× (3582); 203.6 → 65.5 ms, 3.11× (owner Z6) |
| `NikonZ8Raw14` | `nikon_load_raw()` | 6617 | yes | 425.4 → 161.4 ms, 2.64× |
| `NikonZ9Raw14` | `nikon_load_raw()` | 5146 | yes | 394.5 → 133.2 ms, 2.96× |
| `NikonZfRaw14` | `nikon_load_raw()` | 6885 | yes | 212.1 → 71.9 ms, 2.95× |
| `NikonZfcRaw14` | `nikon_load_raw()` | 4812 | yes | 179.0 → 60.0 ms, 2.98× |
| `OMDigitalOM1MarkIIRaw12` | `olympus_load_raw()` | 7262 | yes | 293.4 → 120.0 ms, 2.45× |
| `OMDigitalOM1Raw12` | `olympus_load_raw()` | 5283 | yes | 317.4 → 122.3 ms, 2.60× |
| `OMDigitalOM3Raw12` | `olympus_load_raw()` | 7796 | yes | 315.0 → 123.6 ms, 2.55× |
| `OMDigitalOM5Raw12` | `olympus_load_raw()` | 6343 | yes | 263.6 → 115.5 ms, 2.28× |
| `OlympusEM10MarkIVRaw12` | `olympus_load_raw()` | 4126 | yes | 275.6 → 109.4 ms, 2.52× |
| `OlympusEM1MarkIIIRaw12` | `olympus_load_raw()` | 3800 | yes | 258.0 → 109.5 ms, 2.36× |
| `OlympusEM1XRaw12` | `olympus_load_raw()` | 3041 | yes | 307.9 → 108.7 ms, 2.83× |
| `PanasonicDCG9M2Raw16` | `panasonicC8_load_raw()` | 6999 | yes | 188.3 → 87.2 ms, 2.16× |
| `PanasonicDCGH5Raw12` | `panasonic_load_raw()` | 1516 | yes | 65.5 → 31.6 ms, 2.07× |
| `PanasonicDCGH6Raw16` | `panasonicC8_load_raw()` | 5876 | yes | 181.0 → 97.3 ms, 1.86× |
| `PanasonicDCGH7Raw16` | `panasonicC8_load_raw()` | 8062 | yes | 189.4 → 85.5 ms, 2.22× |
| `PanasonicDCGX7MK3Raw12` | `panasonic_load_raw()` | 5967 | yes | 66.2 → 31.6 ms, 2.09× |
| `PanasonicDCS5M2Raw14` | `panasonicC8_load_raw()` | 7790 | yes | 173.6 → 94.4 ms, 1.84× |
| `PanasonicDCS5Raw14` | `panasonicC6_load_raw()` | 4096 | yes | 42.9 → 28.5 ms, 1.50× |
| `PentaxK1MarkIIDng14` | `lossless_dng_load_raw()` | 3345 | yes | 353.3 → 105.0 ms, 3.37× |
| `PentaxK3MarkIIIRaw14` | `pentax_load_raw()` | 4677 | yes | 223.2 → 66.2 ms, 3.37× |
| `PentaxK70Raw14` | `pentax_load_raw()` | 1141 | yes | 199.6 → 58.7 ms, 3.40× |
| `PentaxKPDng14` | `lossless_dng_load_raw()` | 1824 | yes | 246.3 → 74.2 ms, 3.32× |
| `RicohGRIIIDng14` | `lossless_dng_load_raw()` | 3115 | yes | 251.1 → 76.6 ms, 3.28× |
| `RicohGRIIIxDng14` | `lossless_dng_load_raw()` | 5818 | yes | 237.7 → 70.9 ms, 3.35× |

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

Candidate records require source-grounded mode selectors and capability choices,
with reproducible authentic evidence. A profile can be enabled only with its
required processing implemented and authentic decode/development evidence.
Track concrete blocked modes and missing fixtures; distinguish implementation
from broad image-quality qualification.

The catalog's 259 identities and 316 recording modes are delivered; qualification
applies to the sampled encodings, not every setting of those cameras. Controlled
colour/detail, unsampled modes and native Windows/Linux editor evidence remain
outstanding. Nikon High Efficiency and High Efficiency★ remain refused until
upstream support is pinned and qualified. Sony A7 V Compressed HQ (8846) and
lossless compressed (8845) are delivered; other compression and crop settings
need their own decoder and authentic qualification. Existing metadata,
corrections, source preservation and history remain regressions. New coverage
requires quick and rendered checks; feature completion carries the scoped timing
and full verification before a broader support claim.

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
