# Tool modules and the shared core

Status: implemented (M3, M4). The pixel, transform and crop tools are the built-in modules; the host owns everything a module does not declare here. The registry, descriptors and built-in modules live in `crates/lightwell-core/src/modules/`; the desktop models them in `crates/lightwell-app/src/state/` and renders them in `crates/lightwell-app/src/view/`, and the crop-frame editor's own draft and canvas live in `crates/lightwell-app/src/crop_draft.rs` and `crop_canvas.rs`.

An operation is semantic and programmable: set one pixel, rotate, set crop parameters, restore history, select a preview or change view state. Scripts never simulate pointer movement. Exposure, masks, clone strokes and lifecycle actions inherit this rule when they arrive.

## Ownership

| Owner | Responsibility |
| --- | --- |
| Host (`lightwell-core`) | Asset identity, shared invariants, atomic recipe and history commits, revisions, undo/redo/restore, request deduplication, the decoded-source cache, one-pass rendering of compiled processing, preview jobs, notifications, API discovery and dispatch |
| Module (`lightwell-core::modules::*`) | Its descriptor (identity, effects, actions, parameters, controls), input parsing, state validation and no-op detection, payload validation and compilation of its payloads into host processing primitives |
| Desktop (`lightwell-app`) | Layout, styling, focus, text editing, gesture capture; renders controls, including the `crop-frame` canvas editor, from descriptors and calls the declared actions through the same API every client uses; holds no authoritative state |

Module code never writes catalog tables, never keeps an undo stack and never renders. Preview, undo and restore never ask a module to reverse pixels: a snapshot is evaluated from original pixels and its ordered payloads.

## Descriptor

Every module returns one `ModuleDescriptor` from `descriptor()`. It is plain data, cheap to build and serializable, and it is the single source of API discovery, GUI controls and validation limits.

| Field | Meaning |
| --- | --- |
| `id`, `title` | Stable provider identity, e.g. `lightwell.pixel`; lowercase ASCII words separated by dots |
| `effects[]` | `id` (durable effect identity stored in every layer, e.g. `lightwell.pixel.replace`), `format` (internal payload format marker), `stage` (`geometry` or `pixel`) |
| `actions[]` | `id` (unique across all modules, e.g. `set-pixel`), `title`, `notes`, `summary`, `patch`, `parameters[]` |
| `parameters[]` | `name`, `kind`, `required`, `default`, `unit`, `step`, `precision`, `notes`. Kinds: `integer {min, max}`, `number {min, max}` (finite f64 within the closed range), `enum {options[]}`, `color` (three 8-bit sRGB channels). `step` (the keyboard and slider increment) and `precision` (display decimals, at most 6) are client hints a `number` parameter may declare; registration rejects a step that is not finite and positive, a precision above 6, and either hint on a parameter that is not a number. The host validates and stores what it was given and never rounds a request to a step |
| `controls[]` | Ordered semantic controls: `group {label, controls[]}`, `number {action, parameter, label}` (an integer or number parameter), `color {action, parameter, label}`, `action {action, label, preset}` where `preset` supplies fixed parameter values |
| `canvas` | Optional. `point-pick {action, x, y}`: a pointer pick on the image fills the named integer parameters of that action and never commits. `crop-frame {action, angle, x, y, width, height, fit_action, aspect}`: the host's crop-frame editor edits a transient draft of the named number parameters of `action` and derives its ratio presets from the `aspect` enum of `fit_action`; only Apply calls the action |
| `availability` | `available` or `unavailable {reason}`; an unavailable provider keeps its descriptor and effect identities |

API method names are generated: action `set-pixel` is `edit.set-pixel`. Parameters are top-level request fields beside `asset_id` and `mutation`. `schema.list` lists generated methods with their parameter descriptors, and `module.list` returns every descriptor. There is no hand-maintained list of module methods. A control the desktop cannot render shows an explicit unsupported-control message with the descriptor's kind; it is never silently dropped.

## Module trait

```text
descriptor()                             -> &ModuleDescriptor
parse(action_id, input)                  -> ActionInput { action_id, parameters }
plan(&ActionInput, &StageContext)        -> ActionPlan::{NoOp, Commit(Layer), Update(Layer)}
validate_payload(effect_id, format, payload)
describe_layer(effect_id, format, payload)               -> String
label(&ActionInput)                                      -> Option<String>
values(effect_id, format, payload)                       -> Map<String, Value>
compile(effect_id, format, payload, Stage{width,height}) -> Processing
```

- `parse` validates the request against the schema only and normalizes it. The returned `action_id` is the durable history action identity (`set-pixel`, `rotate-left`, `mirror-horizontal`, ...) and `parameters` is the object stored on the history entry. The host also checks required parameters, integer and number ranges, colors and enum options generically before calling `parse`, so every caller gets the same structured `validation` error.
- `plan` sees the current output stage (`width`, `height`), the current ordered layers, a point sampler over the current stack, `stage_before(i)`, the stage the layer at index `i` receives, `insertion_index(stage)`, where the host would place a committed layer of that effect stage, and `sample_before(i, x, y)`, a point sampler over the first `i` layers. The host answers all of them by compiling recipe prefixes, so a module plans against the stage its layer will actually receive. It rejects out-of-stage coordinates and reports no-ops; it allocates no frame and mutates nothing. `Commit` hands the host a new layer, which the host places by the effect's declared stage: a pixel-stage layer is inserted before the first geometry-stage layer, so the geometry tail carries it, and a geometry-stage layer appends. `Update` replaces the layer with the same identity in place, which is how the crop module edits its existing layer and the transform module folds an action into the orientation layer at the end of the stack without changing history identities. The host rejects an `Update` whose identity is not in the stack. Recipe validation does not reject other orders; a stack renders by its stored order whatever it is.
- `validate_payload` accepts or rejects a persisted payload structurally. `compile` turns it into a host processing primitive at its input stage.
- `label` names the history entry a request deserves when the `summary` template cannot say it, such as a patch naming the one field it changed; the host consults it before the template and the title. `values` reports the parameter values a stored layer represents, which `recipe.describe` returns on that layer's row so a client can seed its controls from the displayed entry. Both read the request or the payload only: no render, no sample, no source. Both have defaults (`None` and an empty object), so a module declares them only when it has something to say. The crop module reports `angle`, `x`, `y`, `width` and `height`.

### Field patches

An action may declare `patch: true`. The generic check then validates the fields the caller sent and fills no declared default, so the module receives exactly those fields and merges them over the state it already holds; omitted fields are preserved by that merge, unknown fields are still rejected, and every parameter is optional however it is declared (`schema.list` lists them all as optional). The history entry stores the patch as sent, not the merged payload, request deduplication covers the patch as sent, and a patch that changes nothing is a no-op with no entry. Defaults stay declared on patch parameters because clients seed and reset fields from them.

Processing primitives are the closed host set for v0: `ExactGeometry` (an integer mapping with declared output size, composable into one raster pass), `PointReplace` (one input-stage pixel, applied through the geometry that follows it), `Color` (pointwise colour over the whole stage) and `Resample` (an affine map from output pixel centers to input coordinates with a declared output size, sampled bilinearly in linear light with edge clamping, as defined in the [crop contract](../specs/single-image.md#sampling)). A resample is a stage boundary: the exact layers before it rasterize into one bounded intermediate frame, the resample writes the next frame, and exact layers after it compose as before. A point replace whose pixel falls outside a later crop is simply not visible. There is no generalized graph.

A `Color` operation is an ordered list of at most eight `PointwiseColor` units. The module owns each unit's equation and declares whether its coefficients are finite; the host owns everything else. The host decodes each channel into f32 linear sRGB (D65) through a 256-entry table computed in f64, hands rows to every unit of every operation of a run in order, and only at the end of that run clamps to `[0, 1]` and quantizes to the code whose exact linear threshold interval holds the value, which equals `floor(255 · encode(v) + 0.5)`. Values outside `[0, 1]` and negative values are preserved between units and between consecutive operations, so an inverse pair returns its input bytes; alpha is never touched. A point replacement, a resample and the end of the recipe are the quantization boundaries: a replacement earlier in the operation list is processed by the colour operations after it and one later in the list is not, and a resample interpolates the quantized frame. Exact geometry commutes with pointwise colour and composes as before. Compilation refuses an operation with more than eight units or a unit whose coefficients are not finite, and a non-finite value after any unit fails the render or sample with `resource-limit`; a neutral payload compiles to no units, which is no processing at all, so the identity byte path and the shared source buffer are kept. The rasterizing pass streams each run over the frame in bounded row chunks, and `render.sample` applies the same phases to one pixel, so a sample equals the rendered byte.

## Host dispatch

An action request goes through one path for the desktop, the JSON API and headless callers:

1. Validate the mutation envelope; find the action in the registry, else `validation: unknown action`.
2. Generic parameter checks, then module `parse`. A patch action's check validates only the fields that were sent.
3. Request deduplication: the stored input hash covers `{action: <durable action id>, mutation}` merged with the parsed parameters. A retry returns the original result; the same request ID with different input is a `conflict`.
4. Revision check, then module `plan` against the current stack with the cached verified source.
5. `NoOp` records the request without a row. `Commit` places the layer by its effect stage; `Commit` and `Update` validate the resulting recipe through the registry, compile it against the cached source so no later layer is left addressing a stage that no longer exists, and persist the snapshot, entry, current pointer, revision, cleared redo and request result in one transaction.

## Drafts

The core holds one draft per client session, in `ClientSession.draft`, reported by `session.state`. A draft is the settings of one gesture: it is bound to one asset and one action, never outlives the session, emits no event and appears in no history. Its methods are `draft.begin {asset_id, action}`, `draft.set {draft_id, fields}`, `draft.read {draft_id}`, `draft.cancel {draft_id}`, `draft.commit {draft_id, mutation}` and `draft.reapply {draft_id}`; only `draft.commit` touches the catalog.

`draft.begin` returns `{draft_id, action, asset_id, base_revision, draft_revision, fields, conflicted}` and is refused with `conflict` when the client already holds a draft and with `validation` while the session previews a historical entry or the action is unknown. `draft.set` validates every named field against the action's parameter descriptors — unknown name, non-finite, out of range or wrong kind is a `validation` error and nothing changes — then merges the fields and increments `draft_revision`. `conflicted` is `base_revision != the asset's current revision`, recomputed whenever the draft is read, set, committed or reported, so a commit by any client, including this client's own undo, redo or restore, marks it without a notification path. `draft.commit` refuses a conflicted draft or a `mutation.expected_revision` that is not the draft's `base_revision` with `conflict` and keeps the draft; otherwise it runs the action with the accumulated fields through the ordinary `apply_action` path and ends the draft, so a gesture that returned to its start is a `no-op` with no entry. `draft.reapply` rebases the draft on the current revision, keeps and revalidates only the fields this client set and clears `conflicted`; whatever another client changed meanwhile stays in the layer the next commit merges over. `draft.cancel` ends the draft and commits nothing. Two clients' drafts never interact, and the crop-frame editor's own desktop-local draft is unchanged.

A draft's effective recipe is the current snapshot with its action's plan applied to its fields, computed on demand by `EditorService::draft_recipe` and never persisted; a `NoOp` plan means the current recipe. Planning it costs the same point queries as a commit and rasterizes nothing. `OwnerHandle::preview_job` takes a `PreviewRequest` carrying the client, the entry, an optional `layer_count` and an optional draft: the owner resolves the draft from that client's own session, the job carries the recipe to render and `draft_revision` for correlation, and another client's draft is a `validation` error. `render.sample {asset_id, x, y, draft_id?}` samples the same effective recipe and stamps the answer with `{draft_id, draft_revision}`.

Rendering and point sampling resolve every layer's effect through the registry: `compile` runs per layer at its input stage, exact geometry composes into one mapping per segment, point replacements map through the suffix geometry of their segment, and a resample separates segments. A preview job may truncate the stack to its first `n` layers; the desktop uses that to show a crop layer's input stage while drafting. `render.locate {asset_id, entry_id?, x, y}` maps one rendered pixel back through the geometry tail to the content pixel it shows, returning `{content_x, content_y, width, height}`; the desktop's canvas pick uses the same mapping to fill point-pick parameters.

## Registry

`ModuleRegistry::builtin()` links the pixel, transform and crop modules; `register` accepts any `ToolModule` and rejects: an invalid module, effect or action identity; a duplicate module, effect or action ID; a control naming an undeclared action or parameter; a preset or default outside the parameter's range; a parameter without a kind. Registration builds hash lookups from descriptors only and touches no image or catalog resource. A module that needs an expensive resource initializes it on first `compile` or `plan`; registration and first-use costs are measured separately.

## Missing effects

Layers whose effect has no available provider stay in every snapshot and entry unchanged. `state`, `history.list`, `history.inspect`, undo and redo keep working. Rendering, sampling, appending an edit and restoring such a stack fail with `incompatible: unavailable effect <id> (layers …)`; a cached preview may only be shown with an explicit stale or unavailable label. Registering a compatible provider again restores evaluation without touching stored data. Removing an edit is a separate explicit action that does not exist yet.

## Current formats

Each module accepts only its current effect format and payload shape. Unsupported formats fail explicitly without modifying the stored recipe. Breaking shape and algorithm changes are expected during pre-release development; tests target the current contracts.

## Resources and external loading

Registration is cheap and expensive resources initialize on demand. Start with linked built-ins. Hiding controls is a UI preference, not effect removal, and persisted processing order never depends on panel order.

External loading remains required later: a separately authored tool must load without editing host source and expose its actions through the same APIs. Before implementing that loader, select a use case, measure startup, first-use, memory and idle costs, and define trust, dependencies, cancellation and packaging. Native binaries, workers or sandboxed runtimes are all still options; a marketplace and hot unload are not requirements. Later operations such as exposure, white balance, masks, clone strokes or presets must carry enough structured data to reproduce their results. Keep geometry and color stages explicit and do not prebuild a generalized graph.

## Acceptance

Descriptor validation rejects duplicate identities, missing handlers, invalid controls and unavailable providers explicitly. Pixel and transform actions invoked through generic controls and an independent API client produce identical stacks, history, pixels and errors. Current-format catalogs reopen with their IDs and undo/restore paths intact. Registration and first-use resources are measured separately. Real native M4 control layout, keyboard behavior and pixels are inspected with correlated state.
