# darktable technical knowledge base

Researched September 2026. Source baseline: **darktable 5.6.1**, commit [`03179f8e080aa9cedebfe14b098b7ba88940a292`](https://github.com/darktable-org/darktable/tree/03179f8e080aa9cedebfe14b098b7ba88940a292), committed 2026-08-26 and released 2026-08-27. Companion to the [Lightroom Classic knowledge base](../lightroom/README.md).

The central finding: darktable stores editable module state separately from source images, resolves that state into an ordered processing pipeline, and evaluates the needed image regions through module callbacks and shared color/blending/cache services. Its source makes it possible to explain actual formulas and execution paths instead of inferring them from slider labels. See [the complete change-to-pixels trace](pixelpipe-and-rendering.md).

## Read by question

| Question | Chapter |
| --- | --- |
| Where do edits live, and how are they restored? | [Storage, history and sidecars](storage-and-history.md) |
| How does a slider change become pixels? | [Pixelpipe and rendering](pixelpipe-and-rendering.md) |
| What does scene-referred mean in this implementation? | [Color and scene-referred processing](color-and-scene-referred.md) |
| How do loading, caches, tiling and GPU processing work? | [Previews and performance](previews-and-performance.md) |
| What do exposure, contrast, tone mapping and color controls compute? | [Tone and color algorithms](tone-and-color-tools.md) |
| What corresponds to Clarity, Texture, sharpening and Dehaze? | [Detail and local contrast algorithms](detail-and-local-contrast.md) |
| How are RAW, highlights and traditional noise reduction implemented? | [RAW and denoise](raw-and-denoise.md) |
| What AI exists in this release, and where does it run? | [AI and derived images](ai-and-derived-images.md) |
| How do crop, masks, healing and lens correction fit together? | [Geometry, masks and retouching](geometry-masks-and-retouching.md) |
| What can modules, Lua, D-Bus and the CLI do? | [Modules and automation](modules-and-automation.md) |
| What differs from Lightroom, and what is relevant to Luxforge? | [Comparison and engineering implications](luxforge-implications.md) |
| Which claims are version-sensitive or still untested? | [Versions and caveats](versions-and-caveats.md) |
| Where should a developer start reading upstream? | [Source register and code map](sources.md) |

## Evidence conventions

- **S — Source-confirmed:** inspected implementation at the pinned commit. Formulas describe the identified path, not all modes or the complete image rendition.
- **D — Documented:** official manual or developer documentation. Source wins when a concrete implementation contradicts older prose.
- **C — Conceptual/inferred:** explanatory reasoning, not a measured result or an exact code transcription.
- **P — Proposal:** possible Luxforge design or experiment; existing decision gates still apply.
- **U — Unverified:** runtime behavior, quality or performance not established by this research.

No darktable binary was installed or run, no user's catalog was inspected, and no GPU/image-quality benchmark was performed. The temporary source checkout was read only for research, without fetching submodules or running upstream build scripts. All durable citations point upstream, not to that temporary folder.

## Findings worth starting with

- History chronology and processing order are separate; modules can have several history entries while their current instance runs once in the pipe. [Storage](storage-and-history.md).
- Cache identity includes upstream state, color profiles and requested image region. A parameter-only cache key would miss important dependencies. [Pixelpipe](pixelpipe-and-rendering.md).
- “Contrast” is not one algorithm: gray-pivot power contrast, display-rendering curves, local Laplacian filtering and wavelet-band gains solve different problems. [Tone](tone-and-color-tools.md), [detail](detail-and-local-contrast.md).
- The current release's configured default is **scene-referred sigmoid**, despite manual sections that still describe filmic as the default. [Version caveats](versions-and-caveats.md).
- Optional AI inference and the ordinary OpenCL pixel pipeline are separate execution systems. A CoreML setting does not turn all image operations into Metal/Neural Engine kernels. [AI](ai-and-derived-images.md).

Luxforge's accepted decisions and implementation roadmap are unchanged by this research. Studying darktable does not adopt its complete interface, module set, persistence model or extension ABI.
