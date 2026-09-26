# Previews, loading and performance

[Knowledge base index](README.md) · Source baseline: 5.6.1. No darktable timing or memory benchmark was run.

## Several caches serve different jobs

**S.** The mipmap API distinguishes 8-bit thumbnail levels, a reduced float input (`DT_MIPMAP_F`) and full input (`DT_MIPMAP_FULL`). Its request modes include best effort, background prefetch, disk-only prefetch, blocking fetch and test-lock. A best-effort request can return a smaller available thumbnail; a blocking request has a different latency contract. [Mipmap/source buffers](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/mipmap_cache.h#L28)

**S.** Mipmap implementation handles loading and cached thumbnail generation, while the pixelpipe cache holds intermediate processing buffers. Image metadata caching is another concern. None of these is the persistent edit-history database. [Mipmap cache implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/mipmap_cache.c#L709) [Pixelpipe cache storage](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/pixelpipe_cache.h#L33)

**C.** Distinguish at least four readiness milestones: selection acknowledged, any usable image visible, current-recipe preview visible, and full-detail/final-quality view ready. A quickly displayed embedded or previously cached rendition can improve interaction without proving that current processing has finished.

## Interactive quality is deliberately conditional

**D.** The manual distinguishes ordinary ROI-based darkroom rendering from full-image high-quality mode, which scales near the end and more closely matches export at greater cost. Some overlay-heavy interactions use a reduced processing path. [Pixelpipe and order](https://docs.darktable.org/usermanual/5.6/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/)

**S.** A concrete example is `diffuse.c`: `process()` checks the fast-mode flag and copies input to output immediately in that mode. This is not merely running the identical filter on fewer CPU threads. The module's visible contribution can temporarily disappear. [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340)

**S.** Haze removal exposes another subtle dependency. Normal full-view ROIs may lack enough context to estimate global atmospheric light, so the module can obtain that estimate from the preview pipe, with hash synchronization. High-quality processing has different handling. Thus previews can supply analysis data as well as display pixels. [Haze removal](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/hazeremoval.c#L559)

**S.** The 5.6.1 release specifically reverted a larger preview-pipe dimension for UI performance. That is useful evidence that preview resolution is a real tradeoff, not proof of a particular speed on M4. [5.6.1 release notes](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/RELEASE_NOTES.md#L1)

## CPU, OpenCL and AI are different layers

**S.** Processing modules commonly have OpenMP/SIMD CPU loops and optional OpenCL callbacks. The evaluator checks module readiness, current mode and estimated device memory. Adjacent GPU operations can reuse device-resident input; CPU fallbacks and color-space transitions may require copies. Kernel arithmetic time alone is therefore an incomplete latency measurement. [Pixelpipe evaluator](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/pixelpipe_hb.c#L1757)

**S.** If a GPU operation cannot proceed, the pipeline can run the CPU path. Late OpenCL errors set error state, disable OpenCL for the affected pipe and restart; sufficiently frequent failures can disable further acceleration. This is explicit recovery code, not a guarantee that every hardware problem is recoverable or invisible. [OpenCL restart path](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/pixelpipe_hb.c#L3189)

**S.** ONNX execution providers belong to a separate optional AI subsystem, including CoreML on Apple platforms. [ONNX/CoreML backend](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/ai/backend_onnx.c#L1714) **P.** On M4, measure actual OpenCL device selection, host/device transfer and AI provider execution separately. Do not call CoreML availability proof that the ordinary pixelpipe uses the Neural Engine, or call a Linux VM result native GPU evidence.

## Tiling is a module capability

**S.** A module's tiling callback estimates memory factors, overhead, single-buffer limits and overlap/alignment needs. The tiler chooses input regions and copies only appropriate interiors into the destination. There are checks for cases where tiling yields little saving or requires impractical overlap. [Tiled processing](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/tiling.c#L592)

**S.** Local contrast is an instructive exception: its local-Laplacian mode sets `process_tiling_ready = FALSE`. A module exposing a tiling callback does not mean all of its algorithms can be tiled. [Local contrast](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/bilat.c#L293)

**C.** A radius-r neighborhood requires an input halo around an output tile. Overlap is wasted recomputation but prevents seams. If r approaches tile size, memory savings and speed can collapse. Global estimators may additionally need a full-image analysis pass; pretending they are local changes the result.

## Memory cost is larger than the final image

**D.** The performance manual describes full-color processing with four 32-bit float components per pixel and multiple simultaneous image buffers. [Memory and performance](https://docs.darktable.org/usermanual/5.6/en/special-topics/mem-performance/) **C.** At 60 megapixels, one such buffer is 960,000,000 bytes, about 916 MiB. Four buffers consume about 3.58 GiB before pyramids, caches, models and application overhead. This is arithmetic, not a measured allocation trace.

**S.** Diffuse or sharpen allocates several scratch buffers plus per-scale high-frequency arrays; scale count and iteration count affect its workload. The code detects allocation failure and logs a problem while copying the input through. [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340) **P.** Luxforge should expose failure to both UI and agents and avoid quietly declaring an export with an omitted effect successful; study recovery behavior instead of copying it unquestioningly.

**S.** Resource preferences distinguish small/default/large configurations. Their descriptive budgets are policy targets, not a process-wide hard guarantee over every third-party allocation. [Resource-level configuration](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/data/darktableconfig.xml.in#L285) [Memory and performance](https://docs.darktable.org/usermanual/5.6/en/special-topics/mem-performance/)

## An evidence-oriented benchmark plan

**P.** Measure these cases separately, using isolated settings/catalogs and a recorded module recipe:

| Workload | Important controls |
| --- | --- |
| First image open | Cold process, disk state, input type, embedded-preview availability |
| Warm reopen | Mipmap/cache hit state and current recipe identity |
| Slider drag | Event-to-present latency, fast-mode use, final settled frame and history commit |
| 100% pan | Visible ROI, neighbor requirements, cache reuse and upload cost |
| Local contrast/diffusion | Image scale, radius, iterations, tiling eligibility, peak memory |
| Export | Full-resolution work, resize quality, color transform, compression and disk writes |
| AI | Model loading/compilation, provider, tile size, inference and output-file creation |

Use the upstream debug facilities as investigation entry points only after checking the selected binary's options. No benchmark commands were executed here, no timing numbers are invented, and no claim that C/C++ or Rust alone explains responsiveness is warranted.
