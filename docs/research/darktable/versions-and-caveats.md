# Version boundaries, contradictions and remaining unknowns

[Knowledge base index](README.md)

## Reproducible baseline

Research date: **2026-09-20**. GitHub's [latest-release endpoint](https://github.com/darktable-org/darktable/releases/latest) resolved to [release 5.6.1](https://github.com/darktable-org/darktable/releases/tag/release-5.6.1), published 2026-08-27. The inspected tag resolves to commit **`03179f8e080aa9cedebfe14b098b7ba88940a292`**, whose commit date is 2026-08-26. The commit date and release-publication date describe different events.

Implementation links in this knowledge base are pinned to that full commit. The official user manual is the **5.6** manual; its URLs can still receive editorial corrections. Developer documents are pinned alongside the source. The [source register](sources.md) distinguishes each evidence type.

The temporary checkout was shallow, without submodule initialization. No upstream build/install script ran. Source paths are discovery aids; symbols and code should be rechecked if updating the baseline.

## Concrete documentation drift found

**S/D.** The selected user-manual pixelpipe text describes filmic as the default scene-referred rendering module. The actual configuration at this commit sets the workflow to **scene-referred (sigmoid)** and offers filmic and AgX alternatives. For this release's configured default, the source is the stronger evidence. Existing user preferences, imported histories and auto-presets can still select something different. [Pixelpipe and order](https://docs.darktable.org/usermanual/5.6/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/) [Workflow defaults](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/data/darktableconfig.xml.in#L3898)

**C.** This discrepancy is a reason to record exact versions and consult implementation, not a reason to dismiss the manual generally. A module's presence, its configured default, a user's active choice and a historical image's stored state are distinct facts.

## Claims that need more than reading code

| Claim | What this research establishes | What is still missing |
| --- | --- | --- |
| “It is fast on M4” | CPU/OpenCL/ONNX paths and memory-relevant code exist | Native measured latency, throughput and peak memory on a known build |
| “GPU and CPU match” | Both paths and fallback logic exist | Fixture comparisons, numeric tolerances and driver/device identity |
| “This camera is supported” | Loader routes and named noise profiles exist | Exact recording-mode fixtures, decode/color checks and failure tests |
| “Every operation can be scripted” | Specific Lua, action, CLI and D-Bus entry points exist | Complete coverage matrix, headless/live-GUI behavior and concurrency tests |
| “The sidecar is a complete backup” | Module/mask/order serialization and import logic exist | Recovery test covering all required catalog/auxiliary/external resources |
| “AI is included” | Optional code and backend contracts exist | Installed-build switches, model assets, provider availability and tested outputs |
| “Same slider means same effect” | Identified module equations and modes exist | Independent cross-editor matching/calibration; no equivalence presumed |
| “Preview equals export” | Shared processing code and known approximation paths exist | Controlled comparisons of scale, ROI, quality, profiles and encoding |

## Retained versions are part of image identity

**S.** Module parameter versions, legacy conversion routines and stored processing order preserve older editing behavior. Some modules retain multiple algorithm generations within a single current file. A current executable does not necessarily render every historical image using the newest default path. [IOP loader](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/imageop.c#L305) [Processing order](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/iop_order.c#L298) [Filmic RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/filmicrgb.c#L888)

**S.** The release notes include upgrade/compatibility guidance and several fixes, including preview behavior. Reopening a database in another version is not an appropriate casual experiment on someone's only catalog. Future tests should use copies and record all artifacts. [5.6.1 release notes](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/RELEASE_NOTES.md#L1)

**C.** Defaults affect initialization; explicit stored parameters affect replay. A source comment may describe a historical algorithm or intention rather than every current branch. This knowledge base prioritizes executable branches for equations and marks documentation-only contracts separately.

## Implementation details intentionally not overclaimed

- Cache hashes described here are in-process implementation identities, not a durable cross-machine content-addressable format or a cryptographic integrity guarantee.
- Native module loading is not a stable public ABI, process sandbox or guarantee of safe third-party code.
- OpenCL eligibility and memory estimates are not proof that a particular frame actually ran on the GPU.
- CoreML provider configuration does not prove every ONNX operator executed on the Neural Engine.
- Approximate formulas omit surrounding color conversions, branches, clipping, numerical approximations and blending unless explicitly included.
- Noise-profile presence and a DNG output extension do not establish original mosaiced sensor data, identical decoder behavior or model quality.
- Reading GPL source does not complete dependency, asset, model or distribution-license review.

## Updating this reference

**P.** Resolve the new release tag to a full commit, diff the source-map entry points, review release notes, and recheck defaults and optional build switches. Audit changed parameter versions, order tables, cache dependencies and model contracts before updating claims. Keep older behavioral qualifications where historical images still select those paths.

Replace a performance unknown only with recorded measurements. Replace an interoperability unknown only with fixtures that compare state and output. Preserve the distinction between a research finding, a proposed Luxforge design and an accepted product decision.
