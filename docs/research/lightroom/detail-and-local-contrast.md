# Detail, local contrast and Dehaze

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## Contrast, Clarity, Texture and sharpening are different controls

This table combines documented behavior with the engineer's frequency explanation. It is a qualitative comparison, not measured transfer functions. [S18: Introducing the Texture Control](https://blog.adobe.com/en/publish/2019/05/14/from-the-acr-team-introducing-the-texture-control) [S23: Retouch photos](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html) [S39: Basic editing techniques](https://www.adobe.com/learn/lightroom-classic/web/basic-photography-editing-techniques)

| Control | Main target | Typical visible effect | What it does not establish |
| --- | --- | --- | --- |
| Contrast | Tonal separation, especially midtones | Darker darks and lighter lights | A fixed global S-curve implementation |
| Clarity | Broader local tonal structure | Stronger dimensional separation | A specified blur radius or unsharp-mask formula |
| Texture | Medium-frequency detail | Stronger or smoother surface texture | Fourier processing or a fixed passband |
| Sharpening | Fine edges/detail | Increased edge definition | Recovery of actual missing scene information |
| Dehaze | Haze-related loss of contrast/color | Reduced veiling or added haze | The same operation as Contrast |

**H.** Texture's lead engineer describes medium-frequency targeting, with less amplification of very fine noise than sharpening in his examples. Negative Texture smooths while retaining fine features. Clarity spans a broader frequency range, including lower frequencies, and affects luminance/saturation more strongly. Texture is available globally and locally. Exact radii, nonlinearities, edge protection and slider mapping are unpublished in that explanation. [S18: Introducing the Texture Control](https://blog.adobe.com/en/publish/2019/05/14/from-the-acr-team-introducing-the-texture-control)

**C.** “Frequency” means spatial variation: broad gradients vary slowly; pores, hairs and noise vary quickly. An algorithm can separate scales with spatial filters or pyramids without performing an FFT. Two controls can both increase apparent detail while affecting different structures and artifacts.

## A conceptual multiscale model

**C.** Let `Gσ(I)` be a smoothed image. A band between two scales can be illustrated by:

```text
D = Gσ_small(I) − Gσ_large(I)
I_out = I + a × D
```

Varying `a` emphasizes or attenuates that band. This explains why a medium-scale adjustment can leave both very fine detail and broad shading relatively stable. It is not Texture's implementation. A naive version can create halos, change color or mishandle strong edges; the choice of scales and edge-aware processing is central.

For ordinary unsharp masking, `I_out = I + k(I − Gσ(I))`. Increasing `σ` affects broader structures, but that alone does not reproduce Clarity. The distinction between an explanatory analogy and a verified algorithm matters here.

## Published local Laplacian foundation

**H.** Paris, Hasinoff and Kautz's 2011 paper constructs edge-aware processing using Laplacian pyramids. Instead of indiscriminately amplifying coefficients, it uses local intensity-centered remappings to assemble an output pyramid, then reconstructs the image. The method separates large intensity transitions from smaller detail and demonstrates smoothing, detail enhancement and tone mapping. [S19: Local Laplacian Filters](https://jankautz.com/publications/LocalLaplacianFiltersSIG11_lowres.pdf)

**H.** There is evidence of actual historical Adobe adoption: UCL's research-impact account identifies Lightroom/Camera Raw, and the Adobe-coauthored 2013 Halide paper connects the technique to clarity and tone-mapping filters. This is stronger than inferring an implementation from similar screenshots. [S21: UCL research impact case study](https://impact.ref.ac.uk/casestudies/CaseStudy.aspx?Id=29898) [S20: Halide image-processing pipelines](https://people.csail.mit.edu/jrk/halide-pldi13.pdf)

**U.** These papers do not disclose today's Clarity settings, Texture algorithm, CPU/GPU kernels or the exact relationship between every Basic-panel slider and a pyramid stage. The 2011 method is a research foundation, not a drop-in Adobe clone. The Halide paper's many-stage example describes its benchmark configuration, not Lightroom's complete processing graph.

**P.** For Luxforge, compare an edge-aware multiscale implementation with a simpler baseline on step edges, texture gradients and noisy shadows. Measure halos, color changes, zoom consistency, memory and latency. A sophisticated paper is a candidate to evaluate, not an automatic dependency decision.

## Sharpening and manual noise reduction

**D.** Sharpening Amount controls strength; Radius controls feature size; Detail changes emphasis on fine information; Masking limits sharpening toward stronger edges. Adobe recommends evaluating at 100%. Manual noise reduction separates luminance and color noise; its detail/contrast controls trade retained structure against residual noise or blotching. [S23: Retouch photos](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html)

**C.** Noise and real texture overlap in frequency. “Remove high frequencies” cannot reliably separate them. Sharpening masks, signal-dependent thresholds and edge-aware processing address that ambiguity. A Fit preview can conceal both noise and sharpening artifacts; compare actual pixels and final output size.

**D.** Export sharpening is an additional operation adapted to output size/media, separate from Develop sharpening. [S32: Export files](https://helpx.adobe.com/lightroom-classic/desktop/export-photos/export-files-disk-or-cd.html) **U.** The cited product pages do not establish a particular deconvolution, wavelet or neural algorithm for the traditional sharpening/manual-denoise sliders.

## Dehaze

**H.** Julieanne Kost's technical notes describe Dehaze as based on a physical model of atmospheric transmission, estimating losses from absorption/scattering. This supports an atmospheric explanation rather than calling it merely stronger Contrast. [S37: Lightroom Classic v13 reference notes](https://jkost.com/blog/wp-content/uploads/2024/02/2024_LrC_v13_Shortcuts.pdf)

**C.** A common simplified image-formation model is:

```text
I(x) = t(x) × J(x) + (1 − t(x)) × A
J(x) = [I(x) − (1 − t(x)) × A] / max(t(x), epsilon)
```

`I` is observed color, `J` an estimated clear scene, `A` atmospheric light, and `t` transmission. This equation illustrates the inverse problem; it is not a recovered Adobe formula. Estimating `A` and `t` from one image is ambiguous. As `t` becomes small, inversion amplifies noise and estimation errors; white balance and gamut handling matter.

**U.** No adequate primary evidence found here identifies Adobe's transmission estimator as dark-channel prior, a specific patent, or a particular neural architecture. Negative Dehaze is a creative control, not necessarily the exact numerical inverse of positive Dehaze. Use current process-version fixtures when comparing it.
