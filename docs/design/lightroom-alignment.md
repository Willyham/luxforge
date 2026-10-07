# Lightroom alignment

Status: decided and planned (2026-10-06); nothing is built and implementation is not authorized ([task plan](../../tasks/lightroom/lightroom-alignment.json)). The owner decided every question on 2026-10-06 ([decided](#decided)): Luxforge's sliders follow Lightroom Classic's responses (level B) for every supported setting, with targeted algorithm changes (level C) where a dimension still differs widely.

It stands alone. [Preset import](presets.md#import) and the proposed [Lightroom import](lightroom-import.md) use its results to convert values, and every Luxforge user gains from controls that behave the way photographers coming from Lightroom expect, but neither importer waits on it.

## Why

Preset import transfers a Lightroom value to the Luxforge control "with the same name, range and direction". It claims nothing more, because nothing more has been measured: the [slider audit](../research/lightroom/slider-parity.md) compared ranges, not responses. Basic's numerics were chosen "starting from the Lightroom research … no value is claimed as Lightroom-equivalent" ([decisions](../decisions.md#basic-adjustments-and-histogram)), and Presence, the mixer, the vignette and Detail were each frozen by a study of their own against an independent reference, not against Lightroom. So Clarity 40 in Luxforge is not known to resemble Clarity 40 in Lightroom; it may be half as strong, or twice.

That costs in three places:

- **Imports.** An imported preset or catalog is only as faithful as the value conversion.
- **Feel.** A photographer's hands know that Highlights −60 tames a bright sky. If Luxforge's −60 does a third of that, the editor feels weak or wrong, whatever its picture quality.
- **Algorithms.** Where Lightroom's control does something different in kind (a locally adaptive Highlights, a Vibrance that protects skin), a matching number cannot help; only a change in what the control does can.

## Levels

AGENTS.md's seventh pillar makes Lightroom "a familiarity reference, not a feature checklist or a rendering target", and the [RAW looks](raw-looks.md) decision keeps Luxforge's own base rendering. Neither changes: Lightroom's picture is not the target. Its controls' responses are, because they are the result of years of research and because photographers' hands know them: a person used to +10 Clarity or −10 Vignette expects about the same result. There are three levels:

| Level | Changes | Who sees it | Risk |
| --- | --- | --- | --- |
| **A. Translation** | Nothing in Luxforge's controls or rendering. A calibrated conversion from each Lightroom value to the Luxforge value with the closest effect, used only by the importers | People importing presets or catalogs | None to existing photographs |
| **B. Response alignment** | A control's scale: the curve from its slider value to the strength of the effect, so a value does about what the same value does in Lightroom. The algorithm is unchanged | Everyone | Existing photographs render differently at the same stored values |
| **C. Behaviour alignment** | A control's algorithm, where Lightroom's behaviour is measurably what photographers expect and Luxforge's is not | Everyone | As B, plus a new study and frozen reference per change |

**Decided** (owner, 2026-10-06): **B for every supported setting**, so a slider value does about what the same value does in Lightroom; **C targeted**, for a dimension where a large difference remains after B ([the threshold](#when-a-setting-needs-c)). A is the bridge: the importers use its fitted conversions for a setting until that setting's B change lands, and afterwards only for what remains of the difference. The residual of the best translation is the measure that ranks the settings for B and selects them for C.

## What is measured

Every setting Luxforge supports that has a Lightroom counterpart:

| Group | Settings | Kind |
| --- | --- | --- |
| White balance | JPEG Temperature, Tint (relative); RAW Temperature (K), Tint | Pointwise colour |
| Basic tone | Exposure, Contrast, Highlights, Shadows, Whites, Blacks | Pointwise tone; Highlights and Shadows checked for locality |
| Basic colour | Vibrance, Saturation | Pointwise colour |
| Tone curve | Point curve | Pointwise; Lightroom applies it per channel, Luxforge to luminance |
| Presence | Texture, Clarity, Dehaze | Spatial |
| Colour mixer | Hue, Saturation, Luminance of eight ranges | Pointwise colour |
| Vignette | Amount, Midpoint, Roundness, Feather (Lightroom's Highlight Priority style) | Positional |
| Detail | Sharpening Amount, Radius, Detail, Masking; Luminance and Colour noise reduction with their Detail | Spatial |
| Geometry | Crop and straighten (expected exact), manual Perspective Vertical and Horizontal, lens profile distortion | Geometric |
| Masks | Linear and radial falloff, feather, brush size, feather and flow, luminance and colour range | Coverage |
| Base rendering | Adobe Color against the Standard look; Camera Matching profiles against Match camera | Whole picture, reported only |

The base rendering is measured so that it can be **subtracted**, not to change it: the RAW looks decision stands unless the owner reopens it ([A5](#decided)).

The decided [colour-grading extension](colour-grading.md) delivers its working controls and direct preset mappings first. Its final TASK-010 adds independent-reference analysis and grading response refinement, reusing this rig or implementing its grading-only slice; neither this programme nor an owner review/export is a prerequisite for the working feature. Actual exports are required for measured Lightroom figures, which remain unmeasured until supplied. The grading-only tooling scope is covered by the owner’s 2026-10-07 [decision](../decisions.md#colour-grading); unrelated alignment work remains separately planned.

## The rig

Lightroom cannot be run by an agent or in CI; it needs the owner's installation and licence. The rig keeps the owner's part to one import and one export per round, and everything else in Rust, so the project keeps one toolchain:

1. **Generate.** `cargo xtask lr-align generate --round N` writes a round folder: for each test image and each variant (one setting at one value, or a pair), a copy of the image named for its variant, with the variant's settings as an XMP sidecar beside a RAW copy or embedded in a JPEG copy; and a manifest of every variant's settings. Originals are never touched; only the round's own copies carry XMP.
2. **Lightroom (owner).** Import the round folder into a scratch Lightroom catalog with "read metadata from files", select all, export with the round's saved export preset: 16-bit TIFF, sRGB, full size, no output sharpening, no metadata. About 2,000 variants a round.
3. **Luxforge.** The same variants render through the CPU reference renderer, the renderer of correctness under [GPU-first](gpu-first.md), headless, to 16-bit buffers in the same sRGB encoding.
4. **Measure and fit.** `cargo xtask lr-align measure --round N` reads both, measures, fits and writes the round's figures, tables and contact sheets under the evidence directory. Reading Lightroom's TIFFs needs a pinned TIFF decoder as a development dependency of the study, never of the editor.

A Lightroom Classic plugin (Lua, through its SDK) could apply settings and export without the import step; it was not chosen, because it would add a second toolchain ([A3](#decided)).

## Test images

- **Synthetic targets, JPEG**, where both editors treat the file as an already-rendered sRGB picture and Lightroom applies no base profile to it, so the measured change is the setting alone: a 256-step grey wedge; an Oklab lightness × hue × chroma grid of flat patches; a step-edge and zone-plate chart for the spatial settings; a flat field for the vignette; geometric grids for crop, straighten and perspective. Patches are large enough to average away JPEG quantization.
- **Real RAW photographs** from the CC0 [sample corpus](sample-corpus.md), chosen across cameras, dynamic range and subject (skies, skin, foliage, night), and a colour-checker frame shot by the owner on the Z6 and X100VI, for RAW white balance, Exposure, Highlights against real sensor headroom, Dehaze on haze, and Detail on real noise.
- **The owner's own edited photographs** for validation only ([below](#validation-on-real-edits)), never committed.

## Measuring a response, not a look

Two editors' base renderings of a RAW file differ (Adobe Color against Standard), and that difference would swamp the effect of one slider. Every comparison is therefore of **responses**: the change a value makes relative to the same editor's rendering of the same image at the setting's neutral value. To first order the base rendering cancels. JPEG targets give the cleanest responses; RAW photographs show what survives a real base rendering.

Per kind of setting:

- **Pointwise tone and colour.** The transfer function each value applies, read from the wedge and the patch grid: output against input lightness, Oklab chroma ratio and hue shift per hue sector. Differences in ΔE2000 and signed ΔL\*.
- **Spatial.** On the edge chart: overshoot height and halo width; on the zone plate: gain per spatial frequency band. On photographs: band-pass energy at three scales, and whether Lightroom's Highlights and Shadows are local (a bright patch's rendering depending on its surroundings) or global as Luxforge's are.
- **Positional and coverage.** Radial profiles of the vignette and of mask falloffs, the radius at 50% and the shape of the ramp.
- **Geometric.** Grid-point positions; crop and straighten are expected to agree within a pixel and are a test of the conversion, not a calibration.

## Fitting the translation (level A)

For each setting `s`, a monotone map `g_s` from the Lightroom value to the Luxforge value that minimizes the response difference over the round's images, stored as a frozen table of knots across the Lightroom range (21 knots across −100..100, more where the response bends). The fit reports:

- **The map** and how far it departs from the identity (the scale gap);
- **The residual** at the best map: the part of the difference no value of the Luxforge control can remove. A small residual means a translation is enough; a large one means the controls do different things, and the setting becomes a candidate for C;
- **Range shortfall**: Lightroom values whose best Luxforge value lies outside the Luxforge range. These are reported, never clamped, as the importer already refuses rather than clamps.

Settings combine non-additively. Round 1 measures one setting at a time; round 2 the pairs that interact most (Exposure with Highlights and Whites, Contrast with Blacks and Whites, Vibrance with Saturation, Clarity with Texture, Dehaze with Contrast) to check that translations fitted alone still hold together; a pair whose combined residual grows sharply is reported.

The maps become rows of the preset mapping with a calibrated rule ("value converted through the alignment calibration"), so preset import, the catalog import and `lightroom.remap` share them. Each map is a frozen fixture with an exact test; a new round that changes a map is an ordinary reviewed change.

## Changing a control (levels B and C)

**B, a response change**, refits the curve from a slider's value to the strength the module's algorithm applies, so that the level A map for that setting becomes the identity within the study's tolerance. The fitted map is folded into the control: what was Luxforge's value `g_s(v)` becomes the value `v`. The algorithm, its stage and its processing cost are unchanged. Every supported setting gets one, in the [order](#decided) A2 sets.

### When a setting needs C

A setting goes to C when, after its B change, the response still differs in a way no value can remove. Decided ([A8](#decided)): on the round's targets at Lightroom values of ±25 and beyond, a median ΔE2000 above 2 against Lightroom's response, or a qualitative difference in kind found by the measures (a local Highlights where Luxforge's is global, a halo of a different radius, a Vibrance that does not protect skin). The study reports the dimension that differs, and the C change targets that dimension only: Clarity's halo radius, say, not Clarity as a whole.

Both kinds of change follow the path every delivered module's numerics followed: a study with figures and contact sheets, an independent `f64` reference frozen with it, the CPU unit exact against the reference, the GPU program within the declared tolerance, the module's payload format marker raised so that recipes of the earlier meaning are refused explicitly rather than re-rendered silently (current shapes only), and the owner's review of the sheets before it is frozen. The level A maps for that setting are refitted afterwards.

C candidates are ranked by residual times how often the setting is used. Likely ones, to be confirmed by measurement, not assumed: Highlights and Shadows (whether Lightroom's are local), Clarity's and Texture's scale and halo radius, Dehaze, Vibrance's protection of skin tones, the mixer's range widths and how far Hue moves a colour, and the vignette's midpoint curve.

**Outputs only** ([A4](#decided)). Only Lightroom's rendered outputs are measured. No Adobe code, SDK source or binary is read, decompiled or used, and no Adobe profile, table or curve is copied or shipped; every frozen reference is justified by measured outputs alone. Published format specifications (DNG, XMP) may be read. No legal review has been done and none is claimed.

## Validation on real edits

The translations are fitted on test images; they are judged on real photographs. Lightroom's own preview of a photograph is its rendering of that photograph's real settings, and the [Lightroom import](lightroom-import.md#lightrooms-previews) already reads it. With the owner's consent, `lr-align validate` reads the owner's catalog read-only, renders a sample of their edited photographs with Luxforge through the import mapping, with and without the calibration, and compares each to Lightroom's preview (downscaled to the preview's size, with base rendering differences measured on unedited photographs and reported separately). The figure that matters is whether calibration moves the median photograph closer, and by how much. Results are recorded as figures only; no photograph leaves the machine or is committed.

## Deliverables

- `cargo xtask lr-align generate | measure | validate`, the synthetic targets, and the round manifests.
- Per round, under the evidence directory: a page per setting with its response curves, map, residual and contact sheet (Luxforge, Luxforge calibrated, Lightroom), and a summary ranked by residual.
- The calibrated mapping rows and their fixtures, for preset import and the catalog import.
- Per setting: its response change, frozen with its study; and, where the threshold is met, the dimension a behaviour change would target, for the owner's review.

## Verification and acceptance

1. **Rig.** A round's variants and manifest are deterministic; every variant's XMP round-trips through Luxforge's own preset reader to the manifest's settings; the rig writes only inside the round folder.
2. **Measurement.** The measures are checked on synthetic cases with known answers (a known gain recovers its gain; a known blur its radius).
3. **Fit.** Each map is monotone, within the Luxforge range or reported as short, and frozen with an exact fixture.
4. **Importers.** A calibrated row converts exactly as its fixture says; an uncalibrated row is unchanged; the report says which applied.
5. **Validation.** The calibrated import's median difference against Lightroom's rendering on the owner's edited photographs is reported beside the uncalibrated one. No equivalence is claimed beyond those figures.
6. **Changed controls** each meet their own study's acceptance as above.

## Cost

The rig and level A are mostly tooling and one or two Lightroom rounds: the owner's part is an import and an export per round, perhaps an hour including the export, and a further round to confirm each batch of B changes. Each B change is smaller than a module's numerics study, since the algorithm stays; each C change is the size of a new module's study. A B change leaves a module's processing cost unchanged; a C change answers the performance-rules checklist with its own change.

## Decided

The owner, on 2026-10-06: Lightroom is not a rendering target, but its controls are the result of years of research and people are used to them. Slider values matter: someone used to +10 Clarity or −10 Vignette expects the same result. So Luxforge's sliders follow Lightroom's responses (level B) for every supported setting; where a dimension still differs widely, a targeted algorithm change (level C) is considered. The rest were decided the same day, each as recommended.

| # | Question | Decided | Not chosen |
| --- | --- | --- | --- |
| A1 | Which levels | B for every supported setting, C targeted | A only; A and B only; Lightroom behaviour throughout |
| A2 | Order | By editing area: Basic tone and colour, then Presence, the mixer and the vignette, then Detail, then masks and geometry, then white balance on RAW | By how often the owner uses each setting; biggest gap first |
| A3 | How Lightroom is driven | Generated XMP and one manual import and export per round by the owner, all tooling in Rust | A Lightroom Classic Lua plugin (a second toolchain) |
| A4 | Sources | Rendered outputs only; no Adobe code, SDK source or binary read, decompiled or used; no Adobe data shipped | Reading Adobe's SDK source; decompiling Adobe's binaries |
| A5 | Base rendering | Measured and subtracted; the Standard look and the RAW looks decision unchanged | An optional look fitted to Adobe Color |
| A6 | Existing photographs under a B or C change | Refused by the raised payload format marker, as every format change is; a new catalog | Re-rendering old values at the new scale; a one-off conversion |
| A7 | Real-edit validation | On the owner's catalog, locally, figures only, with consent asked each run | Standing consent; synthetic and corpus images only |
| A8 | When a setting needs C | After its B change, a median ΔE2000 above 2 at Lightroom values of ±25 and beyond, or a difference in kind; the change targets the dimension that differs | Owner review of every setting without a threshold; a threshold of 1 |

## References

- [Presets](presets.md#mapping), [Lightroom import](lightroom-import.md)
- [Basic tone](basic-tone.md), [Basic colour](basic-colour.md), [Tone curve](tone-curve.md), [Presence, colour mixer and vignette](presence-mixer-vignette.md), [Detail](detail.md), [Masking](masking.md), [Lens and perspective](lens-and-perspective.md), [RAW looks](raw-looks.md), [GPU-first](gpu-first.md), [sample corpus](sample-corpus.md)
- Research: [slider audit](../research/lightroom/slider-parity.md), [tone and colour tools](../research/lightroom/tone-and-color-tools.md), [preset formats](../research/lightroom/presets.md)
