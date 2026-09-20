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
| `actions[]` | `id` (unique across all modules, e.g. `set-pixel`), `title`, `notes`, `parameters[]` |
| `parameters[]` | `name`, `kind`, `required`, `default`, `unit`, `notes`. Kinds: `integer {min, max}`, `number {min, max}` (finite f64 within the closed range), `enum {options[]}`, `color` (three 8-bit sRGB channels) |
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
compile(effect_id, format, payload, Stage{width,height}) -> Processing
```

- `parse` validates the request against the schema only and normalizes it. The returned `action_id` is the durable history action identity (`set-pixel`, `rotate-left`, `mirror-horizontal`, ...) and `parameters` is the object stored on the history entry. The host also checks required parameters, integer and number ranges, colors and enum options generically before calling `parse`, so every caller gets the same structured `validation` error.
- `plan` sees the current output stage (`width`, `height`), the current ordered layers, a point sampler over the current stack and `stage_before(i)`, the stage the layer at index `i` receives, which the host answers by compiling the recipe prefix so a module editing a layer in place plans against that layer's own input stage. It rejects out-of-stage coordinates and reports no-ops; it allocates no frame and mutates nothing. `Commit` appends a new layer; `Update` replaces the layer with the same identity in place, which is how the crop module edits its existing layer without changing history identities. The host rejects an `Update` whose identity is not in the stack.
- `validate_payload` accepts or rejects a persisted payload structurally. `compile` turns it into a host processing primitive at its input stage.

Processing primitives are the closed host set for v0: `ExactGeometry` (an integer mapping with declared output size, composable into one raster pass), `PointReplace` (one input-stage pixel, applied through the geometry that follows it) and `Resample` (an affine map from output pixel centers to input coordinates with a declared output size, sampled bilinearly in linear light with edge clamping, as defined in the [crop contract](../specs/single-image.md#sampling)). A resample is a stage boundary: the exact layers before it rasterize into one bounded intermediate frame, the resample writes the next frame, and exact layers after it compose as before. A point replace whose pixel falls outside a later crop is simply not visible. Color stages are added to the host when the first tool needs them; there is no generalized graph.

## Host dispatch

An action request goes through one path for the desktop, the JSON API and headless callers:

1. Validate the mutation envelope; find the action in the registry, else `validation: unknown action`.
2. Generic parameter checks, then module `parse`.
3. Request deduplication: the stored input hash covers `{action: <durable action id>, mutation}` merged with the parsed parameters. A retry returns the original result; the same request ID with different input is a `conflict`.
4. Revision check, then module `plan` against the current stack with the cached verified source.
5. `NoOp` records the request without a row. `Commit` and `Update` validate the resulting recipe through the registry, compile it against the cached source so no later layer is left addressing a stage that no longer exists, and persist the snapshot, entry, current pointer, revision, cleared redo and request result in one transaction.

Rendering and point sampling resolve every layer's effect through the registry: `compile` runs per layer at its input stage, exact geometry composes into one mapping per segment, point replacements map through the suffix geometry of their segment, and a resample separates segments. A preview job may truncate the stack to its first `n` layers; the desktop uses that to show a crop layer's input stage while drafting.

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
