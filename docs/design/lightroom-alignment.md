# Lightroom alignment

Status: decided and planned (2026-10-06) ([task plan](../../tasks/lightroom/lightroom-alignment.json)). Built: the rig's generator (`cargo xtask lr-align generate`) and synthetic targets ([running a round](#running-a-round)), the measurement (`cargo xtask lr-align measure`, [measuring a round](#measuring-a-round)), the calibrated mapping rows for preset import ([presets](presets.md#mapping)), now all removed as their sliders were realigned, and the fold of measured responses into the sliders (`cargo xtask lr-align fold`). Measured through the CPU reference renderer on Linux, with no GPU output: [round 1](#round-1)'s synthetic targets and [the corpus photographs and round 2's pairs](#round-1-photographs-and-round-2). [Basic is realigned](#basic-realigned) (TASK-006): its tone and colour sliders follow Lightroom's responses (level B), its Exposure, Temperature and Tint act on a measured scene scale (level C, [Basic on the scene scale](basic-scene.md)), and Basic's format marker refuses earlier edits; [Presence, the mixer and the vignette are realigned](#presence-the-mixer-and-the-vignette-realigned) (TASK-007, level B), with their format markers raised and the calibrated preset rows removed. GPU qualification of both on the M4 remains open; the owner reviewed the sheets and the [confirming round](#round-3-the-confirming-round) is measured. The Tone curve has no slider to fold. `lr-align validate` is not built, and TASK-008 onward await authorization. The owner decided every question on 2026-10-06 ([decided](#decided)): Luxforge's sliders follow Lightroom Classic's responses (level B) for every supported setting, with targeted algorithm changes (level C) where a dimension still differs widely.

It stands alone. [Preset import](presets.md#import) and the [Lightroom import](lightroom-import.md) use its results to convert values, and every Luxforge user gains from controls that behave the way photographers coming from Lightroom expect, but neither importer waits on it.

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

Lightroom cannot be run by an agent or in CI; it needs the owner's installation and licence. The rig keeps the owner's part to one import and one export per round, and everything else in Rust, so the project keeps one toolchain. `lr-align generate` and `lr-align measure` exist; `lr-align validate` is a planned deliverable and does not exist yet:

1. **Generate.** `cargo xtask lr-align generate --round N --output NEW [--corpus CACHE] [--manifest RAW_MANIFEST]` writes a round folder: for each test image and each variant (one setting at one value, or a pair), a copy of the image named for its variant, with the variant's settings as an XMP sidecar beside a RAW copy or embedded in a JPEG copy; and a manifest of every variant's settings. Originals are never touched; only the round's own copies carry XMP.
2. **Lightroom (owner).** Import the round's `lightroom` folder into a scratch Lightroom catalog, select all, export as 16-bit TIFF, uncompressed, sRGB, full size, no output sharpening, Copyright Only metadata, each file keeping its variant's name. The round folder's `INSTRUCTIONS.txt` gives every dialog setting, which the owner saves once as an export preset; the rig writes no export preset file, whose format is not published.
3. **Luxforge.** The same variants render headlessly through the whole-frame reference renderer, the oracle [GPU-first](gpu-first.md) holds GPU outputs to, with a study-only 16-bit terminal conversion to the same sRGB encoding: `Render::study_frame16` is the reference's own evaluation with only its terminal boundary widened to 16 bits, so the last colour run, or the last spatial frame when no colour follows it, writes 16-bit codes where `Render::frame` writes bytes, and RAW headroom above 1 takes the top code. Every earlier boundary keeps its width: the source JPEG's own 8 bits and, in the byte domain, a colour run a resample (crop, straighten, perspective) reads are 8-bit quantizations the study keeps and reports. The reference is deterministic and its frozen fixtures keep fitted maps reproducible; the GPU's picture is qualified separately against it. This study output is not a production export kind and the editor never calls it; a new export kind declares its tolerance with that feature.
4. **Measure and fit.** `cargo xtask lr-align measure --round N (--folder ROUND | --exports DIR) --output NEW` reads both, measures, fits and writes the round's figures, tables and contact sheets under the evidence directory ([measuring a round](#measuring-a-round)). Rendering orchestration, TIFF decoding and sheets belong in `xtask`; the TIFF reader is xtask's own (`xtask/src/tiff.rs`), never an editor dependency. Pure measures and fits belong in `luxforge-reference` (`lr_align`), which never depends on `luxforge-core` and cannot run its renderer.

A Lightroom Classic plugin (Lua, through its SDK) could apply settings and export without the import step; it was not chosen, because it would add a second toolchain ([A3](#decided)).

## Test images

- **Synthetic targets, JPEG**, where both editors treat the file as an already-rendered sRGB picture and Lightroom applies no base profile to it, so the measured change is the setting alone: a 256-step grey wedge; an Oklab lightness × hue × chroma grid of flat patches; a step-edge and zone-plate chart for the spatial settings; a flat field for the vignette; geometric grids for crop, straighten and perspective. Patches are large enough to average away JPEG quantization.
- **Real RAW photographs** from the CC0 [sample corpus](sample-corpus.md), chosen across cameras, dynamic range and subject (skies, skin, foliage, night), and a colour-checker frame shot by the owner on the Z6 and X100VI, for RAW white balance, Exposure, Highlights against real sensor headroom, Dehaze on haze, and Detail on real noise.
- **The owner's own edited photographs** for validation only ([below](#validation-on-real-edits)), never committed.

## Running a round

`cargo xtask lr-align generate --round 1 --output NEW` writes round 1 on the synthetic targets alone; `--corpus CACHE` adds the corpus selection from the [corpus tool's](sample-corpus.md) local cache, and `--manifest RAW_MANIFEST` adds the owner's supplied files from a RAW manifest. Round 2 is `--round 2`, and round 3, the confirming round, `--round 3`. `--families ID,...` writes only those families, every variant of each with its subjects' neutral variants, for a session that adds or re-exports part of a round; it is measured as its own round folder. The output must not exist and its parent must; the command writes only inside it, reads RAW originals and checks their SHA-256 before and after, and removes the folder if it fails. The same build, round and inputs write the same bytes.

The round folder holds:

- `targets/`: the synthetic JPEG targets the round uses, without XMP, which Luxforge's side renders from.
- `lightroom/<subject>/`: one copy per variant, named `<subject>__<family>__<value>` (`wedge__exposure__+1`), with the XMP embedded in a JPEG copy or in a sidecar beside a RAW copy. This folder alone is imported into Lightroom; exports go to `exports/` as `<variant>.tif`.
- `manifest.json`: each target with its layout (patch positions and codes, edge panels, dot centres) and pixel hash, each photograph with its hash, each family with the Luxforge control its setting is compared with, each variant with the settings it writes, what Luxforge's preset reader maps from it and what the reader reports unsupported, and what the round leaves out.
- `INSTRUCTIONS.txt`: the owner's catalog, import, check, export and notes steps, with the round's counts and export size.

Every variant writes the whole baseline: every setting the rounds measure at its neutral value (sharpening and colour noise reduction 0, lens profile off, As Shot and Adobe Color on a RAW photograph), with its own settings over it, so a catalog's own defaults cannot change what a variant means. Every variant's XMP is read back through Luxforge's own preset reader before it is written: nothing refused but a RAW white-balance range shortfall, every written setting reported with its written text, each value transfer on its control at the same number, and only settings the importer does not map (Detail, crop, perspective, lens profile, gradient corrections) reported unsupported. A gradient correction's geometry, which the reader reports as a summary, is read back by the rig's own parse.

Round 1 has 706 variants on the targets and 49 on each RAW photograph (about 7 GB of Lightroom exports without photographs; about 41 GB more with the five corpus photographs). Round 2 has 292 and 21. Round 3 has 553 and 29: every realigned slider alone (Basic's ten, Clarity, Dehaze, the 24 mixer sliders, the vignette's Amount and Midpoint) at values between round 1's, on the subjects round 1 measured it on and under round 1's family ids, so `measure --round 3` judges sliders against points no response was fitted to. Recorded choices, delegated to the implementer:

| Choice | Recorded |
| --- | --- |
| Targets | A 256-step grey wedge (48-pixel patches, boustrophedon); an Oklab grid of lightness 0.3 to 0.9 × chroma 0.03 to 0.14 × 24 hues, a colour outside sRGB kept at its hue and lightness with the largest chroma inside; five greys on dark, mid and light surrounds, for whether a setting is local; a 2400 × 1600 spatial chart (zone plate to Nyquist, step edges at high and low contrast, a 5° slanted edge, thin lines, seeded luminance and colour noise of σ 6 codes) and the same chart at 6000 × 4000, for whether a spatial setting's reach follows the picture's size; a 1500 × 1000 flat mid grey; a 1500 × 1000 dot grid with an orientation marker. Untagged sRGB, JPEG quality 100 without chroma subsampling; each target's pixels frozen by SHA-256 in the tests |
| Values | Signed sliders at ±10, ±25, ±50, ±75, ±100; Exposure at ±0.5, ±1, ±2, ±3, ±5; settings whose default is 50 at 0 to 100 around it; Detail qualifiers at Amount 75, Luminance 50 or Colour 50; vignette Midpoint, Roundness and Feather at Amount −50; photographs and pairs at ±50 and ±100 (Exposure ±1, ±2). Round 3 at ±5, ±15, ±35, ±60, ±90 (Exposure ±0.25, ±0.75, ±1.5, ±2.5, ±4), Midpoint 5 to 95 between round 1's, photographs at ±35 and ±75 (Exposure ±0.5, ±1.5), Contrast on photographs too |
| Process | `ProcessVersion` 11.0 (Process Version 5), the string the published research verifies; the owner checks it in the Calibration panel. Round 1 confirmed it: every variant read Version 5, while a target imported with no settings read Version 6 and its export recorded `ProcessVersion` 15.4 |
| Masks | Linear and radial gradients with local Exposure −1 stop on the flat field, written in the gradient-correction form Lightroom has read since Process 2012, the radial with `Flipped` true for an inside effect. `LocalExposure2012` is written as stops over four: round 1's written −1 applied about −5.2 stops where the effect is full, as Lightroom's global Exposure −4 does on the same grey. `measure` reads the direction and the stops applied from each export. Brush and range masks wait for a later round, until a Lightroom-written sidecar of each confirms its encoding |
| Geometry | Straighten at ±1, ±2, ±5, ±10°, each crop the largest centred 3:2 rectangle that stays inside the frame turned by the angle, written as Lightroom reads it: `(CropLeft, CropTop)` and `(CropRight, CropBottom)` are the turned rectangle's opposite corners in the unturned frame's normalized coordinates, which round 1's exports showed and its output sizes confirm to a pixel; four plain crops; Perspective Vertical and Horizontal swept; lens profile on with distortion only, on photographs |
| RAW white balance | Custom 2500 to 10000 K at tint 0, and tints −50 to +50 at 5500 K. The importer's current conversion refuses Lightroom tints of ±50 at 5500 K as beyond Luxforge's ±100; those variants are kept and the manifest records the refusal as a range shortfall |
| Photographs | Corpus selection by camera and sensor diversity, the owner's bodies first: Nikon Z 6 (`pixls-3582`), Fujifilm X100VI (`pixls-7301`), Sony A7 IV (`pixls-6932`), Canon EOS R6 (`pixls-4662`), OM System OM-1 (`pixls-5283`). Their subjects were not reviewed. A DNG is left out and listed: Lightroom reads a DNG's settings from inside the file, and the rig writes sidecars only |
| Colour grading | Not in these rounds: the delivered `grade-align` round measures it |

## Measuring a round

`cargo xtask lr-align measure --round 1 --folder ROUND --output NEW` measures a round folder `generate` wrote, against the owner's exports in its `exports/` (or `--exports DIR`, whose parent is then the round); `--families ID,...` narrows it. It refuses a round whose manifest or targets are not this build's, an existing output, and an output inside the round folder, which it only reads. Each target's export with no XMP, named for the bare target, is Lightroom's control: its base rendering of an untouched JPEG, compared with its neutral variant and the pristine target. Every export's own XMP is read for the process version, the creator and each setting Lightroom recorded, and a setting recorded with another value than the one written is reported. Families on RAW photographs are reported unmeasured when the round has none.

Under the evidence directory it writes `pages/<family>.md` per setting (the response curve, the map as 21 knots, the residual per value, the kind-specific tables and a contact sheet of Luxforge at the identity, Luxforge through the map and Lightroom), `svg/` and `sheets/`, `summary.md`, the settings ranked by residual with the level C candidates, the masks' directions, the exports' metadata and the base rendering, and `lr-align.json`, every figure, with the Basic responses the build applies and each colour family's distance table for [`fold`](#basic-realigned). Its measures and fits are each checked on synthetic cases with known answers in `luxforge-reference`'s `lr_align` study, and its whole path on a round of Luxforge's own renders, which must fit as the identity, in xtask's `lr_align` tests.

Recorded choices, delegated to the implementer:

| Choice | Recorded |
| --- | --- |
| Samples | Patch interiors on the wedge, the patch grid and the surround target; square cells on the spatial charts (4 px at 2400 × 1600, 10 px at 6000 × 4000), on the surround for Presence and Detail (8 px) and on the flat field (10 px); dot centroids on the grid |
| Responses | Lightroom's change from its neutral variant carried onto Luxforge's neutral rendering in Oklab; two responses compared by CIEDE2000 between the colours they reach; a residual is the median over the samples Lightroom's response moves by at least 0.5 |
| Fit | Luxforge's control rendered on a grid across its range (step 5 on patches, 10 on cells, 25 at 6000 × 4000; 0.25 EV, 0.1 px radius, 10 sharpening), each Lightroom value searched on the grid and refined between neighbours through interpolated responses, then pooled into a monotone map in whichever direction leaves the smaller residual, through the neutral anchor only when the family holds nothing else away from neutral. A held setting (Sharpening's Amount under Radius, the vignette's Amount under Midpoint) is converted through its own family's map, measured earlier in the round |
| Threshold | A8's ±25 read, for a range other than −100..100, as an eighth of the Lightroom range from the default (1.25 EV, 18.75 of Sharpening's 0..150) |
| Differences in kind | A response depending on the surround (Basic tone: locality more than 1 CIEDE2000 above Luxforge's); a halo on the hard edge outside 0.67 to 1.5 times Luxforge's, where both are measurable within the panel; Vibrance's protection of skin hues differing by more than 0.35; the flat field's half point more than 0.1 in ρ from Luxforge's; and a map no measurement can determine (every point's span covering 90% of Luxforge's range) while Lightroom's response varies by at least 1 CIEDE2000 |
| Geometry | Straighten by the grid's rotation in degrees, perspective by its keystone in percent, crop by dot positions at the box Lightroom recorded, each in its own units and not ranked with the colour residuals |
| Photographs | Each photograph imported into a scratch catalog in the evidence directory from the round's own copy beside its neutral variant, its SHA-256 checked before and after, and developed by the editor service (As Shot, a custom white balance through `set-raw`, or the detected lens profile); a family's settings render over that development's RAW layer and the Look the import gave it, placed before the Look as the editor places them. Cells laid out in normalized coordinates (240 across), so the two editors' frames match by picture area whatever their own default crops; coarser grids (step 25, 20 on 0..100, 0.5 EV); the measures are each editor's band energy at three scales over its own neutral and a tone locality, the spread of `ΔL*` among cells of one lightness |
| RAW white balance | Two sweeps on Luxforge's own scale: Kelvin (2000..12000 in 500s) at Lightroom's tint 0, and tint (−100..100 in 20s) at Lightroom's 5500 K, each holding the other at what the importer converts Lightroom's 5500 K and tint 0 to; every value its own development. No calibrated row: the RAW white-balance round (TASK-010) calibrates the conversion |
| Lens profile | Luxforge's detected profile, distortion only, against Lightroom's, a conversion at the identity per photograph; a photograph with no eligible profile is reported so |
| Pairs (round 2) | Each pair rendered at the identity and through both settings' calibrated maps (preset import's), a value past a map's shortfall at the end of Luxforge's range, marked; judged by the same carried CIEDE2000 as a single setting |
| Masks and the Tone curve | No value to fit: each is a conversion at the identity, judged by its residual; a mask is also rendered at the stops its export shows, so its falloff is compared apart from its strength |

## Round 1

Measured on 2026-10-13 from the owner's Lightroom Classic 15.6 (Camera Raw 18.7) exports of the 706 synthetic-target variants, through the CPU reference renderer on Linux; no GPU output was measured, and the 11 photograph families had no photographs. The figures below are the A8 basis for the level C candidates; every other figure, page, map and contact sheet is in the `lr-align measure` evidence, not in the repository. Residuals are the worst median CIEDE2000 at Lightroom values a threshold distance or more from the default, after the fitted map, with the identity conversion in brackets.

| Area | Setting: residual (identity) | Level C candidate |
| --- | --- | --- |
| Basic | Temperature 19.3 (19.3), Tint 18.3 (18.3), Whites 10.3 (10.3), Vibrance 4.6 (4.7), Exposure 4.2 (6.6), Contrast 3.5 (3.5), Highlights 3.0 (3.1), Blacks 2.6 (4.7); Shadows 1.9 (6.1), Saturation 1.8 (1.8) | All but Shadows and Saturation. Highlights also differs in kind: Lightroom's response depends on the surround (locality 4.6 against Luxforge's 0) |
| Tone curve | 3.8 at the identity | Yes: Lightroom's curve acts per channel, Luxforge's on luminance |
| Presence | Texture 4.9, at 24 MP 6.4; Clarity 3.4, at 24 MP 3.5; Dehaze 2.3 (4.1), at 24 MP 2.1 | All. Texture differs in kind: Luxforge's edge-preserving band hardly moves the chart's edges and zone plate, so its map is undetermined |
| Colour mixer | 22 of 24 above 2; worst Blue hue 8.8, Blue luminance 5.6, Purple hue 5.7, Blue saturation 5.5, Orange hue 5.4; Magenta hue 1.9 and Red hue 2.3 (6.2) the closest | The 22 |
| Vignette | Feather 17.1, Midpoint 12.4, Roundness 6.8 (8.1), Amount 2.9 (14.4) | All four, and in kind: Lightroom's falloff reaches half its corner effect about 0.23 further out in normalized radius |
| Detail | Sharpen Detail 2.8 (undetermined), Radius 1.8, Masking 1.1, Amount 1.0 (1.5); noise reduction and its Detail 0.3 to 0.7 | Sharpen Detail only, in kind: no Luxforge Detail value can be told from another against Lightroom's response |
| Masks | Linear 6.3, radial 10.5 with the exposure matched (24.3 at the identity) | Both: the falloff differs once the strength is matched |
| Geometry | Straighten 0.03° (Lightroom's angle runs counter to Luxforge's); plain crops 0.003 px; Perspective Vertical 10% and Horizontal 20% keystone, Lightroom's ±50 near Luxforge's ∓100 and short beyond | Not ranked against the colour threshold |

Also found:

- **Masks.** The radial gradient with `Flipped` true takes the effect inside the ellipse and the linear gradient takes it at its full point, both as written, Lightroom recording each as a `MaskGroupBasedCorrections` mask of the written geometry. The written `LocalExposure2012` −1 applied about −5 stops where the effect is full, so the written local value is not read as stops.
- **Straighten.** Lightroom reads a crop's four values as the turned rectangle's opposite corners, so round 1's boxes, written as axis-aligned squares, were different rectangles for each sign of the angle: Lightroom shrank every negative angle's to stay inside the frame and kept the positive ones, of other aspects. The rotation the rig fits is unaffected; the generator now writes the corners.
- **Base rendering.** The bare-target exports match Lightroom's neutral variants and the pristine targets within 0.06 CIEDE2000 median: Lightroom renders an untouched sRGB JPEG as it is.
- **Metadata.** Every export embeds its full develop settings, though Copyright Only was asked.

## Round 1 photographs and round 2

Measured from the owner's Lightroom Classic exports of round 1's photograph families with the fixed masks and straighten (264 variants) and of round 2 (397), on the five corpus photographs (Nikon Z 6, Fujifilm X100VI, Sony A7 IV, Canon EOS R6, OM System OM-1) and the synthetic targets, through the CPU reference renderer on Linux; no `NOTES.txt` was written with either export. Every photograph's export was exactly Luxforge's developed size, the X100VI in portrait. The first measurement rendered Luxforge's side of each photograph without the Look its import gives it, so its photograph figures compared an unrendered development with Lightroom's rendering (9 to 19 CIEDE2000 apart at neutral); they are superseded. With the Look kept, the two editors' neutral renderings differ by 1.7 to 4.1 median.

On the realigned build (2026-10-15), at the identity: Exposure on the photographs is within 1.0 to 2.7 and fits as the identity once it is a plain gain on RAW ([Basic on the scene scale](basic-scene.md)). Lightroom's Highlights and Shadows answer the photographs and the charts differently: on the charts Lightroom's Highlights is stronger at its high end than Luxforge's, on the photographs about a third as strong (Lightroom's +100 fits Luxforge's +31), as a control that adapts to the picture would, which Lightroom's Highlights also shows on the surround target (locality 4.6). No single global curve meets both; an adaptive Highlights and Shadows is a level C candidate. Round 2's pairs of Contrast with Blacks or Whites and of Dehaze with Contrast reach medians of 7.6 to 8.8 on the photographs, where Luxforge's tone controls act before the Look and Lightroom's within its rendering.

## Basic realigned

Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation apply strengths through frozen responses ([`response.json`](../../crates/luxforge-core/src/modules/basic/response.json)): per field, knots `[value, strength]` at the Lightroom values the rounds measured, read by linear interpolation, strictly increasing through `[0, 0]`. Each unit's algorithm and stage are unchanged ([Basic tone](basic-tone.md), [Basic colour](basic-colour.md)) and receive the strength, so their GPU programs are unchanged. Exposure, Temperature and Tint differed in kind: Lightroom's Exposure rolls highlights off rather than clipping them and its white balance keeps white white, so both became one per-channel shift on a scene scale measured from Lightroom's exports, with no response ([Basic on the scene scale](basic-scene.md)). Basic's payload format is 3: a layer of an earlier format holds values of an earlier meaning and is refused as incompatible, naming its format. Auto tone keeps its ruled fields' rules and bounds in strengths and writes the slider values that apply them ([Auto tone](auto-tone.md)); the neutral picker solves on the scene scale; preset import has no calibrated Basic rows ([A11](#decided)), so a Lightroom Basic value transfers as the same number. A mask's Basic adjustment uses the same responses; the masks' own alignment is TASK-009.

`cargo xtask lr-align fold --measured DIR [--measured DIR ...] --output NEW` makes the table from `measure` outputs, which record the responses they measured and, per colour family, each Lightroom value's distance on Luxforge's grid refined four times. It writes `response.json`, `fold.json` and `fold.md` and reads nothing else. Recorded choices, delegated to the implementer:

| Choice | Recorded |
| --- | --- |
| Joint fit | A field's measured families, the targets and (Exposure, Highlights, Shadows, Whites, Blacks) the photographs, fitted together: each Lightroom value matched at the Luxforge value with the least mean distance over the families holding it, each family weighted equally, then made monotone as a level A map is |
| Knots | At the measured values and neutral. A point whose span is wide continues the slope of the point inside it, so a chart that saturates (the wedge at Exposure +5) leaves the slider moving rather than taking the span's first value |
| Slope floor | Every step at least a fifth of its Lightroom step on the measured build's scale, so no stretch of slider is dead |
| Extension | Measured on a study build whose responses stretched each short range (Contrast and Highlights ±150, Whites ±200); the fold composes its map with the measured build's responses |
| Owner's review | Whites above +50 lowered to +75 at 85 and +100 at 100, which the charts' fit put brighter; Blacks, Shadows, Contrast, Highlights, Vibrance and Saturation keep the joint fit, which the owner preferred to a refit from the charts alone (2026-10-15) |
| Stability (A10) | Highlights and Shadows reach at most ±150, where their stage keeps a slope of 0.1 (past about ±170 the lifting side stops being monotone); Saturation stops at −100, greyscale. Contrast is monotone at any strength, Whites and Blacks are a linear remap held apart by their gap clamp. `every_extended_strength_keeps_its_unit_stable` holds the tone curve nondecreasing at every corner of the end strengths |

Folded from round 1's targets and the photographs (2026-10-13), Exposure, Temperature and Tint refitted on the scene scale (2026-10-15), and checked by measuring rounds 1 and 2 again on the realigned build, through the CPU reference renderer on Linux; no GPU output was measured. Residuals are the worst median CIEDE2000 on the round 1 charts at a threshold distance or more from the default, at the identity, before and after; the figures, pages and sheets are in the `lr-align measure` and `fold` evidence.

| Setting | Strength at the slider's ends | Charts | Level C candidate |
| --- | --- | --- | --- |
| Exposure | The scene scale | 6.6 → 1.0 | No |
| Contrast | −136.9, +99.4 | 3.5 → 3.3 | Yes |
| Highlights | −112.5, +93.8 | 3.1 → 3.0 | Yes; local on the surround |
| Shadows | −40, +36.9 | 6.1 → 2.7 | Yes; local on photographs |
| Whites | −45, +100 | 10.3 → 10.3 | Yes, in kind: Lightroom's Whites re-stretches the top of a darkened picture; the owner's review keeps +100 at its former strength |
| Blacks | −71.3, +57.5 | 4.7 → 3.6 | Yes |
| Vibrance | −98.8, +88.8 | 4.7 → 4.7 | Yes |
| Saturation | −100, +87.5 | 1.8 → 1.8 | No |
| Temperature | The scene scale | 19.3 → 4.7 | Yes, at the extremes |
| Tint | The scene scale | 18.3 → 2.5 | No |

Round 2's pairs at the identity on the realigned build have median residuals of 1.9 to 3.7 on the targets; Whites +100 after Exposure −2 still reaches 30. The Tone curve has no slider value to fold: its conversion at the identity stays 3.8, a level C candidate (Lightroom's curve acts per channel). The owner's review of the sheets and a confirming Lightroom round close the batch.

## Presence, the mixer and the vignette realigned

Clarity, Dehaze, the colour mixer's 24 HSL sliders and the vignette's Amount and Midpoint apply strengths through frozen responses in each module's own `response.json` ([Presence](../../crates/luxforge-core/src/modules/presence/response.json), [mixer](../../crates/luxforge-core/src/modules/mixer/response.json), [vignette](../../crates/luxforge-core/src/modules/vignette/response.json)), read as Basic's are by the shared [`response.rs`](../../crates/luxforge-core/src/modules/response.rs), and `slider_response` names every realigned field. The units, stages, halos and GPU programs are unchanged and receive the strength, so a spatial unit's kernel, window and scratch charge are those of the strength it is given; the extended strengths stay within each unit's declared bounds. Presence's payload format is 2, the vignette's 2 and the mixer's 3. Preset import's calibrated rows are all removed ([A11](#decided)). Texture's map is undetermined (its band hardly moves the charts), and the vignette's Roundness and Feather were measured with Amount held, so the three keep applying their value as their strength. Recorded choices, delegated to the implementer, beyond Basic's:

| Choice | Recorded |
| --- | --- |
| Fit | `lr-align fold` over a study build whose responses stretched the short ranges (Clarity's negative side, Dehaze, the mixer's hues), the targets' families jointly as Basic's; Dehaze from the corpus photographs alone, as the charts hold no haze for its estimate to find |
| Stability (A10) | Clarity reaches −130, where its negative gain stays above −1; Dehaze's negative side keeps its added veil below 1; a mixer hue reaches ±115, where the farthest a range's colour travels stays short of its neighbour's centre. Each is held by a test over the module's end strengths |
| Mixer hue (owner's review, 2026-10-16) | The owner found Lightroom's hues more aggressive than the joint fit's. Each hue slider's strength matches the size of Lightroom's response (the median CIEDE2000 from neutral over the patches it moves), measured on a study build with every hue at up to ±115; where Luxforge's hue travel cannot reach Lightroom's size, the strengths follow Lightroom's own size curve in proportion, up to 115. Never weaker than the joint fit, every step at least 0.2 of its Lightroom step |
| Vignette (owner's review, 2026-10-16) | The owner found Lightroom's vignette much stronger at both ends: the joint fit traded the corners for the frame's body. Amount now matches Lightroom's corner change in L* (where the mask is 1), and Midpoint the radius at which the falloff reaches half its corner change, with Amount −50 held, so the default Midpoint of 50 applies 73.6 and the falloff begins further out, as Lightroom's does. Roundness and Feather keep their value as their strength, which the owner found closer than their fitted maps |
| Mixer saturation at −100 | Every saturation slider's −100 is pinned to strength −100, so all eight at −100 still make every pixel grey, as Lightroom's do; the charts' joint fit put Red, Orange, Yellow, Green, Aqua and Purple at −50 to −88, and the pin costs their −100 point up to 3 ΔE2000 on the patches |

Folded from round 1's targets and the corpus photographs, the hues and the vignette refitted after the owner's review (2026-10-16), and checked by measuring round 1, its photograph session and round 2's Clarity and Dehaze pairs again on the realigned build, through the CPU reference renderer on Linux; no GPU output was measured. Residuals are the worst median CIEDE2000 at a threshold distance or more from the default, at the identity, before and after; the figures, pages and sheets are in the `lr-align measure` and `fold` evidence.

| Setting | Strength at the slider's ends | Charts | Level C candidate |
| --- | --- | --- | --- |
| Clarity | −130, +65 | 3.4 → 3.1, at 24 MP 3.5 → 3.3 | Yes, at −100: Lightroom's negative Clarity goes further than the unit's stable gain |
| Dehaze | −84.4, +100 | 4.1 → 4.1, at 24 MP 2.1 → 3.3; photographs 5.6 → 5.0 | Yes: the photographs and the charts disagree, and the photographs still differ at both ends |
| Mixer hue | Red −30, +115; Orange, Green, Blue and Purple ±115; Yellow −77.4, +115; Aqua −74.8, +76.6; Magenta −68.8, +33.8 | Red 6.2 → 3.1, Magenta 7.4 → 2.0, Yellow 4.4 → 4.0, Aqua 3.7 → 3.6; Blue 8.8, Purple 5.7, Green 5.6, Orange 5.4 unchanged. Response sizes now match Lightroom's for Red, Yellow, Green and Aqua and Orange's positive side; Blue, Purple and Orange's negative side move a third to half as far at the most Luxforge's hue travel allows, and Magenta's Lightroom response is the smaller | Yes, in kind: Lightroom's hue travels further (Blue −100 turns blues 45 to 69° against 26 to 50°) and carries lightness and chroma with it (up to +0.05 Oklab L and 1.37 times the chroma), where Luxforge's moves hue alone and stops short of the neighbouring range; whether to follow it is open (owner, 2026-10-16) |
| Mixer saturation | −100 (pinned), +28.8 to +78.8 | Aqua 5.0, Purple 6.5, Green 4.6, Orange 4.1, Yellow 4.0 at −100 (the pin), Red 3.7 → 2.3, Magenta 3.5 → 2.4; Blue 5.5 unchanged | Yes |
| Mixer luminance | about ±25 to ±47.5 | 5.7 to 10.1 → 3.0 to 5.8 | Yes, in kind: Lightroom's luminance moves a colour further at the same chroma |
| Vignette Amount | −100, +100, stronger between | 14.4 → 8.1; the corner change matches Lightroom's within 0.6 L* at every value but −75 (5 short, where the slope floor keeps the slider moving), the half point within 0.08 in normalized radius on the dark side | Yes, in kind: Lightroom's light side has a narrower ramp further out (half point 0.97 to 1.11 against 0.87) |
| Vignette Midpoint | 30.6 to 100, 73.6 at the default 50 | 12.4 → 9.4; the half point within 0.01 up to 75, short of Lightroom's beyond, where Luxforge's falloff cannot begin further out | Yes, at 90 and above |
| Vignette Roundness, Feather | Their value | Roundness 8.2 → 21.9 (its positive side 1.3 → 0.6), Feather 17.1 → 9.2, with the stronger Amount and the new Midpoint | Roundness, in kind: Lightroom's negative Roundness moves the falloff out towards the frame's edges (half point 1.35 at −100), Luxforge's rounded rectangle moves it in (0.81) |

Round 2's pairs at the identity on the realigned build: Clarity with Texture 5.9 → 5.7 on the targets, Dehaze with Contrast 4.1 → 5.7 on the targets and 9.9 → 10.1 on the photographs. The owner reviewed the sheets (2026-10-16), and the [confirming round](#round-3-the-confirming-round) is measured; GPU qualification on the M4 closes the batch.

## Round 3, the confirming round

Measured from the owner's Lightroom Classic exports of round 3 (698 variants, 2026-10-16) on the realigned build, through the CPU reference renderer on Linux; no GPU output was measured. Every value lies between round 1's, so no response was fitted to it. Residuals are the worst median CIEDE2000 at the identity at ±35 and beyond; the pages and sheets are in the `lr-align measure` evidence.

| Area | Residual at the identity | What it shows |
| --- | --- | --- |
| Basic on the charts | Exposure 1.0, Saturation 1.6, Tint 2.0, Shadows 2.5, Highlights 2.9, Contrast 2.9, Blacks 3.3, Vibrance 4.2, Temperature 4.7, Whites 5.8 | Each as at round 1's values or better, so the responses hold between their knots; Whites above +60 and Blacks at −90 are where a different value would still help |
| Basic on the photographs | Exposure 2.1, Highlights 2.4, Shadows 3.6, Whites 5.0, Blacks 5.0, Contrast 7.6 | Lightroom's Contrast, Highlights and Shadows on a RAW photograph act about a third as strongly as on the charts, and Whites and Blacks about half to two thirds: the level C candidates of [Basic realigned](#basic-realigned) |
| Presence | Clarity 2.5, Dehaze 3.1, Dehaze on the photographs 4.2 | Dehaze on the photographs is at its best value and differs in kind |
| Mixer | Hue: Magenta 1.5 to Blue 7.8; saturation 2.0 to 4.2; luminance 2.7 to 5.4 | Green's and Red's positive hues, raised to match Lightroom's response size, have the least residual at about half their strength; the saturation sliders' −90 is too strong where −100 is pinned to grey |
| Vignette | Amount 8.1, Midpoint 7.6 | Amount's light side has the least residual at about a third of its strength, though its corners match Lightroom's (the owner's choice); Midpoint holds to 67 and falls short at 80 and above |

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

The RAW white-balance round includes the calibrated Kelvin/tint conversion for preset import and catalog import (owner, 2026-10-07). The existing preset conversion matches white chromaticity, not a measured rendered response; missing pairs and range shortfalls remain reported.

The maps become rows of the preset mapping with a calibrated rule ("value converted through the alignment calibration"), so preset import, the catalog import and `lightroom.remap` share them. Each map is a frozen fixture with an exact test; a new round that changes a map is an ordinary reviewed change.

## Changing a control (levels B and C)

**B, a response change**, refits the curve from a slider's value to the strength the module's algorithm applies, so that the level A map for that setting becomes the identity within the study's tolerance. The fitted map is folded into the control: what was Luxforge's value `g_s(v)` becomes the value `v`. The algorithm and its stage stay unchanged. Where Lightroom's extreme does more than Luxforge's whole range, the control's internal strength extends past it where the algorithm stays stable, checked per setting ([A10](#decided)); a shortfall that cannot be extended is reported. The maximum halo stays bounded. A fitted scale can change a value's kernel support, halo and processing cost, so those consequences are checked. Every supported setting gets one, in the [order](#decided) A2 sets.

### When a setting needs C

A setting goes to C when, after its B change, the response still differs in a way no value can remove. Decided ([A8](#decided)): on the round's targets at Lightroom values of ±25 and beyond, a median ΔE2000 above 2 against Lightroom's response, or a qualitative difference in kind found by the measures (a local Highlights where Luxforge's is global, a halo of a different radius, a Vibrance that does not protect skin). The study reports the dimension that differs, and the C change targets that dimension only: Clarity's halo radius, say, not Clarity as a whole.

Both kinds of change follow the path every delivered module's numerics followed: a study with figures and contact sheets, an independent `f64` reference frozen with it, the CPU unit exact against the reference, the module or mask kind's WGSL beside its CPU unit changed with it, warmed on the photo surface and tile worker and held within the declared output-kind tolerance by whole-corpus `tools/cargo-cached xtask gpu-qualification --manifest RAW_MANIFEST --output NEW_DIR`, the module's payload format marker raised so that recipes of the earlier meaning are refused explicitly rather than re-rendered silently (current shapes only), and the owner's review of the sheets before it is frozen. The level A maps for that setting are refitted afterwards. Qualification uses every corpus source and output kind: a narrowed selection or missing RAW manifest is incomplete, not a pass. `verify --tier full --manifest RAW_MANIFEST` runs this release gate. Pixel reads and export keep using the tile service, the desktop GPU tile worker and the reference fallback. A spatial change declares its halo, GPU window, incremental change propagation and staged-sweep splits against the 2 GiB preview and separate 2 GiB tile-worker budgets. Detail's Radius currently spans 0.5..3 px; its Gaussian support is `ceil(3σ)`, with sharpening halo at most 16 and denoise at most 32 exact or 46 sampled, so a refitted scale must account for support as well as strength.

RAW white balance changes source development: the CPU converts Kelvin and tint to camera-space gains and develops once per source and committed white balance. Its conversion and development are refrozen against their references. During a drag the GPU applies an approximate leading step over the previous development; release redevelops the source. The `raw-panel --source RAW_FILE` smoke scenario reports that drag against its release at Fit and 100%, failing only on the existing gross-error bound, while whole-corpus qualification gates the redeveloped picture at rest, counts, samples and export. This preserves the current distinction between at-rest correctness and motion reporting.

C candidates are ranked by residual times how often the setting is used. Likely ones, to be confirmed by measurement, not assumed: Highlights and Shadows (whether Lightroom's are local), Clarity's and Texture's scale and halo radius, Dehaze, Vibrance's protection of skin tones, the mixer's range widths and how far Hue moves a colour, and the vignette's midpoint curve.

**Outputs only** ([A4](#decided)). Only Lightroom's rendered outputs are measured. This alignment work introduces no Adobe code, SDK source, binary, profile, table or curve; every new frozen alignment reference is justified by measured outputs alone. The owner clarified on 2026-10-07 that this prohibition applies to new alignment work: the existing RAW importer and its white-coordinate table remain for separate review and are not evidence of rendering equivalence. Published format specifications (DNG, XMP) may be read. No legal review has been done and none is claimed.

## Validation on real edits

The translations are fitted on test images; they are judged on real photographs. Lightroom's own preview of a photograph is its rendering of that photograph's real settings, and the [Lightroom import](lightroom-import.md#lightrooms-previews) is planned to read it for the first grid tiles. With the owner's consent, `lr-align validate` reads the owner's catalog read-only, renders a sample of their edited photographs through the same whole-frame reference path as the rounds and the import mapping, with and without the calibration, and compares each to Lightroom's preview (downscaled to the preview's size, with base rendering differences measured on unedited photographs and reported separately). The figure that matters is whether calibration moves the median photograph closer, and by how much. Results are recorded as figures only; no photograph leaves the machine or is committed.

## Deliverables

- `cargo xtask lr-align generate | measure | fold | validate` (all but `validate` built), the synthetic targets, and the round manifests.
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

The rig and level A are mostly tooling and one or two Lightroom rounds: the owner's part is an import and an export per round, perhaps an hour including the export, and a further round to confirm each batch of B changes. Each B change is smaller than a module's numerics study, since the algorithm stays; each C change is the size of a new module's study. A B change reviews the cost of its per-value kernels and halos within the retained bounds; a C change answers the performance-rules checklist with its own change.

## Decided

The owner, on 2026-10-06: Lightroom is not a rendering target, but its controls are the result of years of research and people are used to them. Slider values matter: someone used to +10 Clarity or −10 Vignette expects the same result. So Luxforge's sliders follow Lightroom's responses (level B) for every supported setting; where a dimension still differs widely, a targeted algorithm change (level C) is considered. The rest were decided the same day, each as recommended.

| # | Question | Decided | Not chosen |
| --- | --- | --- | --- |
| A1 | Which levels | B for every supported setting, C targeted | A only; A and B only; Lightroom behaviour throughout |
| A2 | Order | By editing area: Basic tone and colour, then Presence, the mixer and the vignette, then Detail, then masks and geometry, then white balance on RAW | By how often the owner uses each setting; biggest gap first |
| A3 | How Lightroom is driven | Generated XMP and one manual import and export per round by the owner, all tooling in Rust | A Lightroom Classic Lua plugin (a second toolchain) |
| A4 | Sources | New alignment work uses rendered outputs only and introduces no Adobe code, SDK source, binary or data; the existing RAW importer/table is for separate review (owner, 2026-10-07) | Reading Adobe's SDK source; decompiling Adobe's binaries |
| A5 | Base rendering | Measured and subtracted; the Standard look and the RAW looks decision unchanged | An optional look fitted to Adobe Color |
| A6 | Existing photographs under a B or C change | Refused by the raised payload format marker, as every format change is; a new catalog | Re-rendering old values at the new scale; a one-off conversion |
| A7 | Real-edit validation | On the owner's catalog, locally, figures only, with consent asked each run | Standing consent; synthetic and corpus images only |
| A8 | When a setting needs C | After its B change, a median ΔE2000 above 2 at Lightroom values of ±25 and beyond, or a difference in kind; the change targets the dimension that differs | Owner review of every setting without a threshold; a threshold of 1 |
| A9 | What realignment rests on (owner, 2026-10-13) | Sliders are realigned (B), not only imports mapped; before any control changes, the corpus photographs and round 2's pairs are measured in one Lightroom session, so B rests on targets, real photographs and combinations | Realigning Basic, or every determined setting, from round 1's targets alone |
| A10 | A range shortfall under B (owner, 2026-10-13) | Where Lightroom's extreme does more than Luxforge's whole range, the control's internal range is extended so ±100 reaches it, where the algorithm stays stable (checked per setting); a shortfall that cannot be extended is reported | Capping at today's strongest value (a flat end of the slider); deciding per setting |
| A11 | The importers meanwhile (owner, 2026-10-13) | The calibrated maps stay the importers' bridge; a setting's row becomes the identity, and is removed, when its slider is realigned | Reverting imports to value transfer until then |

## References

- [Presets](presets.md#mapping), [Lightroom import](lightroom-import.md)
- [Basic tone](basic-tone.md), [Basic colour](basic-colour.md), [Tone curve](tone-curve.md), [Presence, colour mixer and vignette](presence-mixer-vignette.md), [Detail](detail.md), [Masking](masking.md), [Lens and perspective](lens-and-perspective.md), [RAW looks](raw-looks.md), [GPU-first](gpu-first.md), [sample corpus](sample-corpus.md)
- Research: [slider audit](../research/lightroom/slider-parity.md), [tone and colour tools](../research/lightroom/tone-and-color-tools.md), [preset formats](../research/lightroom/presets.md)
