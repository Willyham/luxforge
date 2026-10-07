# Colour grading in the mixer

Status: decided and ready for implementation. The owner settled the product choices and requested a plan without approval or research gates on 2026-10-07 ([decisions](../decisions.md#colour-grading)). The [task plan](../../tasks/editing/colour-grading.json) delivers a working extension of `luxforge.mixer` first, then reference analysis and Lightroom response refinement. This planning turn makes no editor changes.

## Outcome and scope

Add tonal-range tinting to the existing Colour mixer: Shadows, Midtones, Highlights and Global, with hue, saturation and luminance, plus Blending and Balance. Existing eight-range HSL adjustments stay available. Both views contribute to the picture at once; selecting a view never enables or disables an effect.

The Lightroom reference is its familiar control model. Adobe describes the four ranges, smooth tonal overlap, brightness controls and hue/saturation wheels in its [engineering introduction](https://blog.adobe.com/en/publish/2020/10/20/introducing-color-grading) and [current Classic guide](https://helpx.adobe.com/ie/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html). The project already requires [measured slider response alignment](lightroom-alignment.md): names and ranges alone do not prove that a value feels like Lightroom. Deliver the familiar controls with a documented initial algorithm first. Independent reference analysis and measured response alignment come at the end; neither is a prerequisite for the working feature. Claim alignment only for responses actually measured.

Delivery scope: JPEG and all currently qualified RAW sources, global and mask-bound mixer layers, continuous drafts, history and reopen, numeric/API access to every adjustment, Luxforge presets, and the grading fields already recognised but reported unsupported by Lightroom preset import. The proposed catalog importer can consume the same mapper when delivered; catalog import itself is outside this plan.

No additional HSL modes, Point Color, targeted adjustment, B&W mixer, LUT loading, grain, eyedropper, automatic looks or new module are included. Per-wheel temporary muting, clipboard menus and saturation-boost diagnostics are deferred rather than shipped as placeholders. A neutral grade must add no image processing.

## Current integration points

- `crates/luxforge-core/src/modules/mixer/mod.rs`: `FieldPatchModule<Mixer>`, twenty-four HSL fields, `edit.set-mixer` and `edit.reset-mixer`, a maskable single effect per target, colour-stage order 10.
- `crates/luxforge-core/src/modules/mixer/unit.rs` and `unit.wgsl`: the frozen HSL unit and its registered GPU program. [Mixer study](mixer-study.md) owns its current equations.
- `crates/luxforge-core/src/modules/field_patch.rs`: fields, defaults, group resets, canonical payloads, parameter state and no-op planning. Its `is_neutral` hook does not by itself distinguish dormant settings from absent settings.
- `crates/luxforge-core/src/modules/descriptor/types.rs`: numeric, RGB colour and curve controls, recursive groups and top-level `ModuleLayout::Tabs`. There is no hue/saturation wheel or group-level tab layout today.
- `crates/luxforge-app/src/app/controls.rs`, `src/state/control_tree.rs` and `crates/luxforge-ui/src/widgets`: generated desktop controls, traversal and widgets. A wheel must use those paths and the core draft lifecycle.
- `crates/luxforge-core/src/presets/mapping.rs`: recognises grading and split-toning fields, their amount dependencies and the `EnableSplitToning` panel switch; currently reports active grading unsupported.
- `crates/luxforge-reference/src/mixer.rs`, its study tests, `xtask/src/mixer_smoke.rs`, field-patch conformance and GPU qualification: independent references and current mixer evidence to extend.

Canonical placement remains Basic → RAW look when present → Tone curve → mixer → Presence → geometry → vignette, with Detail and repairs in their existing declared stages. Stored layers are never reordered to manufacture that order. The host already owns masked-layer placement and blending.

## Controls

| Field family | Range | Default | Meaning |
| --- | --- | --- | --- |
| `grade-{shadows,midtones,highlights,global}-hue` | 0–360°, step 1 | 0 | Tint direction; 0 and 360 render the same direction |
| `grade-{shadows,midtones,highlights,global}-saturation` | 0–100, step 1 | 0 | Tint strength, independent of the HSL saturation controls |
| `grade-{shadows,midtones,highlights,global}-luminance` | −100–100, step 1 | 0 | Brightness treatment, active even at zero tint strength |
| `grade-blending` | 0–100, step 1 | 50 | Overlap of the three tonal ranges |
| `grade-balance` | −100–100, step 1 | 0 | Negative extends shadows; positive extends highlights |

These fourteen numeric fields extend the existing field-patch action; their names are decided for implementation, not shipped parameters. Missing fields use their declared defaults, including Blending 50. Do not route grading through an RGB colour parameter: its stored values and discoverable controls are hue, saturation and luminance.

Hue follows the familiar RGB colour-wheel direction, with red at 0°, green at 120° and blue at 240°. Do not equate those numbers to Oklab hue angles. The implementation documents how the chosen tint maps into the processing space; the final refinement task can improve that mapping. The numeric 360 endpoint remains valid without changing its stored value; the wheel wraps continuously across the seam.

## Workspace

Keep the Colour mixer band. Inside it, a segmented **HSL / Grading** row selects a view, starting on HSL. HSL keeps its existing Hue / Saturation / Luminance tabs. Grading starts in a **3-way** view with Midtones above Shadows and Highlights; individual Shadows, Midtones, Highlights and Global views use a larger wheel. Each wheel has a luminance rail; the individual view exposes hue and saturation numbers, with explicit labels rather than relying on colour alone. Blending and Balance sit below the tonal views and are hidden in Global because they do not affect it. They keep their values when hidden.

Use the existing panel density and theme roles. Make the three-wheel arrangement fit the current panel width with readable labels and usable handles; a narrow panel uses the declared individual-wheel layout when three wheels cannot retain the existing minimum control targets. The implementer verifies the rendered layout and adjusts spacing without an owner-review stop.

All view selection is session/presentation state: no recipe field, history entry, raster request or image upload. The view is discoverable in the module's semantic control tree and selectable/readable through the command service. Extend the existing session workspace schema with bounded, validated view state keyed by declared control identities; this workspace API extension is decided for implementation, not shipped. Desktop and API selection use that same path, with no private business state. An unfamiliar client can still operate every numeric field without rendering wheels.

A wheel is a shared semantic control binding two numeric parameters of one action. Angle edits hue; radius edits saturation. A gesture sends both values in one patch through one draft, updates at the existing bounded cadence, commits once on release and cancels on Escape. At the centre retain the last hue rather than computing an undefined angle. Keyboard/numeric edits reach the same command service. Wheel helpers: Shift constrains hue while saturation moves, Cmd/Ctrl constrains saturation while hue moves, and Option/Alt enables fine adjustment. Resolve conflicts through the existing control focus rules as implementation work; modifiers apply only to the focused wheel gesture. Tab focus, typed exact values, inspection/copy-request and historical/disabled states remain available.

Resets are explicit:

- A wheel reset restores that range's three fields only.
- Reset HSL restores only its twenty-four fields.
- Reset Grading restores its fourteen fields, including Blending 50 and Balance 0.
- The mixer band reset, `edit.reset-mixer`, restores both views.

Group resets use declared patches; no extra reset command is needed unless the existing action/control contract cannot express a required reset. View selection never changes reset scope.

## Payload, history and masks

Keep the durable effect identity `luxforge.mixer.hsl` and colour-stage order 10, extending its current payload, with a new effect-specific format marker. The identity's historical spelling is not a second effect or a second layer. Do not raise unrelated modules' format markers to implement this change. If the field-patch builder cannot declare an effect-specific marker, add the smallest shared declaration needed.

Only the new payload shape is supported after delivery. Unsupported saved shapes fail explicitly without rewriting the catalog, recipe or original; no migration or compatibility reader is added. This applies to histories, stored preset shapes and externally supplied recipes where their current validation requires a marker. The implementation task inventories those boundaries and documents the actual refusal scope.

Preserve dormant settings. A hue change at zero saturation or a Blending/Balance change with no active grade is still parameter state: it must survive later edits, history, presets and reopen. Existing field-patch behaviour can store a non-default layer for these values; compilation separately omits the grade's processing when its four saturation and four luminance amounts are zero. Do not use pixel neutrality to discard a newly selected hue. A payload at all declared defaults is the canonical neutral form. The Custom indicator follows stored settings, including dormant settings, as current field-patch controls do.

History labels distinguish `Shadows tint`, `Highlights luminance`, HSL and Grading resets. A wheel's two-field update is one atomic action, not two entries. Missing/disabled mixer providers keep reporting the affected edit instead of omitting grading.

Each target owns one mixer layer: the global target and each existing mask are separate. Within that layer HSL runs first, grading second. The host blends the complete resulting layer with its input once using mask coverage; neither unit applies a second mask. Zero coverage preserves the input exactly. Duplicating/reordering masks, another client's edits, draft cancel, undo/redo and reopen retain all thirty-eight fields and their target identity.

## Initial numerical implementation

Implement a bounded pointwise grading unit after the unchanged HSL unit in the same colour run, without a new quantization boundary. Omit each inactive unit independently; grading-neutral HSL output stays exact. The implementer chooses and documents initial constants and equations as part of TASK-002, without a separate study, contact-sheet approval or Lightroom input prerequisite. The owner delegates these numerical implementation choices for this delivery; final analysis can refine them.

Start from perceptual brightness and smooth shadow/midtone/highlight weights, with Balance shifting the tonal distribution and Blending broadening overlap. Oklab is the initial candidate because the mixer already uses it; select another bounded representation only if it fixes a concrete implementation problem. Displayed hue stays on the familiar RGB wheel and is converted into the processing representation. Grading intentionally colours greys, so HSL's achromatic suppression is not reused. Mix tint vectors rather than averaging hue degrees.

Document these implementation contracts beside the code:

1. Weights read the post-HSL input brightness once. They are smooth even at minimum Blending; negative Balance extends shadows and positive Balance extends highlights. Global runs after the three tonal contributions and does not read Blending or Balance.
2. Saturation controls tint strength; luminance uses the same tonal selection and works without tint. At zero luminance adjustment black and white remain endpoints; lifting black or dimming white permits tinting. The initial luminance equation is a bounded brightness treatment, not RAW highlight recovery.
3. Handle signed and above-white RAW input explicitly. A bounded coordinate may select weights, but do not truncate the underlying RGB headroom merely to select them. Output must be finite, preserve alpha and use the host's existing output/gamut rules.
4. All four zero saturation and zero luminance amounts omit grading entirely, including when dormant hues or Blending/Balance are non-default. CPU and GPU consume the same bounded coefficient contract.

During implementation, test neutral bypass, hue seams, grayscale tinting, weight continuity, Global independence, luminance-only adjustments, extended RAW input, masks and source preservation. Capture whole-buffer expected outputs for the chosen initial equations and compare native GPU output to the whole-frame CPU renderer within the project's declared tolerance. These are normal correctness checks delivered with the code, not a research phase that prevents UI integration.

## Final reference analysis and Lightroom refinement

TASK-010 starts only after TASK-009 hands off the working feature: all controls, masks, presets, direct Lightroom preset mappings, native evidence and current documentation. No earlier task depends on TASK-010, owner contact-sheet approval or Lightroom exports.

At that point add the independent f64 grading reference, expand the synthetic/corpus analysis, and examine strength, luminance, tonal overlap, balance, gamut and headroom against the initial implementation. Reference discrepancies lead to contained corrections backed by fixtures and GPU comparisons. The implementer reviews response plots and contact sheets; there is no further product-decision or review task.

Reuse the [Lightroom alignment](lightroom-alignment.md#the-rig) tooling if delivered. Otherwise implement the smallest grading-only generate/measure slice using the same decided Rust tooling and generated-XMP method; this feature's final refinement task does not wait for authorization or completion of the whole alignment programme. Do not implement alignment for unrelated tools or the catalog importer.

A real Lightroom comparison needs rendered outputs from the owner's installation. Build and test the measurement tooling with known synthetic responses autonomously, and consume those exports when available. Their absence leaves the Lightroom response figures unmeasured; it does not hold up the feature or completion of the implementation/tooling work. Never substitute fabricated measurements or claim a match from names and ranges.

Measure each editor's change from its own neutral rendering. Include a grey wedge, brightness × hue × chroma grids, interacting tonal ranges, Global/luminance and mixed HSL/grade settings, followed by skin, foliage, sky, low-light and highlight photographs. Apply the alignment programme's B policy and C threshold to measured dimensions, adapting sampling to one-sided saturation and circular hue. Implement warranted grading response changes at this final stage, update direct import conversions where necessary and raise the relevant format marker if stored values change meaning. Only current shapes are supported.

Use published controls and rendered outputs only: no Adobe code, binary, profile or table is inspected or shipped. Owner real-edit validation and its per-run consent remain in the separate alignment programme; this plan needs neither a private catalog nor a new approval step.

## GPU and bounded work

The GPU remains the renderer of record for drags, the picture at rest, histogram, samples, Select previews and export. Add/register the grading colour program and its fixed-size coefficient pack, and exercise existing program discovery, limits, mask wrappers, warm-up and qualification. A parameter edit changes coefficients and uses the existing stale-work cancellation; it never decodes or hashes a source again, creates a new full-frame CPU buffer, or starts a separate worker queue.

Pixel arithmetic stays on render workers/GPU. Validation, no-op checks and coefficient preparation are bounded by the thirty-eight-field payload. The wheel paints from parameter state, without pixel sampling or a photograph-derived cache. No polling or timer is added for a hidden view. Retain current GPU resource limits; any requested increase is an owner decision, not a default in this plan.

Reference tests prove exact neutral bypass and frozen whole buffers; native GPU comparisons use the current [declared tolerance](gpu-first.md), not CPU bit identity. Existing drag/reduced-view exceptions remain reported with scope; the feature introduces no larger tolerance silently. Complete the [performance checklist](../engineering/performance-rules.md#review-checklist) once for the finished change, with native 24/60 MP before/after and supported RAW scope. Time only after feature work is complete.

## Presets and Lightroom fields

Grading ships with Luxforge capture/create/update/apply/export through the existing preset service. HSL and Grading are independently selectable capture groups. A preset containing one preserves the other's values. Grade-only capture includes dormant hue, all four ranges and Blending/Balance, so it reproduces the treatment on a target with different prior grade settings. This requires preset grouping independent of presentation tabs; do not let a nested view lose fields or produce duplicate capture controls.

Replace the existing unsupported mapper rows with direct value transfers alongside the working feature. Do not wait for calibrated response conversions. Cover `SplitToningShadowHue/Saturation`, `SplitToningHighlightHue/Saturation`, `SplitToningBalance`, `ColorGradeMidtoneHue/Sat`, `ColorGradeGlobalHue/Sat`, `ColorGrade{Shadow,Midtone,Highlight,Global}Lum` and `ColorGradeBlending`, respecting `EnableSplitToning` and source era. Share the mapper across XMP and `.lrtemplate`; no invented names or clamps. Reports distinguish direct transfer, unsupported and refused settings at initial delivery; calibrated conversion is reported only if the final refinement actually adds one.

An unambiguous older split-toning document uses shadow/highlight values with Blending 100 and the later grade controls neutral, while preserving HSL. A partial modern grading document keeps the preset service's declared patch semantics. The mapper implementation proves how the reader distinguishes them using representative documents; absence of a modern field alone is not sufficient evidence. Ambiguous inputs remain reported unsupported/refused rather than guessing. This is source-format import into today's shape, not support for old Luxforge recipes or a promise of identical Adobe rendering.

## Acceptance and verification

- All fourteen grading adjustments are schema-discoverable, validated, patchable and maskable through `edit.set-mixer`; numeric, wheel and JSON routes produce equivalent state and history. Existing HSL-only output is exact with grading neutral.
- Every reset preserves the other declared scopes. Dormant hues, seam values and Blending 50 defaults survive history, presets and reopen; invalid/non-finite fields, unknown fields and unsupported formats refuse without data changes.
- Implementation fixtures cover gradients, greys, endpoints, gamut limits, signed/headroom RAW, seam continuity, range overlaps, opposing tints, Global independence, luminance-only and mixed HSL settings. Whole-frame CPU buffers and native GPU comparisons ship with the feature; the separate independent f64 study and response perfection follow in TASK-010.
- Background native journeys correlate captures, state and logs for wheel dragging, numeric input, cancel/commit, view-only switching, external-client refresh, masks, presets, undo/redo/reopen, Compare and actual single/batch JPEG export. Source hashes remain unchanged, including refusal/recovery cases.
- A new registered grading smoke scenario and grading recipes in the existing GPU gate cover Fit, 33%, 50% and 100%, at rest and during drags, histogram and samples. Mask coverage at 0/partial/1 and colour/luminance-range masks exercise the existing host semantics.
- Targeted tests run while editing; quick runs once at each finished handoff, rendered at the integrated UI/image handoff. Full with the supported RAW manifest, including its existing timing and GPU gate, checks the working-feature handoff in TASK-009, without waiting for the final reference/Lightroom task. No foreground app launch is used; ad hoc checks use `tools/cargo-cached xtask develop --background` with isolated evidence/catalogs.
- Record M4 slider/wheel latency, commit, sample/export cost and CPU/GPU memory against HSL-only at 24/60 MP after delivery, with counts and misses. Native Windows/Linux and calibrated display/screen-reader gaps remain explicit where unverified.
- Update this design, the mixer and module/API contracts, presets/mapping, feature status and user guide at the behaviour handoff. Planned fields and controls are not documented as shipped beforehand.

## Decided delivery contract

The owner chose on 2026-10-07:

- HSL / Grading tabs inside Colour mixer, with three-way and individual/Global wheel views.
- All four wheels, luminance, masks, Luxforge presets and direct Lightroom preset mappings in the working feature.
- Retain hue choices at zero saturation, with wheel, HSL and Grading resets and reset-all on the mixer band.
- An implementation-ready plan with no approval, numerical-study or owner-review stops. Deliver the working feature before Lightroom matching and reference perfection; initial numerical/layout details are implementer decisions within this contract.

One mixer layer per target, HSL before grading, host-owned masking and explicit current-format refusal follow the existing module architecture. Ordinary task output dependencies, code tests and rendered verification remain part of implementation. TASK-001 records the completed decisions; TASK-002 and TASK-003 are immediately runnable. No further product questions are outstanding for this scope.
