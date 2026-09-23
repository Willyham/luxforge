# Instant previews: display-bounded proxy rendering

Status: implemented and verified on the M4 Mac; the measured figures against the targets below are in [performance](../specs/performance.md#instant-previews-proxy-phase-hop-rule-and-the-surface-primitive). It changes how the desktop previews a stack while a gesture is open and how the Fit view is produced; it changes no recipe, history, API mutation or export behaviour, and the exact full-resolution render remains the only source of the histogram, the clipping counters and the 100% view. The measurements that motivated it and the ones that qualify it are in [performance](../specs/performance.md).

## The problem, measured

Every slider input today costs one full-resolution render and one full-resolution GPU upload, bracketed by two 16 ms timers ([performance](../specs/performance.md#desktop-slider-to-presented-frame-and-settled-histogram)):

| Step, 24 MP JPEG at Fit | Cost |
| --- | --- |
| Wait for the 16 ms slider tick before `draft.set` is sent | 0 to 16 ms |
| `draft.set` round trip plus the preview job, on the owner | 8 ms (33 ms under a rotated crop, which is CPU contention with the render, not owner work) |
| Full-resolution CPU render: orientation copy, colour pass over 24 MP, crop resample | 15 ms (Exposure alone) to 140 ms (every Basic unit), plus 30 ms for a rotated crop |
| Wait for the 16 ms preview poll after the worker finishes | 0 to 16 ms |
| GPU upload of the 24 MP RGBA raster | 32 ms (50 ms at 60 MP) |

The presented frame lands 75 ms after the input at the median with one unit active, 150 ms under a crop and 125 ms at 60 MP, all before the pointer's own tick wait. A wild drag never sees the newest value on screen: the pipeline keeps at most one job active and one pending, so most inputs are coalesced away and the ones that survive are shown four to nine frames late. The screen shows at most 2880 × 1800 physical pixels, so at Fit at least four of every five rendered and uploaded pixels are discarded by the GPU's minification. RAW is worse: the linear path evaluates every pixel recursively in f64, so a Z6 exposure drag costs 133 ms per frame.

## Goal

A slider dragged back and forth wildly at Fit shows the value under the pointer within two display frames, on the owner's M4 Mac, with every Basic unit active, on 24 MP and 60 MP JPEGs, under a rotated crop, and on the qualified RAW sources for the RAW exposure slider. The targets below are provisional thresholds in the sense the [performance plan](../specs/performance.md#provisional-budgets) gives that word; a miss is reported with its figures.

| Provisional target | Threshold |
| --- | --- |
| Input to presented frame, drained drag, 24 MP and 60 MP JPEG at Fit, full Basic layer, with and without a 7° crop | p95 < 16 ms, acceptable below 32 ms (the owner's target of 2026-09-22; this design was measured against p95 ≤ 33 ms) |
| Input to presented frame, RAW exposure drag at Fit, Z6 and X100VI | p95 ≤ 50 ms |
| Burst drag (120 inputs per second for three seconds, alternating direction): presented frames per second | ≥ 30 |
| Burst drag: staleness of each presented frame (its input's time to its presentation) | p95 ≤ 50 ms |
| Settled exact histogram after the last input, 24 MP | p95 < 200 ms (unchanged) |
| Idle CPU and process memory | unchanged targets; the proxy adds at most one bounded buffer |

"Presented" keeps its harness meaning: the desktop update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests. It is not scanout.

## Design

### Render what the display can show

A **proxy source** is the prepared source downscaled once to the size the display needs, and a **proxy render** is the whole effective recipe rendered against that proxy source through the existing compiled path. Every layer that can be drafted or committed today is resolution independent: the orientation layer is a mapping, the crop payload is normalized, the Basic layer and the RAW development layer are pointwise. So the recipe compiles unchanged against the smaller content stage and produces the same picture at display size, with the same code and the same colour arithmetic, at a fraction of the cost. Nothing is approximated inside the colour path; the only thing that differs from the full-resolution render is the resampling that the display was going to do anyway. (A drafted RAW white balance is approximated before the proxy is involved, at either scale, and is labelled; see [below](#a-raw-white-balance-during-a-drag).)

- `ProxyBounds { width, height }` are physical pixels: the photo area of the window at its scale factor. The desktop derives them from the window size and the open panels exactly as the clipping overlay's cell grid already does, and sends them with every preview request. The core clamps them to 4096 px per side and 8 megapixels.
- The proxy scale is `s = min(bounds.width / stage.width, bounds.height / stage.height, 1)`, where `stage` is the full-resolution output stage of the recipe (`compile`, `O(layers)`, no pixels). The proxy source is the content stage at `round(width × s) × round(height × s)`. When `s == 1` there is no proxy and the exact path runs as today.
- Downscale is an area average (box filter) with fractional coverage, separable, on the shared Rayon pool. JPEG sources average in linear light through the existing decode table and re-quantize through the existing threshold table, so a uniform region is exactly its own code. RAW sources average the planar f32 planes through the image's view, so the proxy is a smaller `LinearImage` with the same fingerprint and an identity view. The proxy of a JPEG is at most 64 MiB and of a RAW at most 96 MiB, and each is counted as a frame against the existing 512 MiB frame limit.
- One proxy source is cached per preview queue, keyed by the source identity (fingerprint, and for RAW the process-unique number of the developed planes, so a white-balance redevelopment invalidates it even when its planes reuse the old allocation's address), the proxy dimensions and the bounds. The cache holds pixels only: a RAW hit takes the developed planes from the cache and the development settings from the job that is rendering, so a drafted exposure renders against the cached planes. The cache is built on the preview worker on a miss and never on the owner thread. The bounds are decided on the desktop thread at the moment a job is requested, so a job queued after the display scale is known is already at it; an open requested at launch, before the window reports its scale, renders at a scale of one and is refitted once when the scale arrives (20–40 ms later on the owner's Mac). A resize, a panel toggle or the display scale arriving re-renders the proxy on screen once, coalesced by the queue, and never during a gesture or a crop draft.
- Eligibility is decided by the registry: a recipe is proxy-eligible when every layer's effect is at the source, colour, spatial, geometry or finish stage. A pixel-stage layer (the point-replacement proof module, whose coordinates are content pixels) makes the stack ineligible and the job takes the exact path, as does any compile failure at the proxy size. Ineligibility is reported in the result and the desktop's state summary, never silently. A spatial-stage layer (Presence) is eligible but approximate: its neighbourhoods scale with the stage they are rendered at, so its proxy frame is close to the exact render at display size rather than the same picture, and the result, the `preview_displayed` event and the state summary say `proxy_approximate` or `approximate` when a frame was rendered that way. The exact phase still produces every number and the 100% view, so the trade is a Fit preview that follows the drag at display cost against one that costs a full-resolution neighbourhood pass per input.

### Two phases per job

A preview job at Fit produces two results under one generation:

1. **Proxy phase.** The worker takes the cached proxy source (or builds it), renders the recipe against it and sends the proxy raster. The desktop uploads it and presents it. This is the frame the input-to-presented figure measures.
2. **Exact phase.** The same worker then renders the full-resolution frame and, when the job asked, reduces it into the exact report. The desktop adopts the report and retains the raster, exactly as it does today, but uploads nothing at Fit. The retained exact raster is what the clipping overlay is derived from, what the 100% view uploads, and what the histogram describes.

A job at a percentage zoom whose displayed size does not fit the bounds — 100% and above on any photo-sized source — has no proxy phase: the single exact result is uploaded and analysed as today, so the 100% view stays the exact render of the exact recipe.

The exact phase is cancellable. Every rasterizing pass, the resample, the streamed colour pass, the linear row pass and the reducer check a cancellation token at chunk granularity, so a newer request stops an exact phase within about a millisecond of asking and returns a `cancelled` error rather than a frame. `PreviewQueue::request` cancels the active job's exact phase whenever the new job supersedes it, so a wild drag never has a full-resolution render competing for the Rayon pool with the proxy render of the newest value; the exact phase of the last value completes once the gesture pauses or ends, and the histogram then reads the drafted population it describes today. Between a proxy frame and its exact phase the plot is marked updating, as it is between any input and its report. A drafted preview is therefore still analysed exactly, and the [histogram contract](basic-and-histogram.md#histogram-and-clipping-contract) is unchanged: no proxy raster is ever reduced into a report. The one drafted preview that is not analysed at all is a RAW white balance approximated on the developed planes ([below](#a-raw-white-balance-during-a-drag)): neither of its phases is reduced, and the plot stays marked updating until an exact frame's report arrives.

During a gesture, before the exact phase of the newest frame has landed, the clipping overlay is re-derived from the proxy raster on screen so it follows the drag; its event and the state summary say `approximate: true` in that case, and the exact overlay replaces it when the exact phase lands. A single clipped pixel is never lost in a settled frame.

### No waiting on timers

- A slider move sends `draft.set` immediately when no round trip is in flight, and records only the newest value otherwise; the answer sends the newest value, as today. The 16 ms slider tick is removed. The bound is unchanged in substance: at most one draft round trip in flight, at most one preview job per accepted value, intermediate values coalesced.
- The preview and overlay workers wake the desktop when a result is ready, through one channel subscription that yields the same `Poll` message the timer used to. The 16 ms preview poll is removed; nothing wakes when nothing has finished, which is also the idle rule.
- The `draft.set` round trip is not changed. Its measured cost is CPU contention with the full-resolution render, which the proxy removes.

### One frame per hop

Measured on the M4 Mac with per-leg timings: the owner answers `draft.set` and plans the preview job in under 0.2 ms, the desktop's own update, model derivation and view take under 0.15 ms together, and every message handed back into the update loop through the runtime — a task result, a worker's wake, an image allocation's answer — arrives about 8 ms later, one frame of the 120 Hz display. A redraw is always in flight during a drag, the main thread waits on its present, and a message that arrives meanwhile waits with it. The per-input path therefore has as few runtime hops as its work allows:

- The gesture's `draft.set` and preview-job requests are made synchronously on the desktop thread. They are two `O(layers)` owner requests; the owner does no frame work by rule, so the wait is bounded by catalog work alone.
- The photograph is drawn by a photo-surface primitive that owns its texture, at Fit and at every percentage: the raster handed to the view is written to that texture in the same frame that draws it, so no allocation round trip stands between the worker's result and the screen, and a redraw with no new raster writes nothing. The overlay and the crop draft keep the toolkit's image path. At a percentage the surface is the whole zoomed box inside a scrollable, far larger than the window, so it hands the renderer only the part on screen: the GPU viewport stays within the window, inside the device's 8192 px limit. An exact render wider or taller than that limit, such as a 60 MP photograph at 100%, is held in a grid of textures that meet without a seam.
- The worker's wake is the one hop that remains, because the raster has to reach the thread that draws.

"Presented" in the harness is the update in which the raster became the surface's source; it is drawn by the redraw that update requests, which is the next frame.

### The Fit view is the proxy

At Fit, and at any zoom whose displayed size fits the bounds, the presented texture is the proxy render, for drafted and committed frames alike. The photograph therefore never changes appearance between the last drafted frame and the committed one: both are the same recipe at the same size through the same filter. This replaces the GPU's bilinear minification of a full-resolution texture with a box-filtered display-size render, which is a visible improvement in aliasing at Fit and a change to what a Fit capture contains. The exact render is still produced for every committed frame and stays the source of every number.

Zooming from Fit to 100% uploads the retained exact raster when the exact phase has landed and re-renders nothing; when it has not, the view waits for that phase with the existing loading state. Zooming back to Fit uploads the proxy again (a cache hit and a small upload). A view change still triggers no render. The proxy's 4096 px per side does not bound the exact texture, which is written whole for 100% inspection, in tiles of at most 8192 px a side when it is larger than that; the proxy is one texture by construction.

### A RAW white balance during a drag

A RAW white balance is applied to the sensor mosaic before the nonlinear demosaic, so an exact frame at a new temperature or tint needs the mosaic redeveloped on the source worker: about half a second on the Z6 and one and a half on the X100VI. A drag therefore previews its drafted value **approximately**, on the planes already developed, and redevelops only for the value it commits.

The retained planes are `R · D(g)` per pixel, where `D(g)` is the native demosaic of the mosaic after the sensor gains `g`, in camera RGB (the Air 2S's gain map and optical warp run per channel inside it), and `R` is LibRaw's `rgb_cam`, the camera-to-linear-sRGB matrix. To first order `D(g') ≈ diag(g'/g) · D(g)`, so planes developed at `g` approximate the planes at the drafted gains `g'` by

`W = R · diag(g'_c / g_c) · R⁻¹`

with `R⁻¹` computed in f64. `LinearSettings::white_balance` carries `W`, and the linear evaluator applies it to each source pixel before the exposure multiply, `2^EV · (W · p)`, at the one point where settings touch source pixels. Without it the evaluation is bit for bit the exact one.

- **Where it applies.** Only `EditorService::preview_job` for an open draft, when the draft's effective gains differ from the development held in memory and that development exists. Equal gains need no approximation. A camera matrix with no usable inverse is refused as `preparation-required`, never rendered through a matrix that cannot describe it.
- **Where it never applies.** A committed or historical preview, `render_entry` and so every export, `sample_entry` and `sample_draft` and so the pointer readout and `render.sample`, `analysis_plan`, and the stage context every action and query is planned against all stay strict: a white balance the planes do not hold is `preparation-required` there. A drafted temperature's point sample is therefore refused while its preview approximates it.
- **Labelled and never analysed.** Both phases of such a job carry `PreviewResult::approximate_white_balance`, and the job is never reduced into a report even when it asked for one. The desktop presents the frames as it presents any frame, keeps the last exact report plotted and marked updating until an exact frame's report replaces it, marks a clipping overlay derived from one `approximate: true`, and says so in `preview_displayed` (`approximate_white_balance`), the state summary and the status bar: "Rendered in 7 ms (proxy, approximate)" at Fit, "(approximate)" at 100%.
- **The proxy and 100%.** The matrix is linear and the proxy's box filter is linear, so `W` applies to the proxy exactly as to the full frame (they commute to f32 rounding, which is a test), and the proxy phase is the approximate recipe rendered against the exact downscale, byte for byte. The proxy cache keys on the development and takes settings from the job, so a drag hits the proxy the committed frame built. At 100% the drag gets the approximate full-size frame with no proxy phase. The spatial estimate store keys an approximate evaluation apart from an exact one of the same recipe, so a committed Presence render never takes a global estimated from approximate pixels.
- **Release.** The commit redevelops the mosaic on the source worker exactly as before, and the last approximate frame stays on screen until the committed frame replaces it: the `raw-panel` smoke scenario checks that the committed frame is the first one handed to the surface after the commit, at Fit and at 100%. A drafted value whose development is not in memory at all — evicted while a redevelopment or source preparation of an earlier request is in flight — is still `preparation-required` and has no frame of its own; the status bar says it shows on release.

**Accuracy.** Measured on the three supplied RAW files by the ignored `measure_the_white_balance_approximation_against_redevelopment` test: planes developed at the camera's as-shot gains, rendered through `W` for each Custom target, against an exact redevelopment at the same gains, both rendered to 8-bit sRGB at a display proxy (bounds 2400 × 1600) and at full size. Mean |Δ| is per channel (R/G/B) in codes; p99 and max are of each pixel's largest channel difference.

| Camera | Target | Proxy mean | Proxy p99 | Proxy max | Proxy > 2 codes | Full mean | Full p99 | Full max | Full > 2 codes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Z6 | 3200 K, 0 | 0.07/0.03/0.06 | 1 | 36 | 0.003% | 0.21/0.07/0.16 | 2 | 54 | 0.26% |
| Z6 | 5000 K, +20 | 0.01/0.00/0.01 | 1 | 6 | < 0.001% | 0.03/0.01/0.02 | 1 | 9 | 0.001% |
| Z6 | 8000 K, −20 | 0.04/0.01/0.05 | 1 | 12 | 0.002% | 0.11/0.03/0.12 | 1 | 35 | 0.09% |
| Z6 | 6504 K, 0 | 0.03/0.01/0.03 | 1 | 13 | 0.001% | 0.08/0.02/0.07 | 1 | 25 | 0.02% |
| X100VI | 3200 K, 0 | 0.14/0.05/0.18 | 2 | 20 | 0.23% | 0.41/0.13/0.43 | 4 | 77 | 4.2% |
| X100VI | 5000 K, +20 | 0.02/0.01/0.01 | 1 | 4 | < 0.001% | 0.03/0.01/0.02 | 1 | 25 | 0.06% |
| X100VI | 8000 K, −20 | 0.07/0.03/0.08 | 1 | 15 | 0.02% | 0.18/0.09/0.20 | 2 | 53 | 0.30% |
| X100VI | 6504 K, 0 | 0.06/0.03/0.05 | 1 | 7 | 0.008% | 0.14/0.07/0.14 | 2 | 33 | 0.19% |
| Air 2S | 3200 K, 0 | 1.23/0.23/0.75 | 14 | 149 | 10.4% | 2.00/0.41/1.19 | 19 | 221 | 28.1% |
| Air 2S | 5000 K, +20 | 0.13/0.04/0.20 | 2 | 32 | 0.68% | 0.21/0.08/0.31 | 3 | 169 | 1.3% |
| Air 2S | 8000 K, −20 | 0.26/0.07/0.43 | 4 | 41 | 2.2% | 0.42/0.13/0.68 | 6 | 173 | 5.4% |
| Air 2S | 6504 K, 0 | 0.15/0.04/0.19 | 2 | 39 | 0.66% | 0.24/0.07/0.30 | 3 | 153 | 1.4% |

For scale, the as-shot frame itself is 24/6/24 codes from the 3200 K target on the Z6 and 26/6/18 on the X100VI, so the approximation removes nearly all of the difference a drag is about. The difference images place the rest where the demosaic couples the channels: on the Z6, thin high-contrast edges (bamboo slats, poles and wire against the sky, distant structures) and small specular glints, with flat and smoothly shaded areas within a code; on the X100VI at 3200 K, a fine per-pixel speckle across the saturated yellow petals and along their edges; on the Air 2S, the sparkling shallow water and sand at every target and, at 3200 K — the largest gain change measured, red × 0.68 and blue × 1.79 — the textured foliage and rock broadly. The approximation is shown only while the pointer is down and is replaced by the exact frame on release; the Air 2S's larger error at strong changes is visible during a drag and is not otherwise bounded.

**Latency.** `editor-latency`, drained drag of 30 inputs at Fit, release build, M4 Pro, macOS 26.5.2, warm cache, input to presented frame p50 / p95, with the one-minute load average at the start and end of each run (none above 8.0):

| Source | Slider | Input to presented frame | Release to committed frame | Load average |
| --- | --- | --- | --- | --- |
| Z6 | Custom temperature | 11.2 / 21.0 ms | 539 / 540 ms (2 commits) | 2.4–4.1 |
| Z6 | Custom tint | 10.9 / 19.3 ms | 534 / 539 ms | 2.9–6.1 |
| Z6 | Exposure, for comparison | 12.0 / 19.9 ms | 21 / 25 ms | 6.1–6.7 |
| X100VI | Custom temperature | 10.5 / 20.6 ms | 1578 / 1639 ms | 6.7–7.0 |
| X100VI | Custom tint | 13.6 / 20.4 ms | 1586 / 1596 ms | 5.2–6.2 |
| X100VI | Exposure, for comparison | 11.9 / 21.3 ms | 26 / 134 ms | 5.8–6.6 |

The approximate proxy phase itself renders in 7.4 / 12.6 ms on the Z6 (1049 × 1576) and 8.5 / 9.0 ms on the X100VI (1716 × 1144), and its full-size phase in 129 / 164 and 143 / 148 ms, the cost of an exact RAW frame. A wild drag (`--mode burst`, 360 values over 3 s) presents 43.2 frames per second with a staleness of 16.5 / 34.7 ms on the Z6 temperature slider and 35.7 at 16.6 / 30.6 ms on the X100VI's, against 48.8 at 16.7 / 40.2 ms for RAW exposure on the Z6 (load average 5.9–7.4). Every drafted value produced a labelled frame and no report was adopted from one: 32 approximate proxy frames and 3 reports (the open and the two commits) in each white-balance drag run, against 33 reports in the exposure runs.

### What is preserved

- Originals, recipes, history, drafts, commits, the API and every mutation are untouched. The proxy is desktop preview state and appears in no history and no persisted data.
- Exactness claims are about the exact phase: fixtures, reference tests and the histogram contract stand. The proxy is proven exact at its own scale: a proxy render equals the exact recipe rendered against the exact downscale of the source, byte for byte, which is the test.
- The RAW retained mosaic and float development are unchanged; only their preview reads a smaller plane set. A committed RAW white balance still redevelops the mosaic on the source worker; a drafted one is approximated on the developed planes, labelled, and never analysed, as described above.
- Bounds: one proxy source (≤ 96 MiB), one proxy raster per job, the cancellation token; no new timer, no private pool.

## Evidence

- Core: exactness of the downscale on synthetic fixtures (integer scales average exactly; fractional coverage weights sum to one; a uniform image is unchanged; RAW planes through a cropped, oriented view), eligibility, cache identity, the two-phase queue order, cancellation latency on a 24 MP synthetic frame, and the proxy-equals-exact-at-proxy-scale test.
- Desktop: the existing `basic`, `basic-crop`, `histogram`, `large24`, `large60`, `crop` and `crop-draft` smoke scenarios keep passing with their correlated state, and their captured frames record `proxy` beside `dimensions`; a `Settle::SliderDraft` or `Settle::Preview` step settles only once the proxy is presented and, when the job asked for a report, its exact phase is adopted.
- Timing: `editor-latency` in drag, commit and the new burst mode on 24 MP, 60 MP and the 24 MP crop stack, 30 samples, and `raw-editor` on the manifest sources; `editor-performance` gains the proxy build and proxy render rows. The `verify` timing tier reports the targets above beside the existing ones.
- Events: `preview_displayed` carries `proxy: bool` and, when true, `proxy_dimensions`, and `render_ms`, the worker's own time for the phase on screen (`PreviewResult::render_ms`), which is also the status bar's "Rendered in N ms"; `analysis_adopted` is unchanged; a new `preview_exact_cancelled` event counts superseded exact phases; the state summary carries `proxy: {eligible, dimensions, bounds}`.
- RAW white balance: core tests of `W` against independent references (applied before the exposure, identity with no approximation, the camera-space mapping, singular and non-finite refusals), the draft-only settings mode, both phases labelled and never reduced, the proxy against the exact downscale under `W`, the downscale commuting with `W`, a drafted job hitting the committed frame's proxy and the estimate store keeping approximate and exact apart; an ignored real-file test proving every strict path refuses while the draft's preview approximates, run on the Z6, X100VI and Air 2S; the desktop's own test of the labels and the histogram; and the `raw-panel` smoke scenario's drags at Fit and at 100%.

## Proposals and later work

Recorded here as proposals, not decisions.

- **Coarser proxy while the pointer moves.** With every Basic unit active, the proxy render is the largest remaining cost per input (about 50 ms at 24 MP under a rotated crop on this host). Rendering at half the display size while inputs keep arriving, then at display size once they pause, would cut that fourfold at the cost of a softer picture during the movement itself; it is a measured proposal, taken only if the GPU stage below is not.
- **GPU colour stage.** If the proxy render of the full Basic layer still misses the two-frame target at Fit, the next step is to draw the proxy of the drafted layer's input stage through an `iced` shader primitive and apply the colour units as a fragment program with the coefficients as uniforms, so a tick costs a uniform write. That needs a WGSL transcription of each unit, a headless readback test against the CPU path within one code, and a fallback to the CPU proxy whenever a unit has no GPU program. The proxy source and the two-phase job are the foundation it needs and are built so that it changes only the presentation of the proxy phase.
- **Viewport tiles at 100%.** A drag at 100% still renders the whole exact frame. Rendering only the visible region plus a margin needs the inverse of the geometry tail over a rectangle, which the crop contract does not yet define.
- **Reduced pool.** Leaving one or two cores out of the shared Rayon pool for the desktop and owner threads may lower jitter; it is a measurement, not a default.
