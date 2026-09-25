# Masking

Status: **authorized by the owner on 2026-09-23; phases A, B, C and D are implemented and verified on the M4 Mac.** UI/API parity is verified from both sides, by [`command_contracts.rs`](../../crates/lightwell-core/src/mask/command_contracts.rs) and the [masking chapter of `editor-acceptance`](../engineering/development.md#the-masking-acceptance-chapter). [Verification and acceptance](#verification-and-acceptance) states what the four `mask-*` smoke scenarios and the exactness, geometry and proxy tests demonstrate; [performance](../specs/performance.md#a-painted-strokes-own-latency) records a brush stroke's own latency, which misses the provisional slider bound at 24 and 60 MP. Outstanding: the range selections' equations are proved only over flat synthetic patches ([range study](range-study.md#limitations-and-what-this-study-does-not-settle)), so a photographic corpus of skies, faces and noisy shadows remains untested. It builds on [Basic adjustments](basic-and-histogram.md), [Presence, mixer and vignette](presence-mixer-vignette.md), [content-space edits](content-space-edits.md), [UI components](ui-components.md) and the [Develop workspace](develop-workspace.md).

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

This is the Zadeh fuzzy-set algebra: union, complement and intersection, frozen by the [mask study](mask-study.md#composition) over the product algebra because it is **idempotent** and order-independent *exactly*, bit for bit — subtracting the same brush stroke twice is the same as subtracting it once — which is what makes a component list safe to reorder, duplicate and re-run. The product algebra fails both by up to `0.248209` of coverage and `1.943e-16` respectively; the study also records what the choice costs, a C⁰ crease where two components cross. That settles [proposal P2](#proposals-with-recorded-defaults).

Strokes inside one brush component do not use this algebra; see [brush](#brush-phase-c).

### Mask space

Every component's geometry is stored in **content-stage** coordinates, the stage that pixel-, colour- and spatial-stage layers already address ([content-space edits](content-space-edits.md)). A mask therefore travels with the picture through every quarter-turn, reflection and crop for free, exactly as a pixel-stage edit does, and a crop change never moves or invalidates it.

- **Positions** are stored as normalized fractions `x, y ∈ [-1, 2]` of the content stage's width and height — the frame is `[0, 1]`, with one stage extent of overshoot on each side, because a gradient dragged from off the canvas and a radial centred outside the frame are ordinary edits, and a gradient whose coverage never reaches 0 inside the frame cannot be expressed with endpoints inside it. Every frozen equation is total on finite inputs, so the range is a validation and payload rule only ([mask study](mask-study.md)); it was widened from `[0, 1]` on 2026-09-23 on that study's recommendation. Normalized storage is what makes a mask resolution independent, so a masked recipe stays [proxy eligible](instant-preview.md#render-what-the-display-can-show).
- **Distances** (a radius, a brush size) are stored in **mask-space units**, where one unit is the content stage's *height*. Evaluation maps a stored position to mask space as `u = x · W/H`, `v = y`, so `v` spans `0..1`, `u` spans `0..W/H`, and a circle is a circle whatever the aspect ratio.

The [mask study](mask-study.md#mask-space) freezes both spellings of that map — the stored-position one above and `u = (px + 0.5)/H`, `v = (py + 0.5)/H` for a pixel centre, which are within `2.220e-16` of each other and not bit-identical — and the legal range of a stored distance, `1e-4` to `64` mask-space units, the floor being what bounds every divisor a falloff takes. It also records the one question it does not settle: whether stored positions stay within `[0, 1]`, which forbids a gradient endpoint or a radial centre outside the frame.

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

The linear, radial and brush equations below are **frozen by the [mask study](mask-study.md)** against an independent `f64` reference, in the exact form and order a production unit transcribes; the range equations are frozen by the [range study](range-study.md). `smooth(s) = s²(3 − 2s)` throughout — the delivered [vignette](vignette-study.md#falloff-midpoint-and-feather)'s falloff, kept so the editor has one falloff shape — so every falloff is C¹ and symmetric.

### Linear gradient (phase A)

Payload `{x0, y0, x1, y1}`: the two ends of the gradient axis as normalized positions, `p0` at coverage 0 and `p1` at coverage 1.

```text
t = clamp(((p − p0) · (p1 − p0)) / ((p1 − p0) · (p1 − p0)), 0, 1)
c = smooth(t)
```

An axis shorter than one legal distance (`1e-4` mask-space units) is a `validation` error, which is the study's spelling of "a zero-length axis is rejected": the axis length is itself a distance, so it takes the same bound and the divisor `l2` is never below `1e-8`. The gesture is Lightroom's: drag from the untouched side toward the affected side; the three drawn lines are `p0`, the midpoint and `p1`. Rotation is inherent in the two endpoints, so there is no separate angle to keep consistent, and both endpoints have number fields.

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

The study freezes this as the one-branch `smooth(clamp((1 − r)/(1 − r0), 0, 1))`, proved bit-identical to the three branches above, with `feather = 0` taken as an explicit hard-edge case so no vanishing span is ever divided by — the discipline the delivered vignette unit's `hard_step` already follows.

**Inside is selected.** Lightroom's radial affects the outside until you tick Invert; ours affects the inside, which is what a person drawing an ellipse around a face expects, and the component's own Invert gives the other reading — exactly, at no precision cost. The study [confirms](mask-study.md#what-settles-p3) it on the bounds rectangle: an inside-selected radial's coverage is zero outside its ellipse, so a masked run skips 82% of a 24 MP frame in the worked case, while outside-selected coverage is non-zero everywhere and no span or tile could ever be skipped. That settles [proposal P3](#proposals-with-recorded-defaults).

### Brush (phase C)

A brush component holds an **ordered list of strokes**, which is what makes "several brushes in one mask" ordinary rather than a special gesture. The component is the thing a person names, combines, inverts and subtracts with; the strokes inside it are how its coverage was drawn, and they are combined before anything is rendered. [History granularity](#history-granularity) states how that maps onto entries.

```text
Stroke { points: [[x, y], …], size, feather, flow, erase }
```

The stored payload is `{strokes: [address, …]}` — the reserved [`strokes` field](#stroke-storage), holding the component's strokes in order by content address. **No coordinate is ever written into a component payload**; the shape above is what one stored *stroke* holds.

`size` and `feather` are the brush at the moment the stroke was made, in mask-space units and 0..100; `flow` is 0..100; `erase` marks a stroke that removes coverage. A `size` is a stored distance and takes the study's own `[1e-4, 64]` rule, refused by name rather than clamped. One stroke's coverage is the capsule profile at the distance to its nearest segment — for a segment `A→B`, `d` is the distance from the pixel to the segment, and a one-point stroke is the single degenerate segment `A→A`, so the distance to the point falls out of the same expression with no branch:

```text
R = size,  f = feather / 100,  band = R · f
s = 1                                       band = 0 and d ≤ R      (the hard edge)
  = 0                                       band = 0 and d > R
  = smooth(clamp((R − d) / band, 0, 1))     otherwise
stroke = s · flow / 100
```

The study freezes the *minimum distance* spelling above rather than "the maximum over its segments of the profile", and proves the two bit-identical: the profile is nonincreasing in `d`, so one `sqrt` and one `smooth` per stroke give exactly what one per segment would.

Strokes accumulate inside the component in stored order, so painting the same area twice builds up and erasing removes:

```text
add     c = c + (1 − c) · stroke
erase   c = c · (1 − stroke)
```

Those are the screen union and the multiply-complement, written in the one spelling that is an **exact identity** when a stroke contributes nothing — which is what lets the grid index evaluate only the strokes near a pixel and still produce the field the whole list would, bit for bit rather than nearly.

Maximum along a stroke and screen union across strokes is deliberate: one pass of the brush has one density whatever the pointer's sampling rate, so coverage does not depend on how fast the hand moved or on how the desktop decimated the path, while a second pass is a second object and does build up. **Density is not delivered**: its Lightroom meaning depends on a build-up model along a single stroke, which would make the result depend on stamp spacing and therefore on resolution. That settles [proposal P4](#proposals-with-recorded-defaults), and the [user guide](../user-guide.md#masks) states it where a person would look for the control; a per-stroke radius for tablet pressure is later work and the payload's point shape is chosen so it can be added without a format rewrite.

### Luminance range and colour range (phase D)

Both read the **input pixel of the operation they modulate**, which costs nothing during a render (the value is in the row being processed) and costs one existing `O(layers)` sample at a point query.

- **Luminance range** `{low, low_feather, high, high_feather}` over the sRGB-encoded luminance of the input pixel, so the numbers mean what the histogram shows: full coverage between `low` and `high`, `smooth` shoulders of the given widths outside them. All four are on the histogram's own `0..100` axis, where one unit is 2.55 output codes; a shoulder is exactly `0` for a hard edge or at least `1`, because the shoulder's slope is `1.5 / feather` and anything narrower is a hard edge asked for indirectly.
- **Colour range** `{samples: [[r, g, b], …], refine}`: up to five sampled colours and one refine slider, with coverage from Oklab **chromaticity** distance to the nearest sample — `L` does not appear, because a surface under a stop of shading moves eight to twelve times further in lightness than in chromaticity, and lightness already has its own component to intersect with. Refine maps geometrically to the selection radius and a higher refine is always a tighter selection, defaulting to `50`; both settle [proposal P14](range-study.md#proposals).

The frozen mathematics is [the range study](range-study.md), with its `f64` reference at `crates/lightwell-reference/src/range.rs`; the production transcription is bit-identical to it, not merely within tolerance. The swatches are **not** a declared parameter — the closed parameter vocabulary has no list of anything — so a sampling kind declares how many colours it holds in the host's kind table and the command family generates `mask.add-<kind>-sample` and `mask.delete-<kind>-sample` over it, a swatch at a time, which is also how a person edits it. Sampling a colour the component already holds is a no-op, because the fold is by nearest sample and a duplicate changes no pixel's coverage.

These two are what makes "select the sky" practical without a model: a luminance range plus a colour range plus a subtract brush selects a sky in the photographs a person actually edits, and every part of it is deterministic, inspectable and reproducible.

## Processing contracts

### The compiled mask

The host compiles a `Mask` against the stage its layer receives into a `CompiledMask`, at the same point in `compile` where a module's payload becomes a `Processing`:

- `evaluate(x, y, rgb) -> f32` at stage pixel centres, pure, so the rasterizing pass and `render.sample` cannot disagree: both reach it through one call with the same arguments. `rgb` is **the pixel the masked operation receives** there, in linear light — the snapshot a masked colour run already takes to blend against, and the tile-shaped snapshot a masked spatial operation already cuts out. A position-based component ignores it and is bit-identical across the argument's arrival; a value-based one is answerable from nothing else. That settles [proposal P12](range-study.md#proposals).
- `bounds() -> Region`, a conservative rectangle outside which coverage is exactly zero. A colour run skips those spans and a spatial tiling copies those tiles, so a small brush mask on a 60 MP frame costs almost nothing.
- `min_feature_px(stage) -> f32`, the smallest feature the mask draws at that stage, which the proxy path uses (below).
- `reads_pixels() -> bool`, whether any component's coverage depends on the pixel's value rather than its position. A caller with no pixel to offer has to be able to ask: the coverage overlay is a function of position over the finished frame, whose pixels are the masked operation's *output*, not its input. [Proposal P16](range-study.md#proposals), decided by the owner on 2026-09-23 and **built**, supplies the overlay the operation's input instead: it evaluates the recipe prefix up to the mask's first bound layer once per display cell — the layer `mask::commands::input_layer_index` already names for the constrained brush's seed and for `mask.sample-input` — so a mask a person *painted* with a colour-limited stroke is readable too. Two refusals remain, named in the host's own words: a mask no layer is bound to, and one whose first bound layer sits behind a spatial layer, where a point query evaluates a tile and a grid would cost one per cell. A brush answers `reads_pixels()` too, exactly when one of its strokes is limited to a colour. The panel's "no coverage overlay" line is a *kind's* line, carried by the two kinds the table marks value-based, so a brush's row does not show it: a person who limits a stroke to a colour is told what the limit does rather than that the overlay is unavailable. The host names the refusal on the frame, and the [user guide](../user-guide.md#masks) states it in the paragraph about Limit to colour. Two other absences of a grid carry no reason, on purpose and not as a gap: a mask with nothing to describe, which is decided in closed form and which a grid of zeros would misreport, and a cancelled request, where a newer one is already on its way.
- A **value-based** component answers the whole stage for `bounds()` and `f32::INFINITY` for `min_feature_px`, both stated rather than guessed: a colour it selects can appear at any pixel, so no span or tile may be skipped for it, and a value test draws no feature a pixel grid can miss. The thin-feature supersample therefore never fires for one, and could not help it if it did — the full-resolution pixels it would have to read are not there at proxy scale — so no proxy frame is marked approximate for it. That settles [proposal P13](range-study.md#proposals); the [user guide](../user-guide.md#masks) carries what it costs a person instead of a flag.
- Brush components build a uniform grid index over mask space at compile time, with a cell side taken from the component's largest stroke radius, so a pixel tests only the segments listed in the one cell it falls in. Each segment is recorded in every cell its own radius-grown box reaches, which is what makes that one cell sufficient. Build cost is bounded by the occupancy cap below and never touches a pixel. Measured on a 400 × 300 stage at fixed local density, with the extra strokes placed where the sampled pixels cannot reach them (`mask_brush::evaluation_cost_does_not_grow_with_stroke_count`, release, M4 MacBook Pro, one thread, medians of three back-to-back runs): evaluating one pixel costs **8.78 ns at 2 strokes and 9.88 ns at 64**, a factor of 1.13 for a 32× stroke count, while the same field folded without an index costs **108 ns and 2696 ns**, a factor of 25. **Provisional**: the host's one-minute load average was 10.13 throughout, above the 8.0 a quotable figure needs, so the absolute nanoseconds are an upper bound. What the runs agree on, and what the index exists for, is the shape — flat against the total stroke count where the unindexed fold is linear in it. The index's memory is bounded by the two limits above and never by the stage: at most `SEGMENTS_PER_PIXEL` entries per cell and at most one cell per segment, so a mask's whole index set is bounded by the declared points-per-mask limit.

A recipe's whole mask table — the per-recipe limits, identities and structure, the serialized-bytes bound, every stroke it references resolved against the store with the points-per-mask bound, and every component through the kind table — is checked **once, where the recipe enters the service** (`Recipe::validate_mask_table`): the admission every commit, composite, `mask.*` command, Restore and import passes, and a drafted `mask.*` gesture's effective recipe, whose table no commit has admitted; a module's draft changes only layers, so its table is the admitted current one's. Compiling trusts it and does not check it again, which is what keeps a preview, a sample and a prefix stage of a masked stack from re-serializing its mask table; a layer's own mask reference and stage are still checked per layer. A stored recipe was admitted when it was written, and what a masked layer needs drawn is refused where it is drawn: compiling its mask parses every component and resolves every stroke, with admission's own words.

No full-resolution mask plane is ever allocated. That is not an optimization, it is the [point-query rule](../engineering/performance-rules.md#rules): a mask must be answerable for one pixel in bounded time, or `render.sample` could not equal the rendered byte.

### Masked colour and masked spatial

Two of the five host primitives gain an optional mask. No new primitive is added and no module code changes.

- `ColorOperation { units, mask: Option<Arc<CompiledMask>> }`. A masked operation is still one operation in its segment's ordered list, but it is evaluated against a copy of the row span it was given and blended back: `out = (1 − M)·in + M·units(in)`, per channel, in linear light, before the run's single clamp and quantization. The copy is one row chunk from the existing 64 MiB float scratch budget, reserved before it allocates. Unmasked operations keep today's exact byte path, including the identity source buffer.
- `SpatialOperation { units, mask: Option<Arc<CompiledMask>> }`. The mask changes nothing about the halo, the tiling or what a unit reads: the operation still reads the finished frame before it and writes the next one. Only the write is blended, at the output rectangle, against the same input the tile already holds. A tile entirely outside `bounds()` is a copy. A masked spatial tile holds one extra tile-sized plane, the snapshot the blend is against, which raises the spatial budget peak and can narrow the plan's concurrency at large stage sizes; the figures are in [performance](../specs/performance.md).

Blending in linear light against the operation's own input is the compositing model the [Lightroom research](../research/lightroom/geometry-masks-and-retouching.md#a-mask-is-an-editable-selection-plus-an-effect) describes as illustrative, and it is stated here as Lightwell's own contract, not as a claim about Adobe's.

### Point queries and proxies

- `render.sample` evaluates `CompiledMask::evaluate` at the pixel it is sampling, so a sampled byte equals the rendered byte for a masked layer exactly as it does for the position-dependent vignette unit. A masked colour layer adds no rasterization. A masked spatial layer keeps the existing declared exception (one tile plus halo), unchanged.
- A masked recipe is **proxy eligible**: normalized geometry means the mask compiled against the proxy stage is the same field at a smaller scale, so the proxy render remains the exact recipe at proxy size. The one caveat is sampling: a feature thinner than a proxy pixel aliases. When `min_feature_px` at the proxy stage is below 2, the host evaluates the mask (not the effect) with a 2 × 2 supersample per pixel and the result is marked `proxy_approximate`, joining the existing spatial-layer approximation in the result, the `preview_displayed` event and the state summary. That is [proposal P5](#proposals-with-recorded-defaults). Delivered: a proxy render of a masked stack is byte for byte the exact recipe over the exact downscale of the source wherever the rule does not fire; the supersample is the same mask compiled against a stage of twice the width and height, whose pixel centres *are* the 2 × 2 subsample positions, so nothing about the frozen equation changes; a mask with no components answers `f32::INFINITY` and is never supersampled; and the one word `approximate` carries a reason beside it, so a person can tell a thin mask from a spatial layer when both are present. Its cost is in [performance](../specs/performance.md#masks-in-the-proxy-phase).

### Geometry for the desktop

A mask is edited in content coordinates while the person sees the cropped, straightened, rotated output, and a brush cannot afford a round trip per pointer move ([performance rule 12](../engineering/performance-rules.md#rules)). The geometry tail is exact transforms plus at most one crop, so the content→output map is always affine. One new read-only host method answers it once per gesture:

```text
render.transform {asset_id, entry_id?} -> {content: {width, height}, output: {width, height}, forward: [6], inverse: [6]}
```

Both matrices are `[m0, m1, m2, m3, m4, m5]`, meaning `x' = m0·x + m1·y + m2` and `y' = m3·x + m4·y + m5`, the coefficient order and the coordinate convention the host's resample primitive already fixes: coordinates are **continuous and pixel-center based**, so pixel index `n` has its center at `n + 0.5`, a coordinate `c` lies in pixel `floor(c)`, and a stage spans `0..width` by `0..height`. The [crop spec](../specs/single-image.md#sampling) states the same thing about the crop's own sampling. `forward` maps a content coordinate to an output coordinate, `inverse` is its exact inverse, and a coordinate outside the output stage was cropped away rather than clamped.

The desktop maps pointer positions and draws handles locally from that matrix, with no hop per move, and `render.locate` keeps its existing job for picks. The two agree by construction: rounding the content coordinate `inverse` gives for an output pixel's center to the pixel that contains it is the pixel `render.locate` walks to, exactly, for every tail the delivered modules produce. The one position where the two roundings may differ by an index is a point that lands exactly on a content pixel boundary under a reflection, which is a tie in the rounding convention and not an error in either answer. A stack the host cannot compile has no output stage and therefore no mapping: the method returns that stack's own reason and never an identity matrix.

## Host commands

Masks are the host's own objects, not a tool module — a module commits layers and must not rewrite the recipe. Their commands are published as one **host descriptor**, `lightwell.masks`, in the same `ModuleDescriptor` shape a module's is (`mask::commands::descriptor`): its actions are the commands, its queries `mask.list` and `mask.sample-input`, its controls the panel's widgets. `module.list` and `schema.list` list it under `host`, beside `modules`, so an agent discovers a mask command exactly as it discovers a module action. A command is an action like any other: the registry resolves it through its one action lookup as `ActionRef::Host` (a module's is `ActionRef::Module`), the one generic check validates its parameters, and the editor's one action path, `EditorService::run_action`, deduplicates, plans, admits and commits it — a module action is planned by its module and a mask command by the host (`EditorService::plan_request`), and a draft plans through the same function its commit does. The desktop generates their number fields with the widgets it already has.

| Method | Purpose |
| --- | --- |
| `mask.list {asset_id, entry_id?}` | Every mask with its components, values, amount, invert and the layers bound to it. Read-only, no history |

The relation is readable from both sides: `mask.list` names the layers bound to each mask, and `recipe.describe` reports each layer's mask on its own row, so a client reading the durable processing order can tell a masked layer from a global one without asking a second question.
| `mask.create-<kind> {…geometry}` | A new mask whose first component is an `add` component of that kind |
| `mask.delete {mask}` / `mask.rename {mask, name}` / `mask.duplicate {mask}` | Mask lifecycle. Deleting a mask deletes the layers bound to it. The history label names the **effects** it removed, deduplicated — `Delete Mask 1 with Basic, Presence`, and one word for two Basic layers — and the layers themselves are named one by one in the result, so the entry reads as a sentence while the report stays exact; duplicating one copies its components **and the layers bound to it**, each with a new identity, because a mask without its adjustments is not a useful copy |
| `mask.set-amount {mask, amount}` / `mask.set-invert {mask, invert}` | Whole-mask modifiers |
| `mask.reorder {mask, index}` | Moves a mask, and with it the masked layers, by the ordering rule |
| `mask.add-<kind> {mask, mode, …geometry}` | A second, third, … component, with its mode given explicitly |
| `mask.set-<kind> {mask, component, …geometry}` | A field patch over that component's geometry. A component of another *known* kind is a `validation` refusal naming both kinds; one whose kind this build cannot evaluate is `incompatible`, as rendering it is |
| `mask.set-component-mode {mask, component, mode}` / `mask.set-component-invert {…}` | Change a component's role after the fact |
| `mask.delete-component {mask, component}` / `mask.reorder-component {mask, component, index}` | Component list edits |
| `mask.add-stroke {mask?, component?, mode?, points, size, feather, flow, erase}` | One brush stroke, and the **only** way a stroke reaches a mask. One command over three edits, because painting is one gesture: with no `mask` it draws a new one whose first component is an add brush, with a `mask` alone it puts a further brush on that mask in the `mode` given, and with both it appends to that brush. A stroke appended to a component that exists takes no `mode` |
| `mask.delete-stroke {mask, component, stroke}` | Remove one stroke by its content address. A forward edit and not an undo: it appends one entry, and a component's last stroke is refused with the reason, as a mask's last component is |

The geometry methods are **generated per component kind**, as `edit.<action>` and `query.<id>` already are: `mask.create-linear`, `mask.add-radial`, `mask.set-linear` and so on, each declaring exactly the parameters its own kind takes. One `mask.create` carrying a kind and a union of every kind's fields cannot be declared honestly — the closed parameter vocabulary has no way to say "these parameters when the kind is linear, those when it is radial", so `schema.list` would advertise a radius on a linear gradient and a client would have to read prose to know better. The kind-independent commands above keep one shape and one name each.

There is **one** table describing a component kind, in the host beside the compiled mask: each row carries the kind's token, its payload parser and the `ParameterDescriptor`s that payload's fields are declared as, declared once in the kind's own module beside the parser that enforces the same ranges. Registering a kind that **declares geometry parameters** is therefore sufficient to make it creatable, addable and patchable — a stored field's name is its parameter's name, and a generated control's range is the range the parser checks — and the command family names no kind at all. Registering is not sufficient on its own, and the one registered kind that declares no geometry says why: a brush is evaluated, parsed, retained and combined like any other kind, and has no `mask.create-brush`, because its shape is a drawn path and not a set of numbers. Four of the five registered kinds are creatable; the fifth is painted. A position is `[-1, 2]`, a distance the study's `[1e-4, 64]` mask-space units, an angle `[-180, 180]` degrees and a feather `0..100`, each with the step, fine step, precision, soft range and unit a generated number field needs.

The mask, component and stroke a command addresses are declared parameters of the **identity** kind (`identity {of: mask | component | stroke}`, decided by the owner on 2026-09-24), listed first, and a rename's name is a parameter of the `string` kind, so each is validated by the generic check — an identity by its own type's parser — and hashed into the request identity like any other parameter. An identity addresses state rather than setting it, so a required one stays required on a patch: `mask.set-<kind>` always names its mask and component while its geometry fields stay optional. No control edits an identity; the panel's selection supplies it. Everything a control *can* edit — an amount, an endpoint, a mode, an inversion — is a declared parameter validated by the same check. A module action's host `mask` field is checked as the same identity kind and published as the method's `target`.

Every one of them is a normal mutation: mutation envelope, request deduplication, revision check, one history entry, one immutable snapshot. The request table stores each command's whole answer — its label, the mask and component it addressed or minted and the layers it removed — in the change's own transaction, so a retry answers with the identities the first attempt created, after a restart too. A gradient drag and a brush stroke are each **one** entry, because each is one draft: `draft.begin` on pointer-down names the mask and component the gesture edits, and must name every identity its command requires; `draft.set` carries the accumulated geometry; `draft.commit` on release sends the drafted fields with those identities, with the existing conflict, Discard and Reapply behaviour unchanged.

### History granularity

A component is the durable object; a stroke, a handle drag and a slider are edits *to* it. That gives the per-stroke history the owner asked for without a second history model:

| Gesture | History entry | What changed in the recipe |
| --- | --- | --- |
| First brush stroke on a new mask | `Add brush` | A mask, a brush component named `Brush 1`, one stroke |
| Second stroke on the same component | `Update Brush 1` | One stroke appended to `Brush 1` |
| Drag a radial's handle | `Update Radial 1` | That component's geometry |
| Add a subtract brush to the same mask | `Add subtract brush` | A second component, `Brush 2`, mode `subtract` |
| Change `Brush 2` to intersect | `Brush 2 intersect` | That component's mode |
| Raise exposure through the mask | `Mask 1 · Exposure +0.45 EV` | The masked Basic layer's payload |

So `history.undo` walks back one stroke at a time, because each stroke is one entry and each entry already stores the complete recipe. Nothing special is needed for that: it falls out of the delivered snapshot model. Rendering never sees the entries — it sees one component whose strokes are already combined into one coverage field, and one mask whose components are already combined into one `M`, so fifty strokes cost one masked operation and not fifty.

A `mask.*` command's label names the mask and the component (`Mask 2 · Update Brush 1`) when more than one mask exists, because a history list shared with every other module cannot afford `Update Brush 1` alone. A masked **module** edit names its mask always, even with one, because its label is the module's own summary and is therefore exactly what that module's *global* edit writes: one mask is already enough for two entries a history row cannot tell apart. The label is rendered at commit and stored with the entry, exactly as a module's `summary` template is.

Two things follow from strokes being objects rather than events:

- **`mask.delete-stroke` is a forward edit, not an undo.** It removes one stroke and appends one entry, so a stroke made ten entries ago can be removed while everything after it stays. `history.undo` still walks entries. The two never mean the same thing and the panel names them differently.
- **Order matters only where it must.** Add strokes combine by screen union, which is commutative, so their order is irrelevant and removing one is well defined. An erase stroke does not commute with an add, so a component stores its strokes in order, and deleting or reordering across an erase changes coverage in the way the stored order states. The component list shows that order.

Two parameter kinds serve masking. `identity {of}` names the objects a command addresses (above), and `points {points_min, points_max}` is the path a stroke draws, an ordered list of `[x, y]` normalized pairs, validated exactly as `curve` already is but without the increasing-x and monotone rules and with a larger bound. It is a parameter kind, not a control kind: the brush is a canvas interaction, and no panel widget edits a path.

Two new canvas interactions join `point-pick`, `sample-apply` and `crop-frame`:

- `mask-shape {create_action, set_action, …}` — the host's handle editor for linear and radial components, editing a transient draft of the named parameters and committing once, exactly as `crop-frame` does for the crop rectangle.
- `brush-paint {action, points, size, feather, flow, erase}` — the stroke capture: it accumulates the path, decimates it to the declared tolerance, and commits one stroke. It is deliberately **not** mask-specific and is not named for masks: it fills the declared parameters of whatever action declares it, so the corrections proposal (`docs/design/corrections.md`, an unmerged owner-review proposal) declares the same interaction for Clone, Heal and AI Remove rather than a second brush. See [one brush system](#one-brush-system). **Delivered, and delivered in the desktop rather than as a host declaration**: the desktop accumulates the path, draws it as the pointer moves, decimates it through [`crate::path`](../../crates/lightwell-core/src/path.rs)'s own contract when it posts, and commits one stroke per press through the delivered `draft.*` lifecycle, over `mask.add-stroke`'s own declared parameters. It is **not** a `CanvasInteraction` variant: the host declares `point-pick`, `sample-apply` and `crop-frame` and no fourth, because nothing in the capture needs the host to name it — the parameters it fills are already declared and the path kind it posts is already a host primitive. Declaring it would be the change a second consumer asks for, and is not made before there is one. It carries no mask identity of its own — the mask and the component are the command's own identity parameters, supplied by the panel's selection as every other `mask.*` command's are — so nothing in its name, its drafted fields or its implementation assumes a mask.

## The Develop workspace

Mask is the canvas-takeover mode the [tool array](develop-workspace.md#tool-array) already reserves a row for, entered from the mode strip, from `M` or from the command palette.

While it is active the tools panel shows the **Masks panel** in place of the module sections:

- A list of masks: name, an amount readout, a non-neutral dot, a visibility eye (view state, commits nothing) and a context menu with Rename, Duplicate, Invert, Delete. **New mask** is a split button naming the kinds.
- Selecting a mask opens it. Its **component list is always visible**: one row per component with the kind's icon, a three-way `Add / Subtract / Intersect` segmented control, an Invert toggle, a drag handle and a delete button. Hovering a row shows that component's own contribution in the overlay; selecting it shows its handles on the canvas and its number fields beneath the row.
- Under the component list, an **Add** row offers each component kind for the next canvas gesture, with the mode chosen before the gesture starts, not guessed from a modifier key afterwards.
- Under that, the **adjustments**: the maskable modules' own generated sections — Basic's three groups, Presence, the colour mixer — bound to this mask. Same widgets, same draft lifecycle, same `Copy as JSON request`, with `mask` in the copied request.

Delivered for the linear gradient: Mask is a host mode in the strip, entered by the strip, by `M` or from the palette, and leaving it with an open gesture is refused with its reason rather than discarding what was drawn — as is Compare. The Masks panel lists the masks with their name, amount, non-neutral dot, eye and row menu, renames through a field beside the list, and shows the open mask's component list with each row's mode, inversion, reorder and delete, each carrying the command family's own reason where that family would refuse it. The gesture's own declared fields sit beside the handles while it is open, and a committed component's fields sit under its row, so nothing is reachable only by pointer. The adjustments below are the delivered generated sections of the maskable modules with the open mask as their target: the field store, the section derivation and the request builder are each parameterized by that target rather than reimplemented, which is why local Temperature through the eight-range mixer arrive with no new controls. The overlay is painted from the coverage grid the exact preview phase returns beside its frame and drawn as one bounded texture over the photograph, so it costs no second render.

Delivered for the component list: every row carries **its own** mode control, inversion, order and delete, so the mode is a property of a component rather than a decision frozen by the button that created it, and a control on row three never edits row one. Each of them is one declared command with a Copy as JSON request built by the same builder that sends it, so what is copied is what is sent. Pointing at a row asks the overlay for that component's own coverage grid and leaving it restores the composed one, which is what makes a subtract on top of a gradient legible; it changes no selection and commits nothing. Changing a mode, inverting or reordering re-renders the frame the overlay rides on, so the canvas follows with no second gesture. The refusals are shown where they apply: a mask's only component offers Delete mask in place of a delete, a move that would leave a non-`add` component leading says so, and New mask is refused with its reason while the Add row's mode is not `add`, rather than silently creating an add. The radial's handle set is delivered too — four radius handles, a centre, a rotation grip and a feather ring on the ellipse's own diagonal, each with its number field beside the row — drawn from the content stage's aspect that `render.transform` answers once per gesture, because mask space is defined in terms of it.

Canvas behaviour: the selected component draws its handles (linear: the three lines with an end handle each and a rotation grip on the midpoint; radial: the ellipse with four radius handles, a centre handle, a rotation grip and a feather ring; brush: the cursor's size and feather circles). `[` and `]` change brush size, `Shift+[` and `Shift+]` feather, holding Option/Alt erases while the stroke lasts. An in-progress stroke is drawn on the canvas by the desktop as the pointer moves, so path feedback never waits on a render, and the drafted picture follows one frame behind it exactly as a slider's does.

Delivered for the brush: the gesture is reached from the panel's own **Brush** section rather than from the Add row, because that row is generated from the kinds whose geometry is declared as numbers and a brush declares none — a Brush button there would be a button with no `mask.create-brush` behind it. The section's Size, Feather and Flow are generated from `mask.add-stroke`'s own declarations, so the bracket keys and the panel's nudges move by one declared step and cannot disagree; the erase modifier is read when a stroke starts and then frozen, so releasing it mid-path does not turn an erase into an add. A press paints, the pointer's positions are appended and drawn locally, and the release commits: **one stroke is one draft and one history entry**, and the gesture re-arms itself on the component that stroke landed on so painting carries on without a second gesture. A press that painted no position commits nothing. Because it re-arms, the brush is in hand when the pointer goes to the adjustments under the component list, and those sliders need the one core draft the armed brush is holding: an armed brush therefore **never refuses anything** — a slider gesture, another mask gesture or the crop draft takes its core draft (the new gesture's own first request cancels it before anything else reaches the core), a `mask.*` command puts it down, and everything else goes ahead around it — having painted nothing and having nothing to Apply, while a gesture that *has* drawn something refuses them all with its own reason rather than being discarded. For the same reason a commit made elsewhere while the brush is armed raises no Changed elsewhere notice: its draft holds nothing to discard or reapply, so the desktop rebases it with one `draft.reapply` and a stroke begun meanwhile carries on into the rebased draft; a brush that has painted shows the notice like any other gesture. The gesture is one run of the desktop's shared core-draft driver, as a slider's is ([develop workspace](develop-workspace.md#message-flow)): Apply pressed before `draft.begin` has answered commits once the draft exists, and Discard while a round trip is in flight waits for that answer rather than racing it. The cursor's two circles are the brush's own size and its feather ring, drawn through the same affine the handles are, so they are the right size at Fit, at 100% and under a rotated crop. A selected brush row lists its strokes in composition order with a Delete each, named as the forward edit it is rather than as an undo. One ordering is load-bearing and was found by a background capture rather than reasoned about: a commit sends the **core** draft's fields, so geometry the gesture has produced but not sent yet goes out first — committing over a queued `draft.set` wrote the path as it was at the press, which on a fast stroke is one position out of six.

The **overlay** is per-client view state through `workspace.set {mask_overlay, mask_overlay_colour}`: `off`, `tint` — a tinted overlay of the selected mask — `mask-on-black`, the mask alone on black, or `image-on-black`, the image through the mask on black. It is computed by the preview worker beside the frame it already renders, as one byte per display cell on the grid the clipping overlay already defines and bounds, so it costs no second render and allocates no full-resolution plane. One component's own grid is asked for by naming its `ComponentId` beside the mask, which is what lets hovering a row show that row's contribution. `Shift+M` toggles it; `O` keeps meaning thirds everywhere.

The tint is `green` or `white`, defaulting to green, and the two tokens are held a measured distance from every clipping colour. Lightroom's red is deliberately not offered: the delivered clipping indicators already own red, blue and the magenta between them on this canvas, and a tint a person cannot tell apart from a clipping indicator is worse than no tint at all. Whether to keep red out, or to move the clipping palette instead, is [proposal P11](#proposals-with-recorded-defaults).

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

The last one is load-bearing. Every history entry stores a complete snapshot ([performance rule 10](../engineering/performance-rules.md#rules)), and a brush session appends an entry per stroke, so mask bytes are multiplied by the number of entries. Three things keep that bounded, each implemented and measured rather than assumed ([storing a path](#storing-a-path)): a captured path is decimated to a stated tolerance of two grid steps before it is ever stored; coordinates are stored at the precision a 16384 px side can resolve and no more; and the per-recipe bound above fails explicitly instead of growing — it is reached at 7040 stroke references, refuses with `resource-limit: recipe masks serialize to N bytes; the limit is 262144 serialized mask bytes per recipe`, and the refusal is checked before anything is written, so the catalog is left byte for byte as it was. Sharing unchanged mask blobs between snapshots by content hash is [proposal P6](#proposals-with-recorded-defaults) and is a measurement, not a default.

### Stroke storage

Every history entry stores a complete recipe, not a delta ([history](../specs/edit-history.md), [persistence](architecture.md#persistence)). One stroke is one entry, so an entry embedding its component's whole stroke list copies every earlier stroke in that mask, and storage grows with the **square** of the stroke count: 200 strokes embed 20,100 stroke copies.

The fix, decided by the owner on 2026-09-23, is a **content-addressed stroke store**: each stroke is stored once under a hash of its contents, and an entry's recipe lists its strokes' hashes instead of embedding their points. A hash is 128 bits as 32 hex characters, so one reference costs 35 bytes of an entry's JSON.

The figures below are measured, not predicted: `lightwell-core`'s own catalog on the JPEG fixture, counting every stored entry's bytes plus the stroke store's, against the same entries with each stroke's positions written into its payload (`two_hundred_strokes_cost_about_a_megabyte_rather_than_about_forty`). The 200 strokes are spread over four masks of 64, because the limits below do not admit 200 strokes of 100 positions in one mask: points-per-mask allows 81 and strokes-per-brush-component allows 64.

| 200 strokes of 100 positions | Embedded | Content-addressed |
| --- | --- | --- |
| One stroke serialized | 968 bytes | 35 bytes per reference |
| Distinct stroke data | 189 KiB | 189 KiB |
| Stored across history | 20.4 MB | 1.11 MB |

This table was first written from a prediction — a stroke at about 1.8 KiB, 364 KiB distinct and 37.5 MB embedded — and was corrected to the measurement when the store was built. A stored position is a whole step of the coordinate grid and is written as an integer rather than a decimal, so a 100-position stroke serializes to 968 bytes and not to 1.8 KiB. That makes the embedded case about half as bad as predicted and the constant factor about 28 rather than 53. The content-addressed total was predicted within 2%, because it is dominated by the 35-byte reference, which the prediction had right.

Why this shape and not another:

- **Entries stay full snapshots.** One lookup previews, undoes or restores; nothing is replayed, and the history graph, its branches and named versions are untouched. Pure deltas were rejected for the opposite reason: they make every entry depend on all the entries before it and require a replay to rebuild one.
- **A missing or corrupt stroke fails explicitly** and names the stroke and the entry, exactly as an unavailable effect and an unknown component kind do. It is never silently dropped and never rendered as an empty stroke.
- **It is a host store for paths, not a mask table.** Corrections has the identical problem, so the store, the hash and the failure behaviour belong beside the recipe, not inside the mask model.

It does **not** make storage linear, and the design does not claim that. An entry still holds one reference per stroke, so the growth stays quadratic; what changes is the constant, from 968 bytes per stroke per entry to 35, a factor of about 28.

The curve that constant produces is measured, over a session run to the largest one a recipe can hold, on the 24 MP and 60 MP fixtures (`measure_mask_growth_across_a_painting_session`, recorded in the [performance plan](../specs/performance.md#brush-heavy-recipes-across-history)). Stored is every entry's JSON plus the stroke store; embedded is the same session with each stroke's positions written into its payload. The two fixtures produce the **same** stored bytes to the byte, because a stroke is stored in normalized coordinates and a mask table does not know the pixel dimensions of what it is drawn on.

| Strokes | Stored | Embedded | Factor |
| --- | --- | --- | --- |
| 200 | 1.10 MB | 20.3 MB | 18.5× |
| 500 | 5.64 MB | 126.5 MB | 22.4× |
| 1000 | 20.9 MB | 505.3 MB | 24.1× |
| 1809 | 66.1 MB | 1652.4 MB | 25.0× |

Least squares over those samples gives `S(n) = 19.30·n² + 1623·n + 2511` bytes, and doubling the stroke count multiplies the stored bytes by 3.49 at 250 → 500 and 3.71 at 500 → 1000, approaching four rather than two. The square term is larger than the 35-byte reference alone — a reference is paid by half the entries on average, which is 17.5 — because each entry also carries the mask and component structure the references hang on. The realized saving therefore *rises* with the session, from 18.5× at 200 strokes to 25.0× at the ceiling, while the per-reference constant stays 28.

**1809 is that ceiling**, measured rather than reasoned out: these strokes are captured at 100 positions and decimate to 67–78, a mask takes the 8192 stored positions the [limits](#resource-and-responsiveness-constraints) allow it, and a recipe takes sixteen masks. A session past it is not a recipe this build will hold. The figure of 105 MB at 2400 strokes that stood here before was a prediction of a session the limits refuse; 66.1 MB at 1809 is what the largest admissible one costs. No recipe of any shape references more than **7040** strokes either, because the 256 KiB per-recipe serialized mask bound is reached there, whatever the strokes are; that bound is what keeps every snapshot a painting session writes under 256 KiB of mask data, so no such session writes more than 7040 × 256 KiB of it.

**The hash-chain variant is therefore not needed, and stays recorded and unbuilt.** The realistic long retouching session — a few hundred strokes — costs one to six megabytes; the largest session of usable strokes that can exist costs 66 MB, which a catalog carries without complaint; and the pathological maximum, 7040 single-position strokes at one entry each, is at most 1.7 GB, which is a bound and not an open end. Nothing in the measurement asks for the linear variant, which would cost an O(strokes) walk to rebuild a list on every read. If a later feature raises those limits, this is the measurement to redo first: the escape hatch is to content-address the *list* as a hash chain, one constant-size node per stroke.

The store is part of the current catalog format 10, alongside the mask table, the preset library and the derived-artifact tables; a format 5 or format 6 catalog from any earlier branch is refused by name and left byte for byte as it was ([current shapes only](../../AGENTS.md)). It landed **before the first brush**, so no catalog ever holds embedded stroke points, and the resolved strokes are a field of the in-memory recipe that is never serialized, so that is a property of the type rather than of the code that writes it. A recipe's strokes are found from its mask table only — the reserved `strokes` field of each component's payload — and no layer payload is read, because masks are the store's one consumer (decided by the owner on 2026-09-24); a second consumer would declare where its strokes live.

### Storing a path

The store's other half is what a path is before it is hashed, and it is a host primitive with no mention of masks in it. All of it is published by `schema.list` under `paths`, so an agent posts a path without reading any desktop code.

- **Coordinates** are `[x, y]` in the content stage's normalized coordinates, `1.0` being the stage's height on both axes, with one frame of overshoot legal on each side (`-1` to `2`), because a stroke that starts or ends off the canvas is an ordinary gesture.
- **Precision** is a grid of **16384 steps per unit** and no finer: 16384 px is the largest side the editor admits, so a step is one pixel of the largest stage a path can be drawn on. A stored position is an integer number of steps, which is what makes two captures of one path the same stored object.
- **Decimation** snaps to that grid, drops repeats and runs Ramer–Douglas–Peucker on the grid at a tolerance of **2 steps**. Running it on the grid is what makes it deterministic — two captures that round to the same grid path decimate identically — and it is idempotent, so a desktop that decimates before posting and an agent that posts a raw path reach the same stored bytes and therefore the same hash.
- **Deviation** from the captured path is therefore at most **2.707 steps**, the tolerance plus the half diagonal of one grid cell; the two coordinates are rounded independently, so the second term is `sqrt(2)/2` of a step and not half a step. That is 1.65e-4 of the content height: under 0.7 px on a 4096 px stage.
- **Bounds.** 1024 positions per stroke after decimation and 8192 positions per mask, each refused with a `resource-limit` error naming it.

### Deleting the last component

A mask never exists empty from a command, so `mask.delete-component` refuses a mask's only component and says to delete the mask instead. The alternative — deleting the last component deletes the mask — destroys the mask's adjustments as a side effect of a smaller gesture, and there is no promotion rule that could preserve them. The panel therefore offers Delete mask in that position rather than a delete that would be refused, and the same refusal protects an ordering or deletion that would leave a non-`add` component leading.

### What a draft covers today

Both kinds of gesture draft. A mask *command*'s `draft.begin` carries the mask and component it addresses, so a gradient handle drag is one entry with the delivered conflict, Discard and Reapply behaviour. A *module* action's `draft.begin` carries the host's one optional `mask` field when its effect is maskable — the same field the committed request carries — so `draft.set` previews the **masked** layer the release will write rather than the global one, and a masked slider follows the drag exactly as a global one does. It never carries a component: a module edits through the whole composed mask and knows nothing of the components that composed it, and naming one is refused rather than ignored. An action whose module declares no maskable effect takes neither, and says so by name.

### An unknown component kind

A stored component whose `kind` this build does not know is treated exactly as a layer whose effect has no available provider ([missing effects](modules-and-api.md#missing-effects)), because it is the same thing: a payload the host retains and cannot evaluate.

It stays in every snapshot and entry unchanged, and `state`, `history.list`, `history.inspect`, `mask.list`, undo and redo keep working. Every render, sample and overlay that has to draw that mask — a layer bound to it that changes anything — and **appending an edit** to or restoring such a stack fail with `incompatible: unknown mask component <kind>` naming it. Refusing an append is the delivered rule and not an oversight: a recipe's whole mask table is checked when it enters the service, so a stack it cannot evaluate is a stack it will not write, and the alternative — letting an edit land on a recipe whose mask cannot be computed — would produce a catalog whose renders silently omit part of a mask. A mask no layer draws changes no pixel, so rendering its stack draws exactly what the stack says. Registering a kind again restores evaluation without touching stored data.

### One brush system

A brush is a host primitive, not a feature of masking. Three things are defined in the core and shared:

| Primitive | Shared by |
| --- | --- |
| The `points` parameter kind and its decimation contract | Any action taking a path |
| The stroke — a path with the size, feather, flow and erase flag it was drawn with — and the frozen accumulation rules a list of them composes by | The brush mask component; the corrections repair operations |
| The content-addressed stroke store | Both, and anything later that paints |
| The `brush-paint` stroke capture — the desktop's own gesture over a path-taking action's declarations, not a declared `CanvasInteraction` | Both, and anything later that paints |

The first three are implemented and live in `crates/lightwell-core/src/path.rs`, in the core beside the recipe: nothing in that file, in the parameter kind or in the store mentions a mask, and corrections declares them for its own actions without adding anything. The accumulation rules arrive with the brush component itself.

The fourth is shared in a weaker sense and the difference is worth being exact about, because it is the one place this section claims more than the code holds. The capture is **desktop code**, not a host declaration: it reads the `points`, `size`, `feather`, `flow` and `erase` parameters the action it drives already declares, and the host's `CanvasInteraction` vocabulary is still `point-pick`, `sample-apply` and `crop-frame`. So a second consumer reuses the *primitives* — the path kind, the decimation, the stroke and the store — and would reuse the gesture by pointing it at its own action, which the gesture is written to allow but which nothing yet exercises. Declaring a fourth interaction is what a second consumer would ask for; it is not done before there is one, and no sharing is claimed beyond that.

The corrections proposal (`docs/design/corrections.md`, an unmerged owner-review proposal) asks for a `brush-mask` interaction with content-space brush geometry for Clone, Heal and AI Remove. That is this gesture under another name. Masking is implementing it first, so masking names it generically and keeps nothing mask-specific in it; corrections would declare its own actions and add no second brush. Neither design owns it, and neither has yet proved the reuse, because corrections is a proposal and has no actions to point it at.

Responsiveness keeps the delivered targets and adds no new class of work: a mask evaluation is a handful of flops per pixel per masked layer, on top of the units it modulates, and the bounds rectangle removes it entirely outside the selection. The [provisional slider budget](../specs/performance.md#provisional-budgets) applies unchanged to a masked slider drag and to a brush stroke's drafted frames, and a measured miss is reported with its figures.

## Non-AI detection: what is honest

The owner asked whether non-AI sky and object detection is worth considering. The answer this design proposes:

- **Yes** to luminance range, colour range and a colour-constrained brush (phase D). They are deterministic, cheap, per-pixel, reproducible and inspectable, they need no model asset and no inference job, and in combination with a subtract brush they select skies, skin, foliage and water well enough to be the tool people reach for.
- **No** to a control named "Sky" or "Subject" backed by heuristics. A hand-written sky detector — blue-ish, bright, connected to the top edge, gradient-aligned — fails on sunsets, overcast, reflections, backlit subjects and anything shot upward, and shipping it under that name would promise a selection the code cannot make. Claiming it would need a labelled corpus and a measured accuracy figure, which is a research task, not a feature.
- Lightroom's **Auto Mask** is worth having and is not AI, but it is edge- and connectivity-constrained, not per-pixel. The tractable, analytically evaluable part is a colour-constrained brush: coverage is multiplied by the similarity of the pixel to the colour sampled where the stroke began. That is what phase D delivers, under that name, with the difference from Lightroom's behaviour written down. A genuinely edge-aware refinement (a guided filter over a coarse selection) is [proposal P7](#proposals-with-recorded-defaults) and needs its own study, because a guided filter is a neighbourhood operation and a mask is currently a point function.

## Verification and acceptance

Each phase ends with the evidence its claims need; a phase is not complete without it.

- **Exactness.** Every coverage equation is transcribed twice — the production unit and an independent `f64` reference under `crates/lightwell-reference/src/` — and tested to be bit-identical for the same inputs, as the vignette unit and its reference already are. Masked rendering is proved against stepwise references on synthetic fixtures: `M = 0` is the unmasked input byte for byte, `M = 1` is the unmasked effect byte for byte, and a half-covered fixture matches the blend computed independently.
- **Sample equals render.** For every component kind, on JPEG and RAW paths, `render.sample` at a pixel equals the rendered byte at that pixel, including inside a feather band and at a bounds edge.
- **Geometry.** A mask committed before a crop, a straighten, a quarter-turn and a reflection lands on the same content pixels afterwards, proved by rendering, not by inspection.
- **Proxy.** A proxy render of a masked recipe equals the exact recipe rendered against the exact downscale of the source, byte for byte, at every stage where `min_feature_px` is at least 2, and is marked approximate below it.
- **Retention.** A stored mask whose component kind this build does not know fails rendering with `incompatible: unknown mask component <kind>` naming it, keeps every byte, and still lists through `mask.list`, `recipe.describe` and history. Deleting a mask is an explicit action that names the layers it removed.
- **UI/API parity.** Every gesture — creating a gradient, dragging a handle, painting a stroke, changing a component's mode, moving a mask — produces the same stack, history, pixels and errors as the equivalent JSON request from an independent client, and the panel's `Copy as JSON request` produces exactly the request that was sent.
- **Rendered evidence on the M4 Mac.** Four smoke scenarios with correlated state, all delivered: `mask-linear` (create, drag, commit, masked Exposure, undo, reopen), `mask-combine` (a radial, a subtract, an intersect and a second add in one mask, each component's overlay, a refused reorder and one that changes the picture, a masked Presence drag and an undo — all four components are radials, because a brush and a range are not creatable until phases C and D), `mask-brush` (several strokes, an erase stroke, feather at two sizes, a stroke deleted, and a third launch that paints over the picture's edge, at 100% and under a rotated crop) and `mask-range` (a typed band, a gradient intersected with it, the grey card it takes as well as the sky, a picked colour range that gives the grey card back, a layer ahead of the mask that stops the selection altogether, the overlay refused for each value-based component and painted for the gradient, the colour range's own limits one at a time, and a colour-held erase across two surfaces). The mask under a rotated crop is the third launch of `mask-brush` rather than a scenario of its own: it is the same claim about the same affine, and a separate `mask-crop` would launch the editor twice more to re-make it.
- **Measurement.** `editor-performance` and `editor-latency` on 24 MP and 60 MP before and after each phase, plus a brush-specific workload: drafted frames per second during a continuous stroke, the settled exact histogram after a stroke, mask compile cost against stroke count, and peak process memory across a 100-stroke session with its snapshot growth recorded. A stroke is not a field-patch slider, so `editor-latency` cannot drive one; the **paced stroke** an evidence script can now ask for measures it instead, one position per interval in real time, and `mask-range` records the distribution it produces with the recipe and the load average it was taken under ([performance](../specs/performance.md#a-painted-strokes-own-latency)).

## Phases in one line each

- **A — foundation and the linear gradient.** The model, persistence in catalog format 7, the `mask` target field, `CompiledMask`, the masked colour primitive, `render.transform`, the `mask.*` commands the gradient needs, the Mask mode and panel, and the overlay, over the [mask study](mask-study.md)'s frozen composition algebra and gradient falloff. At the end of A a person can drag a gradient and lift the sky's exposure, from the panel or from JSON.
- **B — radial and combination.** The radial component transcribed from the same study, Subtract and Intersect, inversion at both levels, amount, reorder and duplicate, the component-list UX, and the masked spatial primitive so Presence runs through a mask.
- **C — brushes.** The `points` parameter kind, the content-addressed stroke store in catalog format 7, the `brush-paint` canvas interaction and its draft, the brush component with multiple strokes, erase strokes, size, feather and flow, the grid index and its cost contract, path decimation and the payload bounds, and brush-over-gradient combination. The path kind, the stroke list and the interaction are host primitives the corrections design reuses rather than reimplements.
- **D — range selections.** Luminance range, colour range and the colour-constrained brush, each with its own study, plus the honest statement in the user guide about what they do and do not select.

## Proposals with recorded defaults

Each is the owner's to decide. The default is what the work runs on if implementation is authorized without a separate answer, and each stays recorded in this design so a later change is a normal edit. **Every row below states where it stands now and what put it there**: P1 to P11 are all settled, most by a study and the rest by having been built and verified; the last one masking carried, [P16](range-study.md#proposals), the overlay for a mask that reads pixels, was decided by the owner on 2026-09-23 and is built, so masking carries no open proposal. A row settled by delivery is still a normal edit to change: the recorded default became the built behaviour, not an irreversible one.

| # | Question | Recorded default |
| --- | --- | --- |
| P1 | Are masks a target for the existing modules, or a separate local-adjustment module with its own sliders? | A target. **Settled by delivery**, through all four phases: `maskable` and one host-owned `mask` field are the whole of it, and local Temperature through Dehaze and the eight-range mixer arrived with no new equation and no second slider. `mask-linear`, `mask-combine`, `mask-brush` and `mask-range` each drag one of those existing sections through a mask |
| P2 | Zadeh (`max`/`min`) or product algebra for combining components? | Zadeh. **Settled** by the [mask study](mask-study.md#what-settles-p2): Zadeh is idempotent and order-independent bit for bit over randomized component lists, while the product algebra changes coverage by up to `0.248209` when a component is duplicated and by `1.943e-16` when two add components are reordered. The two fields differ by up to 12 output codes on 9.03% of a 24 MP frame, so the choice is visible and is made on those properties; its cost, a C⁰ crease of up to `2.218e-3` per pixel where components cross, is recorded there |
| P3 | Does a radial gradient select inside or outside by default? | Inside. **Settled** by the [mask study](mask-study.md#what-settles-p3): the two readings are exact complements, so Invert costs nothing, but an inside-selected radial has a bounded coverage region — 17.6% of a 24 MP frame in the worked case, against the whole frame outside-selected — so the ordinary case is the one the bounds rectangle can skip |
| P4 | Flow and Density, or one amount? | **Settled 2026-09-23** by the [mask study](mask-study.md#the-brush): Flow only, with max along a stroke and screen union across strokes, measured; Density named as not delivered, with its reason, in the design and the [user guide](../user-guide.md#masks) |
| P5 | What happens to a brush stroke thinner than a proxy pixel? | The mask field is supersampled 2 × 2 and the frame is marked approximate. **Settled by delivery**: the supersample is the same mask compiled against a stage of twice the width and height, so no frozen equation changes, and the word `approximate` carries a reason so a thin mask is distinguishable from a spatial layer. [P13](range-study.md#proposals) then settled the value-based half of the same question the other way — supersampling cannot help a component whose input pixels are not there at proxy scale, so no proxy frame is marked approximate for one |
| P6 | Do snapshots share unchanged mask blobs by content hash? | **Decided by the owner on 2026-09-23: yes, a content-addressed stroke store, in phase C before the first brush ships.** Embedded strokes grow quadratically at about 1.8 KiB per stroke per entry; see [stroke storage](#stroke-storage) |
| P7 | Is an edge-aware refinement (guided filter) part of phase D? | **No, and it stays out after phase D delivered the colour-constrained brush.** It is a neighbourhood operation over what remains a point function of one pixel, so it needs its own design and its own study; what is delivered instead is a per-pixel colour similarity, frozen in the [mask study](mask-study.md#the-colour-constraint), named for what it does and stated in the [user guide](../user-guide.md#masks) as *not* Lightroom's Auto Mask. Nothing in the product claims edge awareness |
| P8 | Should Mask mode replace the tools panel, or sit beside it? | Replace it while the mode is active, as the crop draft holds its own section open today. **Settled by delivery**, and every mask scenario's captures show it: the Masks panel stands in place of the module sections and the maskable modules' own generated sections sit under the component list, bound to the open mask |
| P10 | How far outside the frame may a stored position sit? | **Decided 2026-09-23: `[-1, 2]`**, one stage extent of overshoot per side, on the [mask study](mask-study.md)'s recommendation. Nothing in the frozen mathematics depends on it |
| P9 | Phase order | A, B, C, D as listed; brushes before range selections, because a brush is what makes a gradient usable. **Settled by delivery**, in that order, and the reasoning held: the remedy the range selections' own limits point at is "subtract a brush", which `mask-range` exercises as a colour-held erase because the brush was already there |
| P11 | What colour is the mask overlay tint? | **Decided 2026-09-23: green (default) or white; red stays out.** The clipping indicators are shipped and carry rendered evidence; moving a verified indicator's palette to free red for an unshipped overlay would invalidate that evidence for a cosmetic preference. The design's earlier "red by default" was taken from Lightroom before those tokens existed. Recorded rather than silently flipped, and the owner may still prefer the larger change. Not offered, because the delivered [clipping tokens](develop-workspace.md) already own red, blue and magenta on the canvas and an overlay must be distinguishable from them. This replaces the earlier "red by default", which was taken from Lightroom before those tokens existed. The alternative is to move the clipping palette instead, which is a larger change to a shipped indicator |

## References

- [Tool modules and the shared core](modules-and-api.md) · [Content-space edits](content-space-edits.md) · [Instant previews](instant-preview.md) · [Presence, colour mixer and vignette](presence-mixer-vignette.md) · [Basic and histogram](basic-and-histogram.md)
- [Develop workspace](develop-workspace.md) · [UI components](ui-components.md) · [Architecture](architecture.md) · [Performance rules](../engineering/performance-rules.md)
- The corrections design (`docs/design/corrections.md`), an unmerged owner-review proposal and the other consumer of the shared brush
- [Lightroom geometry, masking and retouching research](../research/lightroom/geometry-masks-and-retouching.md) · [darktable geometry, masks, blending and retouching research](../research/darktable/geometry-masks-and-retouching.md)
