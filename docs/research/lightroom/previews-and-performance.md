# Previews, loading and performance

[Knowledge base index](README.md) · Evidence checked 2026-09-22. No local Lightroom benchmarks were run.

## Several kinds of cached image data

**D.** Adobe distinguishes these resources. They should not be described collectively as “the preview.” [S09: Optimize performance](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/performance-guidelines/optimize-performance-lightroom.html) [S10: Smart Previews](https://helpx.adobe.com/lightroom-classic/desktop/viewing-photos/lightroom-smart-previews.html)

| Resource | Purpose | Tradeoff |
| --- | --- | --- |
| Minimal / embedded preview | Fast initial display from camera-provided imagery | May show a different rendition from Adobe processing |
| Standard preview | Lightroom-rendered browsing image sized for display | Up-front rendering avoids later waits |
| 1:1 preview | Full-pixel Library inspection | Higher generation/storage cost |
| Smart Preview | Smaller lossy-DNG source usable for offline edits | Limited source detail; not just a finished thumbnail |
| Camera Raw cache | Reusable early-stage RAW data for Develop | Can skip initial processing; not a cache of every final recipe |
| Live rendered frame | Current image for the active view | Exact retention and invalidation policy unpublished |

**C.** Prebuilding Library previews does not establish that arbitrary future Develop edits are already computed: a browsing rendition and reusable source-stage data answer different requests.

**D.** Smart Previews are stored alongside the catalog and support disconnected editing. With the prefer-proxy setting, Develop uses them even when originals are available; Adobe documents a return to originals at 100% and full-quality final output when the original is available. Availability for a specific AI tool must be checked separately. [S10: Smart Previews](https://helpx.adobe.com/lightroom-classic/desktop/viewing-photos/lightroom-smart-previews.html) [S07: Develop module options](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-options.html)

## What is documented about a slider drag

**D, mirrored SDK.** `LrDevelopController.startTracking` puts Develop into a state with *faster, lower-quality redraw* and suppresses individual history states. `stopTracking` ends that state and creates one history state for the tracked parameter. The SDK also describes automatic tracking exit after a different parameter is changed or two seconds after the last adjustment, and a configurable tracking delay. This is the strongest published evidence found for an interactive-quality path; the two-second value is a history/tracking timeout, **not** a render time or frame interval. The detailed SDK reference is Adobe-authored material on a third-party mirror and has not been checked against an installed current SDK. [S41: LrDevelopController reference](https://lrc.mcor.dev/modules/LrDevelopController.html)

**D.** Adobe says that viewing or editing a RAW photo in Develop generates an up-to-date preview from original image data plus the current adjustments. Its Camera Raw cache can skip early-stage processing on a hit. The Library's Standard and 1:1 previews are documented for browsing and other modules, so prebuilding those previews does not establish a precomputed result for future Develop slider values. [S09: Optimize performance](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/performance-guidelines/optimize-performance-lightroom.html)

**D.** The optional *Use Smart Previews instead of Originals for image editing* preference is a separate speed/quality tradeoff: Adobe says it can improve Develop performance with decreased editing-view quality, while final output uses full size/quality when the original is available; Develop switches back to the original at 1:1. This is a persistent choice of smaller editable source, **not** evidence that every ordinary slider drag automatically swaps among Smart Previews, Standard previews and original pixels. [S45: Preferences](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/setting-preferences-lightroom.html) [S07: Develop module options](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-options.html)

**D.** Lightroom Classic 15.3 release notes say global and local slider performance became “interactive,” but give no latency, test hardware or refresh-rate target. An Adobe 2020 release post claimed up to 2× faster rendering for local correction painting and slider adjustments with GPU acceleration; this is a version-specific relative claim, not a contemporary per-frame timing. [S35: Release notes](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/release-notes.html) [S46: October 2020 photography release](https://blog.adobe.com/en/publish/2020/10/20/lightroom-max-release-more-power-editing-precision-growing-photography-community)

**C.** Together, these sources support an available *coarse interactive redraw → settled higher-quality rendition* model. They do not establish whether the current UI invokes tracking for every slider. They also do not specify whether reduced quality means fewer source pixels, skipped operations, different kernels, fewer iterations or some combination, or whether the settled pass starts only on pointer release. A smaller Smart Preview source and tracking-mode lower-quality redraw are distinct mechanisms. Adobe recommends 1:1 Develop inspection for sharpening/noise decisions, and says Develop previews are most accurate at 1:1 or higher. [S23: Retouch photos](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html) [S15: Color FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/technical-issues/miscellaneous-issues/color-faq.html)

**U.** No Adobe source found gives an absolute slider-to-pixel delay, redraw cadence, draft/full resolution, resampling filter, tile size, cancellation policy or current CPU/GPU thread graph for a drag. No Lightroom capture was made here. CPU/GPU utilization or a fluid slider animation alone would not identify which of those mechanisms produced a visible frame.

## Loading is a sequence of readiness states

**C.** A useful observable sequence is:

`selection acknowledged → any valid preview → current-recipe Fit preview → true-detail 100% view`

The first displayed image can be fast but stale, camera-rendered or lower resolution. “Time to image” and “time to correct image” are different measurements. RAW preview appearance changes can be expected during the transition from embedded imagery to Adobe's rendition. [S15: Color FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/technical-issues/miscellaneous-issues/color-faq.html)

**P.** For Lightwell, each result should identify asset, recipe revision, source/proxy, resolution and color state. Stale results must not replace newer selections. A quick thumbnail is valuable feedback; it should not masquerade as a completed render. These are engineering implications, not claims about Lightroom's internal job objects.

## GPU support is operation-specific and versioned

**D.** Adobe exposes separate acceleration categories for display, image processing and export; configuration can disable acceleration after a capability test. The older FAQ also excludes supported VM GPU operation. Hardware presence alone does not establish which work is accelerated. [S12: GPU FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/technical-issues/gpu-issues/lightroom-gpu-faq.html)

**D.** Classic **14.5** added GPU preview generation. Its documented Auto policy requires full acceleration and at least 16 GB GPU memory; On and Off overrides are available, and turning overall graphics processing off disables this option. This supersedes older blanket claims that preview building is CPU-only. The published criterion is not a recommendation to infer dedicated VRAM from an M4's unified-memory total. [S13: GPU preview generation](https://helpx.adobe.com/lightroom-classic/desktop/kb/gpu-preview-generation.html)

**D.** Classic **15.4** release notes add Apple Neural Engine support for Denoise. That supersedes a May 2026 Enhance page saying it is unsupported. Actual model/hardware routing and timings on the owner's M4 remain unmeasured. [S35: Lightroom Classic release notes](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/release-notes.html)

**D.** Adobe's 2018 performance post says multicore CPU and memory optimizations sped up rendering Develop adjustments; a 2024 post credits memory and caching changes for smoother image navigation. These establish that parallel hardware use and caching affect performance, but neither describes the thread queues or scheduling for one current slider gesture. [S47: February 2018 update](https://blog.adobe.com/en/publish/2018/02/13/announcing-february-update-lightroom-classic) [S48: Adobe MAX 2024 update](https://blog.adobe.com/en/publish/2024/10/14/more-power-photographers-explore-latest-from-lightroom-adobe-max-2024)

## Disk, cache and scheduling

**D.** Adobe recommends fast catalog/preview storage; photos, but not active catalogs, can reside on network storage. Many local corrections/history states can increase costs. [S09: Optimize performance](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/performance-guidelines/optimize-performance-lightroom.html)

**D.** Preview cache limits are not strict immediate byte caps: the catalog settings page says previews newer than 30 days are retained and purging happens while idle, without a fixed schedule. [S11: Create and manage catalogs](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/create-catalogs.html) For XMP, the June 2025 update pauses automatic writes during import and writes the active image at ten-second intervals. This is metadata-write batching, not documented catalog transaction latency. [S36: June 2025 feature summary](https://helpx.adobe.com/lightroom-classic/desktop/help/whats-new/2025-4.html)

**D.** Preferences offers *Generate preview in parallel*, which builds multiple previews concurrently rather than sequentially. This concerns preview-generation jobs; it does not show that one Develop slider update runs on a background thread or that there is a separate full-resolution pass queued after each drag. [S45: Preferences](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/setting-preferences-lightroom.html)

**C.** Separate likely cost categories before blaming a slider or language:

| Workload | Measure independently |
| --- | --- |
| Cold launch | Database opening, folder enumeration, plugin/resource initialization |
| Fast culling | Cache hit ratio, disk/decode latency, visible-item scheduling |
| First Develop visit | Source access and early processing |
| Dragging a slider | Event-to-present latency, temporary quality, queue depth |
| 100% pan/zoom | Required detail, upload and memory pressure |
| Many masks/repairs | Region size, operation count, dependency recomputation |
| Export | Full-resolution processing, resampling, encoding and writes |
| AI processing | Model loading, inference, auxiliary storage and downstream refresh |

This is a measurement taxonomy, not a recovered Lightroom profiler trace. Exact thread pools, tile dimensions, GPU residency, LRU keys, speculative prefetch and cancellation strategies are unknown.

**H.** The Halide paper explains why locality, recomputation and fusion across stages matter alongside parallelism. Its old research timings must not be republished as current Lightroom performance or proof that Lightroom now uses Halide. [S20: Halide image-processing pipelines](https://people.csail.mit.edu/jrk/halide-pldi13.pdf)
