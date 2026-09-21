# DJI Air 2S DNG editing

Status: implemented on the existing continuous RAW architecture, with supplied-file native/JSON/background M4 verification and broader scene/platform qualification remaining. This extends the existing RAW source module; it does not introduce a separate editor or convert originals to an intermediate image.

## Behavior and scope

The initial qualified mode is the supplied DJI FC3411 (Air 2S) DNG: one uncompressed integer Bayer image with 16-bit stored samples. Open/import, exposure, as-shot/custom WB, neutral picking, exact transforms, crop, historical previews, undo/redo, versions, Restore and reopen must retain the original as the source. The source remains byte-for-byte unchanged. Generic DNG, other DJI models, edited/merged DNGs, additional firmware/modes and exporter work are outside this qualification.

The file requires OpcodeList3 GainMap (9) and WarpRectilinear (1). Both must be interpreted and applied in the declared order and processing domain before support is enabled. Unknown mandatory operations, unsupported versions/stages or malformed coefficients fail explicitly while preserving the last good image and catalog. Optional operations have an explicit supported/skipped policy and recorded interpretation; successful decoding alone does not prove correct rendering.

## Integration constraints

- LibRaw remains the pinned unpack/calibration provider and the source mosaic remains immutable. Existing RCD Bayer development remains the demosaicer.
- DNG stage-3 corrections operate on high-precision camera data after demosaic and before camera-to-working conversion, with active-area coordinates, pixel centers, scaling and opcode order established from Adobe's specification and SDK reference. The Lightwell float path preserves signed/highlight values until terminal display, as required by the existing RAW editing contract. Adobe DNG 1.7.1 page 105 instead specifies clipping to [0,1] after each stage-2/3 opcode; this deliberate difference is recorded in the interpretation and is not claimed specification equivalence.
- Parse only bounded TIFF/opcode structures from the already verified encoded snapshot. Capture typed immutable correction metadata, stage/order/version and effective crop/calibration provenance in the source interpretation. Reopen must reproduce it exactly; unsupported stored shapes fail without migration or data loss. Correction provenance is required for DNG, while the existing NEF/RAF metadata shape remains unchanged.
- Share corrections with the existing source worker and cache identity. WB redevelopment reapplies required corrections from retained sensor data; exposure and geometry reuse the corrected float development. No new polling, source reread or UI-only operation is needed.
- Keep source framing and recipe content coordinates stable across adjustments. Neutral picking must map corrected upright content to the relevant sensor sites and account for spatial/color correction; the old direct unwarped coordinate mapping is insufficient. Point queries remain bounded and never develop a full image on the catalog owner.
- Bound dimensions, list lengths, coefficients, temporary buffers and output arithmetic. Prefer bounded plane scratch over another full RGB development; account for actual liveness and cancellation before enabling the mode. Required optical correction is a source stage, followed by the existing recipe geometry without repeated history flattening.
- Existing NEF/RAF decoding, calibration, crop and byte-exact JPEG behavior remain verified. Current APIs and generated RAW controls serve the new source mode; no placeholder controls or separate Basic layer.

## Acceptance

1. Independent numerical references establish GainMap interpolation/planes/pitches and WarpRectilinear coordinates/coefficients/edge behavior, including identity, nontrivial spatial/chromatic cases and malformed inputs. Compare production outputs within a declared tolerance before accepting goldens.
2. The supplied file's sensor identity, active/default areas, levels, calibration, opcode payloads and source fingerprint are recorded locally. Integer unpacking and corrected samples agree with independent references; captured images establish actual editor framing/detail, without treating the camera JPEG as a color target.
3. Actual JSON and background native M4 workflows cover development, WB/exposure changes after geometry, neutral picking, history/versions/Restore, process reopen and unchanged originals. Failed required operations, changed interpretation and unavailable providers preserve data and report explicit errors.
4. Run the full repository check, relevant NEF/RAF/JPEG regressions, photo-sized release latency/RSS diagnostics and rendered checks with correlated source/entry/recipe state. Report CPU/GPU/readback scope and retained tails; no unsupported cross-platform or memory-budget claim.
5. Update current feature status, user guide, source support coverage and the existing RAW task plan only after evidence passes. Record exact qualified scope and remaining limitations.

## Clipping boundary

The existing [RAW pixel/color contract](initial-raw.md#pixel-and-color-contract) requires finite negative values and highlight headroom to survive until terminal display. Air 2S support inherits that contract; it does not introduce an early clipping stage. The supplied GainMap reaches roughly 4.1–4.7 depending on channel, so per-opcode clipping would discard detail before the existing exposure operation. The interpretation explicitly names unclipped evaluation. Independent references distinguish it from the normative clipped DNG variant; this is a neutral Lightwell development, not a strict Adobe rendering match.

## Coordinates, calibration and bounded evaluation

The supplied sensor is 5568×3648. ActiveArea is `(96,0,5472,3648)` and the camera default crop is `(100,4,5464,3640)` in full-sensor coordinates. The required opcode areas use active-local coordinates. DefaultScale and BestQualityScale must both be unity; other scaling and fractional crops fail explicitly in this initial mode. The parser selects one uncompressed 16-bit CFA sensor SubIFD, validates its full-height strip and active rectangle against the native decoder, and rejects ambiguous/duplicate required tags or operations attached to a different IFD.

GainMap evaluates each channel's bilinear map at the normalized pixel center `((x + 0.5) / width, (y + 0.5) / height)`, then replicates map edges. The demonstrated layout is a three-plane map affecting all three camera channels at unit row/column pitch. Correction happens before the camera-to-working matrix. WB remains before nonlinear demosaic as in the existing RAW pipeline; in the unclipped linear optical stage, per-channel gains commute with gain-map multiplication and channel-separated resampling.

WarpRectilinear uses the Adobe SDK's exclusive image bounds to place the normalized center and the maximum distance from that center to the four bounds corners as its normalization radius. Its per-channel radial/tangential mapping locates source samples. Reconstruction uses a separable Keys cubic (`A = -0.75`), 128 fractional phases and replicated active-image edges. An exactly identity channel maps coordinates exactly and skips resampling after gain; floating-point roundoff must not turn an integer coordinate into the preceding fractional phase. The interpretation identifies these choices; geometry follows the SDK coordinate convention rather than claiming literal equivalence to the specification's bottom-right-pixel wording. Finite bounds, radial monotonicity and a bounded Jacobian grid reject collapsed/folded mappings.

The DNG supplies daylight ColorMatrix2 and a tungsten ColorMatrix1. Temperature/Tint uses the validated fixed D65 XYZ-to-camera ColorMatrix2, consistent with the editor's existing fixed-matrix controls. Identity AnalogBalance and the demonstrated absence of extra camera/forward calibration are required. Both embedded matrices and the selected calibration are recorded in provenance. Dual-illuminant interpolation and camera-profile appearance matching are outside this initial scope. The fixed daylight calibration needs a blue sensor gain of about 28.31 at the advertised 2000 K / +100 tint endpoint. The shared gain bound is therefore 32, consistently enforced by resolved payloads, direct-gain commands and development; values are never silently clamped to fit the bound.

Neutral picking maps the upright default-crop coordinate into the corrected sensor domain, then maps each of the 13×13 CFA sites through that channel's warp. Each lookup reads the nearest matching-CFA sensor site and applies the spatial gain. Dark/clipped rejection uses normalized sensor values before gain, so optical gain above one is not itself classified as sensor saturation. This is a bounded sensor-patch estimate, not a full demosaiced color measurement; it visits at most 169 sites without reading the file or allocating a frame.

## Allocation and performance review

For this sensor, retained u16 samples require 38.74 MiB and the three float32 camera planes 232.45 MiB. Demosaic temporarily uses a 77.48 MiB normalized mosaic, released before optical correction. GainMap modifies a plane in place; WarpRectilinear reuses one 76.15 MiB active-area float plane across channels, then copies that channel back. It never creates a second full RGB development. The camera matrix remains in place, source crop/orientation remain views, and terminal default-crop RGBA costs 75.87 MiB. Native/GPU/display overhead is additional; these allocation sizes are not a process RSS bound.

The encoded-source limit, dimensions and existing source-worker liveness gate still bound admission. TIFF traversal permits at most 16 IFDs, 256 entries per IFD, 256 aggregate opcodes and 1 MiB aggregate opcode data; gain grids are capped at 256×256×3. The optical pass checks cancellation every 64 rows and does not publish a partially corrected development.

No source read/hash/decode path, desktop refresh or timer is added. Import still uses the verified snapshot; WB reuses the mosaic; exposure/geometry reuse float data. Owner work adds only strict bounded interpretation parsing and the bounded picker mapping. Thirty complete native DNG editor/reopen trials, a packaged-app journey, Nikon/Fujifilm regressions and photo-sized before/after JPEG diagnostics pass; their latency/RSS distributions and retained tails are recorded in the [performance measurements](../specs/performance.md#air-2s-dng-measurements). These observations do not establish a process-wide memory bound. Native-only timings do not establish editor responsiveness or memory acceptance.

## References

- [Adobe DNG specification 1.7.1](https://helpx.adobe.com/content/dam/help/en/photoshop/pdf/DNG_Spec_1_7_1_0.pdf), including the original opcode definitions used by DNG 1.4.
- [Adobe DNG SDK GainMap implementation in Android](https://android.googlesource.com/platform/external/dng_sdk/+/refs/heads/android14-prebuilt-test/source/dng_gain_map.cpp).
- [Adobe DNG SDK lens correction implementation in Android](https://android.googlesource.com/platform/external/dng_sdk/+/refs/heads/android14-prebuilt-test/source/dng_lens_correction.cpp).
- [RAW integration contract](raw-integration.md) and [performance rules](../engineering/performance-rules.md).
