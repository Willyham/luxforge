# Presence, colour mixer and vignette

Status: implemented and verified on the M4 Mac. The owner authorized implementation on 2026-09-22 on the recorded defaults; the three modules, the host stages, the spatial primitive and the acceptance and rendered evidence are delivered, and [performance](../specs/performance.md#presence-colour-mixer-and-vignette-qualification) records the measurements and the missed slider targets. The [proposals](#proposals-with-recorded-defaults) at the end carry those defaults so the plan runs to completion on agent judgement; each is the owner's to refine, as the Basic defaults were. It builds on the delivered [Basic adjustments](basic-and-histogram.md) integration contract (field patches, the draft lifecycle, pointwise colour runs), the [module and API contract](modules-and-api.md), the [UI components](ui-components.md) vocabulary and the [Develop workspace](develop-workspace.md) tool array.

## Outcome and scope

Three further Lightroom-familiar editing sections over the delivered editor, each a built-in module with generated controls and generated API methods:

| Module | Controls | What it needs from the host that does not exist yet |
| --- | --- | --- |
| Presence (`luxforge.presence`) | Texture, Clarity, Dehaze | A neighbourhood-dependent (spatial) processing primitive with a declared stage, tiled halo-bounded execution and a bounded point-sample path |
| Colour mixer (`luxforge.mixer`) | Hue, Saturation and Luminance for eight colour ranges: red, orange, yellow, green, aqua, blue, purple, magenta | A deterministic order among colour-stage layers of different modules |
| Vignette (`luxforge.vignette`) | Amount, Midpoint, Roundness, Feather | A finishing stage evaluated after the geometry tail in output coordinates, and colour units that know their pixel position |

Lightroom is the familiarity reference for names, ranges and panel placement, not a rendering target: no value is claimed as Lightroom-equivalent, and the equations are selected and frozen by numerical studies against independent references before a control ships, exactly as the [tone](basic-tone.md), [white balance](basic-white-balance.md) and [colour](basic-colour.md) studies did for Basic.

In scope: the supported JPEG subset and the implemented RAW sources through the same modules, with each module's stage joining both evaluation paths. Out of scope, with no placeholders: Lightroom's per-colour mixer view and targeted adjustment drag, Point Color, B&W mix, vignette styles beyond the one selected below, grain, sharpening and noise reduction (the Detail module keeps its own later design), masks, and any approximate preview processing.

## Module boundaries

| Component | Owns | Does not own |
| --- | --- | --- |
| Presence module | Parameters, validation, neutral rules, controls, the texture, clarity and dehaze units (their scales, halos, equations and global estimates) | Tiling, scratch, scheduling, sampling, history |
| Colour mixer module | Parameters, validation, controls, the hue-range basis and the hue, saturation and luminance equations as one pointwise unit | Colour run execution, quantization, ordering among layers |
| Vignette module | Parameters, validation, controls, the mask geometry and the amount equation as one positional unit | Where the finish stage sits, output-stage coordinates, quantization |
| Shared host additions | Two new effect stages and their placement rules, an order among layers of one stage, the spatial primitive, positional colour units, the spatial sample path and its cost contract | Any module equation |

Each module is one editable layer with a fixed internal order, updated in place at the same identity and position, exactly as Basic is: `single_layer` is declared, a neutral payload is a legal layer that compiles to nothing, a reset keeps the layer, and planning or rendering against a stack with two layers of the effect fails with `validation: ambiguous <title> layers` without rewriting anything. Each module persists only its current payload format and rejects others explicitly.

## Placement and stage order

The delivered host places a new layer by its effect stage: pixel and colour layers before the first geometry layer, geometry layers at the end. That is not enough for these modules, because the mixer must run after Basic whatever was touched first, local contrast must run after the pointwise colour run so Basic's neutral picker never has to sample through a neighbourhood operation, and a post-crop vignette must run after the crop it recentres on.

Stage additions (`EffectStage`):

| Stage | Meaning | Host placement of a new layer |
| --- | --- | --- |
| `spatial` (new) | Depends on a bounded neighbourhood of its input stage, in content coordinates | After the last pixel or colour layer and before the first geometry layer |
| `finish` (new) | Pointwise but position-dependent, in output coordinates after the geometry tail | At the end of the stack |
| `pixel`, `color` | As delivered | Before the first spatial or geometry layer, instead of before the first geometry layer |
| `geometry` | As delivered | Before the first finish layer, instead of at the end |

Order within a stage: `EffectDescriptor` gains `order: u16` (default `0`). A new layer is placed after the last existing layer of its stage whose order is at most its own and before the first whose order is greater. Basic keeps order `0`; the mixer declares `10`, so a mixer layer always follows the Basic layer in the colour run. The pixel proof, transform, crop and RAW modules are unchanged. Existing layers never move; a stack stored in another order renders in its stored order, and the one order the host cannot evaluate, a finish layer followed by a geometry layer, fails compilation with `validation: finish layer precedes geometry` instead of being rearranged.

Resulting canonical stack for a fully edited photograph: source (RAW only) → pixel proof → Basic → mixer → presence → orientation → crop → vignette.

## Processing contracts

### Presence: the spatial primitive

`Processing` gains `Spatial(SpatialOperation)`: an ordered list of at most 4 `Arc<dyn SpatialUnit>` units. A unit declares `halo(stage) -> u32`, the neighbourhood radius in input pixels it needs beyond what it writes, `scratch_bytes(region) -> u64`, the working buffer it wants for an input rectangle, `estimate_key() -> Option<Cow<'static, str>>` and `prepare(reduction) -> Option<Global>`, an optional global estimate computed once from a bounded reduction of the whole stage and the identity it is stored under, and `apply(input, output, global, scratch, parallelism)` over planar f32 linear-sRGB rectangles that carry their position in the stage. The host owns everything else:

- **A spatial operation is a stage boundary**, like a resample: the segment before it rasterizes to one frame at the existing quantization boundary, the spatial operation reads that frame and writes the next, and later exact layers compose as before. On the JPEG path the frames are bytes decoded through the existing table. The RAW linear path pulls the compiled prefix's pixels, row by row with their colour run over each row, and holds no intermediate frame, so for a render there the operation pulls each input tile through that prefix and writes its output into one float frame (three f32 planes, within the [RAW planar limit](architecture.md#rendering-and-limits)) that the rest of the evaluation pulls from; the input is never materialized. A RAW point sample instead evaluates only the tile that contains its pixel, as the byte path's does. At most two full frames exist at once on either path, as today.
- **Tiled, halo-bounded execution.** The host streams fixed 512 × 512 output tiles aligned to the stage origin on the shared Rayon pool above the one-megapixel threshold, reading each tile plus the summed halo of the operation's units from the input with edge clamping. Inside a tile the units chain: unit `i` is given the rectangle unit `i − 1` filled and fills that rectangle shrunk by its own halo on every side except a side that lies on the stage edge, where it writes up to the edge. The summed halo is bounded at 512 pixels; a unit whose halo exceeds it at the stage's size is refused at compile time with `resource-limit`, never silently reduced. A tile's working set is the input region (tile plus halo), the output tile and each unit's declared scratch for that region; at 60 MP with all three units that is about 101 MiB for one tile, so spatial work has its own `SpatialBudget` in the render context (a 256 MiB target, separate from the 64 MiB row-chunk budget, with a reserve-before-allocate rule and an observable high-water mark). The target paces the work and never refuses it: the host computes the per-tile need from the units' declarations, and each batch of tiles reserves as many working sets as fit beside what other evaluations hold, capped by the pool's worker count and never fewer than one, so a render that overlaps another slows rather than fails and a tile larger than the target runs alone. A batch the target holds to fewer tiles than the pool has workers — two with all three units at 60 MP — hands each tile `Parallelism::Pool`, under which a unit runs the independent rows or column strips of each pass on the shared pool; a batch as wide as the pool, a stage below the one-megapixel threshold and a point sample get `Parallelism::Serial`. A unit computes every value with the same arithmetic in the same order under both, so the choice never changes a byte. Cancellation is checked between tile batches. A unit's output at a pixel depends only on that pixel's neighbourhood, so rendering with any tile size agrees with a whole-frame evaluation up to float summation order and within the study's frozen tolerance; a render is deterministic across runs.
- **Global estimates.** `prepare` receives a bounded reduction of the stage input at 1/16 scale per side (at most 0.25 megapixels) built by the host, and returns a small (at most 4 KiB) value the tiles read. Only a unit that declares an estimate key is prepared; the key names everything `prepare` reads besides the reduction. The host caches estimates in an eight-entry store keyed by `(source fingerprint, complete upstream pixel identity, stage, estimate key)`, including the development, source view, exposure and approximate white balance on RAW, plus the upstream recipe and mask contents and evicts oldest first, reducing the stage only when a unit that declares a key is missing, so a second evaluation of the same stack reduces nothing and neither does a new amount of a unit whose key does not name it. Dehaze's atmospheric light is the one such estimate in this design and reads no amount, so every Dehaze amount shares it; texture and clarity declare none and never reduce.
- **Point sampling.** `render.sample`, `pixel_in`, the neutral picker's `sample_before` and every other point query keep their contract for stacks whose spatial layer lies after the sampled prefix, which the placement rule guarantees for Basic. A sample through a spatial layer evaluates the stage-aligned 512 × 512 tile that contains the point, plus its halo, through the compiled prefix on the thread that evaluates the sample (for `render.sample`, the catalog owner's point worker, so the owner itself only plans it), allocating at most one tile working set from the spatial budget, so the sampled byte is exactly the byte a render of that tile produces: its cost is O((tile + halo)² × layers) rather than O(layers), and a global estimate on a cache miss costs one 1/16-scale reduction of the stage. This is the declared exception to [performance rule 4](../engineering/performance-rules.md#rules), recorded here with the crop filter's neighbourhood cost as precedent; no-op checks and validation never take it because these modules plan by payload comparison alone.
- **Non-finite values** after any unit fail the render or sample with `resource-limit`, as colour runs do. Alpha is never touched. A neutral presence payload compiles to no operation, keeps the identity byte path and shares the source buffer.

The units, selected and frozen by the [presence study](presence-study.md) against the candidates in the [darktable](../research/darktable/detail-and-local-contrast.md) and [Lightroom](../research/lightroom/detail-and-local-contrast.md) research:

| Unit | Range | Candidate contract to freeze |
| --- | --- | --- |
| Texture | −100..+100, step 1 | A medium-frequency luminance band (about 2 to 8 pixels, scaled with image size) isolated by an edge-aware filter and scaled by a bounded gain; negative values attenuate the band. Luminance is Rec. 709 on linear sRGB, RGB is reconstructed by the luminance ratio with the tone study's near-black rule |
| Clarity | −100..+100, step 1 | Broad local contrast: the residual of luminance against an edge-aware base at 1.6% of the long side (the largest round value whose halo fits the bound at 60 MP), computed on a 4× reduced base, amplified through a compressive curve in the tone study's encoded working domain so that highlights do not clip and halos stay bounded |
| Dehaze | −100..+100, step 1 | The atmospheric model `I = t·J + (1 − t)·A`: atmospheric light `A` from the global estimate, a dark-channel transmission refined by a guided filter with a transmission floor, strength scaling the estimated veil; negative values add haze through the forward model. Applied to RGB, because a veil is chromatic |

Guided-filter and downsampled-base candidates are preferred because they tile; a local-Laplacian candidate is rejected unless it can tile within the halo bound, which the pinned darktable source says its own path cannot.

### Colour mixer: one pointwise unit

The mixer is one `PointwiseColor` unit in the existing colour run, so it joins the Basic run in the same segment with no new quantization boundary and the same output rules. It works in Oklab, the space the [colour study](basic-colour.md) accepted for saturation and vibrance, and its study freezes:

- **Eight hue ranges** over Oklab hue with overlapping smooth weights that sum to one everywhere, centred on the hues of the sRGB reference colours for red, orange, yellow, green, aqua, blue, purple and magenta. Weights vanish on the achromatic axis with a chroma ramp, so greys, near-greys and noise in shadows are never coloured by any slider.
- **Hue** −100..+100 moves the range's centre colour 85% of the way toward the neighbouring centre in the direction of travel, through a strictly increasing warp of the hue circle that never lets two hues cross; **Saturation** −100..+100 scales chroma, and every saturation slider at −100 makes every pixel exactly grey; **Luminance** −100..+100 scales `L` through a compressive response with the near-black rule. Every slider at 0 is the exact identity; production matches an independent f64 reference within the frozen tolerance; a single range's saturation or luminance slider changes no pixel whose weight for that range is zero, and its hue slider none outside the arc between the centres two ranges either side.
- Neither unit clamps internally; the gamut policy is the colour study's.

These are frozen, with their constants, property proofs, fixtures and tolerance, in the [colour mixer study](mixer-study.md).

Twenty-four `number` parameters, one field-patch action `set-mixer` and a non-patch `reset-mixer`, with labels of the form `Red hue +20` and group resets `Reset Hue`, `Reset Saturation`, `Reset Luminance`.

### Vignette: one positional unit

`PointwiseColor::apply_row` gains the row's `y` and starting `x` so a unit may depend on position; existing units ignore them. A finish-stage layer is compiled against its input stage, which is the output stage of the geometry tail, so a later change to the crop, which updates the crop layer in place before the vignette layer, recentres the vignette exactly as a post-crop vignette should. The RAW linear path receives the same coordinates for its terminal evaluation, so a linear sample and a linear frame agree.

The vignette study freezes the mask and the amount equation:

- **Mask** `m(x, y)` in `[0, 1]`: 0 at the centre and inside the midpoint radius, 1 at the corners, over an elliptical-to-rounded-rectangle shape. Midpoint 0..100 (default 50) sets where the falloff begins as a fraction of the half-diagonal; Roundness −100..+100 (default 0) morphs the shape from a rounded rectangle through an ellipse to a circle; Feather 0..100 (default 50) sets the width of the transition. The mask is symmetric under mirror and flip, and the centre pixel is unchanged for every parameter combination.
- **Amount** −100..+100 (default 0): negative darkens toward black in linear light with the gain `1 − |a|·m`; positive lightens toward the display white with a compressive mapping that never exceeds 1 in encoded terms. Amount 0 is the exact identity for every mask. The one style shipped is this luminance-neutral one; Lightroom's highlight-priority and paint-overlay styles are not selected.

One `set-vignette` field patch, `reset-vignette`, labels like `Vignette amount −35` and `Reset Vignette`.

## Controls and interaction

All three sections render from descriptors with the delivered vocabulary; no new control kind and no desktop change is expected. Every slider is a `number` control of a field-patch action, so it drafts through the core lifecycle, previews live and commits once on release, key-up or Enter; Escape cancels; a right-click copies the exact JSON request.

| Section | Registry position and initial state | Groups and controls |
| --- | --- | --- |
| Presence | After Basic, collapsed | One group: Texture, Clarity, Dehaze sliders, zero at 0; module reset |
| Colour mixer | After Presence, collapsed | Hue, Saturation and Luminance groups, each with the eight colour sliders in the order red, orange, yellow, green, aqua, blue, purple, magenta and its own reset. Rails use the `gradient` hint: a hue rail runs from the previous to the next range colour through this one, a saturation rail from grey to the colour, a luminance rail from the dark to the light version of the colour. Hue starts expanded; Saturation and Luminance start collapsed |
| Vignette | After Crop, collapsed | One group: Amount (zero 0, soft range full), Midpoint, Roundness, Feather; module reset |

History labels come from `ToolModule::label`, recipe rows from `describe_layer`, and seeded slider values from `values`, so the panel shows authoritative current or historical values for each module as it does for Basic.

## API

Generated from the descriptors: `edit.set-presence`, `edit.reset-presence`, `edit.set-mixer`, `edit.reset-mixer`, `edit.set-vignette`, `edit.reset-vignette`, all discoverable through `module.list` and `schema.list`, each patch action validating exactly the fields sent, storing the patch as sent, deduplicating by request id and creating no entry for a patch that changes nothing. `draft.begin` accepts each patch action. `recipe.describe` reports the three layers' values. `render.sample` and `analysis.request` carry their existing identities; a report over a stack with a spatial layer is exact full-resolution counts as today. UI and API parity is verified through the [field-patch conformance chapter](../engineering/development.md#the-field-patch-conformance-chapter), not assumed.

## Resource and responsiveness constraints

The existing limits hold: 64 MP, 16384 px per side, 512 MiB per evaluated frame, a 64 MiB aggregate float scratch target, at most two full frames at once, no full-frame float buffer on the JPEG path. New bounded quantities: 4 spatial units per operation, a 512 pixel summed halo, 512 × 512 tiles whose working sets are charged to a 256 MiB spatial target that paces batch concurrency and is passed by at most one working set per evaluation in flight rather than refusing work, 4 KiB per global estimate and eight cached estimates, at most one tile's working set allocated by a point sample.

Every slider of the three modules is measured on the recorded M4 configuration in release builds at 24 MP and 60 MP with a warm source cache and at least 30 samples, separately for slider-to-presented-frame latency, the settled histogram, and the sample cost through a spatial layer. The provisional targets are the Basic ones: the [slider budget](../specs/performance.md#provisional-budgets) at warm 24 MP, and settled histogram p95 below 200 ms. A spatial slider is expected to miss the slider target at 60 MP and may miss it at 24 MP; a miss is reported with its figures and does not block delivery, and no approximate preview, extra cache or timer is added to claim a pass. Peak RSS, scratch high-water mark and tile counts are recorded. Every implementation handoff answers the [performance checklist](../engineering/performance-rules.md#review-checklist).

## Verification and acceptance

1. **Frozen numerics before production.** Each study lands an f64 reference module under `crates/luxforge-reference/src/`, hand-built fixtures under `fixtures/`, and its frozen equations, ranges and tolerances in its own design note, before the corresponding module task starts. Presence fixtures include step edges, sinusoidal frequency sweeps, textured patches, smooth gradients, noise at several amplitudes and a synthetic hazed image built from the forward model with known `t` and `A`; mixer fixtures include the eight reference colours, an achromatic ramp, near-black chroma and a hue wheel; vignette fixtures include a flat field with every parameter combination at the corners and the centre.
2. **Exactness and bounds.** Production matches each reference within its frozen tolerance; neutral payloads keep the identity byte path and share the source buffer; tiled rendering is byte-identical across tile sizes and matches the whole-frame reference; halos are measured on the edge fixtures and stay under the study's bound; greys are invariant under every mixer slider; the vignette centre is invariant and the mask symmetric; non-finite values, over-limit halos, two layers of one effect and a finish layer before geometry are refused explicitly without rewriting anything.
3. **Complete history journey through the API** for each module, by the one [field-patch conformance suite](../engineering/development.md#the-field-patch-conformance-chapter) every field-patch module passes, in `cargo test` and in release inside `editor-acceptance`: gesture commit, cancel and return-to-start no-op, retry deduplication, group and module resets keeping the layer identity, undo, redo, preview, restore and reopen evaluating byte-identically, `render.sample` equal to the rendered raster including through a spatial layer and a straightened crop on both evaluation paths, and an unavailable provider refusing the stack while keeping the layer readable. Each module's own placement is the `editor-acceptance` chapter for these three modules, through the same method table: the vignette recentring after a crop update, the mixer following Basic whichever was touched first, and Presence following the colour run. The original's bytes are unchanged throughout.
4. **Native M4 rendered evidence** from background smoke scenarios `presence`, `mixer` and `vignette` with correlated state, requests and logs at Fit and 100%, inspected for halos on edges, noise in shadows, hue continuity on the wheel, and vignette shape at each roundness extreme.
5. **Measurements** recorded in [performance](../specs/performance.md) with their scope, and the docs updated for demonstrated behaviour only: [feature status](../features.md), the [user guide](../user-guide.md), the workspace tool array and the module design's descriptor and stage tables.

## Proposals with recorded defaults

Recommendations for the owner, recorded as proposals until decided. The plan runs on the defaults.

| Question | Default the plan runs on | Alternatives |
| --- | --- | --- |
| Section names | Texture, Clarity and Dehaze are a Presence section, the label Lightroom users look for; the vignette is its own Vignette section | Keep the workspace mockup's Effects label for the three sliders; fold the vignette into an Effects section once grain exists |
| Stage order | Colour before spatial before geometry before finish, with the mixer after Basic by declared order | Spatial before colour, which would require sampling through neighbourhoods for the neutral picker |
| Mixer layout | Three property groups (Hue, Saturation, Luminance) with eight sliders each | Lightroom's per-colour view and targeted-adjustment drag, which need a view switch and a drag canvas interaction that the vocabulary does not have |
| Vignette style | One luminance-neutral style; positive amount lightens toward white | Highlight-priority and paint-overlay styles, colour-priority desaturation |
| Spatial precision on the JPEG path | The spatial operation reads the quantized frame of the preceding segment, one extra 8-bit boundary on an 8-bit source | A float hand-off from the colour run, which needs a full-frame float plane the memory limits do not allow |
| Gesture latency for spatial sliders | Decided 2026-09-22: at Fit a Presence stack renders through the instant-preview proxy at display size, marked approximate, and the exact phase still feeds the histogram, the overlays and the 100% view | Exact-only drafts, which cost a full-resolution neighbourhood pass per input |
| Point-sample cost through a spatial layer | The declared O(halo² × layers) exception | Refusing samples through spatial layers, which would break readout and acceptance parity |

## References

- Frozen numerical studies: [presence](presence-study.md) (Texture, Clarity and Dehaze equations, radii, halos, measurements and tolerance), [colour mixer](mixer-study.md) (basis, equations, constants and tolerance) and [vignette](vignette-study.md) (mask and amount equations, worked examples and tolerance).
- Repository contracts: [modules](modules-and-api.md), [Basic and histogram](basic-and-histogram.md), [UI components](ui-components.md), [workspace](develop-workspace.md), [architecture](architecture.md), [performance rules](../engineering/performance-rules.md), [RAW integration](raw-integration.md).
- Local research: [Lightroom detail and dehaze](../research/lightroom/detail-and-local-contrast.md), [Lightroom tone and colour](../research/lightroom/tone-and-color-tools.md), [darktable detail and haze](../research/darktable/detail-and-local-contrast.md), [darktable previews and tiling](../research/darktable/previews-and-performance.md). These supply context and candidate mechanisms, not accepted Luxforge behaviour.
