# Lightroom Classic technical knowledge base

Researched September 2026. This describes Lightroom Classic, with explicitly labelled Camera Raw/shared-engine evidence. It is a reference for Lightwell, not a feature commitment or a recipe for exact Adobe output.

There is substantial public information about the architecture and tool behavior, plus a few unusually useful engineering explanations. Exact contemporary kernels, coefficients, scheduling and the complete processing graph remain proprietary or unverified. The source set includes Adobe documentation, engineers' explanations, original papers and SDK references; see the [annotated bibliography](sources.md).

The [darktable companion](../darktable/README.md) examines corresponding behavior in open source, including [a comparison and Lightwell implications](../darktable/lightwell-implications.md).

## Read by question

| Question | Chapter |
| --- | --- |
| Where are edits stored? What is genuinely non-destructive? | [Catalog, recipes, history and sidecars](storage-and-history.md) |
| How do settings turn into displayed/exported pixels? | [Rendering, process versions and color](rendering-and-color.md) |
| Why do loading, zooming and Develop have different costs? | [Previews, loading and performance](previews-and-performance.md) |
| What do Exposure, Contrast, Highlights, Shadows and curves do? | [Tone and color controls](tone-and-color-tools.md) |
| How are Contrast, Clarity, Texture and sharpening different? | [Detail, local contrast and Dehaze](detail-and-local-contrast.md) |
| What is known about demosaicing, denoise and upsampling? | [RAW and computational processing](raw-and-computational-tools.md) |
| How do crop, lens correction, masks and repair fit together? | [Geometry, masking and retouching](geometry-masks-and-retouching.md) |
| What can plugins and programs actually control? | [SDK and interoperability](sdk-and-interoperability.md) |
| Which facts changed recently? Which sources conflict? | [Version notes and evidence caveats](versions-and-caveats.md) |
| Do Lightwell's implemented sliders cover Lightroom's values? | [Slider range and preset-readiness audit](slider-parity.md) |
| What should Lightwell learn, and what needs experiments? | [Engineering implications and research gaps](lightwell-implications.md) |

## Main findings

- Edits are processing instructions associated with externally stored photos. A rendered output is a separate result; the catalog is not a container for the original library. [S01: Catalog basics](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/lightroom-catalog-basics.html)
- Current recipe, historical states, auxiliary edit assets and preview caches have different purposes. A preview cache is not an edit backup. [Storage details](storage-and-history.md).
- Contrast, Clarity, Texture and sharpening act on different aspects of tone/detail. [Tool comparison and engineering evidence](detail-and-local-contrast.md).
- Historical papers establish a local Laplacian connection to Adobe processing. They do not publish today's complete slider implementation. [Algorithm evidence](detail-and-local-contrast.md#published-local-laplacian-foundation).
- Browsing previews, editable proxies and cached early-stage RAW data solve different latency problems. The Classic SDK also documents faster, lower-quality redraw while a Develop parameter is tracked, without publishing its resolution or frame timing. [Performance details](previews-and-performance.md).
- Version matters: new AI storage, GPU preview generation and Neural Engine changes invalidate older blanket explanations. [Version ledger](versions-and-caveats.md).

## How to read the evidence

**D — Documented:** behavior described by Adobe or an identified primary technical source. This does not necessarily reveal implementation.

**H — Historical:** an engineer's explanation or research adoption tied to a date. Useful provenance, not a guarantee about the latest release.

**C — Conceptual:** an explanatory equation or architectural inference. Never presented as Adobe's actual code.

**P — Proposal:** a possible Lightwell design or experiment, still subject to its existing planning gates.

**U — Unknown:** no adequate public evidence found. Absence of a source is not proof that the behavior does not exist.

This research informs decisions; it does not commit Lightwell to any Lightroom feature or algorithm.
