# Comparison with Lightroom and implications for Luxforge

[Knowledge base index](README.md) · This chapter contains **proposals**, not new product decisions or implementation commitments.

## What the two research bases establish

| Question | Lightroom Classic evidence | darktable evidence | Consequence for interpretation |
| --- | --- | --- | --- |
| Where do edits live? | Catalog plus supported metadata/auxiliary persistence | Catalog history, masks, module order and XMP | “Non-destructive” describes a storage relationship, not a guarantee that a single sidecar preserves every application feature |
| How does history relate to rendering? | Public SDK/settings behavior supports recipe-based editing; complete engine order is undisclosed | History replay resolves state; explicit module order drives processing | Do not infer execution order from the sequence of user actions |
| What is contrast? | Documented visual semantics; exact current curves generally undisclosed | Several inspectable global and spatial algorithms | Slider labels and values are not an interchange specification |
| Clarity / texture-like effects? | Adobe explanations and related research, with limits on linking papers to shipping code | Local-Laplacian, bilateral, wavelet and diffusion implementations | darktable provides concrete code to study without proving Adobe equivalence |
| How are previews accelerated? | Multiple preview types and documented GPU/cache behavior | Mipmaps, ROI requests, intermediate caches, approximation modes, OpenCL and tiling | Separate first-visible latency, current-state readiness and full-quality readiness |
| How is color handled? | Documented profiles/process versions; incomplete public pipeline internals | Explicit module domains, profile conversions and rendering modules | “Floating point” alone says little about color correctness |
| Are AI edits always just settings? | Version-dependent auxiliary/derived data | Generated masks and imported DNG/TIFF derivatives, plus model/cache state | Durable derived data and disposable acceleration caches need different recovery rules |
| What can agents control? | SDK-supported operations with documented limitations | Lua, GUI action dispatch, CLI and D-Bus entry points | Neither research establishes Luxforge's required full command/schema parity by analogy |

Evidence: [Lightroom storage](../lightroom/storage-and-history.md), [rendering](../lightroom/rendering-and-color.md), [detail](../lightroom/detail-and-local-contrast.md), [performance](../lightroom/previews-and-performance.md), [RAW/AI](../lightroom/raw-and-computational-tools.md), [SDK](../lightroom/sdk-and-interoperability.md); darktable [storage](storage-and-history.md), [pixelpipe](pixelpipe-and-rendering.md), [tools](tone-and-color-tools.md), [detail](detail-and-local-contrast.md), [performance](previews-and-performance.md), [color](color-and-scene-referred.md), [AI](ai-and-derived-images.md) and [automation](modules-and-automation.md).

## Architectural ideas worth evaluating

| Source-confirmed observation | Proposed Luxforge response | Existing boundary |
| --- | --- | --- |
| Compact settings are prepared into per-pipe execution data | Keep durable intent separate from LUTs, kernels and scratch resources | Core owns transactions/history; modules own validation and processing |
| ROI and color conversions are host/module contracts | Describe each operation's input domain, region needs and coordinate transforms | Do not introduce a speculative generalized node graph |
| Cache identity follows upstream state, profiles and ROI | Include source/revision, processing identity, geometry, color and quality in reuse rules | Start with measured needs; avoid speculative cache layers |
| Fast rendering can omit an expensive effect | Expose draft/final readiness and ensure stale/draft output cannot masquerade as final | Responsiveness and observable state are requirements |
| Global analysis can be shared from a preview | Treat analysis dependencies separately from a local output rectangle | Validate ROI consistency and scale-dependent approximations |
| CPU/GPU fallback changes execution and may restart | Track failures and recovery; make correctness independent of a particular accelerator | Native M4 evidence and portable functional checks remain distinct |
| Module/mask/order state is needed for recovery | Preserve complete edit intent, including disabled/missing-provider operations | Never silently discard incompatible or unavailable edits |
| Parameter metadata helps tooling but does not ensure full automation | Make commands, queries and schemas authoritative from the start | Every UI operation must have programmatic parity |

These ideas complement the existing [module/API design](../../design/modules-and-api.md), [history specification](../../specs/edit-history.md), [source recovery](../../specs/source-recovery.md) and [product decisions](../../decisions.md). They do not supersede them.

## A deliberate disagreement: history policy

**S.** darktable can coalesce current-module history changes and truncate later history when a new edit starts from an earlier endpoint, subject to its internal exceptions. [History implementation](storage-and-history.md#history-is-a-sequence-of-state-assignments).

**P.** Luxforge should retain its already accepted append-only Restore-action behavior, preserving later actions. This is a consequential product difference, not an implementation detail to inherit accidentally from a reference editor. A useful reference can demonstrate both techniques to adopt and policies to reject.

## Focused experiments before architectural adoption

These are **unexecuted research proposals**, not scheduled feature work:

1. **State reconstruction:** edit, duplicate, save/reload and reconstruct from sidecar in an isolated catalog. Compare module order, masks, versions and lossless output; remove only disposable caches.
2. **Render consistency:** compare Fit, 100%, normal/HQ and export with local contrast, haze, denoise and diffusion. Record every approximation and color setting.
3. **ROI boundaries:** pan through strong edges and retouch/mask regions; compare cropped full-frame output with directly requested regions. Inspect seams and global-statistic changes.
4. **M4 costs:** measure cold/warm decode, first display, settled slider latency, OpenCL transfer, peak memory and export separately. AI tests must identify the execution provider and model.
5. **Algorithm fixtures:** use ramps, color patches, noise and frequency sweeps to verify the behavior of any proposed Luxforge operator. Do not assert Adobe matching based on a few photographs.
6. **Camera coverage:** test Nikon Z6 Bayer and Fujifilm X100VI X-Trans recording modes through established decoders before considering custom work.
7. **Automation coverage:** enumerate operations and exercise them without synthetic clicks wherever an API exists. Track missing read/write state, asynchronous completion and undo semantics explicitly.
8. **Failure behavior:** simulate cancellation, unavailable source/profile/model and allocation/device failure with disposable data. Ensure retained state and truthful readiness/error reporting.

Use the project's [development evidence requirements](../../engineering/development.md). A functional result in a VM is not a native GPU benchmark; source inspection is not a runtime pass. Any follow-up implementation requires normal scoping and owner decisions where consequential.

## What this research does not select

No darktable dependency, UI module inventory, algorithm, native ABI, neural model or RAW library is adopted. The owner-selected GPL-3.0-or-later license permits useful reuse investigations, but dependency/model/asset notices still require their own review before reuse. This research changes no implementation milestone or completion status.
