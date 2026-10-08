# Colour grading in the mixer

Status: implemented. The owner settled the product choices on 2026-10-07 ([decisions](../decisions.md#colour-grading)). The working feature, its reference and fixtures, and the grading alignment tooling are delivered; native evidence so far is from an M2 Mac, not the owner's M4, and the Lightroom response is not yet measured ([final refinement](#final-reference-analysis-and-lightroom-refinement)). What remains is in [feature status](../features.md).

## Outcome and scope

Add tonal-range tinting to the existing Colour mixer: Shadows, Midtones, Highlights and Global, with hue, saturation and luminance, plus Blending and Balance. Existing eight-range HSL adjustments stay available. Both views contribute to the picture at once; selecting a view never enables or disables an effect.

The Lightroom reference is its familiar control model. Adobe describes the four ranges, smooth tonal overlap, brightness controls and hue/saturation wheels in its [engineering introduction](https://blog.adobe.com/en/publish/2020/10/20/introducing-color-grading) and [current Classic guide](https://helpx.adobe.com/ie/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html). The project already requires [measured slider response alignment](lightroom-alignment.md): names and ranges alone do not prove that a value feels like Lightroom. Deliver the familiar controls with a documented initial algorithm first. Independent reference analysis and measured response alignment come at the end; neither is a prerequisite for the working feature. Claim alignment only for responses actually measured.

Delivery scope: JPEG and all currently qualified RAW sources, global and mask-bound mixer layers, continuous drafts, history and reopen, numeric/API access to every adjustment, Luxforge presets, and the grading fields already recognised but reported unsupported by Lightroom preset import. The proposed catalog importer can consume the same mapper when delivered; catalog import itself is outside this plan.

No additional HSL modes, Point Color, targeted adjustment, B&W mixer, LUT loading, grain, eyedropper, automatic looks or new module are included. Per-wheel temporary muting, clipboard menus and saturation-boost diagnostics are deferred rather than shipped as placeholders. A neutral grade must add no image processing.

## Implementation

- `crates/luxforge-core/src/modules/mixer/mod.rs`: `FieldPatchModule<Mixer>` with thirty-eight fields, `edit.set-mixer` and `edit.reset-mixer`, a maskable single effect per target at colour-stage order 10 and its own format marker 2, the HSL and Grading groups and views, and the compilation into the HSL unit then the grading unit.
- `crates/luxforge-core/src/modules/mixer/grade.rs` and `grade.wgsl`: the grading unit, its 18-word coefficient pack and its registered GPU program (`lf_mixer_grade`), beside the frozen HSL unit (`unit.rs`, `unit.wgsl`), whose equations the [mixer study](mixer-study.md) owns.
- `crates/luxforge-core/src/modules/field_patch.rs`: the per-effect format marker (`Spec::format`), nested subgroups, tab layouts, presentation-only views and wheels ([modules and API](modules-and-api.md)).
- The `wheel` control, group `layout: tabs` and `view` groups ([UI components](ui-components.md)), drawn by `crates/luxforge-ui/src/widgets/wheel.rs` through the shared draft path, and the session's `workspace.views`.
- `crates/luxforge-core/src/presets/mapping.rs`: the grading rows of the Lightroom preset mapper ([presets](presets.md#mapping)).
- `crates/luxforge-reference/src/grade.rs` and `grade_response.rs`, their studies, `fixtures/mixer/grade-cases.json`, `xtask/src/grade_align.rs`, the `grading` smoke scenario, the grading section of `editor-acceptance` and the grading recipes of the GPU corpus.

Canonical placement remains Basic → RAW look when present → Tone curve → mixer → Presence → geometry → vignette, with Detail and repairs in their existing declared stages. Stored layers are never reordered to manufacture that order. The host already owns masked-layer placement and blending.

## Controls

| Field family | Range | Default | Meaning |
| --- | --- | --- | --- |
| `grade-{shadows,midtones,highlights,global}-hue` | 0–360°, step 1 | 0 | Tint direction; 0 and 360 render the same direction |
| `grade-{shadows,midtones,highlights,global}-saturation` | 0–100, step 1 | 0 | Tint strength, independent of the HSL saturation controls |
| `grade-{shadows,midtones,highlights,global}-luminance` | −100–100, step 1 | 0 | Brightness treatment, active even at zero tint strength |
| `grade-blending` | 0–100, step 1 | 50 | Overlap of the three tonal ranges |
| `grade-balance` | −100–100, step 1 | 0 | Negative extends shadows; positive extends highlights |

These fourteen numeric fields extend the mixer's field-patch action after its twenty-four HSL fields. Missing fields use their declared defaults, including Blending 50. Grading is not an RGB colour parameter: its stored values and discoverable controls are hue, saturation and luminance. Hue and saturation declare the `field` number style, so a client without wheels shows them as exact numbers.

Hue follows the familiar RGB colour-wheel direction, with red at 0°, green at 120° and blue at 240°; it is not an Oklab hue angle, and [the tint mapping](#initial-numerical-implementation) converts it. The numeric 360 endpoint stays stored as 360 and renders as 0; the wheel wraps continuously across the seam.

## Workspace

The Colour mixer band holds a segmented **HSL / Grading** tab row, starting on HSL. HSL is a group whose Hue, Saturation and Luminance subgroups are a nested tab row, as before. Grading is one group owning the fourteen fields, shown through presentation-only views: **3-way**, with compact Midtones above Shadows and Highlights (compact wheels share rows two to a row, an odd one leading alone), then **Shadows**, **Midtones**, **Highlights** and **Global**, each a large wheel with its labelled hue and saturation numbers. Every wheel draws its range's luminance under the disc on a dark-to-light rail. Blending and Balance follow every tonal view and are absent from Global, which does not read them; they keep their values while hidden. The tools panel is a fixed 300 points, which holds two 104-point compact wheels side by side at every interface size, so the 3-way view keeps its targets and no narrow-width fallback is needed; the individual views remain one tab away.

View selection is session state: no recipe field, history entry, raster request or upload. A tab row is named by its module and the label path of its group (`luxforge.mixer`, `[]` for HSL/Grading; `["Grading"]` for the five views; `["HSL"]` for Hue/Saturation/Luminance), read in `session.state` and set by `workspace.set {views}`, validated against the descriptors and refused by name without writing. The desktop selects tabs through the same path. An unfamiliar client operates every field through `edit.set-mixer` without drawing wheels.

A wheel is the shared `wheel` control over two fields of one action, with an optional luminance rail ([UI components](ui-components.md)). Angle edits hue and radius saturation; a gesture sends both in one patch through one draft at the slider cadence, commits once on release and cancels on Escape. A move through the centre keeps the hue the handle had. Shift holds the hue while saturation moves, Cmd/Ctrl holds the saturation while hue moves, Option/Alt moves the handle a tenth as far, each only during that wheel's gesture; arrow keys nudge a hovered wheel. Tab focus, typed values, inspection and Copy as JSON, and historical and disabled states work as for other controls.

Resets are explicit:

- A wheel reset (double-click) restores that range's three fields only, `Reset Shadows`.
- Reset HSL restores only its twenty-four fields; Reset Hue, Saturation or Luminance their eight.
- Reset Grading restores its fourteen fields, Blending 50 and Balance 0 included.
- The band reset, `edit.reset-mixer`, restores both.

Each is a declared patch of the set action, so no reset command is added, and a view never changes a reset's scope. The preset form shows HSL and Grading as one checkbox each; their nested tabs and views are presentation and capture nothing twice.

## Payload, history and masks

The durable effect identity stays `luxforge.mixer.hsl` at colour-stage order 10; its payload gained the grading fields and its format marker is now 2 (`MIXER_EFFECT_FORMAT`), declared with `Spec::format`; no other module's marker changed. Only the current shape is read. A stored mixer layer at format 1, in a catalog's history, an externally supplied recipe or a layer built elsewhere, is refused as `incompatible: unsupported effect format 1` on every path that reads it (validation, description, compilation and rendering) and is never rewritten; a catalog holding one needs a new catalog. Luxforge preset documents carry field values, not layers, so a stored preset's mixer fields stay valid. `Layer::new` and the test kit stamp a built-in effect's current format (`current_effect_format`, held to the registry's descriptors by a test).

Dormant settings are kept. A hue at zero saturation, or a Blending or Balance with no active grade, is stored like any field away from its default: a first such set commits a layer, and the value survives later edits, history, presets and reopen. Compilation omits the grading unit while its four saturation and four luminance amounts are zero, so such a layer, and an HSL-only layer, render exactly as before. The all-default payload is `{}`. The Custom indicator follows stored values, dormant ones included.

History labels name what moved: `Shadows hue 210`, `Highlights luminance -20`, `Blending 70`, `Balance +15`; a wheel's hue and saturation in one patch are one entry, `Shadows tint`; resets read `Reset Shadows`, `Reset Hue`, `Reset HSL`, `Reset Grading` and `Reset Colour mixer`. An unavailable mixer provider reports the affected edit as before.

Each target owns one mixer layer: the global target and each mask are separate. Within the layer HSL runs first and grading second, in one colour run with no quantization between them. The host blends the whole layer with its input once by the mask's coverage; neither unit masks anything itself, and zero coverage keeps the input exactly.

## Initial numerical implementation

A bounded pointwise grading unit runs after the unchanged HSL unit in the same colour run, with no quantization boundary between them ([`grade.rs`](../../crates/luxforge-core/src/modules/mixer/grade.rs), beside its GPU program `grade.wgsl`). Each unit is omitted independently when it would change nothing, so an HSL-only layer renders exactly as it did before grading existed. These are the implementer's initial equations, chosen for smoothness, bounded cost and exact neutrality; they are not measured against Lightroom, and the final refinement may change them. The `f64` reference [`luxforge_reference::grade`](../../crates/luxforge-reference/src/grade.rs) restates them for the tests.

Per pixel, in Oklab (the conversion the HSL unit already uses):

1. **Tonal selection.** The post-HSL lightness `L`, clamped to `[0, 1]`, is the bounded coordinate the weights read, once; the clamp never touches the pixel itself. Two smoothstep transitions of one width `w`, centred on the shadow/midtone boundary `c1` and the midtone/highlight boundary `c2`, give Shadows `1 − s1`, Midtones `s1 − s2` and Highlights `s2`, which sum to one and are never negative. At Balance 0 the boundaries are ⅓ and ⅔; Balance ±100 moves both by ∓0.25, so negative extends Shadows and positive extends Highlights. Blending 0..100 moves `w` linearly from 0.1 to 1.0 (0.55 at the default 50); even the narrowest transition is a smooth ramp.
2. **Luminance.** The tonal luminance amounts, weighted by the same selection, form one exponent `m = 0.5 · Σ weight × luminance/100`, and `L` takes the gamma `2^(−m)` on its `[0, 1]` part with any excess passed through, like the HSL luminance response. The luminance weights use the same boundaries but a width of at least 0.4: below it, two neighbouring ranges driven in opposite directions could fold the tone scale, and the floor keeps the response monotone for every combination (a test checks all 81 extreme combinations at the Blending and Balance extremes). Shadows luminance +100 also lifts black to Oklab `L` 0.15, and Highlights −100 dims white by 0.08, through one increasing affine map; at 0 black and white stay where they are. Global luminance then applies its own gamma and reads neither the weights nor Blending nor Balance.
3. **Tint.** A hue on the RGB colour wheel (red 0°, green 120°, blue 240°) becomes the Oklab `(a, b)` direction of the fully saturated sRGB colour at that angle, so 0 and 360 are the same direction and the seam is continuous. Saturation scales it to an Oklab chroma of up to 0.1. The three tonal tints are mixed as vectors by their weights, Global's is added at full weight, and the sum is added to the pixel's own `(a, b)` through the envelope `1 − (2c − 1)^8` of the output lightness `c` clamped to `[0, 1]`. The envelope is zero at black and white, so an untouched black stays exactly black and white keeps its code, while black lifted by the luminance treatment, or white dimmed, takes the tint. Greys are tinted on purpose; HSL's achromatic suppression is not reused.

**Extended input.** A RAW photograph's linear path can bring signed and above-white values. Only the weight coordinate and the envelope read the clamped lightness; the luminance responses pass the part of `L` outside `[0, 1]` through (after the lift's offset), and the tint is zero there, so the unit is continuous across black and white and never truncates headroom. Every output is finite for finite input, and the unit touches only the three colour channels, so alpha is never read or changed.

**Neutrality and the coefficient pack.** The unit is compiled only when one of the four saturation or four luminance amounts is non-zero; hue, Blending and Balance alone are dormant settings with no processing. Coefficients are computed once per compile in `f64` and cast into one fixed 18-word `f32` pack (two boundaries, two inverse widths, four `(a, b)` tints, four luminance exponents, the lift and the dim) that the CPU unit and the GPU program read alike. The unit's identity is that pack, so a dormant edit changes no cached render.

The tests cover neutral bypass, hue seams, grey tinting, weight continuity and partition, Global independence, luminance-only adjustments, endpoints, opposing tints, monotone luminance, extended RAW input and finite output, against the `f64` reference in linear light (`1e-5 + 1e-5·|reference|`) and through both render paths at the shared code band, and native GPU output against the whole-frame CPU renderer within the declared pointwise limits.

## Measurement tools

The native `editor-latency --control wheel` path measures a two-field wheel gesture in its
individual Grading view; the default is Shadows, or name a declared hue with `--action` and
`--parameter`. Scalar HSL and grading fields use the existing slider path. The paired
`grade-performance` workload compares HSL alone with HSL plus grading through owner commits,
whole-frame reference rendering, verified pixel samples and serial reference JPEG exports on
JPEG or RAW. Both use the host timing lock; release figures need a quiet host. Reports retain
recipes, patches, sample counts, source hashes, resources and load. Native desktop GPU memory
and gesture RSS are scoped separately from the headless reference costs.

## Final reference analysis and Lightroom refinement

This stage follows the working feature, and nothing in delivery depends on it. Its reference and tooling now exist; the Lightroom figures await the owner's exports.

**Reference.** [`luxforge_reference::grade`](../../crates/luxforge-reference/src/grade.rs) restates the initial equations in `f64`. Its study ([`studies/grade.rs`](../../crates/luxforge-reference/tests/studies/grade.rs)) proves the weights a partition of unity with slope at most `1.5 / width`, Balance monotone, the lightness response strictly increasing for all 625 combinations of the four luminance amounts at −100, −50, 0, 50 and 100 at the Blending and Balance extremes, black and white fixed except under the lift and dim, the RGB-wheel directions, a full tint's chroma, Global's independence and finite, continuous extended input, and it freezes [`fixtures/mixer/grade-cases.json`](../../fixtures/mixer/README.md#grade-casesjson). Production meets it within `1e-5 + 1e-5·|reference|` on every case. The study's figures (`grade_figures`) show, at the default Blending, Shadows saturation 100 adding Oklab chroma 0.086 to sRGB grey 16 and none to grey 128, and Highlights luminance −100 taking grey 240 to 209; no discrepancy between reference and production was found, so the initial equations stand unchanged.

**The alignment slice.** The [Lightroom alignment](lightroom-alignment.md#the-rig) rig is not built, so this feature has its own grading-only slice with the same generated-XMP method, `cargo xtask grade-align`:

- `generate --output NEW` writes a round: a synthetic sRGB target of 96 flat patches (a 24-step grey wedge and an Oklab lightness × hue × chroma grid), 110 JPEG copies of it, each carrying one variant's full grading state as embedded Lightroom XMP (Camera Raw 15.4), and a manifest. Each variant's XMP is read back through Luxforge's own preset importer and must map to exactly the manifest's `set-mixer` fields. The variants sweep each wheel's saturation, luminance and hue, Blending and Balance.
- The owner imports the round into a scratch Lightroom Classic catalog, reading metadata from files, and exports every variant as an uncompressed 16-bit sRGB TIFF named for it. The tool reads uncompressed baseline TIFF itself and refuses anything else by name, so no TIFF decoder joins the build.
- `measure --round DIR --output NEW [--lightroom DIR]` measures each editor's change from its own neutral variant, renders Luxforge's side through the whole-frame reference renderer on a dense value grid, and fits every family with [`grade_response`](../../crates/luxforge-reference/src/grade_response.rs): monotone maps for saturation, luminance, Blending and Balance, circular ones for hue. Each point reports its residual as the median CIEDE2000 over the patches the setting affects, any range shortfall, and the span of values the round cannot distinguish. A family whose residual exceeds 2 at |value| ≥ 25 is flagged for a behaviour change, the alignment programme's C threshold. It writes `grade-align.json` and a contact sheet; without exports the Lightroom figures are `unmeasured`.
- `render --round DIR --output NEW` writes Luxforge's own variants as exports. Measuring them as Lightroom's is the self-test: every fitted point of all 14 families holds the identity within its span, with a largest residual of 0.395. Luxforge's side reads the 8-bit display frame (patch-interior means), which limits resolution in the darkest patches: the self-test's Shadows spans reach 26 in saturation and 47.5 in luminance. Lightroom's 16-bit exports have no such limit, so this bounds what a round can resolve in the shadows, not the export's precision.

The measures are tested on known answers: Sharma, Wu and Dalal's published CIEDE2000 pairs, a recovered half-strength scale, a reported range shortfall, a monotone map through noisy samples and a recovered 20° hue rotation.

**Still open.** No Lightroom round has been exported, so no grading response is measured against Lightroom, no B or C change is warranted, and the preset mapper's grading rows stay direct value transfers. When exports arrive, apply the B policy and C threshold to the measured families, change the grading response or the importer's conversions where warranted, and raise the mixer's format marker if stored values change meaning. Photographs (skin, foliage, sky, low light, highlights) join the synthetic round then. Use published controls and rendered outputs only: no Adobe code, binary, profile or table is inspected or shipped, and no private catalog is read.

## GPU and bounded work

The GPU remains the renderer of record for drags, the picture at rest, histogram, samples, Select previews and export. Add/register the grading colour program and its fixed-size coefficient pack, and exercise existing program discovery, limits, mask wrappers, warm-up and qualification. A parameter edit changes coefficients and uses the existing stale-work cancellation; it never decodes or hashes a source again, creates a new full-frame CPU buffer, or starts a separate worker queue.

Pixel arithmetic stays on render workers/GPU. Validation, no-op checks and coefficient preparation are bounded by the thirty-eight-field payload. The wheel paints from parameter state, without pixel sampling or a photograph-derived cache. No polling or timer is added for a hidden view. Retain current GPU resource limits; any requested increase is an owner decision, not a default in this plan.

Reference tests prove exact neutral bypass and frozen whole buffers; native GPU comparisons use the current [declared tolerance](gpu-first.md), not CPU bit identity. Existing drag/reduced-view exceptions remain reported with scope; the feature introduces no larger tolerance silently. Complete the [performance checklist](../engineering/performance-rules.md#review-checklist) once for the finished change, with native 24/60 MP before/after and supported RAW scope. Time only after feature work is complete.

## Presets and Lightroom fields

Grading ships with Luxforge capture/create/update/apply/export through the existing preset service. HSL and Grading are independently selectable capture groups. A preset containing one preserves the other's values. Grade-only capture includes dormant hue, all four ranges and Blending/Balance, so it reproduces the treatment on a target with different prior grade settings. This requires preset grouping independent of presentation tabs; do not let a nested view lose fields or produce duplicate capture controls.

Replace the existing unsupported mapper rows with direct value transfers alongside the working feature. Do not wait for calibrated response conversions. Cover `SplitToningShadowHue/Saturation`, `SplitToningHighlightHue/Saturation`, `SplitToningBalance`, `ColorGradeMidtoneHue/Sat`, `ColorGradeGlobalHue/Sat`, `ColorGrade{Shadow,Midtone,Highlight,Global}Lum` and `ColorGradeBlending`, respecting `EnableSplitToning` and source era. Share the mapper across XMP and `.lrtemplate`; no invented names or clamps. Reports distinguish direct transfer, unsupported and refused settings at initial delivery; calibrated conversion is reported only if the final refinement actually adds one.

An unambiguous older split-toning document uses shadow/highlight values with Blending 100 and the later grade controls neutral, while preserving HSL. A partial modern grading document keeps the preset service's declared patch semantics. The mapper implementation proves how the reader distinguishes them using representative documents; absence of a modern field alone is not sufficient evidence. Ambiguous inputs remain reported unsupported/refused rather than guessing. This is source-format import into today's shape, not support for old Luxforge recipes or a promise of identical Adobe rendering.

## Evidence

- Core: the grading unit against its reference over whole buffers and the frozen fixture; neutrality, dormant settings, endpoints, seams, weights, monotone luminance, Global independence and extended input; descriptor, field-patch conformance, labels, resets, history, undo/redo and reopen through the editor service; the format-1 refusal; both render paths against the references at the shared code band; dormant settings byte-identical to HSL alone; Lightroom mapping on representative XMP and `.lrtemplate` documents.
- GPU: `gpu_colour_grade_meets_the_pointwise_limits` holds `lf_mixer_grade` to the CPU unit on 15 grading cases (each range at both luminance extremes, the seam, Blending and Balance extremes, luminance alone, grading over HSL) on a native Apple M2 within the declared pointwise limits, worst case mean 0.032, worst block 0.24 and p99 0.74 output codes with no non-finite output. The GPU corpus holds four grading recipes (all wheels over HSL, full-blend split toning, a luminance-range mask and a colour-range mask) over every source and view; the whole-corpus gate has not been run with them.
- Desktop: the wheel and nested views are proved on the developer controls proof and the `grading` and `mixer` background smoke scenarios; the `editor-acceptance` grading section covers masks, `render.sample`, single and batch JPEG export and undo/redo through the JSON method table. Which of these ran, where, is recorded with the change that delivered them, and anything not run stays outstanding in [feature status](../features.md).
- Not yet measured: wheel and slider latency, commit, sample and export cost and memory against HSL alone at 24 and 60 MP; native M4, Windows and Linux runs; the Lightroom response.

## Decided delivery contract

The owner chose on 2026-10-07:

- HSL / Grading tabs inside Colour mixer, with three-way and individual/Global wheel views.
- All four wheels, luminance, masks, Luxforge presets and direct Lightroom preset mappings in the working feature.
- Retain hue choices at zero saturation, with wheel, HSL and Grading resets and reset-all on the mixer band.
- An implementation-ready plan with no approval, numerical-study or owner-review stops. Deliver the working feature before Lightroom matching and reference perfection; initial numerical/layout details are implementer decisions within this contract.

One mixer layer per target, HSL before grading, host-owned masking and explicit current-format refusal follow the existing module architecture. Ordinary task output dependencies, code tests and rendered verification remain part of implementation. TASK-001 records the completed decisions; TASK-002 and TASK-003 are immediately runnable. No further product questions are outstanding for this scope.
