# Masks in the Develop workspace

Status: proposal, 2026-09-24. This is the design of the **Masks panel and its gestures** over the delivered mask model, and of the component kinds that come next. It changes nothing about what a mask *is*: the model, the composition algebra, the content-space rule, the `mask.*` command family, the draft lifecycle, the stroke store and the coverage overlay are the [masking design](masking.md)'s, delivered and verified through phases A to D, and every control below is one of that family's commands or one of the maskable modules' own. What this proposes is how those commands are laid out, what a person sees while using them, and three additions to the kind table. Product choices are proposals until the owner decides; see [decisions](#decisions).

Two boards draw it: [Develop · mask mode](develop-workspace/mask-mode.png), the whole screen with a radial handle drag open, and [Mask panels](develop-workspace/mask-panels.png), the panel's rows, states and the proposed kinds side by side. Both use the [module-panel density](develop-workspace.md#module-panels) and the same tokens, so a mask row and a slider row are the same height everywhere.

## What the delivered panel gets right, and what it does not

The delivered Masks panel ([user guide](../user-guide.md#masks)) has the right *shape*: Mask mode replaces the module sections, the mask list is always visible, the open mask's components are an ordered list with each row's own mode, inversion, order and delete, the Add row chooses a mode before a gesture, and the adjustments under the list are the maskable modules' own sections bound to the mask. Every one of those is kept. Against the [Lightroom research](../research/lightroom/geometry-masks-and-retouching.md) it already improves on the reference in the ways the masking design lists, and this proposal adds nothing that would take one of them back.

What the rendered captures show is a panel built from the generic controls before the workspace's density and hierarchy existed, so its rows are text runs rather than rows:

| Today | Cost to the person | Proposed |
| --- | --- | --- |
| Mask rows are a name chip, an amount, an icon and a disclosure; reorder is three text buttons under the row (`Move up · Move down · Copy move`) | A three-mask list takes 90 pt and the eye reads it as a paragraph | One 26 pt row: coverage thumbnail, name, dot, amount, eye, menu. Reorder by the row's drag handle or its menu |
| New mask is a row of kind names that overflows the panel | The last kinds are clipped at 300 pt | One New mask button with a kind menu, each kind with its icon and letter |
| The Brush section is always shown, with steppers for size, feather and flow, whatever is selected | Forty percent of the panel is a tool that may not be in hand | The Brush section appears while a brush is armed or a brush component is selected, with the three values as sliders on the panel's rail |
| The overlay is two rows of raw enum names (`off · tint · mask-on-black · image-on-black`, `green · white`) | Internal names on the screen; the rows take 60 pt | One 26 pt row: four icon segments, two colour swatches, the shortcut |
| A component row is `Brush 1 · Brush · add · ⌄`, then an `Invert component` toggle row, then `Up · Down · Delete · Paint more` | Four rows per component | One 26 pt row: drag handle, kind icon, name, a `+ − ∩` mode control, an invert glyph and a menu; fields under the row only for the selected component |
| Rename is a field plus a Rename button above the components | Persistent chrome for a rare action | Rename in the row's menu, editing the name in place |
| Nothing marks that the module sections below are bound to a mask except their position | A slider under "Basic" looks global | The band carries the open mask's name as an accent **scope chip**; leaving Mask mode drops it |

Each of these is a widget change in `lightwell-ui` or a layout change in the panel's view; none changes a command, a request or a label.

## The Masks panel

Mask mode is entered from the mode strip, `M` or the palette, as delivered. While it is active the tools panel shows, under the histogram and in this order:

1. **The Masks band.** A module-style band (32 pt) titled Masks, with a dot when any mask exists, the count and the way out (`Esc leaves Mask mode`) as its hint. It is a band and not a module: it has no reset and no descriptor, and it is drawn by the panel so the mask list sits where a person expects the first section to be.
2. **Overlay row** (26 pt): a four-way segmented control of icons — off, tint over the photo, selection on black, photo through the selection — then the two tint swatches, green and white, then `⇧M`. It is the delivered `workspace.set {mask_overlay, mask_overlay_colour}` with icons in place of the enum names. The tooltips carry the names.
3. **Mask list**: one row per mask, 26 pt. Left to right: a 28 × 19 pt coverage thumbnail, the name, an accent dot when a layer bound to the mask is non-neutral, the amount as a readout, the eye (the delivered per-mask overlay visibility, view state) and a menu (Rename, Duplicate, Invert, Move up, Move down, Delete, Copy as JSON request). The open mask's row is tinted. Rows reorder by dragging their handle, which sends `mask.reorder`; the menu's Move up and Move down send the same command for keyboard and screen-reader reach. Under the list, **New mask** is one button whose menu lists every registered kind with its icon and letter, then a count (`2 of 16`) that reads as the refusal when the limit is reached.
4. **The open mask**: a group rule carrying the mask's name, its component count and a menu, then **Amount** as a slider (0–100, unipolar from 0) and **Invert mask** as a toggle row, then the components.
5. **Components**: one row per component, 26 pt: a drag handle, the kind's icon, the name, the mode control, the invert glyph and a menu (Rename, Edit shape, Move up, Move down, Delete, Copy as JSON request). The mode control is a compact three-glyph segmented control, `+ − ∩`, so a row reads its role at a glance and changes it in place; the first component's control shows `+` alone, dimmed, with the delivered reason in its tooltip. A subtract reads red and an intersect blue when selected, the two clipping hues, because on a control that never sits over the photograph the clash the overlay palette avoids does not arise. The selected row is tinted and its **fields** sit under it in two columns at 22 pt (a radial's six, a linear's four, a range's four, a brush's stroke list); the delivered kind's own lines — what a range does not select, a model's provenance — sit under the row as a note in tertiary text. Hovering a row shows that component's own coverage in the overlay, as delivered.
6. **Add row**: `+ Add component ▾` with the mode for the next component as the same `+ − ∩` control, so the mode is chosen before the gesture, as delivered, and the choice is visible in the row rather than in a sentence.
7. **Brush** (a group rule) appears only while a brush is armed or a brush component is selected: Size, Feather and Flow as sliders, then Erase (with `hold ⌥`) and Limit to colour (with its refine value) as toggle rows. The rule's caption says `armed · Esc puts it down` while the brush is in hand.
8. **Adjustments**: the maskable modules' own sections, exactly as delivered, each band carrying the open mask's name as a scope chip beside its dot and reset. A collapsed band's hint is the module's; the chip is what says whose layer the section edits.

At the module-panel density the default board's panel holds the histogram, the Masks band, three masks, the open mask with three components and the selected one's fields, the add row and three adjustment bands with one expanded, in 830 pt. A mask list of sixteen still scrolls, as it should.

### The canvas

- The **draft bar** names the mask, the component and its mode with the kind's icon, then the gesture's readout, then Cancel and Apply — `Face · Radial 1 · Add · 0.180 × 0.240 · −12° · feather 60`. For a brush it reads `Face · Brush 1 · Subtract · painting` with Done in place of Apply, because a stroke commits on release and the bar is there to say which mask is being painted.
- **Handles** are the delivered ones. The selected component's handles are white; the centre or midpoint grip is accent, so the one handle that moves the whole shape is the one that reads as the anchor.
- The **overlay** shows the selected mask in the chosen mode. While a shape gesture is open the tint is shown whatever the overlay setting, and the setting returns when the gesture ends, so a person never drags a handle blind. This is view state and the setting itself is not changed.
- The **mode strip** keeps its Mask entry selected for the mode's life; Escape puts an armed brush down first, then leaves the mode, as the delivered order already is.

### Keyboard

The delivered keys stay: `M`, `⇧M`, `[` `]`, `⇧[` `⇧]`, held `⌥` to erase, Escape. Proposed additions, each a delivered command: `X` inverts the selected component (`mask.set-component-invert`), `⌫` deletes the selected component or, with no component selected, the open mask, subject to the delivered refusals, and the kind letters in the New mask menu (`L`, `R`, `B`, `P`) start that kind while the menu is open. Up and Down move the selection in whichever list has focus; `⌥`-Up and `⌥`-Down reorder it. Letters act only when no field has focus, as everywhere.

### What is drawn from what

Every row above is a plain-data widget in `lightwell-ui` fed by the view model: `mask_row`, `component_row`, `mode_control`, `overlay_control`, `range_slider` (below) and `coverage_thumbnail`. The thumbnail is drawn from the coverage grid the exact preview phase already returns for a mask, reduced to 28 × 19 cells and cached by the grid's identity; it costs no render and no allocation beyond the cells. The scope chip is a `chip` in the band with the accent style. Nothing new is authoritative: the selected mask, the selected component, the armed brush and the overlay mode are the session's, and the panel derives from them.

## Kinds proposed next

The kind table is the delivered one: a kind that declares geometry parameters is creatable, addable and patchable through generated commands, and the panel lists only registered kinds. Three additions are proposed, in this order.

### Polygon

`polygon {points, feather}`: a closed path of 3 to 64 normalized positions over the delivered `points` parameter kind, with coverage 1 inside, 0 outside and a `smooth` shoulder of `feather` mask-space units across the boundary. It is a **typed** kind in the table's sense — its geometry is numbers — so it gets `mask.create-polygon`, `mask.add-polygon` and `mask.set-polygon` for free, which is the point: an agent that has found a region (a detector, a person's description, a bounding polygon from any source) posts it as one request, and a person edits the same polygon by dragging its vertices. A pen gesture that places vertices one click at a time is a later canvas interaction; the kind is useful from JSON before it exists. Its equation needs the same study treatment as the others before it ships: an even-odd or winding rule chosen and frozen, the distance-to-boundary shoulder proved bit-identical to an `f64` reference, a bounds rectangle from the vertices grown by the feather.

### Model selections (phase E)

Subject, Sky and Background, and later People and Objects, are a **model** kind whose coverage is a raster the host computed once and stored, not a rule it evaluates per pixel. Everything a model selection needs already exists in the [module capabilities](module-capabilities.md) contract; this design uses it and adds no capability.

| Concern | Design |
| --- | --- |
| The model | A declared, pinned **resource** of the masks host: an open-licence segmentation model with its version, bytes, SHA-256, licence and provenance in the descriptor. Installed through `module.resource.install` under the `download-artifact` grant, verified by hash before it is installed, removable without touching an edit. Nothing about a photograph leaves the machine: inference is local, so no `remote-image-request` is declared |
| Computing a selection | A **task** on the module lane, `task.select-<class>` with `asset_id` and the class's parameters, that reads the content stage through the host, runs the model, and publishes one **derived artifact**: an 8-bit alpha raster at the content stage's aspect, its long side capped at 4096 px, with its kind, dimensions and the model's identity in the artifact's metadata. Progress, cancel and failure are the job contract's |
| The component | `model {artifact, class, model, model_version, source_fingerprint, feather, expand, threshold}`. Coverage is the artifact sampled bilinearly at the pixel's normalized position, thresholded and feathered by the frozen `smooth`; `expand` grows or shrinks the selection by a stated distance, which needs the same neighbourhood treatment as an edge-aware refinement and is therefore **not** in the first slice. A model component is position-based: it ignores the pixel it is handed, answers `bounds()` from the artifact's non-zero rectangle and `min_feature_px` from the artifact's own resolution, so the overlay draws it and the proxy rule treats it like a gradient |
| Creating one | New mask › Sky runs the task and, on success, creates the mask with an `add` model component referencing the artifact, as one history entry labelled `Add sky (model)`. `mask.add-model {mask, mode, artifact, class, …}` is the generated command an agent uses with an artifact it already has; `task.select-sky` is what it calls to get one. The two are separate on purpose: a selection is a durable artifact a recipe references, and a task is a job that produces one |
| Staleness | The component records the source fingerprint and the model version it was computed from. When the asset's source changes (a Locate to a changed file) or the installed model's version differs, the row shows **Needs recompute** with the reason, and offers Recompute (the task again, then `mask.set-model` with the new artifact, one entry) or Keep as is. The stored selection keeps rendering meanwhile, because the artifact is immutable and referenced; nothing is recomputed silently, which is the [Lightroom research](../research/lightroom/geometry-masks-and-retouching.md#a-mask-is-an-editable-selection-plus-an-effect)'s distinction between recomputing an automatic selection and preserving an accepted one |
| Not installed | The kind is listed in the New mask menu because it is registered; choosing it shows the capability block's resource card (name, size, licence, "runs on this Mac, sends nothing") with Download, and the consent notice over the canvas when the grant is missing — the delivered flow, not a new one. A model component in a recipe whose model is not installed still **renders**, because the artifact is what renders; only Recompute needs the model |
| Memory and time | The artifact is at most 16 MiB (4096² bytes) and is pinned through the prepared-artifact cache like any other; inference runs on the module lane, never on the owner or UI thread, and its peak memory is measured per model and recorded before the model is pinned. A task that exceeds a declared budget fails with `resource-limit` |
| Runtime | Local inference needs an inference runtime, and the capabilities contract refuses `local-runtime` today. That refusal is the first thing this phase has to lift, with a measured choice: a pure-Rust runtime (`tract`, Apache-2.0/MIT) keeps the toolchain rule and the licence rule, at the cost of a smaller operator set; ONNX Runtime is broader and native. The decision is the owner's and is recorded below; nothing here assumes either |

What a model selection is honest about, on its row: the model's name and version, when it was computed, and that it is a *stored* selection that ages with the source and the model. Lightroom's "mask needs update" becomes a stated reason and two explicit actions.

### Edge-aware refinement (phase F)

[Proposal P7](masking.md#proposals-with-recorded-defaults) stays out until it has a study. The shape it would take is stated here so the panel does not have to change when it arrives: a **per-mask** refinement — `Refine edge {radius, strength}` under Amount and Invert — implemented as a guided filter of the composed coverage against the masked operation's input, run as a spatial unit with the delivered halo, tiling and budget rules. It is per mask and not per component because a guided filter refines a boundary, and a mask's boundary is the composition's. It would make every mask that carries it read pixels, with the same overlay consequences the range kinds have.

### The range widget

The luminance range's four numbers are the histogram's own axis, and today they are four number fields. A **two-thumb range slider** with shoulder grips — the band between the thumbs, the shoulders drawn as fades outside them, over a black-to-white rail — is the one control the [UI components](ui-components.md#control-kinds) vocabulary lacks. Proposed as a `range {action, low, high, low_feather?, high_feather?, label, rail?}` control kind bound to four number parameters of one action, drafting as a slider does. It is generic: a tone curve's or a future detail module's ranges would use the same widget. The four number fields stay under it, because a typed value is exact.

## Later, named so nothing is drawn for it

- **Depth** range and a depth-based selection need a depth map, which is either embedded (some phones) or a second model; it joins the model kind's shape when there is a source for it.
- **Masks in presets.** The delivered [presets](presets.md) transfer values the modules have controls for. A preset carrying masks is a design of its own: a geometric mask can be copied as data; a model selection is an instruction ("select the sky") that must be re-run on the new photograph, which is what makes an adaptive preset adaptive.
- **Copying masks between photographs** waits for a second photograph.
- **People** (face parts) and **Objects** (a prompted selection) are model classes with their own tasks and their own resources, after Subject and Sky have been measured.
- **Coverage thumbnails as filmstrip** — a row of every mask's thumbnail under the canvas — is a view the list already answers; not proposed until the list proves too slow to scan.

## Constraints

- The panel is view work: no new command, no new draft path, no new render. The thumbnail reduces a grid the preview already returns and is cached by that grid's identity; the scope chip is a band decoration. Re-derivation stays per section, and a brush gesture's 16 ms tick sends exactly what it sends today.
- Model selections add exactly what the capabilities contract already bounds: one resource per model, one task lane, one artifact per selection, the prepared-artifact cache, and a stated peak for inference. No frame is allocated for a selection at render time; the artifact is sampled like a texture.
- A recipe's mask limits are unchanged: 16 masks, 32 components, the serialized bound. A model component's payload is a reference and a handful of numbers, so it costs the serialized bound less than a brush stroke reference does.
- Nothing here changes what `render.sample` returns for any delivered kind, and a model component must satisfy the same rule: the sampled byte equals the rendered byte inside, outside and across the artifact's feather.

## Verification and acceptance

- **Rendered.** A `mask-panel` smoke scenario over the delivered `mask-combine` catalog captures: the list with three masks and the open one's rows, the selected component's fields, a hover on a component row, the overlay control in each of its four modes and both tints, the New mask menu, the Brush section appearing when a brush is armed and disappearing when it is put down, the scope chip on a bound section, and the draft bar for a radial and for a stroke. Each frame's state records the session's selected mask, component, overlay and mode, and the runner checks row placement by state as the workspace scenario does.
- **Parity.** Every control on a row sends the command the delivered `Copy as JSON request` shows, proved by the existing app parity tests extended to the new widgets; `X`, `⌫` and the reorder keys send `mask.set-component-invert`, `mask.delete-component` or `mask.delete`, and `mask.reorder-component` or `mask.reorder`.
- **Density.** The default board's panel contents fit 830 pt at 1440 × 900, checked by the scenario's placement expectations.
- **Model selections**, when authorized: the resource installs and verifies under the delivered flow; the task publishes an artifact whose bytes are identical across two runs on the same source and model; `render.sample` equals the rendered byte for a model component; a changed source marks the component stale and Recompute replaces the artifact as one entry; an uninstalled model still renders the stored selection; peak memory and time per model at 24 MP and 60 MP are recorded in the performance plan before the model is pinned.
- **Polygon and range widget**: the kind's study and `f64` reference before it ships; the widget's geometry functions tested in `lightwell-ui` with no `lightwell-core` link; the gallery gains their states.

## Decisions

| Decision | Recommendation | Effect if changed |
| --- | --- | --- |
| Panel layout | The rows, band, scope chip and Brush-on-demand above, at the module-panel density | Keeping the delivered text-run rows keeps the panel three times taller than the module panels for the same information |
| Component mode control | A three-glyph `+ − ∩` segmented control on every row, first component dimmed to `+` | Words (`Add · Subtract · Intersect`) do not fit a 26 pt row at 300 pt with a name, an invert and a menu |
| Overlay while dragging | Show the tint during a shape gesture whatever the setting, restore after | Without it a person drags a handle over a photograph with no sign of what the mask covers |
| Polygon kind | Add it, study-first, as a typed kind | Agents post regions as brush strokes or radials, which is a poor fit for a bounded region |
| Model selections | Subject and Sky first, local inference only, as resources plus tasks plus artifacts | A remote provider would need per-asset consent for every selection and a data class for the whole content stage |
| Inference runtime | Owner's call: `tract` (pure Rust, keeps the toolchain rule) or ONNX Runtime (broader operators, native) — measured on one candidate model each before the choice | Lifting the `local-runtime` refusal in the capabilities contract is a change to a shipped contract either way |
| Artifact cap | 4096 px long side, 8-bit alpha, bilinear | A full-resolution selection at 60 MP is 60 MB per selection per entry-reachable artifact and gains nothing a feather does not give |
| Range widget | A `range` control kind, generic | Four number fields stay; a band on the histogram's axis is then never seen as a band |
| Refine edge | Stays out until its study; per mask when it comes | Per component would filter a boundary the composition then moves |

## References

- [Masking](masking.md), its [mask study](mask-study.md) and [range study](range-study.md): the delivered model, algebra, kinds, commands, limits and proposals P1 to P16.
- [Develop workspace](develop-workspace.md) and its [Module panels](develop-workspace.md#module-panels): the density, hierarchy and tokens the panel uses.
- [UI components](ui-components.md): the control vocabulary the range widget extends.
- [Module settings, permissions and managed resources](module-capabilities.md): resources, tasks, consent and derived artifacts, which model selections use unchanged.
- [Corrections](corrections.md): the other consumer of the shared brush.
- Research: [Lightroom geometry, masking and retouching](../research/lightroom/geometry-masks-and-retouching.md), [darktable geometry, masks, blending and retouching](../research/darktable/geometry-masks-and-retouching.md).
