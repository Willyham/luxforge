# Masking

Status: **design proposal. Nothing here is implemented and no owner decision has been recorded.** It proposes local adjustments — linear and radial gradients first, then brushes, then non-AI range selections — as a host concept over the delivered modules, with a [phased task plan](../../tasks/implementation-masking.json). The [proposals](#proposals-with-recorded-defaults) at the end carry defaults so the work can run on agent judgement once the owner authorizes it, exactly as the Basic and Presence defaults did; each one is the owner's to refine. It builds on the [module and API contract](modules-and-api.md), the delivered [Basic adjustments](basic-and-histogram.md), [Presence, mixer and vignette](presence-mixer-vignette.md), the [content-space edit rule](content-space-edits.md), the [UI components](ui-components.md) vocabulary, the [Develop workspace](develop-workspace.md) tool array and the [instant preview](instant-preview.md) proxy contract.

## Outcome and scope

A **mask** is an editable selection plus the adjustments that apply through it. Lightroom is the familiarity reference for names, ranges and gestures, not a rendering target: no value is claimed as Lightroom-equivalent, and every coverage equation is frozen by a numerical study against an independent `f64` reference before a control ships, as the [tone](basic-tone.md), [white balance](basic-white-balance.md), [colour](basic-colour.md), [presence](presence-study.md), [mixer](mixer-study.md) and [vignette](vignette-study.md) studies did.

In scope, in this order:

| Phase | Delivers |
| --- | --- |
| A | The mask model, persistence, targeting and the masked colour primitive, proved end to end by the **linear gradient** over the Basic layer |
| B | The **radial gradient**, the Subtract and Intersect modes, per-component and per-mask inversion, mask amount, reorder and duplicate, and masked **spatial** layers so Presence works through a mask |
| C | **Brushes**: multiple strokes per component, erase strokes, size and feather, flow, and brush-with-gradient combination |
| D | Non-AI **range selections**: luminance range, colour range, and a colour-constrained brush (the honest part of Lightroom's Auto Mask) |

Out of scope, with no placeholders drawn anywhere: every AI or model-based selection (Subject, Sky, People, Objects, Background, Depth), Lens Blur, clone and heal, copying masks between photographs (there is no second photograph yet), mask presets, raster mask import or export, and per-mask curves or detail controls that have no module yet. A "sky selector" without a model is **not** proposed; see [non-AI detection](#non-ai-detection-what-is-honest).

## What a mask is

A mask is a host object in the recipe, not a tool module and not a layer. [UI components](ui-components.md#control-kinds) already records the reason: enable, mask and opacity are properties of the recipe that every module shares, so a module must not be able to declare one.

```text
Recipe { layers: [Layer], masks: [Mask] }

Mask      { id, name, amount, invert, components: [Component] }
Component { id, name, mode, invert, kind, payload }
Layer     { id, effect_id, effect_format, payload, mask: Option<MaskId> }
```

| Field | Meaning |
| --- | --- |
| `Mask.name` | A display name, defaulted to `Mask 1`, `Mask 2`, …; renameable, never an identity |
| `Mask.amount` | 0..100, multiplies the composed coverage. Lightroom's per-mask Amount |
| `Mask.invert` | Inverts the composed coverage before `amount` |
| `Component.name` | A display name from the kind and a per-mask ordinal — `Brush 1`, `Radial 2` — renameable, never an identity. The ordinal comes from a monotonic per-mask counter and is **never reused**, so a history row naming `Brush 1` can only ever mean the one component it was written about |
| `Component.mode` | `add`, `subtract` or `intersect`; the first component of a mask is always `add` |
| `Component.invert` | Inverts that one component's coverage before it is combined |
| `Component.kind` | `linear`, `radial`, `brush`, `luminance-range`, `colour-range` |
| `Layer.mask` | The mask this layer is modulated by, or none for a global layer |

Masks live in the recipe beside the layers, so each history entry's complete immutable snapshot already carries them: undo, redo, preview, Restore, versions and lineage work on masks with no new machinery. A layer naming a mask that is not in the snapshot is an explicit `incompatible` error, never a silently unmasked layer.

### Composition

Coverage is a field `m(x, y)` in `[0, 1]`. A mask composes its components in order, starting from `m = 0`:

```text
c  = component coverage, inverted to 1 − c when the component says so
add        m = max(m, c)
subtract   m = min(m, 1 − c)
intersect  m = min(m, c)
M          = (amount / 100) · (invert ? 1 − m : m)
```

This is the Zadeh fuzzy-set algebra: union, complement and intersection. It is chosen over the product algebra (`m·(1−c)` for subtract) because it is **idempotent** — subtracting the same brush stroke twice is the same as subtracting it once — which is what makes a component list safe to reorder, duplicate and re-run. The product algebra is recorded as [proposal P2](#proposals-with-recorded-defaults).

Strokes inside one brush component do not use this algebra; see [brush](#brush-phase-c).

### Mask space

Every component's geometry is stored in **content-stage** coordinates, the stage that pixel-, colour- and spatial-stage layers already address ([content-space edits](content-space-edits.md)). A mask therefore travels with the picture through every quarter-turn, reflection and crop for free, exactly as a pixel-stage edit does, and a crop change never moves or invalidates it.

- **Positions** are stored as normalized fractions `x, y ∈ [0, 1]` of the content stage's width and height. Normalized storage is what makes a mask resolution independent, so a masked recipe stays [proxy eligible](instant-preview.md#render-what-the-display-can-show).
- **Distances** (a radius, a brush size) are stored in **mask-space units**, where one unit is the content stage's *height*. Evaluation maps a stored position to mask space as `u = x · W/H`, `v = y`, so `v` spans `0..1`, `u` spans `0..W/H`, and a circle is a circle whatever the aspect ratio.

A mask is only ever attached to a layer before the geometry tail, so its input stage always *is* the content stage. That is a validation rule, not a convention: a `finish`-stage or `geometry`-stage layer cannot carry a mask.

## How a mask reaches an effect

Lightroom's local adjustments are a fixed panel of sliders that belongs to a mask. Lightwell already has those sliders — in Basic, Presence and the colour mixer — and rebuilding them inside a mask would be a second implementation of every equation. Instead, **a mask is a target for the actions that already exist**.

- An effect declares `maskable: bool`. `lightwell.basic.adjust`, `lightwell.mixer.hsl` and `lightwell.presence.adjust` declare it; every other delivered effect does not.
- The host adds one optional top-level request field, `mask`, to every action of a maskable effect. `schema.list` lists it; sending it to any other action is a `validation` error. It is host-owned: no module parses it, and no module's `parse`, `plan` or `compile` sees it.
- `edit.set-basic {asset_id, exposure: 0.4}` edits the global Basic layer, as today. `edit.set-basic {asset_id, mask: "…", exposure: 0.4}` edits the Basic layer bound to that mask, committing it on the first non-neutral field and updating it in place afterwards, exactly as the global one behaves.
- `single_layer` becomes *at most one layer of this effect per target*, where the global layer and each mask are distinct targets. Two layers of one effect with the same target still fail with `validation: ambiguous <title> layers` and nothing is rewritten.

The consequences are worth stating plainly: local Temperature, Tint, Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance, Saturation, Texture, Clarity, Dehaze and the eight-range colour mixer all arrive with the first masked layer, because they are the modules that already exist. Tone Curve and Detail inherit masking on the day they declare `maskable`.

### Order

Within a stage region the host places layers by the effect's declared `order` and never moves an existing layer. Masked layers need one more rule, because a mask list is reorderable and several masks can hold a layer of the same effect:

1. A masked layer of an effect is placed after the global layer of that effect.
2. Masked layers of one effect are ordered by their mask's index in `recipe.masks`.
3. `mask.reorder` re-sorts exactly those layers, as one host transaction, and nothing else moves.

So overlapping masks apply in the order the mask list shows, which is visible, reorderable and stated in the panel. Lightroom does not tell you this; we do.

## Component kinds

Every equation below is a **proposal frozen by the mask study** (`docs/design/mask-study.md`, written in phase A against an independent `f64` reference, like the vignette study). `smooth(s) = s²(3 − 2s)` throughout, so every falloff is C¹ and symmetric.

### Linear gradient (phase A)

Payload `{x0, y0, x1, y1}`: the two ends of the gradient axis as normalized positions, `p0` at coverage 0 and `p1` at coverage 1.

```text
t = clamp(((p − p0) · (p1 − p0)) / ((p1 − p0) · (p1 − p0)), 0, 1)
c = smooth(t)
```

A zero-length axis is a `validation` error. The gesture is Lightroom's: drag from the untouched side toward the affected side; the three drawn lines are `p0`, the midpoint and `p1`. Rotation is inherent in the two endpoints, so there is no separate angle to keep consistent, and both endpoints have number fields.

### Radial gradient (phase B)

Payload `{x, y, radius_x, radius_y, angle, feather}`: centre as a normalized position, the two radii in mask-space units, `angle` in degrees (−180..180) and `feather` 0..100.

```text
(a, b) = (p − centre) rotated by −angle
r      = sqrt((a / radius_x)² + (b / radius_y)²)
r0     = 1 − feather / 100
c      = 1                            r ≤ r0
       = 0                            r ≥ 1
       = smooth((1 − r) / (1 − r0))   otherwise      (feather = 0 is the hard edge at r = 1)
```

**Inside is selected.** Lightroom's radial affects the outside until you tick Invert; ours affects the inside, which is what a person drawing an ellipse around a face expects, and the component's own Invert gives the other reading. This is [proposal P3](#proposals-with-recorded-defaults).

### Brush (phase C)

A brush component holds an **ordered list of strokes**, which is what makes "several brushes in one mask" ordinary rather than a special gesture. The component is the thing a person names, combines, inverts and subtracts with; the strokes inside it are how its coverage was drawn, and they are combined before anything is rendered. [History granularity](#history-granularity) states how that maps onto entries.

```text
Stroke { points: [[x, y], …], size, feather, flow, erase }
```

`size` and `feather` are the brush at the moment the stroke was made, in mask-space units and 0..100; `flow` is 0..100; `erase` marks a stroke that removes coverage. One stroke's coverage is the maximum over its segments of a capsule profile — for a segment `A→B`, `d` is the distance from the pixel to the segment (to the point itself for a one-point stroke):

```text
R = size,  f = feather / 100
s = 1                            d ≤ R(1 − f)
  = 0                            d ≥ R
  = smooth((R − d) / (R · f))    otherwise
stroke = s · flow / 100
```

Strokes accumulate inside the component in stored order, so painting the same area twice builds up and erasing removes:

```text
add     c = 1 − (1 − c) · (1 − stroke)
erase   c = c · (1 − stroke)
```

Maximum along a stroke and screen union across strokes is deliberate: one pass of the brush has one density whatever the pointer's sampling rate, so coverage does not depend on how fast the hand moved or on how the desktop decimated the path, while a second pass is a second object and does build up. **Density is not delivered**: its Lightroom meaning depends on a build-up model along a single stroke, which would make the result depend on stamp spacing and therefore on resolution. That is [proposal P4](#proposals-with-recorded-defaults); a per-stroke radius for tablet pressure is later work and the payload's point shape is chosen so it can be added without a format rewrite.

### Luminance range and colour range (phase D)

Both read the **input pixel of the operation they modulate**, which costs nothing during a render (the value is in the row being processed) and costs one existing `O(layers)` sample at a point query.

- **Luminance range** `{low, low_feather, high, high_feather}` over the sRGB-encoded luminance of the input pixel, so the numbers mean what the histogram shows: full coverage between `low` and `high`, `smooth` shoulders of the given widths outside them.
- **Colour range** `{samples: [[r, g, b], …], refine}`: up to five sampled colours and one refine slider, with coverage from Oklab distance to the nearest sample. The exact metric and the refine mapping are chosen in the phase-D study; the [mixer study](mixer-study.md) already establishes the Oklab basis this reuses.

These two are what makes "select the sky" practical without a model: a luminance range plus a colour range plus a subtract brush selects a sky in the photographs a person actually edits, and every part of it is deterministic, inspectable and reproducible.

## Processing contracts

### The compiled mask

The host compiles a `Mask` against the stage its layer receives into a `CompiledMask`, at the same point in `compile` where a module's payload becomes a `Processing`:

- `evaluate(x, y) -> f32` at stage pixel centres, pure and position-only, so the rasterizing pass and `render.sample` cannot disagree.
- `bounds() -> Region`, a conservative rectangle outside which coverage is exactly zero. A colour run skips those spans and a spatial tiling copies those tiles, so a small brush mask on a 60 MP frame costs almost nothing.
- `min_feature_px(stage) -> f32`, the smallest feature the mask draws at that stage, which the proxy path uses (below).
- Brush components build a uniform grid index over mask space at compile time, sized from the component's largest stroke radius, so a pixel tests only the segments in the neighbouring cells. Build cost is `O(segments)` and never touches a pixel.

No full-resolution mask plane is ever allocated. That is not an optimization, it is the [point-query rule](../engineering/performance-rules.md#rules): a mask must be answerable for one pixel in bounded time, or `render.sample` could not equal the rendered byte.

### Masked colour and masked spatial

Two of the five host primitives gain an optional mask. No new primitive is added and no module code changes.

- `ColorOperation { units, mask: Option<Arc<CompiledMask>> }`. A masked operation is still one operation in its segment's ordered list, but it is evaluated against a copy of the row span it was given and blended back: `out = (1 − M)·in + M·units(in)`, per channel, in linear light, before the run's single clamp and quantization. The copy is one row chunk from the existing 64 MiB float scratch budget, reserved before it allocates. Unmasked operations keep today's exact byte path, including the identity source buffer.
- `SpatialOperation { units, mask: Option<Arc<CompiledMask>> }`. The mask changes nothing about the halo, the tiling or what a unit reads: the operation still reads the finished frame before it and writes the next one. Only the write is blended, at the output rectangle, against the same input the tile already holds. A tile entirely outside `bounds()` is a copy.

Blending in linear light against the operation's own input is the compositing model the [Lightroom research](../research/lightroom/geometry-masks-and-retouching.md#a-mask-is-an-editable-selection-plus-an-effect) describes as illustrative, and it is stated here as Lightwell's own contract, not as a claim about Adobe's.

### Point queries and proxies

- `render.sample` evaluates `CompiledMask::evaluate` at the pixel it is sampling, so a sampled byte equals the rendered byte for a masked layer exactly as it does for the position-dependent vignette unit. A masked colour layer adds no rasterization. A masked spatial layer keeps the existing declared exception (one tile plus halo), unchanged.
- A masked recipe is **proxy eligible**: normalized geometry means the mask compiled against the proxy stage is the same field at a smaller scale, so the proxy render remains the exact recipe at proxy size. The one caveat is sampling: a feature thinner than a proxy pixel aliases. When `min_feature_px` at the proxy stage is below 2, the host evaluates the mask (not the effect) with a 2 × 2 supersample per pixel and the result is marked `proxy_approximate`, joining the existing spatial-layer approximation in the result, the `preview_displayed` event and the state summary. That is [proposal P5](#proposals-with-recorded-defaults).

### Geometry for the desktop

A mask is edited in content coordinates while the person sees the cropped, straightened, rotated output, and a brush cannot afford a round trip per pointer move ([performance rule 12](../engineering/performance-rules.md#rules)). The geometry tail is exact transforms plus at most one crop, so the content→output map is always affine. One new read-only host method answers it once per gesture:

```text
render.transform {asset_id, entry_id?} -> {content: {width, height}, output: {width, height}, forward: [6], inverse: [6]}
```

The desktop maps pointer positions and draws handles locally from that matrix, with no hop per move, and `render.locate` keeps its existing job for picks.

## Host commands

Masks are host commands in their own namespace, as `history.*` and `version.*` are, not a tool module — a module commits layers and must not rewrite the recipe. The host declares them with the **same** `ActionDescriptor`, `ParameterDescriptor` and `Control` types modules use, so they go through one generic validation path, appear in `schema.list`, draft through the existing `draft.*` lifecycle, and the desktop generates their number fields with the widgets it already has.

| Method | Purpose |
| --- | --- |
| `mask.list {asset_id, entry_id?}` | Every mask with its components, values, amount, invert and the layers bound to it. Read-only, no history |
| `mask.create {kind, …geometry}` | A new mask whose first component is an `add` component of that kind |
| `mask.delete {mask}` / `mask.rename {mask, name}` / `mask.duplicate {mask}` | Mask lifecycle. Deleting a mask deletes the layers bound to it, named in the history label |
| `mask.set-amount {mask, amount}` / `mask.set-invert {mask, invert}` | Whole-mask modifiers |
| `mask.reorder {mask, index}` | Moves a mask, and with it the masked layers, by the ordering rule |
| `mask.add-component {mask, kind, mode, …geometry}` | A second, third, … component, with its mode given explicitly |
| `mask.set-component {mask, component, …geometry}` | A field patch over that component's geometry |
| `mask.set-component-mode {mask, component, mode}` / `mask.set-component-invert {…}` | Change a component's role after the fact |
| `mask.delete-component {mask, component}` / `mask.reorder-component {mask, component, index}` | Component list edits |
| `mask.add-stroke {mask, component, points, size, feather, flow, erase}` | One brush stroke, phase C |
| `mask.delete-stroke {mask, component, stroke}` | Undo a single stroke without undoing the history entries after it |

Every one of them is a normal mutation: mutation envelope, request deduplication, revision check, one history entry, one immutable snapshot. A gradient drag and a brush stroke are each **one** entry, because each is one draft: `draft.begin` on pointer-down, `draft.set` for the accumulated geometry, `draft.commit` on release, with the existing conflict, Discard and Reapply behaviour unchanged.

### History granularity

A component is the durable object; a stroke, a handle drag and a slider are edits *to* it. That gives the per-stroke history the owner asked for without a second history model:

| Gesture | History entry | What changed in the recipe |
| --- | --- | --- |
| First brush stroke on a new mask | `Add brush` | A mask, a brush component named `Brush 1`, one stroke |
| Second stroke on the same component | `Update Brush 1` | One stroke appended to `Brush 1` |
| Drag a radial's handle | `Update Radial 1` | That component's geometry |
| Add a subtract brush to the same mask | `Add subtract brush` | A second component, `Brush 2`, mode `subtract` |
| Change `Brush 2` to intersect | `Brush 2 intersect` | That component's mode |
| Raise exposure through the mask | `Mask 1 exposure +0.45` | The masked Basic layer's payload |

So `history.undo` walks back one stroke at a time, because each stroke is one entry and each entry already stores the complete recipe. Nothing special is needed for that: it falls out of the delivered snapshot model. Rendering never sees the entries — it sees one component whose strokes are already combined into one coverage field, and one mask whose components are already combined into one `M`, so fifty strokes cost one masked operation and not fifty.

An entry's label names the mask and the component (`Mask 2 · Update Brush 1`) when more than one mask exists, because a history list shared with every other module cannot afford `Update Brush 1` alone. The label is rendered at commit and stored with the entry, exactly as a module's `summary` template is.

Two things follow from strokes being objects rather than events:

- **`mask.delete-stroke` is a forward edit, not an undo.** It removes one stroke and appends one entry, so a stroke made ten entries ago can be removed while everything after it stays. `history.undo` still walks entries. The two never mean the same thing and the panel names them differently.
- **Order matters only where it must.** Add strokes combine by screen union, which is commutative, so their order is irrelevant and removing one is well defined. An erase stroke does not commute with an add, so a component stores its strokes in order, and deleting or reordering across an erase changes coverage in the way the stored order states. The component list shows that order.

One new parameter kind is needed, and only one: `points {points_min, points_max}`, an ordered list of `[x, y]` normalized pairs, validated exactly as `curve` already is but without the increasing-x and monotone rules and with a larger bound. It is a parameter kind, not a control kind: the brush is a canvas interaction, and no panel widget edits a path.

Two new canvas interactions join `point-pick`, `sample-apply` and `crop-frame`:

- `mask-shape {create_action, set_action, …}` — the host's handle editor for linear and radial components, editing a transient draft of the named parameters and committing once, exactly as `crop-frame` does for the crop rectangle.
- `brush-paint {action, points, size, feather, flow, erase}` — the host's stroke capture: it accumulates the path, decimates it to the declared tolerance, and commits one stroke. It is deliberately **not** mask-specific and is not named for masks: it fills the declared parameters of whatever action declares it, so the corrections proposal (`docs/design/corrections.md`, an unmerged owner-review proposal) declares the same interaction for Clone, Heal and AI Remove rather than a second brush. See [one brush system](#one-brush-system).

## The Develop workspace

Mask is the canvas-takeover mode the [tool array](develop-workspace.md#tool-array) already reserves a row for, entered from the mode strip, from `M` or from the command palette.

While it is active the tools panel shows the **Masks panel** in place of the module sections:

- A list of masks: name, an amount readout, a non-neutral dot, a visibility eye (view state, commits nothing) and a context menu with Rename, Duplicate, Invert, Delete. **New mask** is a split button naming the kinds.
- Selecting a mask opens it. Its **component list is always visible**: one row per component with the kind's icon, a three-way `Add / Subtract / Intersect` segmented control, an Invert toggle, a drag handle and a delete button. Hovering a row shows that component's own contribution in the overlay; selecting it shows its handles on the canvas and its number fields beneath the row.
- Under the component list, an **Add** row offers each component kind for the next canvas gesture, with the mode chosen before the gesture starts, not guessed from a modifier key afterwards.
- Under that, the **adjustments**: the maskable modules' own generated sections — Basic's three groups, Presence, the colour mixer — bound to this mask. Same widgets, same draft lifecycle, same `Copy as JSON request`, with `mask` in the copied request.

Canvas behaviour: the selected component draws its handles (linear: the three lines with an end handle each and a rotation grip on the midpoint; radial: the ellipse with four radius handles, a centre handle, a rotation grip and a feather ring; brush: the cursor's size and feather circles). `[` and `]` change brush size, `Shift+[` and `Shift+]` feather, holding Option/Alt erases while the stroke lasts. An in-progress stroke is drawn on the canvas by the desktop as the pointer moves, so path feedback never waits on a render, and the drafted picture follows one frame behind it exactly as a slider's does.

The **overlay** is per-client view state through `workspace.set {mask_overlay, mask_overlay_colour}`: off, a tinted overlay of the selected mask (red by default), the mask alone on black, or the image through the mask on black. It is computed by the preview worker beside the frame it already renders, as one byte per display cell on the grid the clipping overlay already defines and bounds, so it costs no second render and allocates no full-resolution plane. `Shift+M` toggles it; `O` keeps meaning thirds everywhere.

Leaving Mask mode returns the tools panel. A mask's layers stay in the recipe list on the left, grouped under their mask's name with the mask's own row, so the durable processing order stays visible where it has always been.

### Where this improves on Lightroom

Every one of these is the same feature made explicit rather than a new feature:

| Lightroom | Lightwell |
| --- | --- |
| Adding a second gradient to one mask hides behind Add-versus-New buttons and an unlabelled component tree | One always-visible ordered component list, with each component's mode as a control you can change afterwards |
| The mode of a new component is decided by which button you pressed and cannot be changed later | Mode, inversion and order are properties of a component, editable at any time |
| Overlapping masks apply in an order the UI does not state | Masks apply in the order the list shows, and the list is reorderable |
| Brush settings are modal state; a stroke cannot be revisited | Every stroke is an object with its own size, feather, flow and erase flag, deletable on its own |
| Gradient geometry is pointer-only | Every handle has a number field, and every gesture is one documented command |
| Density and Flow interact in ways the documentation does not settle | One delivered stroke amount with its build-up rule written down, and the missing control named with its reason |

## Resource and responsiveness constraints

Declared limits, each with a `resource-limit` error naming it — the [limits table](architecture.md#rendering-and-limits) gains this block:

| Limit | Value |
| --- | --- |
| Masks per recipe | 16 |
| Components per mask | 32 |
| Strokes per brush component | 64 |
| Points per stroke | 1024 after decimation |
| Points per mask | 8192 |
| Segments tested per pixel by a brush component | 64 (the grid index's cell occupancy, checked at compile time) |
| Masked colour layers per recipe | 16 |
| Masked spatial layers per recipe | 4 — each is a stage boundary, so each is a sequential full frame |
| Serialized mask bytes per recipe | 256 KiB |

The last one is load-bearing. Every history entry stores a complete snapshot ([performance rule 10](../engineering/performance-rules.md#rules)), and a brush session appends an entry per stroke, so mask bytes are multiplied by the number of entries. Three things keep that bounded, and the plan measures the result rather than assuming it: the desktop decimates a captured path to a stated tolerance before it is ever sent; coordinates are stored at the precision a 16384 px side can resolve and no more; and the per-recipe bound above fails explicitly instead of growing. Sharing unchanged mask blobs between snapshots by content hash is [proposal P6](#proposals-with-recorded-defaults) and is a measurement, not a default.

### Stroke storage

Every history entry stores a complete recipe, not a delta ([history](../specs/edit-history.md), [persistence](architecture.md#persistence)). One stroke is one entry, so an entry embedding its component's whole stroke list copies every earlier stroke in that mask, and storage grows with the **square** of the stroke count. Measured on the shape the payload actually has — a 100-point stroke serializes to about 1.8 KiB — 200 strokes embed 20,100 stroke copies and cost about **37.5 MB** across history, against 364 KiB of distinct stroke data.

The fix, decided by the owner on 2026-09-23, is a **content-addressed stroke store**: each stroke is stored once under a hash of its contents, and an entry's recipe lists its strokes' hashes instead of embedding their points.

| 200 strokes of 100 points | Embedded | Content-addressed |
| --- | --- | --- |
| Distinct stroke data | 364 KiB | 364 KiB |
| Stored across history | 37.5 MB | 1.08 MB |

Why this shape and not another:

- **Entries stay full snapshots.** One lookup previews, undoes or restores; nothing is replayed, and the history graph, its branches and named versions are untouched. Pure deltas were rejected for the opposite reason: they make every entry depend on all the entries before it and require a replay to rebuild one.
- **A missing or corrupt stroke fails explicitly** and names the stroke and the entry, exactly as an unavailable effect and an unknown component kind do. It is never silently dropped and never rendered as an empty stroke.
- **It is a host store for paths, not a mask table.** Corrections has the identical problem, so the store, the hash and the failure behaviour belong beside the recipe, not inside the mask model.

It does **not** make storage linear, and the design does not claim that. An entry still holds one reference per stroke, so the growth stays quadratic; what changes is the constant, from about 1.8 KiB per stroke per entry to about 35 bytes, a factor of roughly 53. That is decisive at the sizes a person reaches — 200 strokes is 1 MB rather than 37 MB — and it returns at sizes they do not: about 19 MB at 1000 strokes and 105 MB at 2400. If that ever binds, the escape hatch is to content-address the *list* as a hash chain, one constant-size node per stroke, which is genuinely linear at the cost of an O(strokes) walk to rebuild a list; it is recorded here and not built.

The store changes the catalog format again, to 6. Pre-release rules allow that: an unsupported format is refused explicitly without rewriting anything ([current shapes only](../../AGENTS.md)). It lands in phase C **before the first brush ships**, so no catalog ever holds embedded stroke points. Until it lands, the declared caps below are what bound the growth, and they refuse the excess with `resource-limit` rather than letting a catalog grow without limit.

### One brush system

A brush is a host primitive, not a feature of masking. Three things are defined in the core and shared:

| Primitive | Shared by |
| --- | --- |
| The `points` parameter kind and its decimation contract | Any action taking a path |
| The stroke — an ordered list of add and erase strokes with size, feather and flow, and the frozen accumulation rules | The brush mask component; the corrections repair operations |
| The `brush-paint` canvas interaction and the content-addressed stroke store | Both, and anything later that paints |

The corrections proposal (`docs/design/corrections.md`, an unmerged owner-review proposal) asks for a `brush-mask` interaction with content-space brush geometry for Clone, Heal and AI Remove. That is this interaction under another name. Masking is implementing it first, so masking names it generically and puts it in the host; corrections declares it for its own actions and adds no second brush. Neither design owns it.

Responsiveness keeps the delivered targets and adds no new class of work: a mask evaluation is a handful of flops per pixel per masked layer, on top of the units it modulates, and the bounds rectangle removes it entirely outside the selection. The [provisional slider target](instant-preview.md#goal) — p95 under 16 ms, acceptable under 32 ms — applies unchanged to a masked slider drag and to a brush stroke's drafted frames, and a measured miss is reported with its figures.

## Non-AI detection: what is honest

The owner asked whether non-AI sky and object detection is worth considering. The answer this design proposes:

- **Yes** to luminance range, colour range and a colour-constrained brush (phase D). They are deterministic, cheap, per-pixel, reproducible and inspectable, they need no model asset and no inference job, and in combination with a subtract brush they select skies, skin, foliage and water well enough to be the tool people reach for.
- **No** to a control named "Sky" or "Subject" backed by heuristics. A hand-written sky detector — blue-ish, bright, connected to the top edge, gradient-aligned — fails on sunsets, overcast, reflections, backlit subjects and anything shot upward, and shipping it under that name would promise a selection the code cannot make. Claiming it would need a labelled corpus and a measured accuracy figure, which is a research task, not a feature.
- Lightroom's **Auto Mask** is worth having and is not AI, but it is edge- and connectivity-constrained, not per-pixel. The tractable, analytically evaluable part is a colour-constrained brush: coverage is multiplied by the similarity of the pixel to the colour sampled where the stroke began. That is what phase D delivers, under that name, with the difference from Lightroom's behaviour written down. A genuinely edge-aware refinement (a guided filter over a coarse selection) is [proposal P7](#proposals-with-recorded-defaults) and needs its own study, because a guided filter is a neighbourhood operation and a mask is currently a point function.

## Verification and acceptance

Each phase ends with the evidence its claims need; a phase is not complete without it.

- **Exactness.** Every coverage equation is transcribed twice — the production unit and an independent `f64` reference under `crates/lightwell-core/tests/reference/` — and tested to be bit-identical for the same inputs, as the vignette unit and its reference already are. Masked rendering is proved against stepwise references on synthetic fixtures: `M = 0` is the unmasked input byte for byte, `M = 1` is the unmasked effect byte for byte, and a half-covered fixture matches the blend computed independently.
- **Sample equals render.** For every component kind, on JPEG and RAW paths, `render.sample` at a pixel equals the rendered byte at that pixel, including inside a feather band and at a bounds edge.
- **Geometry.** A mask committed before a crop, a straighten, a quarter-turn and a reflection lands on the same content pixels afterwards, proved by rendering, not by inspection.
- **Proxy.** A proxy render of a masked recipe equals the exact recipe rendered against the exact downscale of the source, byte for byte, at every stage where `min_feature_px` is at least 2, and is marked approximate below it.
- **Retention.** A stored mask whose component kind this build does not know fails rendering with `incompatible: unknown mask component <kind>` naming it, keeps every byte, and still lists through `mask.list`, `recipe.describe` and history. Deleting a mask is an explicit action that names the layers it removed.
- **UI/API parity.** Every gesture — creating a gradient, dragging a handle, painting a stroke, changing a component's mode, moving a mask — produces the same stack, history, pixels and errors as the equivalent JSON request from an independent client, and the panel's `Copy as JSON request` produces exactly the request that was sent.
- **Rendered evidence on the M4 Mac.** New smoke scenarios with correlated state: `mask-linear` (create, drag, commit, masked Exposure, undo, reopen), `mask-combine` (a radial with a subtract brush and an intersect range, each component's overlay, reorder), `mask-brush` (several strokes, an erase stroke, feather at two sizes, a stroke deleted, the payload bound approached) and `mask-crop` (a mask under a rotated crop at Fit and at 100%).
- **Measurement.** `editor-performance` and `editor-latency` on 24 MP and 60 MP before and after each phase, plus a brush-specific workload: drafted frames per second during a continuous stroke, the settled exact histogram after a stroke, mask compile cost against stroke count, and peak process memory across a 100-stroke session with its snapshot growth recorded.

## Phases in one line each

- **A — foundation and the linear gradient.** The model, persistence in catalog format 5, the `mask` target field, `CompiledMask`, the masked colour primitive, `render.transform`, the `mask.*` commands the gradient needs, the Mask mode and panel, the overlay, and the mask study that freezes the composition algebra and the gradient falloff. At the end of A a person can drag a gradient and lift the sky's exposure, from the panel or from JSON.
- **B — radial and combination.** The radial component, Subtract and Intersect, inversion at both levels, amount, reorder and duplicate, the component-list UX, and the masked spatial primitive so Presence runs through a mask.
- **C — brushes.** The `points` parameter kind, the content-addressed stroke store in catalog format 6, the `brush-paint` canvas interaction and its draft, the brush component with multiple strokes, erase strokes, size, feather and flow, the grid index and its cost contract, path decimation and the payload bounds, and brush-over-gradient combination. The path kind, the stroke list and the interaction are host primitives the corrections design reuses rather than reimplements.
- **D — range selections.** Luminance range, colour range and the colour-constrained brush, each with its own study, plus the honest statement in the user guide about what they do and do not select.

## Proposals with recorded defaults

Each is the owner's to decide. The default is what the work runs on if implementation is authorized without a separate answer, and each stays recorded in this design so a later change is a normal edit.

| # | Question | Recorded default |
| --- | --- | --- |
| P1 | Are masks a target for the existing modules, or a separate local-adjustment module with its own sliders? | A target. Local Temperature through Dehaze arrive with no new equations, and Tone Curve and Detail inherit masking by declaring one flag |
| P2 | Zadeh (`max`/`min`) or product algebra for combining components? | Zadeh, for idempotence |
| P3 | Does a radial gradient select inside or outside by default? | Inside; the component's Invert gives the other reading |
| P4 | Flow and Density, or one amount? | Flow only, with max along a stroke and screen union across strokes; Density named as not delivered, with its reason |
| P5 | What happens to a brush stroke thinner than a proxy pixel? | The mask field is supersampled 2 × 2 and the frame is marked approximate |
| P6 | Do snapshots share unchanged mask blobs by content hash? | **Decided by the owner on 2026-09-23: yes, a content-addressed stroke store, in phase C before the first brush ships.** Embedded strokes grow quadratically at about 1.8 KiB per stroke per entry; see [stroke storage](#stroke-storage) |
| P7 | Is an edge-aware refinement (guided filter) part of phase D? | No; it is a neighbourhood operation over a point function and needs its own design |
| P8 | Should Mask mode replace the tools panel, or sit beside it? | Replace it while the mode is active, as the crop draft holds its own section open today |
| P9 | Phase order | A, B, C, D as listed; brushes before range selections, because a brush is what makes a gradient usable |

## References

- [Tool modules and the shared core](modules-and-api.md) · [Content-space edits](content-space-edits.md) · [Instant previews](instant-preview.md) · [Presence, colour mixer and vignette](presence-mixer-vignette.md) · [Basic and histogram](basic-and-histogram.md)
- [Develop workspace](develop-workspace.md) · [UI components](ui-components.md) · [Architecture](architecture.md) · [Performance rules](../engineering/performance-rules.md)
- The corrections design (`docs/design/corrections.md`), an unmerged owner-review proposal and the other consumer of the shared brush
- [Lightroom geometry, masking and retouching research](../research/lightroom/geometry-masks-and-retouching.md) · [darktable geometry, masks, blending and retouching research](../research/darktable/geometry-masks-and-retouching.md)
