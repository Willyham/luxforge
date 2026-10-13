# Corrections: Clone, Heal and the repair foundation

Status: **implemented; the owner closed the plan as complete on 2026-10-11. The contract was frozen on 2026-10-09, and the module with its operations, actions and queries, the repair stage and its rendering on the CPU reference and the GPU, the desktop workflow ([workspace](corrections-workspace.md)), and frozen patches with their candidate accept, input identity, stale-export acknowledgement and the `repair-patch` corpus family are implemented. The offline workflow is qualified on the M4 against the corpus and on JPEG and RAW, and the owner accepted Heal after a blinded review on 2026-10-11. Set aside by the owner on 2026-10-11 and not run (they are not passes): the timing, memory and idle measurements, the `gpu-qualification` rerun of the 60 MP far-source Heal stack after the read-set atlas, and the `repair-patch` family's gate run.** This page is the contract: the settled owner decisions, what the code provides, and the exact shapes it implements. Numerical choices (brush rasterization, interpolation, Heal's solver, the automatic-source score) are frozen by the [numerics study](corrections-numerics.md), which this contract references and does not repeat. Everything under [frozen contract](#frozen-contract) is implemented as described; [current implementation](#current-implementation) lists what exists.

Corrections owns offline Clone/Heal, the repair processing primitive, the shared brush interaction and the repair layer's frozen-patch operation. [AI editing](ai-editing.md) owns model qualification, inference, downloads and activation, automatic selection, provider adapters, generation and candidate-review UI. The [Corrections workspace](corrections-workspace.md) owns what the person sees and does: the mode, gestures, panel, keys and the behaviour of automatic sources. The implemented [module capabilities](module-capabilities.md) supply settings, resources, jobs and durable artifacts; they are reused, not another delivery slice.

## User result and delivery

The Develop workspace gains a Corrections mode with Clone and Heal. Clone copies chosen pixels from another region. Heal borrows detail and adapts it to the destination's tone. Both work offline without accounts, downloads or provider configuration. Each correction is a numbered operation (`Heal 5`) that can be selected, moved, resampled, resized, changed, reordered and deleted; each change is one undoable history entry with the same semantic API action as the desktop.

The offline workflow is delivered and qualified independently of frozen patches or AI. Beside it, the same repair layer accepts immutable patches through a bounded host candidate seam verified with a test producer. AI Remove later uses that foundation; no unavailable mode or provider is drawn as a placeholder.

## Settled decisions

These are the owner's and are not reopened here.

| Date | Decision |
| --- | --- |
| 2026-10-05 | The repair stage sits after source development and before colour, in upright content coordinates ([decisions](../decisions.md#ai-editing)) |
| 2026-10-05 | The developer-only pixel proof and `Processing::PointReplace` are deleted, not reused as the repair primitive |
| 2026-10-05 | A stale frozen patch **keeps rendering**; its row, the status bar and `recipe.describe` say why, and export asks for an explicit acknowledgement. The earlier fail-on-stale rule is gone |
| 2026-10-05 | Remote consent is remembered per provider profile and data class, delivered by AI editing's remote tier; Clone/Heal and the patch foundation need no consent |
| 2026-10-07 | Repair precedes Detail (`restoration`): Detail treats repaired and original regions together, its changes never stale a patch, and a repair tick reruns Detail's downstream link |
| 2026-10-07 | Correction-operation actions declare `preset: false` |
| 2026-10-07 | Clone, Heal and the frozen-patch composite qualify in the existing spatial GPU class; repair drags follow GPU-first's motion contract (motion differences reported separately, strict at-rest gate) |
| 2026-10-09 | The [Corrections workspace](corrections-workspace.md) and its boards, with five defaults: **100 corrections per photograph**; a set source **persists** until Auto or Escape; **moving** a correction keeps its source in place; a stroke's **pin** is at its first point; `⌥`-click sets the source, `⌘`-drag places and pulls out a source, `⇧` draws straight strokes |

## Current implementation

What exists on `main` today and is reused as is:

- **Paths and strokes.** `ParameterKind::Points`, `path::capture_grid`, `PathCapture` and the grid (16,384 steps per unit; a position's `x` is a fraction of the content width and `y` of its height, a distance is in content heights; range −1..2; 1,024 stored and 16,384 posted points per stroke) ([one brush system](masking.md#one-brush-system)). Consumer-declared `StrokeKind`s share the one content-addressed `StrokeTable`; a stroke is read back only as its declared type; catalog commits write fresh strokes only. `StrokeCarrier` declares a consumer's references, and `Recipe::stroke_references` lists only the mask table. `path::references` reads the reserved top-level payload field `strokes`. The test-only `path::tests::RepairStroke` proves a second stroke type; it is not a correction tool.
- **Drafts.** `DraftTarget` and `declared_target` derive a draft's target from the action's declared identity parameters; `EditorService::draft_target` checks it through the commit's checks. The public `draft.begin` accepts only `mask` and `component`. A refused `draft.commit` keeps the draft; a commit may park a planning read (a colour-limited stroke's seed does).
- **Identities.** `IdentityKind` has asset, entry, draft, job, preset, mask, component, stroke, catalog folder and collection. `ParameterKind::host_only` makes every identity, `text` and `json` parameter host-only, so a module action cannot declare one.
- **Canvas.** `CanvasInteraction` is `point-pick`, `sample-apply` and `crop-frame`. The mask brush is desktop code driving `mask.add-stroke`, not a declared interaction.
- **Stages.** `EffectStage` is `source`, `repair`, `restoration`, `color`, `spatial`, `geometry`, `finish`. The developer pixel proof and `Processing::PointReplace` are deleted; `pixel` became `repair` in the same slot, with the placement, admission, scaling, proxy and input-grid rules of [module, effect and stage](#module-effect-and-stage).
- **Planning reads.** Every pixel read a query or mutation needs is planned on the catalog owner and answered by the tile service; a mutation parks and replays once with what it read; follow-on computation runs on the analysis worker (`luxforge-analysis`, at most 9 queued). `query.auto-tone` and `edit.auto-tone` use this path today.
- **Rendering.** The GPU is the renderer of record ([GPU-first](gpu-first.md)). A repair layer compiles to `Processing::Repair` and renders on both the CPU reference and the GPU as [Rendering](#rendering) describes, with its delivered choices listed there. Other spatial boundaries keep one halo-grown rectangle (`MAX_SPATIAL_HALO` 512). `render::spatial::prefix_hash` hashes a layer prefix and the masks it reads, not the source; a frozen patch's input identity adds the source and the earlier operations to it.
- **Corrections module.** `luxforge.corrections` with the payload, actions, queries, labels, answers, identity, strokes, limit and automatic source below, as [modules and API](modules-and-api.md#corrections) describes. It compiles its layer to the repair table with its strokes' positions (`ToolModule::compile_with_strokes`).
- **Catalog and jobs.** Catalog format 1 with immutable snapshots, the stroke store and `artifact_refs`. `catalog.empty-removed` scans surviving entries whose JSON contains `"strokes"` for 32-hex addresses. A job result is JSON plus published artifact IDs, and a task that takes an asset publishes candidates into the bounded host store, read with `job.read`, `job.read-candidate` and `job.release-candidates`, with an accept seam that publishes a pinned candidate as a `repair-patch` artifact ([job candidates](module-capabilities.md#job-candidates)).
- **Frozen patches.** The `patch` operation, `edit.accept-correction-patch`, the input identity and stale reasons, the CPU and GPU composite and the stale-export acknowledgement are implemented as [frozen patches](#frozen-patches) describes, with the delivered choices listed under [delivered frozen-patch choices](#delivered-frozen-patch-choices). The desktop's row of a patch says `frozen patch` or why it is stale, its status bar counts stale patches, and its single and batch export ask before including them; no desktop control generates or accepts a candidate (AI editing's). The required `repair-patch` family of `cargo xtask gpu-qualification` is in the corpus, generating its patches through the developer producer; its gate run was set aside by the owner on 2026-10-11 and has not been run.

## Frozen contract

### Module, effect and stage

One built-in module, `luxforge.corrections` (title `Corrections`, hint `Clone and Heal`, `collapsed: true`, not developer), applies to every source kind and declares:

| Declaration | Value |
| --- | --- |
| Effect | `luxforge.corrections.repair`, format `1`, stage **`repair`**, order `0`, `single: true`, not maskable, every source kind; `artifacts: true` (patches); `operations: {identity: correction, max: 100}` ([limits](#limits)) |
| Actions | `add-correction`, `update-correction`, `delete-correction`, `reorder-correction`, `reset-corrections`, `accept-correction-patch`. All `preset: false` |
| Queries | `corrections`, `correction-sources` |
| Reset | `reset-corrections` |
| Canvas | `brush-mask` ([interaction](#the-brush-mask-interaction)), title `Corrections`, shortcut `Q`, icon `heal` |

`operations` is a new optional `EffectDescriptor` field, serialized only when declared: `{identity, max}` says the effect's layer holds an ordered list of at most `max` operations that the module mints and addresses by `identity`. Registration accepts it only with `single: true`, `max` in `1..=1000` and `identity: correction` (the one module-addressable identity kind). It is how the host admits the identity into the module's declarations and how a client learns the limit.

Generated methods: `edit.add-correction`, `edit.update-correction`, `edit.delete-correction`, `edit.reorder-correction`, `edit.reset-corrections`, `edit.accept-correction-patch`, `query.corrections`, `query.correction-sources`.

**The repair stage replaces `pixel`.** `EffectStage::Pixel` is renamed `EffectStage::Repair`, serialized `repair`, in the same slot (the proof is deleted). No stored data names a stage — a layer stores its effect identity — so the rename rewrites nothing. The rename is deliberate: every consumer of `pixel` assumed a pointwise, content-pixel-addressed layer, and a repair layer is neither, so each match arm is revisited rather than inherited:

- **Placement.** A new repair layer goes before the first `restoration`, **`color`**, `spatial`, `geometry` or `finish` layer, after a leading source layer. Adding `color` to the region rule is the fix that puts repair before colour on a stack that has colour but no Detail; today's `pixel` rule would append after Basic. The insertion rule still moves no stored layer.
- **Admission.** A repair layer anywhere but the leading run (`source?` then `repair`) is refused at admission with `incompatible: the Corrections layer must follow only the source development`, and retained byte for byte. Insertion never produces such a stack; the refusal keeps the input-grid prefix and the patch key below well defined.
- **Scaling.** Repair geometry is in normalized content coordinates, so a repair layer compiles against a reduced stage (`render/gpu/fit.rs` stops treating it as unscalable); the reduced-stage behaviour is the numerics study's, reported as a motion difference.
- **CPU proxy.** A repair layer is proxy-ineligible with a named reason; a session without a GPU draws repair on the reference. Corrections adds no proxy work.
- **Input grid.** See [rendering](#rendering).

### Operations

The repair layer's payload (format 1), with operations in **application order** (index 0 is applied first and is the bottom of the desktop's stack):

```json
{
  "next": 8,
  "strokes": ["<stroke id of operation 0>", "<stroke id of operation 1>"],
  "operations": [
    {"id": 2, "kind": "heal",  "stroke": "<stroke id>", "feather": 50, "opacity": 100, "offset": [-1573, 0],  "origin": "auto"},
    {"id": 5, "kind": "clone", "stroke": "<stroke id>", "feather": 20, "opacity": 80,  "offset": [0, -2458], "origin": "set"}
  ]
}
```

| Field | Rule |
| --- | --- |
| `next` | The next operation number to mint, ≥ 1 and greater than every `id` present. Adding mints `next` and increments it; deleting, reordering and resetting never lower it |
| `strokes` | The host-visible reserved field ([`path::STROKES_FIELD`](../../crates/luxforge-core/src/path.rs)): exactly the `stroke` of each Clone/Heal operation, in operation order (duplicates allowed). Validation refuses any other content, so the host's carrier and the module's operations cannot disagree |
| `id` | Integer `1..=4294967295`, unique in the layer. The API spells it `correction-<id>` |
| `kind` | `heal`, `clone`, or `patch` |
| `stroke` | A `correction` stroke ([strokes](#strokes-and-the-catalog)): the path and radius in content space. Clone/Heal only |
| `feather`, `opacity` | Integers `0..=100`: feather is the share of the radius over which coverage falls to zero (profile frozen by the numerics study); opacity scales the blend |
| `offset` | `[dx, dy]` integer grid steps, source anchor minus destination anchor. The **anchor** is the stroke's first stored point (a spot's centre, a stroke's pin). Each component within `±49152` (the full coordinate range), not both zero |
| `origin` | `auto` (the automatic search chose the offset) or `set` (a person or client did) |

**Ordering.** Let `P₀` be the repair layer's input, the developed source. Operation `i` reads only `Pᵢ` — its input with operations `0..i` applied — for both its source and its destination's surroundings, and writes `Pᵢ₊₁`; the layer outputs `Pₙ`. No operation reads a later one or its own output, so overlap is well defined and recursion impossible. Reordering changes what an operation samples and re-renders it; it never moves a stored source.

**Recompute, never freeze.** Clone and Heal render from their saved geometry and the current `Pᵢ` on every evaluation. An upstream change (RAW white balance, an earlier operation) recomputes them; they are never stale. An automatic source is resolved once and stored as an ordinary offset, so a later edit never moves it.

**Limits.** At most `100` operations per layer, so per photograph, patches included. One stroke per Clone/Heal operation, at most 1,024 stored positions (the host bound). Radius `0.0005..=0.25` content heights (about 2 px to a quarter of the frame on a 4,000 px-high photo); the numerics study may narrow the upper bound with a measured resource bound, and the descriptor always publishes the core's range. Region, source-window, solve and residency bounds are the render integration's, refused by name ([limits](#limits)).

### Actions

Every action is a `luxforge.corrections` module action: the revision envelope (`mutation.expected_revision`, durable per-asset `request_id` deduplication), `preset: false`, labels from `ToolModule::label`/`planned_label`. Positions are in the stored path grid's normalized coordinates, the same space `points` and the mask brush use: `x` a fraction of the content stage's width and `y` of its height, the frame `0..1` on both, range −1..2. Distances — the radius — are in content heights, so a spot is round at any aspect ratio. An offset's grid steps become whole stage pixels as `round(k / 16384 · W)` and `round(k / 16384 · H)`, half away from zero. **`source-x`/`source-y` are absolute positions of the source anchor**, never offsets: the module stores `offset = quantize(source) − first stored point`, and answers `source = first stored point + offset`, both exactly on the grid. **A source is kept inside the frame**: an add, and an update that sends `points`, `radius` or a source, moves the offset, on each axis where the source footprint (the path's bounding box grown by the radius, moved by the offset) would leave the content frame, just far enough inward that it lies a stage pixel inside it; where the footprint is wider than the frame on an axis, its part inside the frame is what stays inside. The answer reports where the source went. An update of numbers or kind moves nothing. `luxforge_core::fit_correction_source` publishes the rule for clients (`SourceFit::Clamp`), and its `SourceFit::Turn` form, which first tries the offset's mirror on each axis that leaves, is what the desktop sends a kept offset through.

| Action | Parameters | Effect |
| --- | --- | --- |
| `add-correction` | `points` (points, 1..16,384, required), `radius` (number, required), `mode` (`heal`\|`clone`, default `heal`), `feather` (integer 0..100, default 50), `opacity` (integer 0..100, default 100), `source-x`, `source-y` (numbers −1..2, both or neither) | Appends one operation at the top. With a source: `origin: set`. Without: the [automatic source](#automatic-sources) resolves against the request's revision, `origin: auto` |
| `update-correction` | `operation` (identity `correction`, required), optional `points`, `radius`, `mode`, `feather`, `opacity`, `source-x`+`source-y`, `source-origin` (`auto`\|`set`, only with a source, default `set`) | A field patch of one operation (`patch: true`). `points` replaces the path, captured at the resulting radius, and **keeps the source position**: the offset is recomputed from the new first point unless a source is also sent. A source sets `origin` to `source-origin`. `radius` keeps the stored positions and changes only the radius (a new stroke address) |
| `delete-correction` | `operation` | Removes the operation |
| `reorder-correction` | `operation`, `index` (integer 0..99) | Moves the operation to `index` in application order (0 = bottom), counted after removing it; `index` ≥ the count is refused |
| `reset-corrections` | none | Empties `operations` and `strokes`, keeps `next`, so numbers are not reused in this lineage. The module reset |

**Answer.** `add-correction` and `update-correction` answer the mutation result plus `correction: {operation, name, index, source-x, source-y, source-origin}`; a deduplicated retry answers the same object, read from the entry the first request created (the stored input records the minted operation and the resolved source, so neither is recomputed). `accept-correction-patch` answers `correction: {operation, name, index}`.

**Validation.** Refusals change nothing:

- `validation`: unknown operation (`no correction correction-9 in this photograph`); a malformed identity; points or radius out of range; a destination footprint (the path's bounding box grown by the radius) that does not meet the content frame; a zero offset, including one that keeping the source inside the frame leaves at zero; a source without both coordinates; `source-origin` without a source; `index` out of range; a geometry field on a `patch` operation (only `opacity` updates one).
- `resource-limit`: an add at 100 operations (`100 corrections is this photograph's limit`, `data: {limit: 100}`); a stroke over 1,024 positions after decimation; any render region or residency bound.
- No automatic source: `validation` with `data: {reason: "no-source"}`, `No automatic source fits; send source-x and source-y`. Nothing is committed and a draft stays open.

**No-op.** An update whose every sent field equals the stored value after capture (the same stroke address, offset, numbers and kind), a reorder to the operation's own index and a reset of an empty layer are `no-op` outcomes with no entry. A delete always changes the stack.

**History labels.** `<Name>` is the kind's title and the number: `Heal 5`, `Clone 5`, or a patch's stored noun (`Remove 7`); an update is named by the operation's kind before the change.

| Request | Label |
| --- | --- |
| `add-correction` | `Add Heal 5` |
| `update-correction` with only `points` | `Move Heal 5` |
| … only a source, `source-origin: set` | `Move Heal 5 source` |
| … only a source, `source-origin: auto` | `New source Heal 5` |
| … only `radius` | `Resize Heal 2` |
| … only `opacity` / only `feather` | `Heal 5 · Opacity 80` / `Heal 5 · Feather 40` |
| … only `mode` | `Change Heal 5 to Clone` |
| … any other combination | `Edit Heal 5` |
| `delete-correction` | `Delete Heal 5` |
| `reorder-correction` | `Reorder Heal 2` |
| `reset-corrections` | `Reset corrections` |
| `accept-correction-patch` | `Add Remove 7` |

**Auto-collapse** applies by the existing rule ([auto-collapse](versions-and-lineage.md#auto-collapse)): consecutive updates of the same operation and field set by one actor collapse, as slider runs do; an add never collapses, because planning it on the run's base mints a different stack, and neither does a patch's accept, which would place the patch on a stack its candidate was not generated against. The collapse comparison plans the stored, resolved input, so it never repeats a pixel read.

**Presets and settings.** `preset: false` keeps every correction action out of preset capture, creation, import, Copy/Paste settings and batch settings. Applying an ordinary adjustment preset keeps the repair layer, its strokes and artifacts; upstream changes recompute Clone/Heal and stale patches by the rules below.

### Queries

`query.corrections {asset_id, entry_id?}` answers the layer as stored, reading the payload and its hydrated strokes and no pixel: `{limit, next, operations: [{operation, name, kind, index, points, radius, feather, opacity, source-x, source-y, source-origin, stale?}]}`, points as normalized `[x, y]` positions, `stale` only on a stale patch. It is how the desktop draws pins, outlines and rows and how an agent reads what it changed. This query is an addition to the workspace's table, which named no read.

`query.correction-sources {asset_id, entry_id?, operation?, points?, radius?, count?}` ranks automatic sources without writing:

- Exactly one of `operation`, or `points` with `radius`. With `operation`, the input is that operation's `Pᵢ` and the destination is its stroke; with a path, the input is the layer's output `Pₙ` (what a new top operation samples) and the destination is that path at that radius.
- `count` is `1..=8`, default 8.
- Answer: `{outcome: "found" | "no-source", candidates: [{rank, source-x, source-y, score}]}`, best first; `no-source` with an empty list when nothing qualifies. The ranking is the one `add-correction` uses (rank 1 is what an add would store), deterministic for one stack and source.
- The desktop's `/` takes the next rank and sends `update-correction {operation, source-x, source-y, source-origin: auto}`. The host does not re-verify that an `auto` source came from the ranking; `origin` is provenance the client asserts.

### Automatic sources

The search's candidate set, rejection rules, score, colour domain and tie-break are the [workspace's](corrections-workspace.md#automatic-sources), with numbers frozen by the numerics study. Its execution is frozen here:

1. **One query.** The search is `query.correction-sources`. `add-correction` without a source asks it from its plan through the existing deferred-query seam (`StageQuestions::query`, "answered off the catalog owner, then replayed into action planning") with `count: 1`, so the add and the query rank identically by construction.
2. **One bounded read.** The query's plan asks the host, in `O(layers)` and reading no pixel, for one rectangle of `Pᵢ` as linear values at the search's reduction, covering the candidate window. `Pᵢ` is the output of the repair layer with its payload replaced by the operations before `i` (every operation for a path, or `P₀` when the stack has no repair layer yet). This is a new `StageQuestions` read beside `grid_before` — a layer's output with a module-supplied payload, over a bounded region — named `StageQuestions::region_before`; its pixel bound is the tile service's one-frame bound.
3. **Off the owner.** The tile service answers the read on its thread (the GPU tile worker, or the reference service), and the search runs on the analysis worker (`Step::Then`, as Auto tone's solve does), never on the owner or UI thread, under the call's cancellation. Queue bounds are the tile service's and the analysis worker's (9 waiting each); a full queue refuses with `resource-limit` at once.
4. **Against the request's revision.** The add parks and replays once with the ranked answer; the owner checks that what the read was read from is still current (`pixel_key_current`) and the replay commits against `mutation.expected_revision`, so a change in between is the ordinary `conflict`. It counts against the call's four planning reads (`MAX_PIXEL_READS`).
5. **Never in a preview.** A drafted add without a source plans no operation for its preview, which defers no read: the preview is the current recipe and the desktop draws the tinted footprint. The search runs only when the add commits (`edit.add-correction` or `draft.commit`).

### Identity and drafts

**Identity kind.** `IdentityKind::Correction`, serialized `correction`, spelled `correction-<n>` with `n` a decimal integer `1..=4294967295` without leading zeros (`with_article`: `a correction`). The number is the operation's `id`; the display name is presentation.

**Uniqueness.** An operation number is unique within every recipe and is never reused along a lineage, because `next` only grows. Each entry is a self-contained snapshot, so after an undo a new branch may mint a number an abandoned branch also used; history labels then name different operations in different branches. This is the delegated reading of "a number the photograph never reuses" ([open questions](#open-questions)).

**Admission into module declarations.** `ParameterKind::host_only` stays true for every identity kind except `correction`, and `text` and `json` stay host-only. A module's action or query may declare `identity {of: correction}` only when one of the module's effects declares `operations` with that identity; registration refuses it otherwise, naming the parameter. The module resolves the identity against its own layer; an unknown one is `validation`. Every other host-only restriction is unchanged.

**`draft.begin`.** Gains `operation` (identity `correction`, optional): the correction an operation-bound module action edits. It joins `mask` and `component` in the target the host checks through `draft_target` with the commit's checks; `draft.begin` additionally refuses an operation the current layer does not hold, in the commit's words, and refuses `add-correction` at the limit with the same `resource-limit` before any draft opens.

**Gesture bindings.** One draft per gesture; pointer motion sends `draft.set` only; release or Return sends `draft.commit` (one entry); Escape sends `draft.cancel`. Conflict, Discard and Reapply are the existing lifecycle. A Reapply of an update whose operation another client deleted is refused and the draft stays conflicted until Discard.

| Gesture | `draft.begin` | Drafted fields | Commit label |
| --- | --- | --- | --- |
| Click, paint, `⇧`-click (automatic source) | `add-correction` | `points`, `radius`, `mode`, `feather`, `opacity` | `Add Heal 7` |
| Paint with a set source (after `⌥`-click) | `add-correction` | the same plus `source-x`, `source-y` (the press plus the persistent offset) | `Add Clone 6` |
| `⌘`-drag | `add-correction` | `points` (the press), then `source-x`, `source-y` following the pointer | `Add Heal 7` |
| Pin drag | `update-correction`, `operation` | `points` (the whole path translated by whole grid steps) | `Move Heal 5` |
| Source pin or outline drag | `update-correction`, `operation` | `source-x`, `source-y` | `Move Heal 5 source` |
| Size handle | `update-correction`, `operation` | `radius` | `Resize Heal 2` |
| Row sliders | `update-correction`, `operation` | `radius`, `feather` or `opacity`, committed on release | `Heal 5 · Opacity 80` |

Kind changes, `/`, delete, reorder and reset are direct actions with no draft. A refused add at commit (no source, limit) keeps the draft, so the workspace's no-source notice can finish it with `⌥`-click.

### The brush-mask interaction

`CanvasInteraction::BrushMask`, serialized `brush-mask`, is the declared form of the masking design's `brush-paint` capture, so the desktop's one capture serves both the mask brush and Corrections. Fields:

| Field | Corrections' value | Meaning |
| --- | --- | --- |
| `action` | `add-correction` | The action a new stroke drafts |
| `points`, `radius` | `points`, `radius` | Its path and radius parameters (a `points` and a `number` parameter of `action`) |
| `feather`, `opacity`, `mode` | `feather`, `opacity`, `mode` | Optional parameter names the brush settings fill; the mask brush's flow and colour limit are not part of this vocabulary |
| `source` | `{x: source-x, y: source-y, query: correction-sources}` | Optional: the source parameters and the ranking query |
| `edit` | `{action: update-correction, operation: operation, list: corrections}` | Optional: the action pins and handles draft through, its identity parameter, and the query that lists drawable operations |
| `title`, `shortcut`, `icon` | `Corrections`, `Q`, `heal` | As every interaction |

Registration checks that each named parameter exists on its action with the right kind and that `edit.operation` is an identity the effect's `operations` declares. Capture, overlays and hit tests map through `render.locate`/`render.transform` and the shared warped-path drawing, correlated with the entry or draft that produced the displayed frame; invalid mappings and stale frames have explicit outcomes. The desktop never inverts geometry itself.

### Strokes and the catalog

**Stroke type.** `CorrectionStroke {points: Vec<[i32; 2]>, radius: i32}` (grid steps; canonical compact JSON in that field order), `StrokeKind::NAME = "correction"`, captured by `path::capture_grid` at its radius; a spot is one point. `parse_stored` re-checks the path (`stored_path_fault`) and the radius range. Feather, opacity, kind and source live in the operation, so changing them writes no stroke; moving or resizing writes a new content-addressed stroke.

**Carrier.** The repair layer is a `StrokeCarrier` reading `path::references(layer.payload)` typed `correction`, and `Recipe::stroke_references` lists it after the masks. Every path that admits or hydrates mask strokes does the same for it: commits (fresh strokes only), drafts' effective recipes, restore, history preview, version restore, reopen, render, sample, query, export and batch work. A missing, corrupt or wrong-type stroke is `incompatible`, naming the stroke and `operation correction-5 of Corrections`; history stays readable and nothing renders as an empty repair.

**Empty Removed.** The scan's key is the serialized `"strokes"` field, which every repair layer carries (empty or not), so `catalog.empty-removed` retains correction strokes named by a surviving entry with no change to its query. A mixed mask/correction, cross-photograph retention test holds it.

**Catalog and originals.** The catalog format is unchanged; no migration, second store or collector. Photographs enter through `pick.develop`, `source.prepare` and `job.adopt` (`EditorService::import` in tests). Same-byte `source.locate` and folder recovery preserve operations and patch validity; a missing, offline or changed original is a source-availability failure. Library Remove/Put back and their undo keep all entries, strokes and artifacts; Send back refuses an edited photograph. Select's rendered previews and filmstrip include repair through the existing entry-keyed preview lane and its GPU/reference tiers.

### Rendering

**Primitive.** The module compiles its layer to one new spatial processing primitive (`Processing::Repair`) holding a bounded operation table: per operation its kind, opacity, feather, radius, offset, segment range in a shared segment grid, destination box `Dᵢ` (the path box grown by the radius) and source box `Sᵢ = Dᵢ + offset`, plus Heal's declared boundary ring. One table of at most 100 rows, uploaded as the `GpuDescription` storage block; operations evaluate in table order. The GPU groups consecutive operations into ordered passes, starting a new pass when an operation's source, ring or destination meets an earlier destination in the current pass; a pass is one dispatch over its destinations reading the table, so independent dust spots cost one dispatch. Heal's iterations are the numerics study's.

**Declared reads.** Producing window `W` of the repair output reads a **rectangle set**, not a halo: walking operations from last to first with `X = W`, an operation whose destination meets `X` adds its source box and, for Heal, its whole destination and ring (a Heal solve changes its whole patch) to `X`. `SpatialEntry::halo_reads` and `Entry::plan_gpu_window` generalize to return this bounded set (at most 16 rectangles after merging overlaps; past that, their bounding box); every existing spatial operation keeps returning its one halo-grown rectangle. `MAX_SPATIAL_HALO` is not raised. The window walk, the tile service's tiles and export bands charge the set's pixels before reading. Because repair is always first after the source, every read is a read of the developed source.

**Sweeps.** The repair operation ends a sweep when its read set leaves its window grown by `SWEEP_SPLIT_REACH`, as a staged-light reader does; its output is then the next sweep's stage texture.

**Incremental changes.** Between two plans of one gesture, the repair change is computed from the two tables: the boxes of every added, removed or changed operation (old and new), then forward over later operations, adding the destination of any whose source or ring meets the changed area. The result is `GpuChange::Inside(bounding box)` in the boundary stage, or `Anywhere` past half the stage's area; downstream links (Detail first) grow it by their reach as today. A change upstream of the repair layer is a source development change, which is already `Anywhere`. A drafted operation is the top one, so a painting tick changes only its own destination: near the new segments for Clone, the whole destination for Heal.

**Admission.** The operation table, segment grid, source and destination textures and Heal scratch are charged before allocation on the photo surface's GPU (2 GiB preview budget, 256 MiB held boundary), on the separate GPU tile worker (its own 2 GiB) and on the CPU reference (the 256 MiB spatial budget paces tiles; a new `RenderPass` on the shared pool, never RAW's executor). Over-budget work is refused with `resource-limit`, naming the operation; geometry and quality never change silently. A stack the GPU cannot draw falls back to the reference with a named reason and never omits an operation.

**Reference mask-input grid.** The materialized leading run becomes `source?`, `repair?`, `restoration*`: a repair layer makes the prefix spatial even without Detail, and `split` falls after the last repair or restoration layer. The key is unchanged in structure — the source key plus the SHA-256 of the prefix layers (whose payload names strokes and artifacts by content address) and the masks they read — so editing a correction rebuilds the grid and editing a later mask or colour layer reuses it.

**Warm-up.** Stage classification gains `repair`, and `plan_warm_list` warms, at open and after each commit, the repair links of the current table plus one drafted top operation of each kind and an update drag of an existing operation. The current list covers only colour/finish and restoration/spatial drags and field-patch first uses.

**Qualification.** Module-owned WGSL for Clone, Heal and the patch composite, with required corpus families in `cargo xtask gpu-qualification` for display, histogram and clipping, samples and export, in the spatial class.

**Delivered.** What the code does, with the choices made within this contract:

- `Processing::Repair(RepairTable)`: at most `MAX_REPAIR_OPERATIONS` (100) rows in application order, each the operation's id, kind, stroke positions and radius in the stored-path grid, feather, opacity and offset; the host re-validates the table and binds it to whatever stage it renders at (`render::repair_entry::RepairEntry`). An empty table is dropped.
- **Read sets.** `RepairEntry::reads` walks the operations backwards as above; Clone's whole destination joins the set as Heal's does, so every operation evaluates over its whole destination and the set holds the whole frame's values. The CPU reference holds the set itself (at most 16 merged rectangles, else their bounding box). The GPU step holds its read set itself, not a window around it: every rectangle its operations read, merged where they overlap, is packed into an atlas plane of the step's own (shelves, tallest first, at most 8,192 texels a side, sides rounded up to 256 so a gesture's ticks keep its size; `render::gpu::repair::pack`), and a read table at the head of the step's block indexes the rectangles by 64-pixel cells of the stage and of the atlas. Before its passes the atlas is filled with the step's input: a rectangle the boundary holds whole is copied from it, any other derived from the source by the boundary's own derivation, a cut or a reduction, at its placement (`luxforge_gpu`'s `execution::fill`). The photo surface holds the whole source; the tile runner holds a window of the source for each rectangle beside its band's window, charged and released with it. The groups' gathers, composites and state planes run over atlas texels and the applies map each window pixel to its texel, so a window, a tile's or a sweep's, holds its own rectangle and halos wherever a source lies, and the repair's output is exact wherever a later link reads. The fill, and with it every pass of the step, reruns only when the boundary's content or window, the source or the rectangles change. The step's passes read the atlas alone, so what its planes hold is no window's: the tile runner keeps a repair step's solved planes (at most two steps' worth, charged to `GPU_TILE_BUDGET`, let go with another source or when the runner is released), keyed by the source's version, the rectangles and the step's words and blocks, and every later tile of a stream or read of a call binds them for its apply, filling nothing and running no pass. A read set with a rectangle past 8,192 texels a side, or that does not pack into one atlas, is drawn by the reference with a named `repair` fallback (`one rectangle it reads is …, past the 8192 a side of one GPU atlas`, `the N rectangles it reads … do not pack into one GPU atlas`).
- **CPU reference.** `render::repair_entry::repair_frame` fills the destinations' read set as `f32` planes, reserved on the 256 MiB spatial target (pacing, never refusing), runs the numerics study's units in order and writes the frame in bands; its gate is `RenderPass::Repair`, provisionally the spatial threshold. A Heal past the study's solve bound is `resource-limit` naming the operation (`Heal 5: …`). The units are exact against the independent `f64` reference on JPEG bytes and RAW-precision values.
- **GPU.** One spatial step per repair layer, its program the study's `repair/repair.wgsl` over its per-level atlas. Operations are grouped by conflicts, not order: an operation joins the group after the latest one holding an earlier operation whose writes meet its reads or whose reads meet its writes, so independent corrections anywhere in the table share one set of dispatches (a hundred spread over a frame: three or four groups, about 600 passes). Groups chain in the one step: groups before the last blend into alternating atlas-sized state planes (`rgba16float` on the byte path, `rgba32float` on the linear path), each at the read set's placements, and the last group's apply blends over the last state. A step past 2,048 passes, or whose Heal level planes pass 512 MiB, is drawn by the reference with a named `repair` fallback. A repair carries no reach into a sweep's window, so its sources never end a sweep. A tick's change is computed from the two tables as above, and the executor hands it to the links after the repair step (`contained`), which still runs whole. `plan_warm_list` warms the stack's repair links, an update drag, a new top Heal and Clone, or the first Heal and Clone where a repair layer would go. `GPU_PLAN_LINKS` holds the repair link (21) and the pipeline caches grew to 65.
- **Measured here** (Apple M4 Pro, loaded host): against the reference, 72 plan cases (overlap, chained, far, edge-clipped, both paths, before Basic and Detail) worst mean ΔE00 0.013, p99 0.50, max 1.54, the Basic and Detail programs' own differences; alone, at most one code moves. Integrated timings were set aside by the owner on 2026-10-11 and have not been run. The release gate over the whole corpus holds every repair stack's picture at rest, samples, histograms and exports within the spatial class, but the 60 MP stroke healed from across the frame, whose histogram and export the gate measured before a step held its read set in an atlas; the owner set the rerun aside on 2026-10-11, so that stack is not qualified at the gate ([performance](../specs/performance.md#corrections-qualified-on-the-m4)). A Heal and a Clone sampling across a 9,000-pixel frame, wider than one texture, drawn in tiles as an export streams them are within the spatial class on both paths, alone and before Detail (`gpu_tiles_tests`, worst block 0.031).

**Detail rerun cost.** Because repair precedes Detail, a repair tick reruns Detail over the changed box grown by Detail's reach (at most 48 px of denoise and 16 px of sharpen halo at full resolution): a 100 px Heal spot reruns about 0.11 MP of Detail, about 0.5% of a 24 MP frame. This is analysis from the declared halos, not a measurement; the GPU tick, chain and staged-sweep costs on a quiet host were set aside by the owner on 2026-10-11 and have not been run; the study's loaded-host tick figures are in the [numerics](corrections-numerics.md#gpu-form). Existing evidence bounds the other side only: a whole-window Detail drag is expected to miss the Fit budget ([Detail](detail.md)), and painting at 100% over three masked Presence layers misses 16 ms at p95 ([feature status](../features.md)); neither is a repair measurement.

### Frozen patches

**Operation.** A `patch` operation: `{id, kind: "patch", label, artifact, rect, opacity, input, provenance}`.

| Field | Rule |
| --- | --- |
| `label` | The display noun, 1..32 characters, from the producer (`Remove`; the test producer's `Patch`) |
| `artifact` | One `artifact-…` of kind `repair-patch`: `width × height` RGBA, IEEE half floats little-endian, RGB in the repair stage's linear representation and A the coverage `0..1`, not premultiplied; at most 64 MiB |
| `rect` | `[x, y, width, height]` in content pixels; `width`/`height` equal the artifact's, so the full-resolution composite is 1:1 with no resampling |
| `input` | The [input identity](#input-identity) at generation |
| `provenance` | `{module, task, …}`: at most 2 KiB of string and number fields the producer declares (model, version, seed) |

The host-owned `layer.artifacts` lists the patch operations' artifacts in operation order, each artifact once; admission refuses any other list. Composite: `out = in + c·(patch − in)` with `c = α·opacity/100`, and exactly `in` where `c = 0` or outside `rect`. Rendering binds prepared, verified bytes through `CapabilityModule::compile_bound` (which also receives the recipe's strokes) and never calls the producer. Patch words reach both GPU devices as resident storage blocks of the repair step ([delivered choices](#delivered-frozen-patch-choices)); missing or corrupt bytes fail `source-unavailable` naming the artifact.

#### Input identity

`input` is `{source, dimensions, upstream, earlier}`, four SHA-256 hex digests (or values) computed by one core function:

- `source`: the verified source fingerprint, its kind and the decoder's interpretation of it (a JPEG's orientation and decode; a RAW's unpack and development pipeline identity), excluding everything the recipe's layers set. The locator is not part of it.
- `dimensions`: the content stage's `[width, height]`.
- `upstream`: `render::spatial::prefix_hash` of the layers before the repair layer and the masks they read. Admission keeps only the source development there, so this is the RAW development's payload (white balance included).
- `earlier`: the canonical JSON of the repair layer's operations before this one (stroke and artifact identities included).

A patch is **stale** when its stored `input` differs from the current one, and each differing component is a reason: `source`, `dimensions`, `development` (from `upstream`) and `earlier-correction`. Later operations, downstream layers (Detail, colour, geometry), unrelated masks and a same-byte Locate change none of them; `spatial::prefix_hash` alone would miss `source` and `earlier`. A stale patch keeps rendering. `query.corrections` reports `stale: {reasons}`; the repair layer's `recipe.describe` row reports `stale: [{operation, name, reasons}]`; the desktop's row and status bar say it (`Remove 7 is stale · the RAW development changed since it was generated`). Moving the catalog with its `.artifacts` root keeps patches valid; copying only the database leaves them failing `source-unavailable` while history stays readable.

#### Candidates

A producer is any module task with `artifacts: true`; the test producer is a developer-only deterministic module. Candidates are disposable job results, never artifacts or JSON pixel arrays.

- **Publish.** The task calls `ModuleContext::publish_candidate` on its worker with the patch, its colour interpretation, coverage, `rect`, a preview (sRGB 8-bit, longest side ≤ 1,024) and provenance. The host validates bounds, finite values and alpha, converts to the stored `repair-patch` form on the worker and stores it under `<artifacts root>/tmp/candidates/<job_id>/`. A job holds at most **4** candidates of at most **64 MiB** each (patch plus preview); the host holds at most **1 GiB** across jobs. A publish over a bound is refused to the producer with `resource-limit`.
- **Address.** `{job_id, candidate}`, `candidate` `0..=3`. The task job also holds the asset, revision and `input` it was generated against.
- **Read.** `job.read`'s result gains `candidates: [{candidate, state: live | accepted | released, released?: budget | released | cancelled | closed, width, height, bytes, rect, preview: {width, height}, revision, input}]`. Pixels are read with `job.read-candidate {job_id, candidate, part: preview | patch, offset?, length?}` in chunks of at most 768 KiB, answered base64 with `{width, height, format, offset, length, total}`. Same ownership rules as `job.read`.
- **Release.** `job.release-candidates {job_id, candidates?, mutation: request}` releases explicitly. Past the global budget the host releases the oldest finished job's live candidates first. Cancelling the job releases them, and so does closing the catalog or ending the process; a task job is catalog-owned, so a client disconnecting releases nothing. A candidate being accepted is pinned and never released under it. Every release is reported on `job.read` with its reason. Files are deleted on release and swept when the catalog opens.
- **Accept.** `edit.accept-correction-patch {asset_id, mutation, job_id, candidate, opacity?}` (the module action, `preset: false`): the owner pins the candidate, checks it is live and belongs to the asset, checks `expected_revision` and that the candidate's `input` equals the current input for a new top operation (otherwise `conflict`, `data: {reasons}`), publishes the stored bytes as an artifact on the source worker, then replays and commits one ordinary atomic entry that appends the `patch` operation and its artifact reference, rechecking the revision and the input. A late or refused commit leaves only an unreferenced artifact for `artifact.collect`; it never leaves a partial edit or resurrects a removed or emptied photograph. Cancelling the request before the commit releases the pin. A second client accepting the same candidate after the first commit meets a changed revision or a changed `earlier`, and is refused.

#### Stale export acknowledgement

Extensions of the shipped methods, implemented as described here ([export](export.md#api)).

- **`export.plan`** answers `stale: [{operation, name, reasons}]` for the planned entry, always present and empty when nothing is stale.
- **`export.write`** accepts `acknowledge_stale: {entry_id, operations: [correction-…]}`. When the frozen entry has stale operations, the request must carry an acknowledgement naming that exact entry and exactly its stale operations; otherwise it is refused before any job with `conflict`, `data: {code: "stale-unacknowledged", entry_id, stale: [...]}`. An acknowledgement on an export with nothing stale is accepted and ignored. The accepted answer and the ready result echo `acknowledged_stale`. An omitted `entry_id` resolves to the current entry at acceptance, so a commit between plan and export makes the old acknowledgement name another entry and refuses it.
- **`batch.export`** accepts `acknowledge_stale: [{asset_id, entry_id, operations}]` (at most one per photograph). Each photograph's current entry is checked when its turn comes; one with stale operations and no exactly matching acknowledgement is skipped with code `stale-unacknowledged` and `{entry_id, stale}` in its skipped row, and the batch continues. Nothing is acknowledged implicitly or omitted silently. The desktop's batch report lists such skips and offers to export them with acknowledgements ([open questions](#open-questions)).

#### Delivered frozen-patch choices

Made under the owner's authorization:

- **One layer artifact per distinct patch, at most 16.** `layer.artifacts` is each patch's artifact once in operation order (two patches of identical bytes share one), within the host's 16 artifacts a layer; a seventeenth distinct patch is refused `resource-limit` (`16 patches is this photograph's limit`). The prepared-artifact admission (256 MiB) bounds what one stack can hold ready anyway.
- **A candidate-accepting action.** `ActionDescriptor::accepts_candidate` marks an action the owner runs through the accept flow; registration requires an `artifacts` effect, `preset: false`, a required `job_id` (an identity of a job, the one module parameter allowed that host spelling and kind) and a required `candidate` within `0..=3`. Such an action is never drafted: planned without a pinned candidate it is refused. Its label is `Add <label> <n>`; a patch's update is opacity only (`Patch 3 · Opacity 40`), a geometry, mode or source field on it is `validation`, and `query.correction-sources` refuses one.
- **The accept flow.** The owner answers a retry from the catalog's request log before anything is pinned, checks the session is on current, the revision, that the job is a task the caller may read, and the candidate's asset and input; a caller that may not wait (`call_unparked`) is `not-ready` with `data.reason: accept-publishes`. The pinned candidate travels to the source worker as an `artifacts` job (activity `artifacts.publish`, `Publishing patch`) with the call parked; on completion the owner records the artifact live and commits through the ordinary action path with the candidate in hand (`StageQuestions::accepting`), rechecking revision and input. A disconnect, or `job.cancel` of the accept job, drops the call and the pin before anything is committed.
- **Input identity.** `source` is the SHA-256 of the asset's fingerprint, source kind and RAW interpretation and decoded dimensions; `dimensions` is a value pair, the others hex digests. Candidates carry the input a new top correction had when their task was queued (`job.read`'s `input`); a stack that cannot say leaves it absent, and such a candidate is refused `conflict`.
- **Rows and reports.** `query.corrections` lists a patch as `{operation, name, kind: patch, index, label, artifact, rect, opacity, provenance, stale?}`. `recipe.describe`'s repair row carries `stale: [{operation, name, reasons}]`, omitted when empty.
- **The composite at a reduced stage.** Where a GPU fit frame draws the repair stage smaller than the patch was placed, each stage pixel takes the box means of the patch over its footprint (`Ā`, the coverage, and `P̄`, the coverage-weighted colour, pixels outside `rect` counting as none) and becomes `in + o·(P̄ − Ā·in)`: the full composite averaged with the input taken constant. The CPU reference computes the same at that stage.
- **GPU representation.** A spatial step of `luxforge-gpu` gains resident blocks: shared storage blocks packed before the program's block, written once while a tick hands the same allocations and keyed by a content key rather than hashed per tick. A repair step's resident blocks are its patches' words — their own half floats (two words a pixel) at the stage they were placed in, box means (four `f32` a pixel) at a reduced one — built once in a bounded 128 MiB cache shared with the reference's frames; the blocks buffers holding them are charged to each device's budget as every block is. Each patch is a phase of its own after the rest of its conflict group: the step's apply when last, a pass into the next state plane otherwise. A step needing more than 64 MiB of patch words falls back to the reference, named. `repair.wgsl` decodes the halves itself.
- **Stale export acknowledgement.** `export.write`'s accepted answer and ready result echo `acknowledged_stale` as the stale rows acknowledged (`[]` when nothing was stale); `batch.export` refuses two acknowledgements of one photograph.
- **The desktop.** The status bar counts stale patches (`1 patch stale`, `3 patches stale`), its tooltip naming each patch and why. An export refused `stale-unacknowledged` is held, not failed: a card names each stale patch and why and offers **Export anyway**, which sends the same export again as a new request acknowledging exactly the entry and operations the refusal named, and **Cancel**. A batch export's report lists each photograph it skipped as stale with its patches and reasons and offers **Export anyway · N photographs**, a new `batch.export` of exactly those photographs into the same folder, each acknowledged by the entry and operations its skipped row named.
- **The patch corpus family.** `repair-patch` is a required family of the spatial class. Its recipes are developer recipes (`"developer": true`): the run opens their catalog with the developer registry and calls `task.generate-proof-patches`, and the accept names its job as `{"started_by": "task.generate-proof-patches"}`, the job the run's latest call of that task started, once it is ready ([development](../engineering/development.md)). Three recipes cover patches among a Clone and a Heal, a large patch under Detail, and a stale patch after an earlier correction changed. The reduced-stage composite is held on the GPU to the CPU reference at the same stage by plan tests of a patch alone, one a Clone reads and overlapping patches (zero ΔE on the M4 Pro).

### Limits

| Limit | Value | Declared |
| --- | --- | --- |
| Operations per photograph | 100 | `operations.max` on the effect, listed by `module.list` and `schema.list`, and `limit` in `query.corrections`; the desktop reads it and never hard-codes it |
| Positions per stroke | 1,024 stored, 16,384 posted | the `paths` block of `schema.list` |
| Radius | 0.0005..0.25 content heights | `radius`'s declared range |
| Ranked sources | 8 | `count`'s declared range |
| Candidates | 4 per job, 64 MiB each, 1 GiB across jobs | the candidate seam |
| Frozen patches | 16 distinct per photograph; 64 MiB of patch words per GPU repair step | `resource-limit` at accept; the GPU step's named fallback to the reference |
| Regions, source windows, Heal scratch and iterations, GPU residency | The numerics study's and render integration's measured bounds | the shared rendering limits, refused with `resource-limit` |

## Delegated engineering choices

Frozen here under the owner's authorization, revisable by a focused plan update:

- `pixel` is renamed `repair` rather than adding an eighth stage; colour joins its insertion boundary; a repair layer outside the leading run is refused at admission.
- The operation number is the `id`; identities are spelled `correction-<n>`; `next` only grows, including across reset.
- Absolute `source-x`/`source-y` in the API, stored offsets in the payload.
- Feather and opacity are integers; the defaults are feather 50 and opacity 100 (the desktop's New correction values are view state).
- `update-correction` is one field-patch action with a label per field, not one action per gesture.
- `query.corrections` is added as the read of the layer.
- `brush-mask` keeps the name the plan gave it.
- Candidates live as files under the artifact root's `tmp/`, read in base64 chunks; a task job's candidates are released by explicit release, budget, cancellation or closing, never by a disconnect.
- Resizing keeps a stroke's stored positions; moving re-captures the posted path.
- The `correction` an add or update answers is read from the stack the request left current and stored with the request's answer, so a retry answers it without planning; the entry's stored parameters are the request as sent (an automatic add's resolved source is in its snapshot, not its parameters).
- Patch geometry is an axis-aligned rectangle in content pixels with no resampling at full resolution; a stale patch whose rectangle leaves a changed frame is clipped.
- Stale reasons are `source`, `dimensions`, `development` and `earlier-correction`.

## Open questions

No consequential product question is open. Two recorded defaults may deserve the owner's eye:

1. **Numbers after undo.** A number is never reused along one lineage, but a new branch after an undo may mint a number an abandoned branch used. Strict photograph-wide uniqueness would need a counter outside the snapshots; the default keeps snapshots self-contained.
2. **Batch export with stale patches.** The default skips such photographs, lists them with their reasons in the report and offers **Export anyway**, which exports exactly them into the same folder with acknowledgements. It matters only once AI Remove produces patches.

## Measurement

No quiet-host timing exists for Corrections, and a skipped measurement is not a pass. Two figures are arithmetic, not measurements. The Detail rerun area is above. Storage: one operation is about 165 bytes of compact JSON in the layer payload plus its stroke, stored once (37 bytes for a spot, about 750 for a 60-position stroke); an entry holding 100 operations carries about 16.5 KB of repair payload, and a history of 100 single adds about 0.83 MB in total, quadratic in the count as every snapshot is ([stroke store](../../crates/luxforge-core/src/path.rs)). The numerics study measured solver and GPU tick costs on photo-sized inputs on a loaded host. The integrated chain, staged sweeps, the reference input-grid rebuild and the whole-workflow timing on the M4 (24/60 MP JPEG and qualified RAW: input to overlay, stroke commit, source-handle drag, view changes, render, sample, export, peak RSS and GPU memory, disk per operation and history, idle CPU) were set aside by the owner on 2026-10-11 and have not been run. `cargo xtask editor-latency --mode corrections` paces a set-source Corrections stroke and a source-handle drag on 24/60 MP JPEGs and pairs each position with its drafted frame and the release with the repaired frame ([development](../engineering/development.md)); it is registered in the timing tier and has not been run (set aside by the owner on 2026-10-11).

## Acceptance and verification

Offline acceptance covers independent exact CPU fixtures and `gpu-qualification` on required Clone/Heal corpus families, including windows, staged sweeps and incremental ticks against whole evaluations; overlap, edges, tiny and large strokes, far sources, JPEG and continuous RAW; nonlinear geometry; UI/API parity and conflicts; original hashes; history, versions and reopen; preparation, missing-source recovery, Remove/Put back/Empty Removed; Select rendered previews; and real single and batch JPEG export. Frozen patches are verified separately: outside-coverage identity, input invalidation, stale-export acknowledgement, offline rendering, corrupt and missing artifacts, bounded candidate ownership, cancellation and races, crash recovery and catalog relocation. Model and provider qualification belongs to AI editing and cannot block the offline handoff.

**Qualified (M4 Pro, 2026-10-10):** the release gate on the required Clone and Heal families, including RAW and nonlinear-geometry stacks, with the one 60 MP gap above (its rerun was set aside by the owner on 2026-10-11 and has not been run); the `corrections` smoke and, on each supplied RAW, the `raw-corrections` smoke (history, versions, undo, redo, Restore, reopen, source hashes, Select's preview, Remove and Put back, Locate, the overlay through lens and perspective, GPU, reference, single and batch exports), both in the tiers; the photographic review recorded in the [numerics](corrections-numerics.md#the-corpus-review) as non-owner evidence. The owner reviewed the corpus cases blind on 2026-10-11 and accepted Heal as delivered ([numerics](corrections-numerics.md#the-corpus-review)). Set aside by the owner on 2026-10-11 and not run: the timing, memory and idle measurements, the `gpu-qualification` rerun of the 60 MP far-source Heal stack and the gate run of the `repair-patch` family. Empty Removed with correction strokes is held by the core's library tests (`remove_and_empty_keep_correction_strokes_every_surviving_entry_names`), not a desktop journey.

Run only targeted tests while building, `quick` once at each finished handoff, `rendered` at UI and image integration, and timing after the corresponding feature is complete on a quiet host; `full` with the supported RAW manifest precedes a milestone claim. Register the background-only `corrections` smoke scenario and its independent checks in the rendered tier. Update this design, module and API documentation, feature status and the user guide as behaviour is delivered. A skipped check is not a pass.
