# Popular camera support

Status: implemented and verified on the M4 Mac.

Luxforge should open the RAW files most photographers actually make. The [popularity study](../research/popular-cameras.md) ranks the cameras most in use and their recording modes. Most of those modes need only an entry in the [camera catalog](raw-camera-profiles.md), because LibRaw decodes the file; this design admits them, often at the camera's factory default, and makes the one unsupported default, Nikon High Efficiency, an explicit refusal.

## Behavior

**Admitted modes.** Each camera below has the listed recording modes, qualified with an authentic CC0 sample the way every catalog mode is ([modern camera support](modern-camera-support.md#reproducing-adapter-qualification)). The sample IDs are raw.pixls.us IDs. Where a label is ambiguous, the mode is confirmed from the decoder, bit depth, size and container marker the adapter reads, not from the file name.

| Camera | Admitted modes | Samples |
| --- | --- | --- |
| Sony A7 III | Compressed | 2414, 2415 |
| Sony A7 IV | Compressed; lossless compressed | 6932; 6929 |
| Sony A7C II | Compressed; lossless compressed | 6868; 6867 |
| Sony A7R V | Compressed; lossless compressed | 6235; 6232 |
| Sony a6700 | Lossless compressed | 6736 |
| Sony ZV-E10 | Compressed | 4855 |
| Fujifilm X-T5 | Uncompressed; lossy compressed | 6124; 6123 |
| Fujifilm X-M5 | Uncompressed; lossless compressed | 7747; 7748 |
| Nikon D850 | Lossless compressed; lossy compressed | 1840; 1841 |
| Nikon Z6III | Lossless compressed | 7819 |
| Nikon Z50II | Lossless compressed | 7762 |
| Nikon Z5II | Lossless compressed | 7745 |
| Canon EOS 5D Mark IV | Single-frame RAW | 983 |
| Canon EOS 90D | RAW; C-RAW | 4649, 4650 |
| Canon EOS 5D Mark III | RAW | 771 |
| Canon EOS R6 Mark II, R6, R5, R5 Mark II, R7, R10 | C-RAW | 6409, 4660, 4698, 7882, 5634; none for the R10 |

Canon RAW and C-RAW share one decoder, and each C-RAW sample is in the evidence. The R6 Mark II's 6188×4120 sample (6405) and the R5's 8352×5586 sample (4697) are Dual Pixel RAW: two full-size frames, of which the adapter decodes the primary one, as for the 5D Mark IV's Dual Pixel CR2. They are admitted as Dual Pixel modes. Sony's lossless compressed ARW stores the sensor in 512-pixel tiles, so its modes declare the padded frame LibRaw reports, the one catalog capability added for these modes ([camera profiles](raw-camera-profiles.md#field-reference)). The Z50II and Z5II use configured calibration, from RawSpeed's camera data, because pinned LibRaw has no matrix for them. The R10 has no C-RAW sample in the public corpus.

The ranked cameras not in the table open in their default and common modes through their own catalog entries: Canon R50, R10, R7, R6 Mark II, R5, R5 Mark II, R6 and 6D Mark II RAW; Sony a6400 and a6700 compressed; Nikon Z6II, Zf, Z8 and Z9 lossless, and D750 lossless and lossy; Fujifilm X-T5 lossless; OM System OM-1; Ricoh GR III; Panasonic S5II.

**Nikon High Efficiency is refused explicitly.** A NEF whose Nikon maker note records compression 13 (High Efficiency) or 14 (High Efficiency★), or whose raw data begins with the JPEG XS start-of-codestream and capabilities markers (`FF10 FF50`), is refused before LibRaw unpacks it, whatever the camera model, and whether or not the model is catalogued. The error is a distinct RAW error that the core maps to `unsupported-input`. Its message names the format and the remedy: record Lossless compressed RAW. As a second check, the native adapter refuses any file for which LibRaw selects `nikon_he_load_raw()`. Without these checks a catalogued body's High Efficiency file fails inside LibRaw's unpack as a generic decode error, and on the Z50II and Z5II LibRaw 0.22.2 misreads the file as lossless and returns corrupt samples. Luxforge waits for upstream LibRaw support and has no High Efficiency decoder.

**Further sampled modes.** Sony A7 V Compressed HQ (8846) is admitted through the staged upstream ARW6 decoder, with its own 1 GiB native working-space budget. Exact sampled alternate frame sizes are catalogued individually. Unsampled crop, resolution and compression settings fail explicitly; see [corpus camera support](corpus-camera-support.md).

## Constraints

- Data only where the catalog's capabilities suffice. A new mode is a catalog entry with evidence, never a camera-name branch ([camera profiles](raw-camera-profiles.md#acceptance)). If a mode needs a new capability, such as a new container probe value, implement the capability generally with tests before admitting the mode.
- Every admitted mode has authentic evidence: source hash before and after, the full mosaic hash, crop and calibration metadata, and finite as-shot and perturbed-white-balance development, in [the evidence manifest](../../fixtures/modern-camera-evidence.json). The [selection](../../fixtures/modern-camera-selection.json) records each sample's URL, hash and licence. Its format holds more than one sample per model, so the evidence covers every mode, not one per model.
- The High Efficiency check reads only bounded container structure through the NEF reader and runs before any native unpack. It adds no full-file scan beyond the raw-strip marker read.
- No compatibility shims: admitting a mode adds its identifier and changes no other mode's.
- Original bytes are never written. Refused files keep the previous photo on screen, as every failed Open does.

## Acceptance

- Every mode in the table either passes `qualify_profiles` with evidence recorded, or is listed in [modern camera support](modern-camera-support.md) with the concrete reason it is refused.
- The six High Efficiency samples (Z9 5147, Z8 6618, Zf 6886, Z6III 7815, Z50II 7763, Z5II 7743) are refused with the High Efficiency error, before unpack, including the uncatalogued bodies. The same bodies' lossless samples are admitted.
- Unit tests cover the High Efficiency detection with synthetic NEF maker notes (13, 14, absent, truncated and malformed) and the JPEG XS marker, and the core's error-kind mapping.
- Representative background editor verification (`cargo xtask raw-editor`) passes for one admitted mode per brand: Sony compressed, Fujifilm uncompressed, and Nikon lossless and Canon each on a body this design catalogues.
- [Feature status](../features.md), the [user guide](../user-guide.md) and [modern camera support](modern-camera-support.md) state the counts and the High Efficiency refusal.

## Performance

Admission adds catalog entries to a static table searched once per open. The High Efficiency check reuses the NEF maker-note reader that container validation already runs and one four-byte read at the raw strip offset, on the source worker. Refusing High Efficiency before unpack avoids wasted LibRaw work. Development, rendering, the UI thread and the catalog owner are untouched.
