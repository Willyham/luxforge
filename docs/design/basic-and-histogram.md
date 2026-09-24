# Basic adjustments and histogram

Status: implemented and verified on the M4 Mac. The owner's product choices ([decisions](../decisions.md#basic-adjustments-and-histogram), 2026-09-21) and the [integration contract](#integration-contract) below govern the delivered behaviour; the [tone](basic-tone.md), [white balance](basic-white-balance.md) and [colour](basic-colour.md) studies freeze the equations; [verification and acceptance](#verification-and-acceptance) states what is demonstrated and [performance](../specs/performance.md) records the measurements against the provisional targets. It builds on the M3 modules, the M4 crop drafts and conflicts, content-space edits, the orientation layer and the [Develop workspace](develop-workspace.md).

## Outcome and delivery order

Make the current single-image editor useful for everyday brightness, tone and color corrections, with immediate, trustworthy feedback about the result. Use the supplied Lightroom screenshot as a familiarity reference, not as an instruction to reproduce every visible feature or Adobe's processing.

Decided: start with the supported SDR sRGB/greyscale JPEG subset. Linearizing a rendered JPEG makes exposure arithmetic meaningful but does not turn it into scene-linear RAW data. RAW is separate later work; PNG, broad ICC conversion and calibrated display output keep their existing separate scope.

| Slice | User-visible result | Main dependency |
| --- | --- | --- |
| A — inspect and expose | RGB histogram, output clipping indicators/overlays, pixel readout, Exposure slider with live preview and one undo step per gesture | Histogram can use today's raster; exposure needs decimal controls, shared drafts and pointwise float processing |
| B — complete the core Basic controls | Contrast, Highlights, Shadows, Whites, Blacks; Temperature, Tint and neutral picker; Vibrance and Saturation | Selected numerical contracts and the Slice A processing/editing foundation |
| C — candidate next modules | Tone Curve, then Detail and local presence tools | Separate designs and owner priority; not implementation tasks in this plan |

Slice A is independently demonstrable before all of Slice B. It does not wait for export, Locate, MCP, RAW or a library. Final mixed-stack acceptance does require the actual M4 crop implementation. No milestone numbers are assigned here.

## Module boundaries

Two linked built-ins, with lazy resources:

| Component | Owns | Does not own |
| --- | --- | --- |
| Basic adjustment tool | Parameters, validation, neutral/no-op rules, declarative controls and compilation of White Balance, Tone and Color processing | History transactions, source decode, job scheduling or catalog writes |
| Histogram inspector | A deterministic RGB reduction, clipping predicates, inspectable result schema and histogram presentation | A recipe effect or an undo entry |
| Shared host additions | Decimal parameter/slider semantics, revision-bound drafts, updating an identified layer, bounded pointwise color execution and analysis jobs | Tool-specific equations or bespoke Basic-only transaction logic |

Decided: Basic is one editable recipe effect with internal algorithm units for white balance, exposure, tone and color. This avoids making the result depend on the order in which a person first touches individual Basic sliders. It does not require a new crate or external module loader. The histogram uses the common operation registry and a small analysis interface; do not invent a no-op image layer to fit it into today's effect-only interface.

Internal order: **White Balance → Exposure → tonal curve → Vibrance → Saturation**. Under the accepted [content-space edits](content-space-edits.md) rule the host places a new layer by its effect stage, so a color-stage Basic layer joins the stack before the geometry tail like a pixel layer; it then stays at that saved position. First non-neutral use inserts it; later adjustments replace that layer's payload at the same ID and position in a new immutable snapshot. Never move an existing layer or a pixel replacement. Initially create at most one Basic layer per recipe; if an unsupported/imported stack contains multiple candidates, report ambiguity rather than silently choosing one. More than one Basic instance is never user-facing in this plan.

Each slice persists only its implemented state and supports only the current Basic payload and algorithm. Breaking changes may replace that shape; update the internal effect format when its interpretation changes and reject unsupported formats explicitly. Do not retain earlier evaluators, conversions or dormant controls.

## Relationship to the delivered core and workspace

The checkout now delivers what this design first listed as prerequisites, so the integration work is smaller than first planned:

| Delivered | Where | What Basic reuses |
| --- | --- | --- |
| `number` parameters with finite ranges, units and defaults; generic range validation | M4 descriptors | Every Basic parameter is a `number`; step and display precision are the one descriptor addition still needed |
| `ActionPlan::Update` replacing an identified layer in place in a new immutable snapshot | M4 host | The Basic field patch updates the one Basic layer at its saved identity and position |
| Layers placed by effect stage before the geometry tail | [Content-space edits](content-space-edits.md) | A colour-stage Basic layer joins the stack before quarter-turns and crop and stays there |
| The resample stage boundary and one-pass exact segments | M4 renderer | The pointwise colour stage is a new bounded operation inside a segment, not a second pipeline |
| Draft conflict rules: an external commit keeps the draft and marks it conflicted until Discard or Reapply | M4 crop draft, desktop-local | The semantics to reuse; the crop draft itself lives in the desktop and is not the core draft lifecycle Basic needs |
| Generated tools panel: sliders that commit once on release, drag sends nothing, group and module resets, section hints, history labels from `summary` templates, recipe summaries, the Developer section | [Develop workspace](develop-workspace.md) | A Basic descriptor with sliders and grouped resets renders without desktop changes; the histogram sits above the modules in the tools panel, the clipping toggle joins the title bar, and the neutral picker is a declared `picker` control in the White balance group |

What is still missing from the core: the revision-bound draft lifecycle for slider gestures (begin, set, read, cancel, commit, reapply through the API, at most one preview request per frame while dragging), the bounded pointwise float colour operation and its point sampler, the analysis job for the histogram, and the field-patch action. Shared files likely to need coordinated integration are `modules/descriptor.rs`, `modules/processing.rs`, `modules/mod.rs`, `render.rs`, `preview.rs`, `editor.rs`, `api/methods.rs` and the desktop's `app/`, `state/` and `view/` layers under `crates/`. Keep algorithm and reducer implementations in dedicated files. One integrator reconciles the shared host changes; independent algorithm and reducer tasks run alongside.

## Integration contract

Frozen on 2026-09-21 against the delivered core and workspace before any core code was written. Every Basic and histogram task builds against this section; a change to it is a design edit, not an implementation detail. Names are chosen against the delivered method table and are the names the code uses.

### Module, effect and payload

- Module `lightwell.basic`, title `Basic`, hint `Exposure, tone, white balance and colour`, not a developer module. It registers in `ModuleRegistry::builtin()` after the pixel module and before transform, so its section is the first photo-editing section of the tools panel.
- One effect `lightwell.basic.adjust`, format `1`, stage `color`. `EffectStage` gains the `color` variant; the host places a colour-stage layer exactly like a pixel-stage one (`insertion_index` returns the index of the first geometry layer), so the Basic layer joins the stack before quarter-turns, reflections and the crop and never moves afterwards.
- Payload: a JSON object whose keys are the implemented parameter names with their values as stored numbers. A missing key means neutral (`0`), an unknown key is refused by `validate_payload`, and a value outside the parameter's declared range is refused. Slice A persists only `exposure`; Slice B adds `contrast`, `highlights`, `shadows`, `whites`, `blacks`, `temperature`, `tint`, `vibrance` and `saturation` as further optional keys of the same format, because adding a neutral-defaulting key changes no existing interpretation. Changing the meaning or range of an existing key bumps the format and older payloads are refused, never rewritten.
- A neutral payload (every key neutral or absent) is a legal layer that compiles to no processing. Resetting keeps the layer with its identity, as crop reset does.
- Internal evaluation order inside the one layer: white balance → exposure → tone (contrast, highlights, shadows, whites, blacks as one frozen curve) → vibrance → saturation. The module compiles a neutral unit to nothing.
- At most one Basic layer per recipe is created. Planning against a stack holding more than one `lightwell.basic.adjust` layer fails with `validation: ambiguous Basic layers`, and so does rendering it; nothing is rewritten.

### Actions and labels

| Action | Parameters | Plan |
| --- | --- | --- |
| `set-basic` | Every implemented Basic parameter, all optional (`required: false`, `default: 0`), `number` kind with finite range, `unit`, `step` and `precision` | Merge the given fields over the existing Basic layer's payload, or over neutral when none exists. Equal merged payload: `NoOp`. No layer and neutral result: `NoOp`. No layer: `Commit` a new layer at the colour insertion index. Otherwise `Update` the existing layer in place |
| `reset-basic` | none | `Update` the existing layer with the neutral payload; `NoOp` without one or when already neutral |

`set-basic` is the only mutation path for every slider, numeric field, keyboard step, double-click reset, group reset, module reset, picker result, draft commit and API patch. Group resets are `set-basic` presets holding the group's fields at neutral; the module reset is `reset-basic`. There is no per-field action and no second transaction path.

Field-patch semantics: `ActionDescriptor` gains `patch: bool` (default `false`). For a patch action the generic parameter check validates the fields that were sent and fills no declared defaults, so the module receives exactly the fields the caller named; omitted fields are preserved by the module's merge, unknown fields are rejected by the generic check, the stored history parameters are the patch as sent (not the merged payload), request deduplication covers the patch as sent, and a patch that changes nothing creates no entry. A retry of the same request id returns the original result. Defaults stay declared on patch parameters because clients seed and reset fields from them.

History labels: `ToolModule::label(&self, input: &ActionInput) -> Option<String>` is a new trait method with a default of `None`, consulted by the host before the `summary` template and the title. Basic labels one changed field as `Exposure +0.50 EV` (sign always shown, `precision` decimals, unit when declared), a patch that returns one group's fields to neutral as `Reset Tone`, `Reset White balance` or `Reset Colour`, `reset-basic` as `Reset Basic`, and any other patch as `Basic (n fields)`.

Descriptor additions: `ParameterDescriptor` gains `step: Option<f64>` (the keyboard and slider increment) and `precision: Option<u8>` (display decimals). Both are hints for clients; the host validates finiteness and range only and stores what it was given. Registry validation rejects a non-positive or non-finite step and a precision above 6.

Generic panel rule (the one desktop change the field patch needs, applied to every module): a generated control of a patch action submits its own parameter only, and a reset or action control of a patch action submits its declared preset only. Controls of non-patch actions keep sending every parameter as today, so crop and pixel requests are unchanged; a Basic slider sends exactly one field.

Effective values: `ToolModule::values(&self, effect_id, format, payload) -> Result<Map<String, Value>, Error>` (default: empty) returns the parameter values a stored layer represents. `recipe.describe` adds a `values` object to each `LayerDescription`. The desktop seeds each generated field from the displayed entry's values for the one layer of that module when a field is not being edited or dragged, so sliders show authoritative current or historical values; a module without a layer shows its defaults.

### Draft lifecycle

The core owns one draft per client session, held in `ClientSession.draft` and reported by `session.state`. A draft is bound to one asset and one action and never outlives the session.

| Method | Params | Result |
| --- | --- | --- |
| `draft.begin` | `asset_id`, `action` | `{draft_id, action, asset_id, base_revision, draft_revision: 0, fields: {}, conflicted: false}`. Refused with `conflict` when the client already holds a draft, with `validation` when the session previews a historical entry or the action is unknown |
| `draft.set` | `draft_id`, `fields` | Validates every field against the action's parameter descriptors (unknown field, non-finite or out-of-range value: `validation`, nothing changes), merges them into the draft, increments `draft_revision`, returns the draft |
| `draft.read` | `draft_id` | The draft with `conflicted` recomputed |
| `draft.cancel` | `draft_id` | Ends the draft; commits nothing; returns `{cancelled: true}` |
| `draft.commit` | `draft_id`, `mutation` | Refused with `conflict` when conflicted or when `mutation.expected_revision` differs from `base_revision`; otherwise runs the action with the draft's fields through `apply_action` and ends the draft. Returns the `MutationResult`, so a return-to-start gesture is a `no-op` outcome with no entry. A successful commit ends the draft; a retry of the same request id goes through `edit.<action>` with the same fields and is deduplicated there |
| `draft.reapply` | `draft_id` | Sets `base_revision` to the asset's current revision, keeps only the fields the client set, revalidates them against the action's descriptors, clears `conflicted`, returns the draft |

`conflicted` is `base_revision != current revision` of the asset, evaluated whenever the draft is read, set, committed or reported, so a commit by any client, including this client's own undo, redo or restore, marks the draft without a notification path. Draft state is session state: it emits no event and appears in no history. Two drafts of different clients never interact. A draft's effective recipe is the current snapshot with the action's plan applied to the draft's fields, computed on demand and never persisted; a `NoOp` plan means the current recipe.

Preview during a gesture: `OwnerHandle::preview_job` accepts a `draft: Option<DraftId>` beside `layer_count`; the job renders the draft's effective recipe and carries `draft_revision` for correlation. `render.sample` accepts an optional `draft_id` and samples the same effective recipe. The desktop bound is one round trip in flight: a pointer move sends `draft.set` and the one preview job for the value it accepted the moment nothing is in flight, synchronously on the desktop thread ([instant previews](instant-preview.md#one-frame-per-hop)), and only records the newest value while one is; the answer sends that newest value. There is no tick and no timer. At Fit the job's proxy phase is what the drag shows and its exact phase is what the histogram reads. Release, key-up or Enter sends `draft.commit`, Escape and focus loss send `draft.cancel`, and an external revision shows the existing Changed elsewhere notice with Discard (`draft.cancel`) and Reapply (`draft.reapply`) while the slider keeps the drafted value. Leaving the mode or starting Compare with a draft open is refused as it is for crop. The crop draft stays desktop-local; only its conflict semantics are shared.

### Pointwise colour processing

- `Processing` gains `Color(ColorOperation)`. A `ColorOperation` is a bounded ordered list (at most 8) of `Arc<dyn PointwiseColor>` units, each with `fn apply_row(&self, rgb: &mut [[f32; 3]])` over linear-sRGB rows and `fn is_finite(&self) -> bool` over its coefficients. The host owns the pipeline; a module owns its units' equations. `Processing` loses `Copy` and compares operations by their unit descriptions.
- Domain: input codes decode through the sRGB transfer function into f32 linear sRGB (D65) using a 256-entry table computed in f64. Units run in declared order in f32 with coefficients computed in f64. Values outside `[0, 1]` and negative values are preserved between units of one operation and between consecutive colour operations in the same segment. Alpha is never touched.
- Output boundary: at the end of a run of consecutive colour operations the host clamps each channel to `[0, 1]` and quantizes to the code `k` whose exact linear threshold interval contains the value, using 255 thresholds `t_k = decode((k − 0.5) / 255)` precomputed in f64; this equals `floor(255 · encode(v) + 0.5)` for every representable value and costs no per-pixel power function. A point replacement, a resample and the end of the recipe are the quantization boundaries and are explicit in the compiled segment. A non-finite value after any unit fails the render or sample with `resource-limit: colour processing produced a non-finite value`, never a NaN in a frame.
- Segment placement: colour operations join the segment's operation list in stack order. Exact geometry commutes with pointwise colour and composes as today; a point replacement earlier in the list is processed by later colour operations and one later in the list is not; a resample quantizes before it interpolates. The rasterizing pass keeps one u8 frame per segment: geometry pass, then the operation list in order as phases of replacements and colour runs, each colour run streamed over the frame in place in bounded row chunks on the shared Rayon pool above the existing one-megapixel threshold. `Evaluation::pixel_in` applies the same phases to one pixel, so samples and rasters agree byte for byte.
- Buffers: an identity segment with a colour operation materializes its frame with the existing `has_pixels` copy, bounded by the 512 MiB frame limit; peak memory stays two frames. Row-chunk scratch is reserved from a process-wide `ScratchBudget` (a 64 MiB target that never refuses a chunk, with a high-water mark) before allocation and released after each chunk. Nothing else scales with image size.
- A recipe with only neutral Basic layers compiles to no colour operation, keeps the identity byte path and shares the source buffer.

### Analysis jobs and identity

- `analysis::reduce(raster) -> Report` is a pure function in `lightwell-core`: three `[u64; 256]` arrays and the endpoint counters `r0 g0 b0 r255 g255 b255 any_shadow any_highlight all_shadow all_highlight both`, reduced serially below one megapixel and on the Rayon pool above it with worker-local bins merged by addition. `analysis::clip_class(rgba) -> Option<Clip>` (`Shadow`, `Highlight`, `Both`) is the one predicate the counters, the overlays and the API share: any channel at code 0 is shadow, any at 255 is highlight.
- Identity: `{asset_id, source_fingerprint, entry_id, snapshot_id, recipe_hash, draft: Option<{draft_id, draft_revision}>, width, height, domain: "srgb-8bit-output"}`. `recipe_hash` is the SHA-256 of the effective recipe's canonical JSON. A report is bounded to 16 KiB before encoding.
- Methods: `analysis.request {asset_id, target}` with `target` one of `{"kind": "current"}`, `{"kind": "entry", "entry_id"}` or `{"kind": "draft", "draft_id"}` returns `{job_id, status, identity}` promptly; `analysis.read {job_id}` returns `{status, identity, report?, error?}` with `status` one of `pending`, `ready`, `failed`, `superseded`, `cancelled`; `analysis.cancel {job_id}` drops this client's interest and cancels the job only when no other client holds it. A pending, failed or superseded result carries no counts.
- Scheduling: the owner loop hands jobs to one analysis worker with one active and one replaceable pending slot; replacing the pending job marks its requesters `superseded`. The worker renders the effective recipe from the cached verified source, reduces, retains no raster and posts the report back to the owner. Identical identities share one job; completed reports are kept in an eight-entry store evicted oldest first; a client's disconnect releases its interests.
- The desktop reuses its preview evaluation: `PreviewJob.analyse` makes the preview worker reduce the raster it just rendered and return the report with the raster, and the desktop submits that report to the owner store through `OwnerHandle::submit_analysis` so an API request for the same identity is a cache hit. No second render happens for a displayed target.
- Overlay settings are per-client workspace state: `workspace.set` accepts `clip_shadows` and `clip_highlights` booleans and `session.state` reports them. The display overlay is derived on the desktop's preview worker from the same raster with `analysis::overlay(raster, cells)`, which ORs `clip_class` over the source pixels of each display cell so an isolated clipped pixel survives Fit reduction, and at 100% maps cells one to one. Its buffer is bounded by the viewport.

### Ownership of the shared files

`modules/descriptor.rs`, `modules/processing.rs`, `modules/mod.rs`, `modules/registry.rs`, `render.rs`, `preview.rs`, `editor.rs`, `api/methods.rs`, `api/owner.rs` and `api/mod.rs` are changed by the integrator tasks (draft lifecycle and field patch; pointwise colour stage; analysis jobs) in that order. Algorithm units live in `modules/basic/`, the reducer and predicates in `analysis.rs`, the f64 references and hand-counted fixtures under `crates/lightwell-core/tests/reference/` and `fixtures/basic/`. The desktop's `app/`, `state/` and `view/` layers change only in the slider gesture driver, the histogram model and view, and the generic submit rule above.

### What this contract does not decide

Tone, white balance and colour equations, ranges and tolerances were frozen by the numerical studies ([tone](basic-tone.md), [white balance](basic-white-balance.md), [colour](basic-colour.md)) against independent references before their controls shipped. The provisional performance thresholds, the overlay colours, the picker patch and the order of later modules keep their recorded defaults.

## Basic controls and interaction

Every Basic control in the table below is implemented — Temperature and Tint, Exposure, the five Tone controls (Contrast, Highlights, Shadows, Whites, Blacks), Vibrance and Saturation — at the ranges, steps and precisions shown, together with the `neutral-sample` picker query behind the White Balance group.

These are the Lightwell ranges and units, taken from the [Lightroom research](../research/lightroom/tone-and-color-tools.md) as the owner decided, not claims of numeric equivalence to Lightroom. Defaults are neutral, zero. The numerical studies confirmed every range against independent references before the controls shipped. Ranges, step, display precision, defaults and descriptions live in the descriptor and the API schema.

| Group/control | Starting UI range | Required behavior |
| --- | --- | --- |
| Tone / Exposure — **implemented** | −5.00 to +5.00 EV, 0.01 step | Multiply linear-light channels by `2^EV` |
| Tone / Contrast — **implemented** | −100 to +100, step 1 | Change midtone separation with a defined pivot and smooth monotone curve |
| Tone / Highlights, Shadows — **implemented** | −100 to +100, step 1 | Smoothly change bright/dark ranges while retaining ordering and exposing retained detail |
| Tone / Whites, Blacks — **implemented** | −100 to +100, step 1 | Control bright/dark endpoints separately from broad highlight/shadow shaping |
| White Balance / Temperature, Tint — **implemented** | −100 to +100, step 1, precision 0 | Relative warm/cool and green/magenta correction of rendered JPEGs; not Kelvin |
| Color / Vibrance, Saturation — **implemented** | −100 to +100, step 1 | Chroma-dependent versus uniform color-intensity adjustment; saturation −100 is neutral grayscale |

Each slider has editable numeric text, keyboard steps, an accessible name/value/unit, and an explicit reset action. Double-click reset can be an additional shortcut. Group reset and Reset Basic preserve other groups/effects as appropriate and commit once. Reset on a neutral layer is a no-op; resetting an existing layer retains its ID with neutral parameters. No hidden group-enable/bypass feature is required.

Decided: slider release is the commit boundary, matching common photo-editor interaction. Pointer-down begins a revision-bound draft, pointer updates replace its settings and preview request, and release commits once. Escape cancels; numeric Enter commits once; an arrow-key hold is one gesture ending on key-up. Focus loss cancels an unfinished gesture rather than committing an intermediate value. Invalid text stays editable with a useful error and commits nothing. A gesture that returns to its start is a no-op.

Add the draft lifecycle to the core with Basic's commit timing, reusing the crop draft's conflict semantics rather than its desktop-local state machine. Independent clients can begin, set, inspect, cancel and commit drafts semantically without pointer simulation. Keep at most one active tool draft per client/asset; changing tools with a draft requires explicit commit or discard, not silent replacement.

An external commit preserves the draft and marks conflict. Discard or explicit Reapply is required. Reapply preserves the user's changed fields, merges them onto the newest compatible Basic state and revalidates geometry/picker inputs; it must not overwrite unrelated fields edited by another client. Undo/redo, Restore and history preview follow the same conflict rules. Historical previews remain read-only, and their values and histogram remain attached to the selected entry.

## Numerical and color contract

### Pointwise foundation and exposure

- Input is the existing verified, oriented sRGB/greyscale JPEG decode. Preserve source/profile rejection behavior. Do not silently reinterpret Adobe RGB, Display P3 or malformed ICC inputs as sRGB.
- Decode the sRGB transfer function into float linear sRGB, D65, apply the ordered operations, then encode and quantize at an explicitly declared output boundary. Use standard sRGB transfer constants; the [W3C conversion reference](https://www.w3.org/TR/css-color-4/#color-conversion-code) is an independent reference for conversion tests, not a dependency or an adopted CSS rendering pipeline.
- Exposure is `out = in * 2^EV`. Preserve finite values outside `[0,1]` between color operations so a later operation can bring them back. For the initial SDR output, explicitly clamp at the output boundary, encode, and round nonnegative codes using `floor(255 * encoded + 0.5)`. Do not clip/quantize after each slider or transient preview.
- Preserve alpha. The supported JPEG path is opaque. Identity/no-color stacks retain the exact existing byte path and source sharing; zero-valued Basic must not introduce round-trip changes. Pixel replacements enter at their original recipe position as encoded sRGB values converted to the working domain only when required.
- Specify and test finite bounds for compiled coefficients, layer depth, scratch and intermediate values. Overflow/non-finite results are explicit errors, not NaNs uploaded to the GPU or silently clipped intermediate state.
- Match a separate f64 stepwise reference: the default float tolerance `1e-6 + 1e-6 * abs(reference)` for supported non-clipped exposure/conversion cases, with at most one output code of rounding difference where permitted. Identity, current geometry/pixel fixtures and histogram counts remain exact. Freeze additional per-algorithm tolerances before implementation.

Geometry/color operations are ordered, not globally commutative. Nonlinear tonal processing before interpolation can differ from processing after it. M4's interpolation domain and output boundaries must be explicit; preserve its accepted behavior and prove mixed stacks against a stepwise reference. Fuse only transformations with equivalent results under the established tolerance. Do not add a whole-image float buffer to get around that requirement.

Point sampling and the future neutral picker use the same compiled evaluation as rendering without rasterizing a frame. A bounded crop filter may require a fixed local neighborhood; document its cost instead of claiming every interpolated query has the old exact-geometry cost. The initial Basic algorithms are pointwise and add no image-sized analysis to a sample or no-op check.

### Tone

These controls reshape retained information. A clipped JPEG plateau cannot regain missing detail. Adobe describes its tone controls as image-adaptive; its formulas and numeric behavior are not this contract. [Adobe tone procedure](https://helpx.adobe.com/lightroom-classic/desktop/help/tone-control-adjustment.html)

Frozen: [Tone mathematics](basic-tone.md). Luminance is Rec. 709 on linear sRGB; the curve runs on the analytically continued sRGB encoding with the pivot at encoded 0.5; Whites and Blacks are an endpoint remap with a crossing-prevention clamp, Highlights and Shadows are windowed odds-bias curves (`K_HS = 1.5`), Contrast is a normalized logistic S-curve, composed in that order; RGB is reconstructed by the luminance ratio with an additive near-black rule; the composed curve is nondecreasing for every parameter combination (minimum slope about 0.033 inside [0, 1], strictly positive on the extended domain), and production is checked against the independent f64 reference within `1e-5 + 1e-5 × |reference|`. Its documented limitation is the one a global curve has: lifted shadow noise loses some local contrast, and an edge-aware stage stays a later proposal.

### White balance and neutral picker

Frozen: [White balance mathematics](basic-white-balance.md). A von Kries adaptation in Bradford LMS anchored at the sRGB D65 white, Temperature as a mired shift along the daylight locus (about 3900 K to 20000 K across ±100) and Tint as a perpendicular CIE 1960 uv offset with positive tint magenta; 0/0 is the exact identity; the neutral picker averages the 5 × 5 patch in linear light, rejects clipped, near-black and non-finite samples with a reason, solves by bounded Newton iteration, rounds to whole steps and refuses corrections outside ±100 rather than clamping.

Implemented: Temperature and Tint are the White balance group of the one Basic layer, compiled as one composite 3 × 3 linear-sRGB matrix that runs first in the layer's unit order, and the neutral picker is the module query `neutral-sample` reached as `query.neutral-sample`, bound to the canvas by a `sample-apply` interaction and offered in the panel by the `picker` control at the end of the White balance group. The picker reads the 5 × 5 patch from the stage the Basic layer receives, so it evaluates before that layer's own correction; UI and API use the same coordinates, the same patch and the same solver, and a refusal commits nothing.

### Saturation and vibrance

Frozen: [Saturation and Vibrance mathematics](basic-colour.md). Oklab passed every property test, so it is the accepted space; saturation and vibrance both scale Oklab `a`/`b` about the achromatic axis, vibrance's gain depends on existing chroma and a skin-like Oklab hue band (55° ± 35°, at most 60% gain reduction at the band centre), neither unit clamps internally, and production is checked against the independent f64 reference within `1e-5 + 1e-5 × |reference|`.

## Histogram and clipping contract

The inspector describes the **rendered SDR sRGB output of the full current composition**, after crop and edits, before UI overlays, scaling for the viewport or monitor conversion. It is not the camera/RAW histogram. Panning, zoom and display scale do not change its population. Label this domain in the UI/API so users do not read output endpoint counts as evidence of sensor clipping or recoverable detail. The desktop states it on the plot itself, as the plot's hover tooltip ("Output · sRGB · after crop"), and the API in every report's `domain`.

- Three arrays of 256 unsigned 64-bit counts; each output byte selects its exact channel bin. Every channel sum equals the full output pixel count. Use integer reductions with deterministic merging. An overlapped gray area means overlapping RGB counts, not a separate luminance histogram.
- Plot filled RGB channels with a shared linear vertical scale and visible overlap. Counts returned by the API stay raw; presentation normalization must not change their meaning. Empty, pending, unavailable, superseded and failed are explicit states. A stale result may remain only with a visible stale label: the plot is dimmed while a newer frame renders. A state with no report is written inside the plot's own area — "No analysis yet", or "Unavailable:" with the reason — never in a row of its own. The inspector is the plot and the triangle row and nothing else, so its height never depends on the pointer, the analysis status or the counts, and nothing below it moves during a gesture.
- Shadow/highlight counters report per-channel endpoints (`code == 0`, `code == 255`) plus any-channel and all-channel pixel counts. Endpoints include values quantized to those codes; these are output clipping warnings, not an inference about the original capture. A colored indicator identifies channels with endpoint pixels; the counts are also stated in words, in the triangles' tooltips: the shadow triangle's gives the code-0 line, the highlight triangle's the code-255 line and the both-endpoints count. With no report behind them each shows a dash, never zeros.
- Clicking a triangle toggles its overlay; hover may preview it. Default mask rule: any-channel endpoint, blue for shadow, red for highlight; a pixel matching both uses magenta. Tooltips state this rule, above the counts. Masks never alter the raster, saved recipe, histogram population or future export. UI and API share the predicate.
- RGB hover readout uses the compiled sample at final image coordinates, reports 0–255 codes, and includes the selected render identity. The desktop shows it in the status bar, in a slot that is laid out whether or not the pointer is over the photograph, so it moves nothing in the tools panel or the bar. The histogram computation itself is a full-image worker operation, not a repeated owner-thread point query.
- Key each result to asset/source fingerprint, entry/snapshot, effective recipe identity, client draft ID/revision when present, output dimensions and color contract. Carry render generation with delivery. Counts and overlays must match the image currently presented, including drafts and history preview, not simply the newest catalog revision.

First implementation uses exact full-resolution counts. Reuse the final raster allocation, reduce during its production where practical, or scan it on a worker without a second render/copy. Exposure or view-only changes must not cause duplicate source decoding. During active gestures the previous histogram can be marked updating while an exact replacement is pending; do not secretly switch to thumbnail counts that miss single-pixel clipping. Approximate draft analysis is a later measured proposal, not the default contract.

Do not allocate a full-resolution mask: derive overlay values from the same final samples, then reduce/upload only the bounded display overlay. A Fit overlay must retain any clipped source sample contributing to a display cell rather than test a blurred thumbnail. 100% overlays use exact source-detail coordinates. Count and overlay tests cover isolated clipped pixels, crop edges and both-endpoint pixels.

## Shared API and jobs

The following are the required capabilities; the [integration contract](#integration-contract) names the methods that provide them against the delivered method table (`module.list`, `recipe.describe`, `workspace.set`, `render.locate`, `render.sample`).

| Capability | Required semantics |
| --- | --- |
| Inspect Basic | Effective parameter values, layer ID/position, payload format and availability for an explicit current or historical snapshot |
| Set Basic fields | Atomic field patch with expected revision, request ID and actor; omitted fields preserved, unknown fields rejected; one history entry or no-op |
| Draft lifecycle | Begin/set/read/cancel/commit/reapply through the core, explicit draft/base revision and conflict state |
| Reset field/group/Basic | Neutral parameters applied by the same transaction path; unrelated effects unchanged |
| Neutral sample | Image-space point/patch and explicit source stage; returns validated settings or a structured error |
| Request/read/cancel histogram | Frozen current, history or caller-owned draft target; job/result identity, readiness, exactness, domain, counts and structured errors |
| Overlay settings/readout | Per-client view state and semantic sampling; no history mutation |

All capabilities appear in discovery and work from an independent JSON client; MCP later inherits them. Read-only analysis must work without a GUI and cannot require switching the GUI selection. Requests return promptly; full-frame work never runs on the catalog owner. Reuse shared preview evaluation where targets coincide. Bound analysis to one active plus one replaceable pending job globally; report superseded requests explicitly and avoid a pending queue per slider event. Shared work is reference-counted so one client's cancel/disconnect does not invalidate another's result. Cap completed small reports/handles to the eight live clients; retain no per-result raster. Test competing clients and document scheduling fairness.

## Resource and responsiveness constraints

Preserve the current 64 MP, 16384 px/side, 512 MiB evaluated-frame and source-size limits. Float color arithmetic should stream per pixel or use bounded rows/tiles: a 60 MP float RGBA frame alone would exceed the current frame budget. The default new scratch limit is 64 MiB aggregate across active work, with reservations before allocation; do not raise existing limits without a measured decision. Source and byte raster remain shared `Arc` buffers. A histogram report is bounded to 16 KiB of counters/metadata before protocol encoding; worker-local bins are charged to scratch. Display overlays fit existing upload limits and the aggregate memory accounting.

Use the shared Rayon pool above a measured threshold. Registration/hiding an unused tool starts no worker and allocates no processing resources. Cancellation is checked between bounded chunks; stale frames/results are rejected even if computation finishes. No new idle polling is needed; retain existing event-sync behavior and gate completion polling on actual jobs.

Measure release builds on 24 MP and 60 MP, source cache cold/warm separately, with at least 30 samples. Separate algorithm, histogram, scheduling, GPU upload and presented-frame latency. Default targets: the [provisional slider budget](../specs/performance.md#provisional-budgets) for warm 24 MP slider-to-presented-frame latency, settled exact histogram p95 below 200 ms after the final input, and existing memory/idle hypotheses from [performance](../specs/performance.md). They are provisional thresholds, not current performance claims: a measured miss is reported with its figures and does not block delivery, and the owner refines the thresholds after review. Record 60 MP tails and peak RSS/GPU/scratch even if targets are missed. Do not add approximate processing, another cache or a timer just to claim a pass.

Every implementation handoff answers the [performance checklist](../engineering/performance-rules.md): verified-source reads; allocation bounds/sharing; no frame work in samples, validation or owner handlers; narrow UI refreshes; timers; 24/60 MP measurements; reference-buffer and buffer-sharing evidence.

## Verification and acceptance

1. Hand-counted tiny histograms: all-black/white, RGB primaries, uniform gray, ramps, single clipped pixels, both-endpoint pixels and cropped-away clipping. Sums, channel/any/all counts and overlay predicates are exact.
2. Independent f64 reference color evaluation and lossless expected buffers: identity, ±EV, inverse non-clipped exposure, gradients, step wedges, saturated and portrait-like colors, order around pixel replacement and exact/interpolated geometry. Frozen tolerances precede production implementation.
3. Complete history journey: gesture commit/cancel/reset, repeated values, retry/deduplication, undo/redo, preview/restore, reopen, failed write, unavailable provider, source missing/changed and current-format fixtures. Originals remain byte-identical.
4. Two-client races: conflict during drag/picker, explicit field-preserving Reapply, selected historical entry during external edits, analysis cancellation/reconnect and superseded results. No frame/histogram identity mismatch and no unexpected history entries.
5. Native M4 rendered inspection at Fit, 100% and available display scales: sliders, keyboard/numeric editing, focus, reset, clipping overlays, histogram overlap/readout, crop and photo changes. Correlate captures with snapshot/revision/draft/render generation and logs. Inspect smooth gradients, saturated colors, backlit portraits and noisy shadows. Record calibrated-color and screen-reader limitations honestly.
6. Extend existing acceptance/performance harnesses for these paths; run `cargo xtask check` on each implementation handoff and release diagnostics on the final editor. Headless/VM results are distinct from native rendered evidence. No performance claim follows from small fixtures alone.
7. Update current module/color contracts, feature status and user guide only for demonstrated behavior. If export is implemented concurrently, validate it uses the same evaluator/output contract; this plan does not own export encoding or metadata.

### What is demonstrated

Item 1 is demonstrated by the hand-counted fixtures in `fixtures/basic/` and the core reducer tests
over them, and on a photo-sized stack by the acceptance chapter's cropped-population check, which
builds a Basic layer that clips the border and a crop that removes it and proves the counts are the
interior population alone.

Items 2, 3 and 4 are demonstrated, display-independently, by the Basic and histogram chapter of
`cargo xtask editor-acceptance`, driven through the JSON method table against the independent f64
reference: whole-raster agreement within the frozen one output code for exposure, the nine-field
patch and the frozen unit order; `render.sample` byte-identical to the rendered raster and to an
open draft's later commit; the draft lifecycle, the return-to-start no-op, retry deduplication,
group and module resets keeping the layer identity, undo, redo, preview, restore and a catalog
reopen all evaluating byte-identically; an unavailable Basic provider refusing to render the stack
that names it while keeping the layer and module readable and reporting the analysis failed with no
counts; two Basic layers and an unsupported payload format both refused without rewriting anything;
`analysis.request/read` on current, historical and drafted targets equal to an independent
reduction; mixed stacks against a stepwise quantize-then-bilinear reference, around a point
replacement on both sides and under an orientation layer; and the two-client races — a draft
conflicted by another client's commit, a refused commit, a field-preserving reapply committing one
entry, a historical selection and its analysis staying attached to their entry, and one client's
cancel leaving another's shared job intact. The original file's SHA-256 is unchanged throughout.

Item 5 is demonstrated natively on the owner's M4 by the rendered scenarios `basic`, `basic-panel`,
`basic-crop`, `basic-restart`, `histogram`, `workspace`, `crop`, `crop-draft`, `unavailable`,
`large24` and `large60`, each correlating its captures with the recorded revision, entry, draft,
render generation and state. The `histogram` scenario's hover frame shows the readout in the status
bar with the tools panel pixel for pixel the frame before it and the status bar changed only inside
the readout's slot; every frame's triangle tooltips carry the independent reduction's counts, and
`unavailable` shows the reason inside the plot. Tooltips are checked through the recorded state,
not in a capture: the harness cannot hover a widget. Calibrated colour and screen-reader behaviour
are not claimed: every pixel measurement is renderer readback of displayed brightness or channel
balance.

Item 3 of the histogram and clipping contract — counts and overlays matching the image currently
presented, drafts included — holds during an active gesture as well. The drafted preview job asks
for the reduction, so the worker that rendered those pixels reduces them, and the report, the
raster and the texture are adopted together under the drafted frame's own generation: the plot is
the drafted population, its identity carries the draft revision the pixels were planned from, and
the clipping overlay is re-derived from the drafted raster. Between an input and the exact phase of
its frame landing, the previous report stays plotted and is marked updating, as it is for a
committed render; it never goes to zero counters. At Fit the drafted pixels on screen are the job's
proxy phase and the report is reduced from its exact phase, which a newer input cancels, so during a
fast drag the plot follows the frames the gesture pauses on and the counts are never approximate.
The bound is one `draft.set` and one preview job per accepted value, with the analysis riding that
job and no second render. The one exception is a drafted RAW temperature or tint, which is previewed
approximately on the planes developed at the committed white balance
([instant previews](instant-preview.md#a-raw-white-balance-during-a-drag)): no phase of that job is
reduced, so the previous exact report stays plotted and marked updating until the committed frame's
own report replaces it, and the counts are never taken from approximate pixels.
The `histogram` scenario's open-gesture frame proves it on the M4 — status ready,
`identity.draft_revision` present, and the eleven counters equal to an independent core render and
reduction of the drafted stack rebuilt from the committed layers the frame displays and the drafted
payload it records — and the desktop's own tests cover the adoption, the updating window, an older
generation being dropped and the overlay following the drafted raster. The drafted composition's
arithmetic is separately exact through `analysis.request {target: draft}` in the acceptance chapter.

## Later module candidates

| Candidate | Useful next scope | Why separate |
| --- | --- | --- |
| Tone Curve | Monotone point curve, composite first; numeric point API and reset | Reuses pointwise color stage but needs curve interaction and interpolation contracts |
| Detail | Sharpening, then noise reduction, judged at 100% | Requires scale, neighborhood halos and noise/detail quality evidence |
| Texture / Clarity | Fine/mid-scale local contrast | Requires bounded multiscale processing and halo/edge tests; cannot be relabeled global Contrast |
| Dehaze | Global atmospheric-haze correction with explicit limits | Requires a selected estimation model, color/noise review and photo-sized analysis budget |
| B&W / Color Mixer | Explicit grayscale treatment and sampled/ranged color control | Additional interaction and color contracts; saturation −100 alone is not a B&W mixer |

Auto Tone, profiles/presets, HDR, local masks, healing, red-eye and screenshot metadata rows are not selected for the first two slices. No disabled placeholders for them. Histogram dragging can follow once tone control semantics are proven; first ship the histogram as feedback. The existing export/Locate/MCP commitments keep their own priorities. Default order after Slice B: Tone Curve, then Detail, Texture and Clarity, Dehaze and the colour mixer, unless the owner reorders them.

## Decisions

Decided by the owner on 2026-09-21 and recorded in [product decisions](../decisions.md#basic-adjustments-and-histogram).

| Decision | Decided | Effect |
| --- | --- | --- |
| JPEG now versus RAW prerequisite | JPEG now; RAW is separate later work | Input and colour contracts stay the current SDR JPEG subset |
| Editable Basic organization | One layer with a fixed internal group order, at a stable position before the geometry tail | No ordering or targeting UX between Basic controls |
| Commit timing | Slider release, key-up or Enter; Escape cancels | No Apply/Cancel panel for adjustments; the draft lives for one gesture |
| First highlights/shadows quality scope | Global pointwise curve first; a visual case it cannot pass ships as a documented limitation with an edge-aware proposal recorded for later | Edge-aware processing is a later, separately measured proposal |
| Numerical limits and visual quality | Start from the Lightroom research and the ranges above; select and freeze against references in the numerical tasks | No formula or tolerance is accepted merely because a candidate was documented |
| Relative priority | Slice A, then Slice B | Export, Locate and MCP keep their own follow-up priority |

The remaining choices (performance thresholds, the tone fallback, the overlay rule, the picker patch, one Basic layer, the float tolerance and the order of later modules) have recorded defaults in [product decisions](../decisions.md#basic-adjustments-and-histogram), so the plan runs to completion on agent judgement and the owner refines afterwards.

## References

- Repository contracts: [modules](modules-and-api.md), [history](../specs/edit-history.md), [crop](../specs/single-image.md), [architecture](architecture.md), [performance rules](../engineering/performance-rules.md).
- Local research: [Lightroom tone/color](../research/lightroom/tone-and-color-tools.md), [rendering/color](../research/lightroom/rendering-and-color.md), [darktable algorithms](../research/darktable/tone-and-color-tools.md). These supply context, not accepted Lightwell behavior.
- Adobe describes Basic controls, relative JPEG temperature and RGB histogram/clipping interactions in [Image tone and color](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html). Familiar labels do not establish implementation equivalence.
