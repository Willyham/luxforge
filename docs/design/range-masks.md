# Range masks: picking and the unbound stage

Status: **decided by the owner on 2026-10-16; implemented and functionally qualified on the M4, measurement open.** The [input site](#the-input-site), the area query, the [pick commands](#pick-commands), the desktop's [range picker](#canvas-and-panel) (pick bar, drag rectangle, Pick on both rows) and the rewritten `mask-range` scenario are implemented, with exact reference tests and controller tests. On the M4 (native Metal) the GPU area read passes its device test within the declared tolerance, the `mask-range` scenario passes in the background, a `gpu-qualification` selection of the range stacks passes every gated kind (a selection, not the gate's pass), and the `range-corpus` scenario has recorded picks and coverage captures on the three corpus RAWs. Open: the timing of a whole-frame drag and a click, which the [task plan](../../tasks/editing/range-masks.json)'s TASK-006 holds ([outstanding work](#outstanding-work)). The board is [Range masks](develop-workspace/html/range-masks.html); its PNG export is not made yet. The frozen selection equations of the [range study](range-study.md) do not change.

## The problem

The luminance and colour range kinds work in the core and the reference renderer, but the desktop makes them look broken. Creating one is hidden behind the off-by-default experiment `masks.range-selections`. A new range component is not selected, so its band and swatches are hidden. A new mask has no bound layer, and a value-based mask reads "the input of the first layer bound to the mask", so its overlay, thumbnail, Pick and a colour-held brush's Limit to colour are refused until an adjustment is bound, and the refusal reaches only the event log. The overlay defaults to Off and creating a typed kind never turns Tint on. The overlay hides while the selection's own band, Refine and Amount are dragged, a rule meant for adjustment sliders. Pick is a canvas mode of its own that leaves Mask mode, so the overlay disappears and the adjustment sections then edit the global layer.

## Behaviour

Creating stays behind `masks.range-selections`, off by default; the flag is unchanged and gates only `mask.create-<kind>`, `mask.add-<kind>` and their drafts. Everything below about picking, reading and the overlay applies whatever the flag says, as editing an existing range component already does.

**After creating.** Choosing Luminance range or Colour range from New mask or Add component creates the component with its current defaults (the whole tonal range with 5-unit shoulders for a band; no samples for a colour range), opens its mask, selects the new component row, shows the overlay as Tint and **arms the eyedropper**. No adjustment needs to be bound first: the selection is read on the stage a first Basic adjustment will receive ([the input site](#the-input-site)).

**Picking, luminance.** A click sets the band around the clicked tone. A drag over an area sets the band to that area's tonal span.

**Picking, colour.** A click replaces the samples with the clicked colour. Shift+click adds it as one more sample, up to five; a sixth is refused with the host's sentence. A drag over an area takes the area's representative colour as the sample (Shift adds it rather than replacing) and loosens Refine just enough to hold most of the area.

**Ending.** Escape, Enter or Done ends picking. Escape during a drag cancels that drag, sends nothing and leaves the eyedropper armed. Picking also ends when Mask mode is left, another mask or component is selected, the component is deleted or undone away, another mask gesture starts or the photograph changes. The row's **Pick** button arms it again.

**While picking** the person stays in Mask mode: the open mask, the selected component, the adjustment sections' target (the open mask, with its scope chip) and the overlay's target are unchanged, so an adjustment dragged between picks edits the mask's own layer. The overlay updates after each pick.

The band slider, the four number fields, the swatch slots with Remove and the Refine slider remain; picking only writes the values they edit.

## The input site

One rule names where a mask's value-based parts read their input, `mask::commands::input_site`:

`input_site(registry, recipe, mask) -> Result<InputSite { index, mode }, Error>`

- **A mask with a bound layer**: the first bound layer in evaluation order, with its mode from `MaskInputMode::for_layer`, as today.
- **A mask with no bound layer**: the earliest position a **colour-stage** maskable effect bound to this mask would take, by `ModuleRegistry::insertion_index_for_target`, with mode `ColourRun`. With the registered modules that is Basic's position for this mask: after the global Basic layer and the Basic layers of masks listed before it. The mode is passed explicitly, because `for_layer` at a not-yet-inserted index would answer for whichever layer sits there now.
- With no colour-stage maskable effect registered, the earliest position of any maskable effect, with the mode its stage implies (colour `ColourRun`, otherwise `Boundary`). With no maskable effect registered at all, a refusal by name. That is the only refusal that replaces `rules::bound_layer`; `rules::limit_on_new_mask` is lifted, so a colour-held stroke on a new mask seeds from the input site.

Consequences, documented in the user guide:

- Binding a first **Basic** adjustment leaves coverage identical: the layer lands at the input site.
- Binding a first **Tone curve, colour mixer, Presence or Detail** adjustment moves the site to that layer, by the first-bound-layer rule, and the selection may shift, as it already does when a layer is added ahead of a bound mask.
- Where a Detail layer leads the stack, the site reads after it through the bounded input grid, as a bound Basic layer does. The boundary width Detail hands on is the whole recipe's choice, so the first active adjustment can change that precision and move coverage by it.
- P16's other refusal, a first bound layer behind a later spatial layer such as Presence, is unchanged. An unbound site is never behind a spatial layer, because the colour stage precedes the spatial one.
- Render and export are unaffected: a mask with no bound layer modulates nothing.

Every reader that takes the input layer takes the site and its explicit mode: the overlay and the thumbnail key (`preview/coverage.rs`), `render/entry.rs`'s `layer_input`, `render/input_grid.rs`'s `GridRequest`, `tiles/region.rs`'s `RegionRequest`, `editor/pixels.rs` (`PixelReadKey`, `mask.sample-input` and `TileStage::read`), `editor/masks.rs` (the seed and `mask_input_sample`) and `editor/plan.rs`'s `HostStage` reads. The GPU path needs no change: `ReadStage::Before` already carries the mode.

## Pick commands

Both are host commands in the `luxforge.masks` descriptor, discoverable through `module.list` and `schema.list`.

**`mask.sample-input-area {asset_id, mask, x, y, width, height}`**, read-only and implemented. The rectangle is in normalized coordinates of the stage the masked layer receives, as stroke positions are. It is widened to whole pixels (at least the one pixel its corner falls in) and clipped to the stage; one wholly outside it is refused as `outside the stage`, not clamped. The area is read at the mask's input site, reduced by the smallest power of two that leaves at most **4,096 points**, through the bounded region read, which widens it to whole blocks of that reduction on the stage's block grid. The answer is `{stage: {width, height}, rect: {x0, y0, width, height}, reduction, size: [columns, rows], points: [[r, g, b], …], renderer}`: the stage, the block-aligned pixel rectangle that was read, the block side, the reduced size, each block's mean linear-sRGB colour in reading order and the renderer that read it. On the catalog owner the tile service answers it, from the GPU as the renderer of record; off the owner the reference does. It writes no history and emits no event.

**`mask.pick-<kind> {asset_id, mask, component, x, y, width?, height?, add?, mutation}`**, implemented as `mask.pick-luminance-range` and `mask.pick-colour-range`. Each is generated from the `pick` entry in the host kind table (`mask/mod.rs`), so registering a kind with a pick rule makes it reachable as the sample commands are. `width` and `height` absent is a click at the pixel holding `(x, y)`, read as `mask.sample-input` reads it; present, a drag over that rectangle, read as `mask.sample-input-area` reads it. One without the other is refused. `add` (default `false`) is declared only by a kind that holds samples. The command reads through the deferred pixel-read path (the owner checks entry, revision, draft and source before replaying), takes only the colour or the points from the answer, so a reference answer naming a reason no session carries (`refused` in a launch with `--no-gpu-render`, `adapter-mismatch`, `tiles-budget` or a plan's code) picks as any other does, derives the new values with the pure functions below and commits **one history entry**, labelled `Pick Colour range 1` (with the mask's name first when more than one mask exists, as other `mask.*` labels are). A pick that changes nothing, such as a colour already held, writes no entry. It is never flag-gated.

Refusals, all before anything commits:

- A component the mask does not have.
- A component of another kind, naming the pick command for its kind, or that its kind is not picked from the photograph.
- A click or area outside the stage the masked layer receives.
- A sixth colour, refused by the declared limit after the pixel is read, because a colour already held is not a refusal.

`mask.add-<kind>-sample`, `mask.delete-<kind>-sample` and `mask.sample-input` stay for the API; `mask.sample-input` answers unbound masks from the input site too.

The derivations are pure functions in `mask/range.rs`, over the band's own `0..100` axis (the sRGB-encoded Rec. 709 luminance times 100) and the colour range's Oklab `(a, b)` metric:

| Pick | Result |
| --- | --- |
| Luminance click, tone `e` | `low = e − 10`, `high = e + 10`, both clamped to `0..100`; both shoulders `10` |
| Luminance drag | `low` and `high` the 5th and 95th percentiles of the area's tones; each shoulder kept, raised to `2` where it is narrower (a hard edge on a fitted span speckles at its ends) |
| Colour click | `samples = [pixel]`, or the pixel appended with `add`; a colour already held changes nothing; a sixth is refused by `rules::room_for_sample`. Refine unchanged |
| Colour drag | The **medoid**, the reduced point whose `(a, b)` is nearest the area's mean `(a, b)`, as the sample (replacing, or appended with `add`). With `d90` the 90th-percentile `(a, b)` distance of the area's points from the medoid and `r = d90 / PLATEAU`, `refine_for(r) = 100 · ln(r / RADIUS_MAX) / ln(RADIUS_MIN / RADIUS_MAX)`, clamped to `0..100` (`r = 0` gives `100`), and `Refine := min(Refine, refine_for(r))`: lowered, never raised |

Percentiles interpolate linearly between the sorted values at rank `(n − 1) · p / 100`. The medoid of a tie is the earliest point in reading order. The medoid is a point of the area rather than its mean colour, because the mean of a two-coloured area may be a colour the area does not hold. `refine_for` inverts the frozen mapping `radius = RADIUS_MAX · (RADIUS_MIN / RADIUS_MAX)^(refine / 100)`, so 90% of the area falls on the plateau. Refine is one value per component, so loosening it for one drag loosens every swatch's selection; the user guide says so.

**Canvas declaration.** `CanvasInteraction::AreaPick {action, title}` (`area-pick`) replaces the host's `SampleApply` declarations (`mask/commands/declare.rs`), one per kind with a pick entry: the action is the kind's pick command and the title reads `Pick colour range`. Only the host declares one; a module's is refused at registration. Modules keep `SampleApply` (Basic's neutral picker). No `AreaPick` action is a workspace mode: `workspace.set` accepts only the pointer, Mask mode and the modules' declared canvases.

## Canvas and panel

The board draws these states.

- **New mask menu**: drawn kinds, a rule, then Luminance range and Colour range while the experiment is on. Unchanged.
- **After creating**: the new mask open, its component row selected and tinted, the overlay Tint and the eyedropper armed. Under the selected row a one-line **pick bar**: the eyedropper glyph in the accent, a hint and **Done**. Luminance: `Click a tone · drag over an area`. Colour: `Click a colour · Shift-click to add · drag over an area`, with the count (`2 of 5`).
- **Luminance row fields**: the two-thumb band over its black-to-white rail with shoulder fades, the four number fields (Low, Low feather, High, High feather) and a **Pick** button with the eyedropper.
- **Colour row fields**: five swatch slots, filled and empty, the Refine slider and Pick. With no samples the row note reads `Pick a colour to select`.
- **Canvas while picking**: a crosshair cursor; a drag draws a dashed accent rectangle; the tint updates after each pick. Resting handles are not hit targets while picking.
- **Adjustments** below the component list keep targeting the open mask while picking.
- **Coverage refusals**, such as no maskable effect registered or a first bound layer behind a spatial layer, are a muted note on the open mask's row and in the status bar, not only an event-log line.

Desktop structure:

- `MaskPanel.picker: Option<RangePicker>` (`range_pick.rs`, controlled by `app/range_picker.rs`), a sub-state of Mask mode holding the mask, component, kind, the content map of the displayed entry, the drag in progress and the one waiting pick: the workspace mode, the section target and the coverage target are unchanged. It is armed after a typed create or add of a kind with a pick entry and by the row's Pick button, and checked after every message against the endings above. No host pick mode remains: `api/methods.rs`'s `canvas_modes` lists no `AreaPick` action, and the desktop's pointer and tool routing have no host pick target.
- Creating selects the created component (`app/masks.rs`'s `open_created_mask`), and adding one selects the added component through the same sync path (`app/sync.rs`).
- `effective_overlay` shows Tint while the picker is armed unless the person explicitly chose Off. `mask_adjustment_held` excludes mask-family controls, so band grips, selection fields, Refine and Amount keep the overlay; module-section sliders still suppress it, as [masking interactions](masking-interactions.md#visibility-and-keys) states.
- The canvas widget, `view/range_pick_canvas.rs`, owns pointer capture only and publishes the press, its moves and its release with the Shift held then; the editor decides: a press that moves less than 3 pt is a click; a drag is clipped to the output stage and its corners are mapped through the held geometry map (`render.transform`, asked again whenever the displayed entry changes) and sent as their content-space bounding box. The pick commands read `x`, `y`, `width` and `height` normalized against the mask's input stage, which is the content stage for the colour-stage site, so under a rotation or crop a drag's box is the content area the rectangle covers, larger than the rectangle on screen. One pick is in flight at a time; a pick made meanwhile waits, also for a fresh map, the newest replacing an older waiting one. A pick that cannot be placed (off the photograph, or a lens warp that does not converge) is refused in the status bar.
- Escape and Return reach the picker through the keymap after an open rename, menu or draft has taken them and before Mask mode's own Escape.
- Captured frames report the picker (`masks.picker`: `armed`, `mask`, `component`, `kind`, `hint`, `count`, the drag `rectangle`, `waiting` and `mapped`) beside the overlay's stored and effective state. The Pick button's Copy as JSON request is the row's last pick, or a click at the centre before any.

## Constraints

- **No UI- or owner-thread pixel work.** Area reads run on the tile service or a reference worker; the owner holds only the deferred read's bookkeeping. The desktop sends a command and draws a rectangle.
- **Bounded.** An area read answers at most 4,096 points and holds one band of the region read; a click is one point. Derivations are `O(points)` over at most 4,096 points.
- **Latest job.** The overlay and thumbnails keep their latest-job workers and fences; a pick's commit is an ordinary new entry they follow.
- **Cost accepted.** A whole-frame drag on the reference renderer is one full prefix render of the input site. That is acceptable for a one-off gesture; it is measured on the M4 with the qualification, not budgeted now.
- No timers, polls or subscriptions are added. No new durable state: the picker is view state, and a pick writes only the component's existing fields.

## Verification and acceptance

- **Input site, exact.** Unit tests for each branch (bound, unbound with Basic registered, no colour-stage effect, none maskable). Exact-buffer reference tests: an unbound range mask's coverage grid equals, byte for byte, its grid after a first masked Basic adjustment is bound; a first Tone curve binding moves the site; a Detail-led stack reads through the input grid; render and export bytes are unchanged by creating an unbound mask.
- **Derivations.** Unit tests against hand-computed values: band clamping at both ends, percentiles, shoulder floor, medoid choice, `d90`, `refine_for` at `r = 0`, at `RADIUS_MAX` and beyond, and the never-raised rule.
- **Commands.** `mask.sample-input-area` bounds, clipping, refusal outside the stage and the renderer record; each pick is one entry with its label; a duplicate changes nothing; a sixth sample is refused; picks work with the flag off; UI and JSON requests produce the same stack, history and pixels.
- **GPU.** The tile service's area answer is held to the reference within the declared tolerance in a native M4 device test (`gpu_tiles_area_tests`): bound and unbound luminance and colour range masks, at a click's size and over the whole frame, over a pointwise and a Detail-led stack, each answer naming the GPU and, for a first Basic bound, unchanged by the binding. A pick in a launch that refused the GPU reads the reference and commits. Headless or software runs do not establish it.
- **Corpus.** The `range-corpus` scenario (`--source RAW`) creates one unbound mask per pick on a supplied photograph, picks through the API (clicks and drags aimed at the corpus RAWs' skies, skin, foliage and neutrals, the generic whole-frame drag and centre click on any other photograph), asks `mask.sample-input-area` over the same rectangle and captures the mask's coverage on black. It records what each pick stored, the renderer that read it and the subjects the photograph does not hold; it judges no selection's quality.
- **Desktop.** Controller tests for the picker's arming, ending, Escape during a drag, Shift, the click threshold, selection after create and add, the overlay rule and the refusal notes.
- **Rendered.** The `mask-range` smoke scenario is rewritten around the flow: create each kind from the menu with the experiment on, see the picker armed and the overlay Tint with no adjustment bound, click and drag picks, Shift-add, Done, then bind an adjustment and check coverage is unchanged, and a colour-held stroke on a new mask. Its existing photograph-against-overlay checks remain. It also asks `mask.sample-input-area` over the sky, unbound and then behind a global exposure, and holds each answer to the GPU's record, the sky's own tone and, behind the global layer, a tone past the band's upper shoulder.
- **Measurement.** `editor-latency --mode pick` over a generated JPEG creates an unbound colour range and makes `--samples` pairs of a whole-frame drag and a click, each from its press to the pick's command sent and to the overlay adopted for the entry it committed; `--reference-renderer` launches with `--no-gpu-render`, so the picks read the reference. The report names the renderer that answered the area read.

## Decisions

**Owner, 2026-10-16:**

- Creating range masks stays behind `masks.range-selections`, off by default.
- Lightroom-style picking: creating a range mask arms an eyedropper. Colour: a click replaces the samples, Shift+click adds one (at most five), a drag samples an area. Luminance: a click sets a band around the clicked tone, a drag sets the band to the area's tonal span. Escape, Enter and Done end picking. The band slider, number fields, swatch slots and Refine remain.
- This reverses [P17](range-study.md#proposals)'s rule that a mask no layer is bound to is refused by name: an unbound mask reads at its [input site](#the-input-site), which follows from choosing the picking flow.

**Delegated and recorded here:** the colour-stage input site (the earliest maskable effect of any stage would be Detail's restoration position, which a first Basic adjustment would then move); the derivation constants (±10 and shoulders 10 for a click, the 5th/95th percentiles and the 2-unit shoulder floor for a drag, the medoid, `d90` and the never-raised Refine); the 4,096-point bound; one history entry per pick and its label; picking never flag-gated; `AreaPick` in place of host `SampleApply`; the 3 pt click threshold, Shift read at release, Escape during a drag keeping the picker armed, and one pick in flight; Tint while picking unless Off was chosen; the hint and note wording on the board.

## Outstanding work

- **Measurement.** A whole-frame drag pick at 24 MP and 60 MP on the GPU and on the reference, and a click pick, press to adopted overlay, p50/p95 over 30 pairs on a quiet M4: `editor-latency --mode pick --samples 30` over `24mp.jpg` and `60mp.jpg`, each with and without `--reference-renderer`. Not taken yet.
- **The gate.** `gpu-qualification` over the whole corpus; a selection of the range stacks passes, which is not the gate's pass.
- **Corpus breadth.** The corpus RAWs hold one sky and one small face (the Nikon Z 6) and no portrait; skin and skies on more photographs, and the [sample corpus](sample-corpus.md), are not picked yet.
- **Platforms.** Windows and Linux evidence, unless run natively.
- The board's PNG export.
