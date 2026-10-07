# High-zoom minimap

Status: **planned; implementation is not authorized by the planning request**. The owner requested a minimap at zoom levels of 200% or more. The owner chose click-to-jump navigation and dragging the viewport rectangle. The layout and engineering defaults below remain recommendations. [Tasks](../../tasks/interface/minimap.json) contain the implementation work.

## Behavior and scope

In Develop, show a small overview of the whole photograph with a rectangle marking the visible region. It appears when percentage zoom is **at least 200**, including typed fractional percentages and macOS pinch; it disappears immediately below 200 or at Fit. Use the actual view value, not the rounded status-bar text or the zoom-stop index. Fit remains Fit even for a small photograph that it enlarges past 200%.

Navigation: click inside the overview to centre the main viewport there; drag the viewport rectangle to pan, retaining the initial grab offset. A press outside the rectangle centres there and can continue as a drag. Clamp to the image edges; an axis that fits stays centred. Zoom stays unchanged. Letterboxing around the thumbnail is inert. A full-image rectangle remains visible when the whole photograph fits, and navigation on fitting axes has no effect.

The minimap is canvas chrome, anchored at the lower right inside the viewport, above the scrollbars and filmstrip. It stays visible when either side panel is hidden and does not enlarge the scrollable image. Start with a 180 × 120 pt image area, aspect-fitted without distortion, a 12 pt inset, a quiet theme surface and a viewport outline with a contrasting inner/outer edge. These dimensions and ordinary responsive positioning are implementer choices: shrink or reposition it on a small canvas to keep the mode strip and essential chrome usable. Use existing theme tokens and interface scaling. No new preference, pin/hide control, panel, dependency or keyboard shortcut is in scope.

The minimap shows image pixels only, without thirds, clipping colours, mask tints, handles or notices. Its rectangle describes the underlying geometric viewport, including the area under floating chrome, rather than a fragmented region around overlays. It follows scrolling, Space-drag, minimap navigation, pinch, zoom stops, typed/API zoom, resize, display/interface scale and panel/filmstrip changes.

Navigation remains view-only while a crop or mask draft is open. It must not paint, sample, move a crop handle, alter a comparison divider, enter another mode, create/cancel a draft or commit an edit. Pointer events beginning inside the minimap belong to it through release, including outside its bounds; wheel input there follows ordinary pan, and native pinch there must not also reach the photograph. A drag ends on release, Escape or focus loss, retaining its last applied view. Escape consumed by that drag must not cancel an underlying edit draft.

Use the same stage the main canvas is navigating:

- Ordinary, masked and historical views: the selected entry's output after orientation, crop, straighten and other geometry.
- Crop draft: the rotated input-stage box used by the existing crop surface, with the same placement and empty corners; the rectangle refers to that box, not the committed cropped output.
- Before/After divider: a single overview of the fixed After side, labelled After while Compare is active; both halves share its geometry and pan. A held Before view shows the corresponding Original overview. Shift-held uncropped Original uses its own uncropped stage. Switching these views must adopt an overview with matching geometry before enabling navigation.

No photo, Select workspace, gallery, unrenderable preview or zero-sized canvas: no minimap. During opening or a geometry change, the eligible card may show a quiet updating placeholder until a matching overview exists; do not show another photograph or enable navigation against an old stage. Failed rendering withdraws stale image pixels consistently with the existing preview-unavailable behavior.

## Geometry and command parity

Reuse the placement arithmetic in `CanvasView`, `scrolled`, `pinched_view` and the editor's view geometry. One pure placement/projection helper should serve both the viewport rectangle and navigation; the widget owns only pointer capture and emits semantic pan intent. It holds no authoritative editing state.

For a percentage view, stage pixels become layout pixels by `s = zoom / 100 / effective_display_scale`. For each axis with stage extent `D`, viewport extent `V` and effective pan `p`, the image origin relative to the viewport is `max(0, (V - D*s)/2) - clamp(p, 0, max(0, D*s - V))`. Invert that transform at the viewport edges, intersect with `[0, D]`, and fit the resulting stage rectangle into the overview's image bounds. Invert the same thumbnail placement for clicks/drags; turn the requested stage centre into clamped pan. The actual crop-box transform and device-pixel snapping must agree with the main surface. Do not apply crop/EXIF transforms a second time or confuse layout points with physical pixels.

Use the existing `view.set` pan fields and per-client `ViewState`; no minimap-specific mutation command or durable recipe field is needed. Extend the registered schema's pan descriptions to state the existing units and origin, and expose the resulting pan through the existing session response. GUI navigation and a same-client API request with equivalent pan values must produce the same session and visible region. Clients remain isolated.

Reuse the desktop's immediate local pan and one-in-flight/one-newest-pending coalescer. Programmatic minimap motion must also move the real scrollable, not just the session or drawn rectangle. A late answer cannot undo a newer local pan or restore an old asset/stage after a switch. Navigation changes no recipe, history, preview content generation or histogram identity. Newly exposed main-canvas regions still use ordinary view planning.

## Overview rendering and lifetime

At 100% and above, the GPU's picture-at-rest tiles already cover the full output stage for histogram/clipping counts; their reduction is currently absent. Add an optional bounded overview reduction to that existing final tile sweep at eligible zooms. Keep it independent of the main region output: it must neither replace the full-detail main view nor start a second photo surface that prepares/uploads another source or renders the stack again. Support staged and chained tiles, light sweeps and the existing fallback. CPU reduction borrows an already-required retained reference frame on a worker. It never requests a full reference render just to update the minimap.

An overview processes first at full stage resolution, then area-averages linear light and quantizes by the existing output encoding. It is a navigation thumbnail of the selected edited image, not the source, a camera/catalog preview or a stretched GPU region. Its settled output is held to the current declared display tolerance against the whole-frame CPU reference reduced with the same coverage. Add this output kind to the qualification cases; do not invent a looser minimap tolerance. Exact CPU reduction tests cover the same coverage independently.

Retain one current overview and at most one replacement in progress for the active stage. Its key includes asset, selected/framing entry, source signature/development, recipe content, stage geometry, renderer/output encoding and overview dimensions. It excludes pan, window position and high-zoom percentage when the overview dimensions and content are unchanged. Reuse a valid overview across pan, resize and zoom changes; do not repeat a tile sweep or upload solely to move its rectangle. A first threshold crossing without a retained overview may add the reduction to the ordinary required tile sweep, or request one existing bounded sweep when its counts have already settled; it must not delay navigation or the main frame.

An edit gesture may retain the last settled overview **with an Updating indication**; do not run a separate full-stage overview render per draft tick. After commit/cancel, adopt the matching settled overview from the ordinary render. If geometry changes, remove the incompatible thumbnail and disable navigation until the matching stage is ready. Completed results are checked against their full identity; photo switches, history changes, Compare changes, late jobs and cancellation cannot reintroduce stale imagery.

The overview raster is aspect-fitted within **256 × 256 physical pixels**. Bound all added overview GPU storage together at **4 MiB**, including current/replacement textures, accumulation, coverage/uniform data and retiring allocations, charged inside the existing GPU-preview budget before creation and deducted from tile availability. CPU overview buffers/scratch together have a **1 MiB** cap and share retained image buffers rather than cloning them. Smaller views can use smaller raster bounds. No additional full-photo allocation, source residency or GPU device is allowed. If storage cannot be reserved, retain a correctly keyed ready overview or show an unavailable placeholder without disabling normal image navigation. Release work/storage on photo replacement, leaving Develop and visibility teardown; suppress background overview work for hidden/minimized windows and use existing lifecycle wakes on return. Cache bytes and rebuild/reuse counters are attributable in the existing diagnostics/resource reporting.

## Acceptance and evidence

1. Exactly 200% shows the minimap; 199.9% and Fit do not. Test 200.1%, all stops above 200, 1600%, pinch crossing and API/typed zoom. Status rounding never decides visibility.
2. The viewport rectangle and click/drag result agree with independent geometry calculations at centre and every edge, landscape/portrait, a fitting axis, 1×/2× display scale, interface scaling, panel/filmstrip changes and resize. Letterboxing is inert; captured drags remain bounded and end safely.
3. Navigation reaches the existing command service, moves the scrollable and changes only view state. Equivalent same-client API pan has the same result; other clients, recipes, source checksums, history, draft contents and histogram identity stay unchanged.
4. Crop, masks, warped geometry, historical selection and both Compare forms obey the stage and event rules. Clicking the minimap with a brush/picker/crop mode active never edits the image. Rapid switches, cancellation, unavailable originals and failed/stale outputs show no mismatched image or actionable old rectangle.
5. Panning with a ready overview performs no overview render, source upload or unchanged-thumbnail upload. All storage is reserved within the named caps, including replacement/retirement; no new idle timer/poll or owner/UI-thread image work is introduced. Tests exercise exhausted budgets and no-GPU/reference fallback.
6. A real background rendered journey on the owner's M4 correlates minimap bounds/identity/rectangle, session pan, main-surface geometry, logs and captures on generated 24 MP and 60 MP JPEGs and the existing supplied RAW fixtures. Include threshold, click/drag, edge clamp, edits, mode interception, crop, Compare, resize and recovery. The CPU reference is exact; settled GPU overview comparisons use the declared tolerance. Headless/software checks do not establish native GPU correctness.
7. Once delivery is complete, record photo-sized release measurements of first-overview latency, pan input-to-presented-frame p50/p95, added retained/peak bytes, overview rebuild/reuse/upload counts and idle behavior on the M4. Use 199.9% as a threshold control and 200%/800% with the minimap, declaring that differing zoom windows are not an isolated rendering comparison. Measure warmed/cold availability separately; no latency or total-memory claim follows from the plan itself.

## Performance-rules review for implementation

- Original access stays with the verified source cache; no minimap input reads, hashes or decodes it.
- There are no new full-frame buffers. The 256-pixel-side overview and explicit CPU/GPU caps bound all additional storage; existing source/reference frames are borrowed.
- No point query, validation or no-op navigation renders a frame. The owner handles session pan/schema data and bounded plan metadata only; reductions run on the renderer/worker.
- Pan keeps the existing narrow completion and view-plan path, with no added `asset.state`, history refresh or analysis request. A changed content/stage uses its ordinary required render; first overview availability may need one bounded tile sweep.
- Existing input, render completion, retirement and visibility wakes suffice. No idle subscription timer or polling loop is added.
- The overview key and byte cap above bound reuse. Qualification measures rebuild/reuse/upload counts and bytes; unchanged inputs must not rebuild it. The lifecycle must not keep RAW planes alive.
- The final measurement task covers 24 MP and 60 MP interaction costs; generic core `editor-performance` does not isolate minimap projection/presentation. Record that limit rather than claim a speed gain.
- CPU reduction gets exact-buffer tests, GPU overview reduction gets declared-tolerance reference comparisons, and sharing/reservation/retirement tests prove allocation claims. Existing main-image and histogram output remain qualified.

## Execution

Four tasks, three dependency waves: shared placement/navigation projection and bounded overview rendering can start independently; the canvas widget integrates both; final qualification, measurements and documentation follow working delivery. Ordinary correctness and a targeted rendered integration check accompany the integrated widget. Extensive qualification and measurement do not hold up that useful delivery. Re-review against current `main` before implementation. The plan uses the roadmap's high-tier model minimum.
