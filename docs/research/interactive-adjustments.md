# Responsive adjustments at 100% and through heavy stacks

Status: research and implementation recommendations, not an implemented performance result.
Sources checked 2026-09-27; Luxforge code inspected at `456cd3c`. No new Lightroom benchmark,
Luxforge latency distribution or native GPU comparison was run for this research. Existing
measurements below retain their original workload and scope.

The owner accepts temporary softness at 100% while dragging, full detail restored on pause/release,
and the histogram marked updating meanwhile. The [decision](../decisions.md#interactive-previews-at-100)
sets the interaction priority; implementation and measured quality/latency remain outstanding.

## Finding

There is a credible path to immediate feedback at 100% without reducing settled detail or export
quality. Luxforge currently makes a 100% adjustment wait for a CPU render and GPU upload of the
whole photo. The visible rectangle is much smaller. Fit already avoids that wait with a proxy,
but its cost still grows with the effective recipe. The largest opportunities are to bound
interactive work by the visible rectangle and a frame budget, then reuse unaffected computation
and execute eligible adjustment chains on the GPU.

This is an architectural feasibility conclusion, not a promise that arbitrary stacks will meet a
particular frame rate. A viewport, a smaller source, a cache and GPU processing solve different
parts of the problem.

## What Lightroom actually documents

The [Lightroom knowledge base](lightroom/previews-and-performance.md) already separates these
mechanisms. Rechecking the primary documentation supports that separation:

| Mechanism | Evidence | What it does not establish |
| --- | --- | --- |
| Smaller editable source | Adobe documents optional Smart Preview editing and a switch to the available original at 100% (1:1). [Develop options](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-options.html) | A permanently low-resolution 100% view, or automatic Smart Preview use on every drag. |
| Temporary interactive quality | Adobe-authored SDK documentation describes `LrDevelopController.startTracking` as faster, lower-quality redraw; `stopTracking` ends tracking. The reference is a third-party mirror, not a verified installed current SDK. [SDK reference](https://lrc.mcor.dev/modules/LrDevelopController.html#LrDevelopController.startTracking) | The resolution, omitted work, kernels, current UI call sites, or a guarantee about every zoom level. Its two-second default tracking timeout is not a rendering latency. |
| GPU image processing | Adobe distinguishes image processing from display acceleration. Its 2020 announcement specifically includes local-correction sliders, gradients and brush work. [GPU FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/technical-issues/gpu-issues/lightroom-gpu-faq.html), [local adjustments](https://blog.adobe.com/en/publish/2020/10/20/lightroom-max-release-more-power-editing-precision-growing-photography-community) | An exact current GPU graph, tile size or device-residency policy. The historical relative speed claim is not an M4 timing. |
| Reusable RAW preparation | Develop generates current adjusted previews using original data; the Camera Raw cache can avoid early processing. [Performance guidance](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/performance-guidelines/optimize-performance-lightroom.html) | Full-quality computation being deferred exclusively to export, or Library 1:1 previews precomputing future slider values. |
| Continuing optimization | Classic 15.3 release notes report improved interactivity for global and local sliders; 15.4 reports more responsive mask brushing. [Release notes](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/release-notes.html) | Absolute input-to-pixel latency or uniform performance with any number of edits. |

The owner's hypothesis is partly supported: Adobe exposes temporary quality reduction during
interaction. The stronger claim that Lightroom stays on a smaller file at 100%, or only renders
properly on export, is inconsistent with its documented original-at-1:1 and Develop behavior.
The precise coarse-to-fine scheduling remains unpublished.

The [darktable knowledge base](darktable/previews-and-performance.md) provides a useful independent
comparison with inspectable code. Its [manual](https://docs.darktable.org/usermanual/5.6/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/)
explicitly describes viewport processing, an interactive quality tradeoff, and a slower full-image
quality mode. That demonstrates a practical design family; it is not evidence of Lightroom's
private implementation. Luxforge should preserve every effect and handle its required context,
rather than copy a mode that temporarily omits expensive modules.

## Why Luxforge behaves differently

| Current behavior | Code evidence | Consequence |
| --- | --- | --- |
| At 100% and above, no proxy is requested. | `Editor::proxy_bounds` in [desktop preview](../../crates/luxforge-app/src/app/preview.rs). | Each accepted adjustment waits for a whole exact frame, even though most pixels are off screen. |
| At Fit, the worker renders the complete recipe over a cached source proxy, then starts the exact frame and requested analysis. | [Preview worker](../../crates/luxforge-core/src/preview/worker.rs), [draft requests](../../crates/luxforge-app/src/app/tasks.rs). | The source downscale is reused; the edited image is still recomputed. Exact work can occupy the worker until cancellation before the next interactive frame starts. |
| The surface uploads every texture tile of a changed raster; its fragment shader samples that raster. | `PhotoPipeline::write` in [photo surface](../../crates/luxforge-ui/src/photo_surface.rs), [shader](../../crates/luxforge-ui/src/photo_surface.wgsl). | GPU display and texture tiling do not constitute GPU adjustments or viewport-limited computation/upload. |
| Colour operations are fused and masks already restrict work to their bounds. The row engine snapshots each masked operation's input and evaluates its coverage. | `apply_units`, `apply_masked_operation` in [renderer](../../crates/luxforge-core/src/render.rs). | We do not render a separate full image for every Basic slider or blindly evaluate every small mask everywhere. Multiple overlapping operations still add work per affected pixel. |
| Spatial operations need halos; estimates depend on upstream content. | [Spatial processing](../../crates/luxforge-core/src/render/spatial.rs), [pipeline](../../crates/luxforge-core/src/render/pipeline.rs). | Changing Exposure before Dehaze or a range mask can invalidate their inputs. A viewport alone cannot eliminate global dependencies. |
| Region back-propagation already exists for cropped proxies. | `WindowPlan::of` / `apply` in [window planning](../../crates/luxforge-core/src/render/window.rs). | The old claim that we lack inverse rectangle geometry is obsolete. Extending this to a requested viewport is feasible, with remaining coordinate and eligibility work. |

The prepared-source cache already avoids reopening and demosaicing a RAW for every Exposure
change. RAW white-balance release is a separate case: it deliberately redevelops the retained
mosaic, while its drag approximates on the existing development. More decoder work alone will
not fix ordinary Exposure drags.

Cancellation also matters: the Fit proxy phase is allowed to finish after a newer draft value
arrives, while its exact phase is superseded. At 100% that supersedable exact phase is the only
frame producer. If inputs arrive faster than it can finish, they can repeatedly cancel the only
new picture. This is a consequence of the worker's token policy, not a measured starvation rate.
A viewport/interactive path must preserve forward progress as well as reject obsolete view state.

### What the recorded measurements establish

- The latest recorded JPEG control diagnostic has full Basic plus geometry at **182.5 / 188.9 ms
  p50/p95** for a 60 MP source, and its proxy at **49.3 / 53.1 ms**. These are core diagnostic
  timings with specific geometry, not input-to-screen times or a current 100% viewport test.
  [Scope and host load](../specs/performance.md#shared-pool-contention-and-rendering-controls).
- Earlier native Fit measurements put a single Clarity slider at **16.7 / 17.3 ms** to the
  presented proxy but **216.1 / 227.8 ms** to its exact histogram. That demonstrates the value of
  separating feedback from completion; it does not qualify a combined heavy stack.
  [Presence evidence](../specs/performance.md#after-the-instant-preview-merge).
- A 2880 × 1800 rectangle has **5.184 MP**, versus 60 MP: **11.6× fewer output pixels**, before
  panels reduce the photo area. Its RGBA8 payload is **19.8 MiB**, versus **228.9 MiB** at 60 MP.
  These are arithmetic bounds for that rectangle, not a measured speedup. Halos, rotation,
  reductions, scheduling and transfer overhead prevent linear extrapolation.

At 100%, a screen pixel corresponds to an image pixel; it does not require evaluating the rest
of the photograph first. At 200%, the visible source rectangle is smaller still. A whole-image
Fit proxy enlarged into a 100% view would waste detail; a viewport rendered at an intentional
interactive scale is the more useful approximation.

## Recommended approach

### 1. Make the visible region the interactive unit of work

Extend the existing rectangle walk to a requested output region, compiled against the full stage.
Render and upload that region with its origin, preserving true 1:1 pixels, mask coordinates,
spatial halos and whole-image estimates. Show the visible current result before computing the
off-screen remainder and the full histogram. Reuse the current bounded worker and cancellation
mechanism; pan/zoom supersedes obsolete region requests as well as obsolete recipe requests.

The [viewport proposal](../design/instant-preview.md#viewport-rendering-at-100-proposal) describes
the remaining work. This is the strongest first step for 100% and can preserve exact pixels.
It cannot by itself guarantee a frame budget for many expensive spatial layers.

### 2. Budget interactive quality and defer non-visible completion work

For expensive stacks, the proposed first quality level renders the visible region at half its
linear resolution while inputs arrive, then restores full viewport detail on pause/release. Half in each dimension is
one quarter as many output pixels, not an assured fourfold speedup. Use measured frame cost,
bounded quality levels and hysteresis to prevent visible oscillation. The first drag frame must
also have a bounded path; do not wait for a slow exact frame to learn that approximation is needed.

Keep all recipe effects present at the chosen scale. A reduced-resolution spatial effect or thin
mask remains explicitly approximate; sharp mask edges and strong texture need comparison. Do not
apply an exposure multiplier to the previous final 8-bit picture: clipped highlights and nonlinear
downstream edits cannot be reconstructed from it.

Global analysis needs an interactive path too. A cropped proxy currently can ask for an exact
whole-stage Dehaze estimate, so reducing its displayed pixels alone may leave a full-image wait.
For these expensive cases, investigate a bounded, reduced whole-image guide for the **current**
draft that supplies approximate global estimates shared by every visible tile. Keep that guide
independent of the pan rectangle and keyed by upstream state and quality; using only the viewport
or an old estimate can make the picture change while panning or ignore the new adjustment. Measure
the guide's own cost and image error, including multiple spatial layers. Replace it with exact
estimates on refinement, and never use its estimates for exact analysis or export. This is an
additional preview approximation to qualify, not an existing capability or accepted quality bound.

Avoid starting a full-image render/histogram after every moving input. There is already conditional
owner authorization to test exact-phase deferral **at Fit** in the
[post-consolidation decisions](../decisions.md#post-consolidation-review). The owner also accepts
temporary softness and an updating histogram at 100%. On pause/release, the recommendation is to
refine the viewport first, then finish full-image analysis; displayed numbers retain their last exact values
and say updating until a matching full-image report arrives. Viewport clipping can follow current
visible pixels, with its quality identified, but is not a whole-photo clipping count.

### 3. Reuse work according to what actually changed

Evaluate one bounded cache of the stage immediately before the active adjustment, using the
renderer’s existing numerical representation without introducing quantization. Recompute that
adjustment and its dependent suffix. Start with the currently edited viewport/scale, not one
full-resolution buffer per layer. Record hit rate, rebuild cost and memory before retaining it.

Changing a late masked adjustment can benefit substantially; changing RAW Exposure near the start
still changes most downstream pixels. Cache mask coverage only while its dependencies are stable:
brush/gradient geometry may be reusable, but luminance/colour ranges depend on the image at their
binding stage. Dirty regions must grow through spatial halos and geometry; global estimates may
invalidate the whole downstream stage. Key reuse by source development/view, relevant recipe
prefix, mask dependencies, stage/region, scale and quality. Existing source/proxy and estimate
caches are useful foundations, not a general cache of intermediate recipe results.

### 4. Accelerate eligible adjustment chains on the existing GPU backend

Prototype high-precision source/input tiles resident on the existing `wgpu` backend (Metal on
macOS), with Exposure, Basic colour and mask blending evaluated in recipe order. Parameter changes
then update small uniforms while GPU programs evaluate the pixels; they do not require uploading
a newly computed CPU RGBA image each time. Prefer complete eligible chains to repeated CPU/GPU
round trips. A preceding expensive stage can be cached only while it is unchanged; a downstream
CPU-only effect still has to be evaluated or triggers the explicit CPU preview fallback.

This is a larger numerical and resource change than viewport CPU rendering. The current shader
handles display only. GPU math must be tested across complete stacks, mask edges, negative and
over-range RAW values and supported devices. A candidate preview error budget (for example one
8-bit code) is a proposal until measured and accepted; settled exact rendering remains the
reference. A fast Exposure shader alone does not solve arbitrary masked spatial stacks.

## How to qualify the result

The existing [rendering plan](../../tasks/rendering.json) already includes exact-phase deferral,
viewport design, row reads for estimates and other byte-identical optimizations. Extend that work
with the accepted interaction priority and explicit quality/overlay contracts, rather than start a
competing renderer or duplicate plan. This research does not mark any implementation task complete.

Measure the current checkout first, then each finished change, on the owner's native M4 with
24/60 MP JPEGs and the supplied Z6, X100VI and Air 2S RAWs. Compare Fit, 100% and 200%; Exposure
alone, full Basic, multiple overlapping masked colour layers, Presence combinations, range masks,
brush strokes and a straightened crop. Include cold-region pans and movement during refinement.

Report at least 30 observations for claimed distributions, quiet-host load, actual viewport/scale,
queue wait, render/estimate/overlay time, upload/presentation cost, displayed-value staleness,
frame gaps and pause/release-to-exact time. Retain the existing 16 ms target and 32 ms acceptable
bound as targets, not achieved results. Presented means the harness's surface assignment and
requested redraw, not physical scanout. A separately controlled Lightroom comparison needs its
version, GPU and Smart Preview settings recorded; UI slider motion alone is not image feedback.

Prove exact viewport buffers equal crops of the full reference at every origin, orientation and
tile edge, including masks, vignette, halos and global estimates. Test cancellation during pan,
draft cancel/release, history selection, zoom/scale changes, device fallback and cache eviction.
Correlate desktop captures with recipe/view generations and quality; prove the settled full-image
histogram and pixels are exact. Count CPU/GPU caches and scratch against explicit byte budgets,
including old/new tiles in flight. Use background launches under the repository's evidence rules.

The recommendation is viewport rendering plus interaction scheduling first, adaptive preview
quality for expensive cases under the accepted interaction priority, and measured stage reuse/GPU
execution where necessary.
This can pursue Lightroom-like responsiveness while retaining full-detail inspection and exact
final results; the latency claim must come from the finished implementation.
