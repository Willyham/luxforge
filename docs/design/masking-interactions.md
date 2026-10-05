# Masking interactions

Status: implemented and verified with final quick/rendered checks, native RAW liveness and
scoped 24/60 MP hover and continuous-paint measurements. The [performance evidence](../specs/performance.md#native-masking-interaction-qualification)
records timing, memory and diagnostic limits; total editor memory and display latency remain unqualified.
The [mask performance work](mask-performance.md) moves exact overlay painting off the UI thread,
keeps its handoff bounded, and records a scoped native brush comparison.
The [masking model](masking.md), frozen coverage equations, content coordinates, stroke storage
and current catalog/API shapes remain the shared contract. The [workspace design](masking-workspace.md)
describes the panel.

## Placement and ownership

New mask and Add component → Linear or Radial arm an unplaced tool. Choosing the kind creates no
core draft, initial geometry, handles, history entry or mask. One `render.transform` answer supplies
the content map; pointer moves use that map locally. A valid drag opens the ordinary draft on the
actual geometry; if that draft cannot open, the tool stays in hand, unplaced. A click or invalid
extent remains unplaced. As in Lightroom, the release of the placing drag commits it as one entry:
there is no Apply, and the draft bar ends with Done, which with nothing placed puts the tool down,
as Escape does. Escape while the pointer is down discards the drag.

New mask creation owns the tool from arming until its successful creation commit or cancellation,
and an Add gradient owns it from arming until its release commits or it is put down. Unrelated mask selection,
row editing, adjustment fields, history, presets, Open, Export, mode changes, zoom, panel toggles
and the palette are disabled, and desktop dispatch refuses them through one message
classification. The mask's own shape/brush controls, coverage controls, canvas, row hover, focus
movement, native scrolling of a zoomed view and Done/Cancel remain available. Active brush
strokes refuse selection changes. Between strokes, selecting a different mask/component puts the old brush down;
an explicit Paint more/Add/New action starts painting on the visible target.

The first successful new-brush stroke completes creation. Each released stroke is one history
entry; subsequent strokes continue on its committed component. A recoverable refusal or conflict
keeps recovery and Cancel available. A commit already in flight decides publication before a
requested cancellation is resolved. Cancellation never removes an accepted entry or changes an
original file.

### Resting handles

A committed linear or radial keeps its handles, as Lightroom's do. Committing a gradient selects
the component it created or added, opening a mask selects its first gradient (a mask with none opens
with nothing selected), and in Mask mode the selected gradient's handles rest on the canvas
whenever nothing is held, the listing describes the displayed entry at the current state and no
comparison is shown. Resting handles are desktop view state: no core draft, no `session.state`
draft, no draft bar and no ownership of other controls. They are drawn from the stored payload and
reopen from it whenever the payload changes (a commit, an undo, a typed field). Each new displayed
entry asks `render.transform` again; the previous map keeps drawing the handles so they do not blink
after a drag, but a press waits for the fresh map ("Waiting for mask coordinates").

A press on a resting handle opens that drag's draft in the press's own update, under the start
refusal every mask gesture answers, exactly as a brush press opens its stroke: `draft.begin` and the
first `draft.set` carry the stored shape. Moves are drafted; the release commits one entry
(`Update Radial 1`), and the handles return to rest on the committed shape. A release that moved
nothing discards its draft and writes no entry. Escape during the drag discards it, and a commit
refused as changed elsewhere keeps the ordinary Discard and Reapply notice. A press away from the
handles is not captured, so it never redraws a committed shape; there is no separate Edit shape
gesture. A drag is not a tool start: an Off overlay and a hidden eye stay as they are.

Late answers stay with the target they were asked for. A canvas pick whose selection moved
while its locate or sample was out is dropped with a reason. Cancel or Escape during a stroke's
commit lets that commit decide its entry and puts the brush down. An Add brush in hand takes the
Add row's current mode, and mask thumbnails follow the Masks panel into the picks taken from it.

Changing masks clears incompatible component hover. Hovering a component row requests its own
contribution; leaving the row restores composed coverage. Selection, keyboard navigation and
mouse actions use the same controller rules.

## Coverage and presentation

The canvas draws one actual evaluated coverage field. Brushes draw cursor outlines, with no
separate painted-path tint. Flow, feather, colour hold, erase, component modes, inversion and mask
Amount are evaluated by the production mask evaluator for the accepted candidate recipe. Live
coverage and committed coverage therefore use the same equations and exact first-bound-layer
input for pixel-dependent masks.

`Evaluation::mask_overlay_coverage` fills only bounded display cells on an independent latest-job
worker: one active request, one replaceable pending request and one bounded cached grid. Requests
carry source/entry, accepted draft ID/revision, pixel content, view/region, mask/component and a
selection epoch. A newly created target is resolved inside the same effective evaluation that
fills its grid. No temporary identity from another planning call or old selection is substituted.
Old coverage is cleared at a target/visibility boundary; stale replies cannot restore it. A
request cancelled there while it computes delivers nothing, so it is not pending coverage and no
evidence step waits on it. Refusal reports an explicit outcome and reason. Neither the desktop nor
the idle coverage cache retains an evaluation, RAW source or full-resolution coverage plane.

Continuous drawing lets an accepted snapshot finish while a newer snapshot waits. Progressive
feedback stays within one draft and one unchanged source, base entry, layer stack, other masks,
target and view; revisions cannot move backwards. A bound mask's grid is shown only with its
matching photograph content. Selection and commit results retain strict identity checks. Exact
coverage with unchanged inputs can be rebound to a refined photograph generation/quality without
recomputing or uploading photograph pixels.

Coverage-only changes reuse photograph pixels. An unbound candidate or mask-only commit may also
reuse settled photograph pixels when the pixel-content identity and view agree and no old photo
completion is pending. Logical entry/draft metadata and a matching exact report advance together.
A changed bound mask still renders its adjustments through the ordinary photo pipeline. Missing
providers and exact pixel-input refusals remain explicit.

Fit and viewport requests use the same content transform and exact evaluator. The grid is a
quantized coverage display, independent of whether the photograph is currently a proxy or exact
frame. Green/white tint and both black presentations paint that one grid once on the coverage
worker, using an exact 256-code palette. Delivered results retain identity, dimensions and shared
RGBA; the cache alone keeps the coverage plane. A handoff lease bounds worker and consumer
buffers through adoption or replacement of a waiting result, and semantic cancellation wakes a
blocked worker. The [performance design](mask-performance.md) states the unchanged byte bound.
Coverage that finishes before its matching photograph is drained when that photograph is adopted,
without first waking an undrawable window redraw. A later completion, feedback on unchanged photo
pixels and an unavailable outcome wake normally; publication before draining prevents lost wakes.

## Visibility and keys

Starting a drawn tool shows coverage automatically when the stored setting is Off. A setting
already showing coverage is kept. Automatic visibility and the gesture's explicit manual choice
are separate local view facts; the UI selection reflects the effective presentation, while
captured state reports stored/effective/forced values. A held tool shows its own mask's coverage
whatever that mask's eye says. An explicit Off or `O` while drawing is honoured for the remainder
of that tool interaction.

In Mask mode, `O` toggles coverage between Off and Tint during unplaced creation, placed gradients,
armed brushes and held strokes. Both extra black presentations remain UI choices. Outside Mask
mode, `O` toggles thirds. Text capture and repeat/modifier rules remain those of the common keymap;
Cmd/Ctrl+O is Open when creation allows it. No auxiliary modifier binding is added.

## Capture bounds and the pointer

The grid capture retains at most 16,384 distinct positions, the existing posted-input bound;
consecutive positions in one grid cell cost only a count. A failed capture stops growing, reports
`ResourceLimit` or its original validation error, and cannot post/commit a truncated or empty
successful stroke. Releasing it discards that stroke's draft and keeps the brush in hand on the
same target, so the next press is the new stroke. The frozen grid and decimation rule are unchanged; the stored limit
is still 1,024 positions and the existing 64-segment occupancy limit still applies. Decimation is
cached per accepted capture/brush size, so summaries do not repeat whole-path work. No live path
is tessellated to impersonate coverage.

Nothing is read under the pointer as it moves. The status-bar readout that asked `render.sample`
on every move was removed on 2026-10-04 (owner): through a stack with Detail, Presence and masks
the exact point path cost the point worker minutes, and the desktop ran that wait on the update
loop, so the window froze with it. A pointer move now publishes its position for a pick and for
the evidence cursor sync and asks the owner for nothing; `render.sample` remains the API's exact
point query.

A value typed into a generated field but not submitted belongs to the mask and component it was
typed for: when the fields address another target, the edit is dropped and the fields reseeded.

## Verification scope

Focused actual-owner tests cover unplaced placement/cancellation, controller and visible guards,
selection/disarming, O overrides, capture failures, exact candidate IDs/coverage, cache and delayed
completion fencing, pixel reuse and bound rerendering. Independent reference/exact-buffer tests
remain the mask equation and source/history authority.

`mask-interactions` is the dedicated 44-frame native regression: disjoint A/B selection, an old
armed brush put down on selection, live linear/radial/feathered retracing brush coverage versus
commit, manual hiding during a new gradient, and placement under an 8° crop at Fit and 100%.
Existing mask, viewport, clipping and history scenarios check the surrounding editor integration.
The final quick and rendered tiers pass, including all 36 native scenarios. Dedicated masked
Basic/Presence cancellation tests restore exact committed bytes, public sample answers, history
and coverage, and reject a late draft reply. Workspace capture tests also reject an old overlay
mode or colour before the session answer.

The supplied Nikon Z6, Fujifilm X100VI and DJI Air 2S RAW sources each pass 30 native captures and
seven exact white-balance redevelopments. Current photo, histogram and coverage identities agree;
cancellation, Off, mode changes and a 100% viewport recover without stalled source gates. Black
mask probes have zero code drift, history contains only the mask and seven requested RAW edits,
and every original hash remains unchanged. This is functional liveness evidence.

`editor-latency --mode hover` routes native cursor movement through the actual laid-out widget
tree after a masked Clarity adjustment and a new brush is armed. Input epochs, pointer updates,
workspace derivation, drawn cursor geometry and renderer readbacks are correlated. CPU geometry
submission and image readback do not measure display scanout. The final matrix has 18 paced hover runs, 540 emitted moves and 60 separate corresponding GPU
readbacks; hover triggers no point queries, photo jobs, mask mutation or photograph uploads. Bound
and unbound painting also progress on 24/60 MP inputs. No numeric cursor budget is implied by this
repair.

## Acceptance matrix

| Behavior | Evidence |
| --- | --- |
| No initial gradient; valid placement committed on release, and cancellation | Actual-owner mask tests and native `mask-interactions`, `mask-linear` and `mask-combine` |
| Creation owns unrelated controls; recovery stays available | Controller/state tests for masks, keys, history, presets, controls and Performance; correlated native draft/control state |
| Selected mask/component, armed brush and visible coverage agree | Selection and delayed-result tests; disjoint native A/B pixels and subsequent stroke target |
| Actual flow, feather, retracing, composition and commit coverage | Frozen independent mask-reference tests, exact candidate grids and native live/committed probes; no separate painted-path layer |
| O/manual visibility, extra UI modes and hidden eyes | Key/controller tests, stored/effective state and native renderer captures |
| Crop/rotation, percentage zoom and refinement | Content-map tests, transformed live-gradient captures and native viewport scenarios |
| Cancellation, conflict, invalid capture and resource refusal | Actual-owner/worker tests and brush/range recovery scenarios; source/hash/history checks |
| RAW development can continue after coverage work | Dedicated background native RAW coverage/redevelopment checks |
| Hover and continuous painting remain bounded and progress | Real widget-route diagnostic, photo-sized paced strokes, capture-limit and progressive-worker tests |

A native capture proves the rendered frame and its correlated state; timing and resource figures
retain their own sample count, workload and host scope. The 1000-position native diagnostic exceeded
the event-log ceiling and remains failed; successful 400-position holds and deterministic capture
limit tests establish the stated progress/bounds, without worst-case native latency claims.

## Allocation and request review

These changes cover `luxforge-core`, the desktop adapter, the shared evidence schema and the
small disclosure-widget enablement change. Sources still enter through the verified prepared-source
cache. Coverage workers receive the existing evaluation and release it after their job; the idle
cache holds only a bounded grid. No new full-photo allocation, source hash/decode path or persistent
pixel data is introduced.
Capture storage is capped at 16,384 snapped positions and 1,024 posted reduced positions.
Coverage retains the existing 4,096-cell side limit (at most 16 MiB per byte grid); painted RGBA
overlays and native backend staging remain outside the provisional photo-texture ceiling.

The owner plans/validates recipes and identities; workers evaluate coverage and photograph pixels.
Selection, overlay presentation and unbound mask changes submit no photograph work or uploads when
their pixel identity is unchanged. Bound edits still use the ordinary narrow mutation refresh;
history is not listed per move. Point queries and no-op validation allocate no photo frame. No
ordinary timer or polling loop is added: the coverage worker blocks while idle and wakes on results;
the bounded native cursor probe runs only during evidence collection.

Frozen independent reference tests remain the numerical authority. Worker tests verify
identity fences and buffer reuse. The masking measurements in the [performance specification](../specs/performance.md#native-masking-interaction-qualification)
record photo-sized native scope separately from rendered correctness and from display scanout.
The whole-recipe `editor-performance` before/after matrix was not rerun: no effect equation or
renderer kernel changes, and the measured bottleneck was the desktop hover query path. Targeted
native hover and paint diagnostics are the performance evidence, with exact `render.sample` and
frozen reference tests preserving query/effect correctness. Retained raster and source/region
sharing tests prove the reuse claims. Coverage outcomes and narrow owner responses preserve
UI/API parity, source hashes and committed history.

## Remaining scope

Density, edge-aware refinement, model selections, copying masks between photographs and mask
presets remain outside these repairs. Range masks still need photographic-corpus qualification.
Additional modifier shortcuts and relaxed navigation during creation require a separate product
choice. Native M4 results do not qualify Windows/Linux GPU behaviour.
