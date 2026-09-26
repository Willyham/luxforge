# RAW decoding, reconstruction and traditional denoise

[Knowledge base index](README.md) · Source baseline: 5.6.1. AI restoration has a [separate chapter](ai-and-derived-images.md).

## Decode is only the beginning

**S.** Image loading routes by file signatures and loader capabilities. This release contains RawSpeed and LibRaw paths with format-specific choices and fallbacks; it is inaccurate to describe darktable simply as “a LibRaw frontend.” Sensor decoding and later image operations are separate responsibilities. [Image loader routing](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/imageio/imageio.c#L201)

The pinned repository records these submodule revisions:

| Dependency | Gitlink revision | Inspection limit |
| --- | --- | --- |
| RawSpeed | `7cf3dc3b9d9c82b414198b1f57460478be6c6c9d` | Recorded from the release tree; submodule not fetched or benchmarked |
| LibRaw | `f74ddd995c5447458f132a5377ca0f4b394dff6e` | Recorded from the release tree; submodule not fetched or benchmarked |

The [pinned `src/external` tree](https://github.com/darktable-org/darktable/tree/03179f8e080aa9cedebfe14b098b7ba88940a292/src/external) supplies those identities. Loader presence is not proof of every camera's compression/bit-depth mode, and a release tag alone does not identify optional dependencies installed by a distributor.

**S.** RAW preparation includes sensor crop and black/white normalization, with per-channel handling and gain-map support. Early white-balance/highlight processing and demosaic then prepare full-color data for later editing. The exact sequence is governed by the module-order table, not by the order in which a photographer touched controls. [RAW normalization](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/rawprepare.c#L46) [Processing order](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/iop_order.c#L298)

## Demosaic is sensor- and quality-dependent

**S.** `demosaic.c` contains separate choices for Bayer and X-Trans layouts. Bayer-related choices include RCD, AMaZE, VNG4, PPG and LMMSE; X-Trans has VNG and Markesteijn variants. Dual approaches blend results from different methods according to image structure. Not every option applies to every sensor. [Demosaic](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/demosaic.c#L53)

**S.** Reduced-resolution and fast processing can use different paths from full-resolution rendering. Consequently, a Fit preview's detail or artifacts are not by themselves evidence of the export demosaicer's behavior. Inspect at a specified scale and record the chosen method and pipe mode when comparing quality. [Demosaic](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/demosaic.c#L53)

**C.** Demosaic reconstructs missing color samples from spatially interleaved sensor measurements. Its compromises include false color, zippering, detail and noise response. Resizing a camera JPEG is not a substitute for testing this stage on a RAW fixture.

## Highlight reconstruction is distinct from a tone slider

**S.** The highlight module retains several reconstruction algorithms, including opposed-channel and other spatial/color approaches. Algorithm and sensor conditions affect eligibility and input-region needs; some paths need a broader image context. Different reconstruction choices can therefore change both results and cost before scene-to-display tone mapping occurs. [Highlight reconstruction](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/highlights.c#L59)

**C.** If one sensor channel clips while others remain usable, reconstruction can infer plausible color/structure from surviving information. If all channels are saturated, no tone curve can recover the original measurement. A smooth-looking highlight is not proof that physically correct lost detail was recovered. Filmic/sigmoid/AgX then compress the available scene values into a display rendition; they do not replace this earlier task.

## Profiled denoise: noise model plus an estimator

**S.** Profiled denoise uses camera/ISO noise-profile data and Poisson/Gaussian-related parameters, with variance-stabilizing transforms and corresponding inverses. These transform signal-dependent noise into a domain where the selected denoiser can use more uniform assumptions. There are retained versions/variants of this processing, so a single textbook Anscombe expression is not a complete implementation specification. [Profiled denoise](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/denoiseprofile.c#L69)

Two important processing families are present:

| Path | Source-level idea | Practical consequence |
| --- | --- | --- |
| Non-local means | Compare neighborhoods and average contributions according to similarity | Search/patch scales affect cost, noise suppression and texture retention |
| Wavelets | Decompose into scales and shrink/modify noisy detail before reconstruction | Different scales and color components can be treated differently |

These run within the module's noise-model/preconditioning machinery. Neither is just a Gaussian blur, and neither should be described as AI inference. [Profiled denoise](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/denoiseprofile.c#L69)

**S.** The release's noise-profile dataset contains Nikon **Z 6** and Fujifilm **X100VI** entries. That is useful evidence for Luxforge's camera research, but it establishes only the presence of profile data at this revision. It does not verify RAW decoding for the owner's exact recording modes, profile accuracy across all settings, or performance on the M4. [Camera noise profiles](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/data/noiseprofiles.json#L3550)

## What to benchmark before proposing a new decoder

**P.** Preserve Luxforge's existing requirement to benchmark established libraries first. Split the work into file reading/unpacking, black/white normalization, demosaic, color conversion, denoise and final rendering. An end-to-end export time cannot identify which component is slow.

Use actual Nikon Z6 and X100VI fixtures with recorded bit depth, compression and capture settings. Check orientation, active sensor dimensions, black levels, saturated channels, color and corrupt/truncated-input recovery. Hash the original before and after. Record native M4 timings separately from portable functional checks. These are future experiments, not evidence that any new RAW dependency has been selected or qualified.
