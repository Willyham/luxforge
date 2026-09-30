# Tone curve

Status: planned, not implemented. This design and its [task plan](../../tasks/tone-curve.json) answer the "Tone Curve" row of the [later module candidates](basic-and-histogram.md#later-module-candidates), which the owner accepted as the next module candidate on 2026-09-21 ([decisions](../decisions.md#basic-adjustments-and-histogram)). The choices below are recorded with their reasons; the consequential ones are [proposals with recorded defaults](#proposals-with-recorded-defaults) the plan runs on and the owner refines. Nothing here claims Lightroom or darktable numeric equivalence: Lightroom's Tone Curve panel is the familiarity reference for the name, the plot and the point gestures, not a rendering target.

The planned section is drawn on the [Tone curve board](develop-workspace/tone-curve.png) (a point drag, with every planned section in its panel place) and in its states on the [planned module panels](develop-workspace/planned-module-panels.png); [its own render](develop-workspace/modules/tone-curve.png) shows the neutral and S-curve states. The boards are design references, not evidence of anything built.

It builds on what is delivered: the [field-patch module contract](modules-and-api.md#field-patches), the [`curve` control kind and parameter kind](ui-components.md#control-kinds) with the point editor, the channel switch and the histogram background that the developer controls proof already exercises, the [pointwise colour run](basic-and-histogram.md#pointwise-colour-processing), the [masked colour primitive](masking.md#masked-colour-and-masked-spatial), [presets](presets.md) over every field-patch action, and the frozen [Basic tone](basic-tone.md) vocabulary: the encoded luminance domain, the luminance-ratio reconstruction and its near-black rule.

## Outcome and scope

One Lightroom-familiar section, **Tone curve**, that shapes the tones of the whole photograph with a point curve a person drags and an agent posts as the same list of points.

In scope for the first version:

- One module, `luxforge.curve`, owning one colour-stage layer with **one composite channel** that maps encoded luminance to encoded luminance, on the supported JPEG subset and every implemented RAW source alike.
- Points a person adds, drags, nudges, types and deletes in the delivered curve editor, with the histogram drawn behind the plot; the same points through `edit.set-curve`; the module's sampled curve through `query.sample-curve`.
- Masks, presets, Lightroom preset import of the composite point curve, history, undo and redo, with the exact-image and numerical verification every module has.

Out of scope, with no placeholders: per-channel Red, Green and Blue curves, Lightroom's parametric region sliders and region splits, a "linear versus strong contrast" preset menu, dragging the histogram, targeted-adjustment drags on the photograph, non-monotone curves, a second curve editor, and any approximate preview. Per-channel curves are the one extension this design keeps a place for: the control kind already carries up to eight channels, so they would be further curve fields of the same layer, evaluated after the composite one, under their own study.

## Module boundaries

| Component | Owns | Does not own |
| --- | --- | --- |
| Tone curve module | The field table, neutrality, the history words, the curve control and its sample query, the interpolant (its construction, extension and per-segment coefficients), and the one pointwise unit that applies it to luminance | Colour run execution, quantization, placement among other modules' layers, masks, drafts, history |
| Shared host, as delivered | Field-patch parsing, planning and labels; the `curve` kind's validation (point count, `0..=1` coordinates, strictly increasing `x`, non-decreasing `y`); the colour run and its output clamp; masked blending; the draft lifecycle; presets over `set-curve`; the curve editor and the sample-query plumbing | Any curve equation |
| Shared curve editor, changed with this module | Double-click on a point removes it; the numeric point list is closed behind a Points disclosure until the person opens it ([controls](#controls-and-interaction)) | Any recipe state: both are gestures and view state |

No new host primitive, control kind, parameter kind, stage, limit, timer or desktop message is expected. The delivered vocabulary's acceptance was that "the Tone Curve … module can be declared against this vocabulary with no widget or desktop change" ([UI components](ui-components.md#verification-and-acceptance)), and the declaration still needs none. The owner asked on 2026-09-30 for two changes to the shared curve editor itself, which every `curve` control receives, the developer controls proof included: double-click on a point removes it, and the point list starts closed. They change the widget's gestures and view state, not the descriptor vocabulary or any request.

## The module

- **Identity.** Module `luxforge.curve`, title `Tone curve`, hint `A point curve over the photograph's tones`; effect `luxforge.curve.tone`, stage `color`, order `5`, `single`, `maskable`; actions `set-curve` (the field patch) and `reset-curve`; query `sample-curve`. The registry lists it directly after Basic, so its section follows Basic in the tools panel, collapsed, where the [workspace tool array](develop-workspace.md#tool-array) placed it, and ahead of the planned [Detail](detail.md) section, keeping the order a Lightroom user expects.
- **One field**, `luminance`: kind `curve` with `points_min 2`, `points_max 16`, `monotone true`, no `fixed_x`; default `[[0, 0], [1, 1]]`; `step 0.01`, `precision 2` (the point list's display and the arrow-key nudge; Option nudges by the default fine step, `0.001`). The host stores exactly the points it was sent, which the `curve` kind has already checked: between 2 and 16 points, every coordinate in `0..=1`, `x` strictly increasing, `y` non-decreasing. A patch that changes nothing is a no-op; `{}` is the canonical neutral payload and `{"luminance": [[0, 0], [1, 1]]}` the same state written differently.
- **One group**, `Tone curve`, whose only field is the curve channel, so the section draws the curve editor alone under its band with no sub-group header: `Control::curve("set-curve", [{luminance, "Luminance"}], "Tone curve", "sample-curve")` with `background: histogram`. One channel declares no channel switch. The band carries the module reset.
- **History words.** A field patch is labelled by the delivered rule for a curve, its point count: `Tone curve 3 points`. A patch that returns the field to its default is `Reset Tone curve`, and so is `reset-curve`. A recipe row reads `Tone curve 3 points` or `Neutral`.
- **Neutrality** is the framework's rule: the layer is neutral exactly when `luminance` is canonically its default two points. A person who double-clicks the drawn diagonal adds a point exactly on it (the desktop sends the pointer's own fraction, so a press on the line's pixels gives `x == y`, and it commits an add at once rather than drafting it), and a geometric rule would make that add a no-op whose point vanishes on the next refresh. So an on-diagonal three-point curve is a stored, described, labelled layer. `compile` still maps a curve that is the exact identity map (first point `[0, 0]`, last point `[1, 1]`, every point on the diagonal) to a colour operation with no units, so such a layer keeps the identity byte path and shares the source allocation, as the controls proof's layer does. Any other curve compiles to exactly one unit.
- **Points are a function of the encoded tone.** The plot's `x` axis is the sRGB-encoded luminance the histogram is drawn in, so the histogram background and the curve share one axis by construction, and `y` is the encoded output luminance. This is the same domain the Basic tone controls act in; the curve's `x = 0.5` is Basic's contrast pivot.

## Placement

The delivered host places a new layer by its effect's stage and declared order and never moves an existing layer ([registry](modules-and-api.md#registry)). Basic declares order `0` and the mixer `10` in the colour stage; the curve declares **order `5`**, so a curve layer always lands after the Basic layer and before the mixer layer whichever was touched first, and a stack stored in another order renders in its stored order. Within a mask the same rule places the masked curve layer after the masked Basic layer and before the masked mixer layer, and masked curve layers follow the global one in mask-list order ([mask order](masking.md#order)).

Why after Basic: the curve finishes the global tone shaping Basic starts, in the same encoded domain, so the composition is one monotone luminance map a person reads from the plot. Why before the mixer: the mixer addresses the colours of the toned photograph, and its order-10 place is kept. Why not after the mixer: a luminance curve after Oklab hue and chroma changes would still be hue-preserving, but the panel would then read top to bottom in an order the pipeline did not follow. The neutral picker samples the stage the Basic layer receives, so the curve never sits in front of it.

Canonical stack for a fully edited photograph: source (RAW only) → pixel proof → Basic → **Tone curve** → mixer → presence → orientation → crop → vignette.

## Processing contract

The unit is one `PointwiseColor` in the layer's colour operation, evaluated in the existing run with no new quantization boundary and the run's single clamp at the end: values outside `[0, 1]` and negative values are preserved, nothing is clamped inside the unit, alpha is never touched, and a non-finite value fails the render or sample as every colour unit's does. Per pixel, in the delivered vocabulary:

```text
L      = 0.2126*R + 0.7152*G + 0.0722*B          Rec. 709 luminance on linear sRGB
x      = encode(L)                               the sRGB OETF, analytically continued (basic-tone.md)
y      = C(x)                                    the curve, below
L_out  = decode(y)                               the inverse, continued the same way
rgb_out = reconstruct(rgb, L, L_out)             the luminance ratio, additive below |L| < 1e-6
```

`encode`, `decode`, the luminance weights and `reconstruct` are the Basic tone contract's, reused from `crates/luxforge-core/src/colour.rs` unchanged, so hue and every cross ratio between channels are preserved exactly: a grey stays grey with three bit-identical channels, and a pixel's Oklab hue is invariant under the ratio rule because a uniform scale of RGB scales `a` and `b` equally. Relative saturation (chroma over luminance) is preserved; absolute chroma follows luminance. This is the one way the composite differs from a per-channel curve, which colours greys and shifts hues; the difference is stated to people in the user guide and recorded as a [proposal](#proposals-with-recorded-defaults).

### The curve `C`

Given the checked points `(x_0, y_0) … (x_{n-1}, y_{n-1})`, `n` in `2..=16`, `x` strictly increasing, `y` non-decreasing:

1. **On the span `[x_0, x_{n-1}]`: a monotone piecewise-cubic Hermite interpolant** (PCHIP, Fritsch–Carlson monotonicity with Fritsch–Butland weighted-harmonic-mean knot slopes), the construction the [mixer's hue warp](mixer-study.md#the-warp) already froze, here open rather than periodic. With `h_i = x_{i+1} - x_i` and secants `Δ_i = (y_{i+1} - y_i) / h_i`:
   - interior knot slope `d_i = (w_1 + w_2) / (w_1 / Δ_{i-1} + w_2 / Δ_i)` with `w_1 = 2 h_i + h_{i-1}` and `w_2 = h_i + 2 h_{i-1}`, and `d_i = 0` when either adjacent secant is `0`;
   - end slopes `d_0 = Δ_0` and `d_{n-1} = Δ_{n-2}`;
   - on segment `i`, with `t = (x - x_i) / h_i`: `C(x) = y_i h00(t) + h_i d_i h10(t) + y_{i+1} h01(t) + h_i d_{i+1} h11(t)`, the cubic Hermite basis, and a segment whose two `y` are equal is exactly constant by a direct rule rather than by evaluating the basis.
2. **Inside `[0, 1]` but outside the span: held flat** at `y_0` below `x_0` and at `y_{n-1}` above `x_{n-1}`, the rule the controls proof states and the one a moved endpoint means in a curve editor (an endpoint dragged inward clips the tones beyond it).
3. **Outside `[0, 1]`: a unit-slope extension**, `C(x) = C(0) + x` for `x < 0` and `C(x) = C(1) + (x - 1)` for `x > 1`. Exposure and white balance run before this unit and routinely push encoded luminance past `1` on the JPEG path, and scene-linear RAW planes carry values above white until the terminal boundary. Holding flat there would collapse every over-white value to one output, which the mixer and Presence downstream would then see as a plateau; a tangent extension would amplify them by the end segment's slope, which the tone study measured and rejected for its own family. Slope one keeps them finite, keeps their ordering, keeps them apart from each other by exactly the distance they had, and is continuous with the curve at the boundary. For the identity curve it is the identity.

Properties, each proved by the study and then by production tests rather than asserted:

- **Interpolation.** `C(x_i) = y_i` at every knot.
- **Monotone.** `C` is non-decreasing on the whole real line for every admissible point list: on each segment both Hermite slopes lie in `[0, 3 · min(Δ_{i-1}, Δ_i)]` (the harmonic mean is at most three times the smaller secant, the end slopes are exactly one secant), which is inside the Fritsch–Carlson monotone region; the flat holds and the unit-slope tails are monotone by inspection; and the pieces meet continuously. A non-decreasing curve on a non-decreasing luminance never inverts two tones, so the composition with Basic's monotone curve is monotone by the chain rule. Strict positivity is not claimed: a person may draw a flat segment, and it is exactly flat.
- **No overshoot.** On segment `i`, `C(x)` lies within `[y_i, y_{i+1}]`, so a curve never leaves `[0, 1]` for an input in `[0, 1]`, whatever the points.
- **Smooth.** `C` is C1 across every knot (the two segments share `d_i`), which is what keeps a gradient from showing a crease at a point; it is only C0 at the span's ends where it meets a flat hold, and at `0` and `1` where it meets the tails, and that is stated.
- **Identity.** Points all on the diagonal with the span covering `[0, 1]` give slopes exactly `1` and `C(x) = x` to within f64 rounding; production compiles that case to no unit at all.
- **Two points are an affine map.** `[[0, a], [1, b]]` is `C(x) = a + (b - a) x`: the black and white points, the same remap Basic's Whites and Blacks perform, so an agent can post one without a slider.
- **Finite.** Every finite input and every admissible point list gives a finite `C(x)` (a composition of `+`, `-`, `*` and one division by a strictly positive `h_i` or by a strictly positive harmonic-mean denominator), and the sRGB encode and decode are finite on the tested `[-0.5, 2.0]` extended domain as the tone study proved.

Why this interpolant and not another, measured by the study on the same point sets (an S-curve, a lifted black, a cut white, a mid-tone bump, a 16-point wiggle and a set with a flat pair) over a 4001-point grid and an 8-bit ramp: a natural cubic spline and a Catmull–Rom spline overshoot between points and can leave `[0, 1]` or invert tones, so a person could not trust that the plot's monotone points give a monotone map; linear interpolation is monotone but only C0, and its slope steps at every knot show as creases on a smooth gradient; Akima is not guaranteed monotone. PCHIP is the smallest construction that is shape-preserving, local (moving one point changes at most the two segments either side plus their neighbours' slopes) and already frozen in this repository. Its known cost is that its slope at a knot is bounded by the smaller neighbouring secant, so a curve rises a little less steeply into a sharp corner than a free spline would draw; that is the trade for never overshooting.

### Cost

Per pixel, in `f32` with coefficients computed once in `f64` and cast: the luminance, one `encode`, a segment search over at most sixteen knots (four comparisons), one cubic in `t` by Horner, one `decode` and three multiplies. That is the Basic tone unit's own work minus its `exp`, so one curve unit is expected to cost less than the composed tone stages measured at about 48 ms at 24 MP and 127 ms at 60 MP ([performance](../specs/performance.md#isolated-rendering-kernels)); the plan measures it rather than promising it. No lookup table: a 4096-entry table with linear interpolation can miss a steep segment's cubic by more than the frozen tolerance, and the direct cubic is already cheap. The unit runs in the existing colour run, so it takes the colour pass's measured parallel threshold, the row-chunk scratch budget and the masked-run blend as delivered; it adds no pass kind, no allocation that scales with the image, no timer and no hop on the input path.

### Sample query and point queries

`query.sample-curve {asset_id, entry_id?, luminance}` returns `{points: [[x, C(x)] × 257]}` at `x = i / 256`, computed in `f64` by the same construction production casts its coefficients from, so the plot shows the curve the pixels receive. It runs on the catalog owner, allocates nothing and touches no pixel; it is answered for the entry the desktop shows, and only while the curve is on screen, as the delivered sampling rule says.

`render.sample`, the neutral picker and every other point query keep their `O(layers)` contract: the unit is pointwise, and no-op detection and validation compare payloads, never pixels.

## Frozen tolerance

Production versus the `f64` reference: **`1e-5 + 1e-5 × |reference|`** in linear float and at most one output code after quantization, the Basic tone tolerance, because the unit composes the same `f32` `encode` and `decode` around one cubic whose coefficients are exact to `f32` casting. The study records the measured largest deviation over its fixture, as the tone and mixer studies did.

## Controls and interaction

Every gesture is the delivered curve editor's, with the two owner changes marked: a point drag drafts through `draft.begin`, `draft.set` and `draft.commit`, previews each accepted value with the proxy following motion and the exact phase and histogram settling after the shared quiet policy or release, and commits once on release; Escape cancels; arrow keys nudge the selected point by `0.01`, Shift by ten times and Option by a tenth; double-click on the plot away from a point adds a point at the pointer (refused past sixteen); **double-click on a point removes it** (refused below two), the same request Delete sends for the selected point; right-click copies the exact `edit.set-curve` request. The first press of a double-click on a point selects it and moves nothing, so it opens no draft. Adding and removing commit at once, as the desktop already does for every curve.

**The point list is closed until opened.** Under the plot a one-line hint names the double-click gestures, then a **Points** disclosure row carrying the count (`4 of 16`); the numeric list of input and output fields appears only after the person opens it, and Enter in a field then commits a typed pair. Open or closed is view state like the channel switch: it changes no recipe, sends no request and stays as the person left it while the window is open. The drag's draft bar and the status bar carry the moving point's values, so a closed list hides nothing a gesture needs; an agent posts the same points through `edit.set-curve` either way. The histogram drawn behind the plot is the inspector's current one; it is a hint and edits nothing. Original and Custom on the section follow the delivered rule: Original when the points equal the default.

## API

Generated from the descriptor and listed by `module.list` and `schema.list`: `edit.set-curve {asset_id, luminance: [[x, y], …], mask?}`, `edit.reset-curve {asset_id, mask?}`, `query.sample-curve`, every field-patch rule (the patch stored as sent, deduplication by request id, no entry for a patch that changes nothing) and `draft.begin` over `set-curve`. `recipe.describe` reports the layer's points as its `values`. The [built-in descriptor snapshot](../engineering/development.md#the-built-in-descriptor-snapshot) changes with registration and is regenerated and reviewed. UI and API parity is proved by the [field-patch conformance chapter](../engineering/development.md#the-field-patch-conformance-chapter), which finds the module from its descriptor once it is added to the modules the suite recognises.

## Masks

The effect declares `maskable`, which is the whole of what a module does to inherit masking: the host adds the `mask` target to both actions, places masked curve layers after the global one in mask-list order, blends the masked run against its own input in linear light before the run's clamp, keeps `render.sample` equal to the rendered byte through it, and shows the Masks panel's scope chip on the section. What the plan verifies is that a masked curve behaves as the masked Basic layer does — one layer per mask target, two refused by name, the drag through a mask target, the coverage overlay reading the curve layer's input — and that the [masking design](masking.md#how-a-mask-reaches-an-effect) lists the curve among the maskable effects.

## Presets

`set-curve` is presettable the day it registers, because every field-patch action is: a settings set may carry `{"set-curve": {"luminance": [[…]]}}`, capture reads the layer's points, apply overwrites them, and the create form offers a Tone curve group. Lightroom import maps the composite point curve and nothing else:

| Lightroom setting | Luxforge target | Rule |
| --- | --- | --- |
| `ToneCurvePV2012` | `set-curve.luminance` | A value transfer: each `"x, y"` item on the 0–255 scale becomes `[x / 255, y / 255]`, in order. **Refused** with its reason when the list holds more than sixteen points, fewer than two, an `x` that does not strictly increase or a `y` that decreases; never decimated, clamped or reordered. An identity list is neutral. Like every mapped control this carries the value: Lightroom applies its composite curve to each channel and Luxforge applies it to luminance, so the tonal intent transfers and the colour rendering is Luxforge's own |
| `ToneCurveName2012` | none | Neutral: it names the points the list already carries |
| `ToneCurvePV2012Red`, `Green`, `Blue` | none | Unsupported, "Luxforge's tone curve has no per-channel curves"; neutral when identity |
| `ParametricShadows`, `Darks`, `Lights`, `Highlights` and the three splits | none | Unsupported, "Luxforge has no parametric curve"; neutral at their defaults |
| `EnableToneCurve` | panel switch | As delivered: when false, the mapped curve is refused as disabled in the preset |

The legacy `ToneCurve` and `ToneCurveName` stay refused in an earlier-process preset. The import report and the user guide's list of what does not carry over change with this.

## History

Each committed drag, add, remove, typed pair and reset is one entry with the labels above; undo, redo, a read-only preview and restore return the stack of the entry they name and re-render byte for byte; the stored parameters are the patch as sent (at most sixteen pairs); a preset that includes the curve is one `Preset: <name>` entry. Nothing new is asked of the history model.

## Resource and responsiveness constraints

The existing limits hold unchanged; the unit adds no allocation, queue, timer or subscription. New bounded quantities: sixteen points per curve (the kind's own maximum is thirty-two; sixteen is enough for any tone shape a person draws by hand and keeps the per-pixel search to four comparisons), 257 samples per query answer. A point drag is measured on the recorded M4 configuration in release builds at 24 MP and 60 MP with a warm source cache and at least 30 inputs, separately for input-to-presented-frame at Fit, the settled histogram, and the unit's own frame cost, against the [slider budget](../specs/performance.md#provisional-budgets) (16 ms p95, acceptable below 32 ms, at warm 24 MP) and the settled-histogram budget (200 ms p95). A miss is reported with its figures and blocks nothing; no cache, table or timer is added to claim a pass. The delivered `editor-latency --control curve` drives the developer proof's curve; measuring the Tone curve needs it to drive a named module's curve, which is part of the measurement task. Every implementation handoff answers the [performance checklist](../engineering/performance-rules.md#review-checklist).

## Verification and acceptance

1. **Frozen numerics before production.** An `f64` reference `crates/luxforge-reference/src/curve.rs` written from this document, its study `tests/studies/curve.rs` proving interpolation, monotonicity by dense forward differences over the extended domain for the named point sets and for seeded random admissible lists, no overshoot, C1 continuity, identity, the affine two-point case, exact flat segments, finiteness, hue and grey invariance of the reconstruction, and the measured comparison against linear, natural-cubic and Catmull–Rom candidates; a committed oracle `fixtures/curve/curve-cases.json` with a README, generated by the reference and reloaded on every run; an `#[ignore]` visual review that writes ramp and patch sweeps to a temporary directory; and this document's numbers filled from what the study prints.
2. **Exactness and bounds.** Production matches the reference within the frozen tolerance on every fixture case directly and through `render` on the byte and RAW linear paths; the neutral payload and the exact identity map keep the identity byte path and share the source buffer; every other admissible curve compiles to one unit; an over-white linear pixel passes through the unit-slope tail; the descriptor's limits refuse a seventeenth point, a decreasing `y`, a repeated `x` and a coordinate outside `0..=1` by name.
3. **Conformance and placement**, through the JSON method table: the module joins the field-patch conformance suite (discovery, drafts, no-ops, deduplication, resets, one layer per target and per mask, history, undo, redo, restore, sample equal to render on both paths through a straightened crop, an unavailable provider, reopen) in `cargo test` and in release inside `editor-acceptance`; its own chapter proves the curve after Basic and before the mixer in every touch order with identical bytes, masked curve layers after the global one in mask order, and `query.sample-curve` equal to the compiled unit's curve at every sample.
4. **Native M4 rendered evidence** from a background `curve` smoke scenario over a generated fixture holding an encoded grey ramp beside the four quadrant colours: expand, drag a mid-tone point with drafted frames, release, add a point, type an S-curve, reset; inspected for a monotone grey ramp with no inversion, greys that stay grey, quadrant hues unchanged, the lower half darkened and the upper lightened under the S-curve, no crease at the points, and the editor's plot with the histogram behind it, all with correlated state, requests and logs.
5. **Measurements** recorded in [performance](../specs/performance.md) with their scope, and the docs updated for demonstrated behaviour only: [feature status](../features.md), the [user guide](../user-guide.md), the [tool array](develop-workspace.md#tool-array), the [module contract](modules-and-api.md) descriptor paragraphs and registry list, the [presets mapping](presets.md#mapping), the [masking](masking.md#how-a-mask-reaches-an-effect) maskable list, the [architecture](architecture.md) crate and module lists and the [decisions](../decisions.md#ui-components) note that the curve vocabulary has its consumer.

## Limitations

- **Global and pointwise.** Two tones map the same way wherever they are; the curve cannot lift a backlit subject without lifting the sky's equal tones, exactly the [Basic tone limitation](basic-tone.md#limitations).
- **Composite only.** No per-channel colour work: an S-curve does not increase saturation as a per-channel curve would, and a colour cast cannot be corrected here. Hue and relative saturation are preserved instead, which is stated as a property rather than hidden.
- **Monotone only.** The host refuses a decreasing `y`, so solarization and other inversions are not expressible; a flat segment is expressible and discards the tones it covers.
- **Bounded by the points.** Sixteen points; a Lightroom curve with more is refused at import rather than decimated.
- **No parametric mode.** Lightroom's region sliders are not built; Basic's Highlights, Shadows, Whites and Blacks already cover that shape of adjustment.

## Proposals with recorded defaults

Recommendations for the owner, recorded as proposals until decided. The plan runs on the defaults.

| Question | Default the plan runs on | Alternatives |
| --- | --- | --- |
| What the composite channel acts on | Encoded luminance with the luminance-ratio reconstruction: hue-preserving, one axis shared with the histogram, the frozen Basic vocabulary reused | Lightroom's per-channel composite, which applies one curve to R, G and B and so saturates on an S-curve; or both, as two channels of the one control |
| Channels | One, `luminance` | Red, Green and Blue curves as further channels of the same control, evaluated after the composite one, under their own study and per-channel arithmetic |
| Order in the colour run | `5`: after Basic, before the mixer | After the mixer (order above 10) |
| Section place | After Basic, before Presence, collapsed | After the mixer |
| Endpoints | Free: `x` and `y` of every point move, with the curve held flat inside `[0, 1]` beyond the span | Endpoints fixed at `x = 0` and `x = 1` with only their `y` movable, which needs a validation hook the field-patch framework does not have |
| Behaviour past white and below black | The unit-slope tail | Flat (collapses over-white values) or tangent (amplifies them) |
| Point limit | 16 | The kind's maximum, 32 |
| Neutrality | The framework rule (points equal the default), with the exact identity map compiled to nothing | A geometric rule (any identity map is neutral), which makes a point added on the diagonal a no-op that vanishes |
| Lightroom `ToneCurvePV2012` | Transferred onto the luminance curve as a value, refused above sixteen points or when not monotone | Refused outright as a different rendering |
| Interpolant | Open PCHIP with harmonic-mean slopes and secant end slopes | Linear (creases), a free cubic spline (overshoots), or PCHIP with three-point end slopes |

## References

- Repository contracts: [modules and API](modules-and-api.md), [UI components](ui-components.md), [Basic and histogram](basic-and-histogram.md), [Basic tone](basic-tone.md), [mixer study](mixer-study.md) (the frozen PCHIP construction), [masking](masking.md), [presets](presets.md), [Develop workspace](develop-workspace.md), [architecture](architecture.md), [performance rules](../engineering/performance-rules.md), [development](../engineering/development.md).
- Local research, context only: [Lightroom tone and colour](../research/lightroom/tone-and-color-tools.md#contrast-and-curves) (a point curve's interpolation matters; per-channel curves shift hue), [Lightroom presets](../research/lightroom/presets.md) (`ToneCurvePV2012` as an `rdf:Seq` of `"x, y"` items on a 0–255 scale), [darktable tone and colour](../research/darktable/tone-and-color-tools.md).
- F. N. Fritsch and R. E. Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J. Numer. Anal. 17(2), 1980; F. N. Fritsch and J. Butland, "A method for constructing local monotone piecewise cubic interpolants", SIAM J. Sci. Stat. Comput. 5(2), 1984.
