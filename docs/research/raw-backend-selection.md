# Initial RAW processing selection

Implementation direction: use LibRaw for unpacking and metadata, with librtprocess RCD for Bayer and one-pass Markesteijn for X-Trans. The native adapter is private and statically built from pinned source; Rawler remains an independent experiment, not an automatic runtime fallback. The owner NEF/RAF pass editor editing and reopen; broader visual quality, resources and portability remain separate qualification gates.

## Reproducible candidates

| Component | Pin | Role |
| --- | --- | --- |
| [LibRaw](https://github.com/LibRaw/LibRaw/tree/0.22.2) | 0.22.2, revision `b93f6e45c194f5df9b02a43b1af9a54b4f41f33f`; source archive SHA-256 `627928088300ecde6ca91ffd202e189203f04ad61ad12f0fe9dc57b9a7a0fb3c` | Unpack original sensor samples and read interpretation metadata |
| [Rawler](https://github.com/dnglab/dnglab/tree/v0.8.0/rawler) | crate 0.8.0; independent probe lockfile | Sample and metadata comparator |
| [librtprocess](https://github.com/CarVac/librtprocess/tree/9a858270acb2096e2e403d932760ee688fcac425) | revision `9a858270acb2096e2e403d932760ee688fcac425` | Established float demosaicers extracted from RawTherapee |
| [ExifTool](https://github.com/exiftool/exiftool/tree/13.59) | 13.59; source archive SHA-256 `87d3317882fdae9cb4dcfe57a96a378d0132ffc02c731315bf128b19ddcf7aac` | Independent technical capture metadata, local diagnostics only |

The isolated comparison workspace is `probes/raw`; source provenance and coverage are in [fixtures](../../fixtures/README.md). The supplied files comprise a Z6 14-bit lossless NEF, X100VI 14-bit uncompressed RAF and DJI FC3411 DNG1.4 with uncompressed 16-bit storage. Four individually obtained public CC0 samples cover Z6 12/14-bit lossless and X100VI uncompressed/lossless. Stored u16 precision is not an assertion about sensor precision. Originals remain outside Git.

## Findings that determine the implementation

Both unpackers successfully decoded all seven inputs and source hashes were unchanged. Every full-sensor u16 sample agrees for the three Fujifilm inputs and the DJI input. Nikon samples do **not** agree: LibRaw is one code higher at roughly three quarters of positions and otherwise equal. In the pinned Rawler source, `decoders/nef.rs` routes compressed NEF values through `LookupTable::dither` in `bits.rs`; even an identity curve maps about three quarters of inputs to `i-1`. LibRaw stores the curve entry directly. The observed lossless NEF mismatch follows that code path. Rawler's default unpacking path therefore cannot be used as an exact sensor oracle or the selected lossless decoder without a separately tested correction.

Metadata must retain its domain and provenance. LibRaw's scalar black level is zero for Fuji and DJI because their values live in its repeating `cblack` table; the equivalent levels are 1023 for Fuji and 4096 for DJI. The Nikon scalar level is 1008 at 14 bits and 252 at 12 bits. Rawler's camera-table Nikon white is 15520/3880, while LibRaw reports sensor saturation 16383/4095. The adapter uses explicit sensor saturation rather than unrecorded maximum-pixel scaling. Default crop and active-area rectangles differ between libraries and must be reconciled against container metadata. The rotated owner NEF also demonstrates that Rawler's raw-image orientation field alone is insufficient: it reports Normal while independent EXIF reports orientation 8.

Neither high-level converter is the retained editing pipeline. LibRaw's dcraw-compatible processing produces integer output. Rawler's float helper clips below-black values while scaling and changes negative/overrange colors in its calibration step. Calling either and relabeling the result as unbounded linear data would violate the continuous editing contract. The selected path instead retains the unpacked mosaic, applies explicit normalization and WB, runs the established float demosaicer, then preserves signed/out-of-range calibrated RGB through the recipe until terminal display conversion. RCD's own nonnegative reconstruction clamp is an algorithm-stage property; it is separate from later camera-matrix negative values, which must remain intact.

WB belongs before demosaic. On one 2040×2040 crop from each owner file, comparing the same gains before versus after demosaic produced materially different results after proper per-site black subtraction: absolute sensor-scale differences had p95 about 16.7 codes for Z6 RCD, 21.4 for X100VI Markesteijn1 and 678.5 for DJI RCD. These are algorithm differences, not visual acceptance thresholds. A fixed as-shot demosaic cannot be reused for arbitrary WB by multiplying its output. Retain the immutable u16 mosaic, redevelop on WB changes and reuse the float development for exposure/composition changes.

## Measured scope and limits

The initial Mac experiment used release builds, full photo-sized inputs, static librtprocess with OpenMP disabled and one trial per case. It did not control the OS file cache and is not a p95 responsiveness or full-editor memory claim. Full-frame float demosaicing measured approximately 0.30 s / 425 MiB peak process RSS for Z6 RCD, 1.62 s / 707 MiB for X100VI one-pass Markesteijn and 0.22 s / 353 MiB for DJI RCD. X100VI three-pass Markesteijn took about 3.85 s in the same isolated experiment. The working choice is one-pass; real 100% rendering remains an acceptance gate.

RCD and Markesteijn ignore the Boolean return from the progress callback. Cancellation during such a stage suppresses its eventual result and releases storage when the call returns; it cannot claim immediate interruption. LibRaw unpack callbacks and checks between stages can cancel earlier. The [integration ledger](../design/raw-integration.md#allocation-and-liveness) must bound overlap between retained mosaic, old/new float frames, display buffers, native scratch and GPU uploads. A single native peak does not establish the whole-editor budget.

## Outstanding qualification

- Complete editing/resource qualification over the adapter's validated mode/crop/orientation metadata. The supplied DJI DNG now has independently checked GainMap/WarpRectilinear corrections and actual editor evidence under the [Air 2S contract](../design/air2s-dng.md). Further controlled-scene/mode qualification remains separate; generic DNG support is not implied. Unsupported mandatory corrections preserve the previous photo.
- Extend the supplied-file editor evidence with controlled chart, deliberate highlight/shadow, fabric/foliage and broader ISO/lighting/DR/shutter scene coverage. Signed/headroom numerical references do not replace those scenes.
- Expand the passing WB/exposure/history/reopen and source-preservation workflows with systematic native fault injection, malformed containers, repeated replacement and cancellation stress.
- Measure full-editor stage timings, p50/p95 and CPU/GPU memory after integration; verify the existing 24/60 MP JPEG path.
- Complete portable static packaging checks and inventory. Native Windows/Linux desktop evidence and manual dependency/asset audits remain explicitly deferred.
