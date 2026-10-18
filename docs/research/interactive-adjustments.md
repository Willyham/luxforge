# Responsive adjustments at 100% and through heavy stacks

Status: research. Luxforge's interactive behaviour is specified by [GPU previews](../design/gpu-preview.md)
and [GPU-first rendering](../design/gpu-first.md); the stage-reuse proposal below applies to a session
without a GPU. Sources checked 2026-09-27. No Lightroom benchmark, Luxforge latency distribution or
native GPU comparison was run for this research.

Immediate adjustment feedback takes priority while dragging; settling must not produce a large,
noticeable jump in the image ([decisions](../design/gpu-preview.md#decisions)).

## Finding

There is a credible path to immediate feedback at 100% without reducing settled detail or export
quality: bound interactive work by the visible rectangle and a frame budget, reuse unaffected
computation and execute adjustment chains on the GPU. Luxforge takes that path: the GPU draws a
gesture over the visible region at full scale ([at 100% and above](../design/gpu-preview.md#at-100-and-above)).

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


The hypothesis that Lightroom drags at reduced quality is partly supported: Adobe exposes temporary quality reduction during
interaction. The stronger claim that Lightroom stays on a smaller file at 100%, or only renders
properly on export, is inconsistent with its documented original-at-1:1 and Develop behavior.
The precise coarse-to-fine scheduling remains unpublished.

The [darktable knowledge base](darktable/previews-and-performance.md) provides a useful independent
comparison with inspectable code. Its [manual](https://docs.darktable.org/usermanual/5.6/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/)
explicitly describes viewport processing, an interactive quality tradeoff, and a slower full-image
quality mode. That demonstrates a practical design family; it is not evidence of Lightroom's
private implementation. Luxforge should preserve every effect and handle its required context,
rather than copy a mode that temporarily omits expensive modules.

## The visible rectangle

A 2880 × 1800 rectangle has **5.184 MP**, versus 60 MP: **11.6× fewer output pixels**, before
panels reduce the photo area. Its RGBA8 payload is **19.8 MiB**, versus **228.9 MiB** at 60 MP.
These are arithmetic bounds for that rectangle, not a measured speedup. Halos, rotation,
reductions, scheduling and transfer overhead prevent linear extrapolation.

At 100%, a screen pixel corresponds to an image pixel; it does not require evaluating the rest
of the photograph first. At 200%, the visible source rectangle is smaller still. A whole-image
Fit proxy enlarged into a 100% view would waste detail; a viewport rendered at an intentional
interactive scale is the more useful approximation.

## Recommended approach

### Reuse work according to what actually changed

Start with a cache at an **existing materialized segment boundary**, storing that boundary's exact
representation: bytes where the byte-domain boundary already quantizes, and the existing float
planes where a spatial boundary materializes them. Recompute the changed segment and dependent
suffix. Do not split a fused colour run by inventing an 8-bit intermediate. Begin with one cached
viewport/scale region under a proposed 64 MiB retained-byte cap; evict before replacement, charge
construction scratch separately, and skip caching if the boundary representation does not fit.
Record hit rate, rebuild cost and memory before adopting it.

A cache immediately before an individual Basic unit would require a new persistent intermediate:
today its units run in a fixed white-balance → Exposure → Tone → Colour order within one layer,
and the byte colour run uses f32 row scratch. An RGB f32 frame is 62.2 MB (59.3 MiB) for
2880 × 1800 or 720 MB (686.6 MiB) at 60 MP, excluding masks and halos. RAW per-pixel evaluation
also has f64 values, so a new f32 checkpoint is not automatically exact there. Such an intra-segment
cache is deferred pending a numerical contract and byte budget; an Exposure edit invalidates
the units after white balance, so it cannot reuse their results.

Changing a late masked adjustment can benefit substantially; changing RAW Exposure near the start
still changes most downstream pixels. Cache mask coverage only while its dependencies are stable:
brush/gradient geometry may be reusable, but luminance/colour ranges depend on the image at their
binding stage. Dirty regions must grow through spatial halos and geometry; global estimates may
invalidate the whole downstream stage. Key reuse by source development/view, relevant recipe
prefix, mask dependencies, stage/region, scale and quality. The existing source and proxy
caches are the foundation; the boundary cache is recorded as a proposal in
[instant previews](../design/instant-preview.md#proposals-and-later-work), under
[performance rule 14](../engineering/performance-rules.md#rules).
