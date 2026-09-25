# Detail, local contrast, texture and haze

[Knowledge base index](README.md) · Source baseline: 5.6.1.

## Mapping the user's words to actual operators

There is no justified one-to-one numeric mapping from Lightroom Clarity or Texture to darktable. The useful comparison is the visual task and the spatial scales involved.

| Desired effect | darktable implementation to study | Main mechanism |
| --- | --- | --- |
| Broad local contrast / “clarity” | Local contrast (`bilat.c`) | Bilateral-grid or local-Laplacian filtering |
| Selectively increase medium/fine texture | Contrast equalizer (`atrous.c`) | Edge-aware wavelet-band processing |
| Diffuse detail or produce more complex sharpening | Diffuse or sharpen (`diffuse.c`) | Iterative multiscale diffusion |
| Conventional small-scale sharpening | Sharpen (`sharpen.c`) | Thresholded Gaussian unsharp mask on lightness |
| Reduce atmospheric veil | Haze removal (`hazeremoval.c`) | Estimated atmospheric light and transmission |

These mechanisms are source-confirmed; any claim that one preset matches Lightroom would require its own image comparison. [Local contrast](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/bilat.c#L293) [Contrast equalizer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/atrous.c#L350) [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340) [Sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/sharpen.c#L250) [Haze removal](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/hazeremoval.c#L559)

## Local contrast: two processing paths

**S.** The module is named “local contrast” in the UI but implemented in `bilat.c`; searching only for a file called “clarity” would miss it. Its parameter preparation dispatches between bilateral-grid and local-Laplacian paths. The bilateral path splats into a grid, blurs and slices back; spatial scale is adjusted for the requested image scale. This is a neighborhood operator, not a global S-curve. [Local contrast](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/bilat.c#L293)

**S.** In the local-Laplacian path, a pyramid separates image scales. Remapped versions of the input contribute coefficients, which are interpolated according to the guide intensity and reconstructed. This implementation samples six guide levels `(k + 0.5) / 6`. Six is an implementation choice at this revision, not a mathematical requirement of all local-Laplacian filters. [Local Laplacian implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/locallaplacian.c#L313)

### What its internal “clarity” term actually does

**S.** In `curve_scalar`, let `c = x - g`, where `g` is the current guide level. After a piecewise shadow/highlight remap, the helper adds:

```text
clarity * c * exp(-3*c*c / (2*sigma*sigma))
```

The code uses a fast exponential approximation. This expression enhances signed differences near a guide level and decays for larger differences. Here `sigma` is an intensity-domain threshold in the remapping function, not a pixel-radius setting. It is one component of the remap used by the pyramid algorithm, **not** the final output-pixel formula. [Local Laplacian implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/locallaplacian.c#L313)

**S.** The module disables tiled processing readiness for this local-Laplacian path. Thus a generic assumption that every local-contrast filter can be chopped into independent tiles with a small border would contradict this release's actual contract. [Local contrast](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/bilat.c#L293)

**C.** Spatially broad edges, fine grain and texture are different frequency content. A local-contrast change can make both desirable structure and unwanted noise more visible; a useful control needs a deliberate scale/noise response, not only a compelling name.

## Contrast equalizer: editing bands of detail

**S.** The current contrast equalizer is `atrous.c`, backed by edge-aware wavelet helpers in `eaw.c`. Decomposition produces progressively smoother approximations and detail bands. Per-scale curves control the treatment of luminance/chroma detail and edge/noise-related behavior. Synthesis combines the modified bands into an image. Do not confuse this with the older deprecated `equalizer.c` implementation. [Contrast equalizer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/atrous.c#L350) [Edge-aware wavelet helper](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/eaw.c#L111)

**S.** Its helper can decompose and accumulate synthesized detail immediately, using temporary buffers rather than retaining every band's full image indefinitely. The implementation is useful to study for memory behavior as well as its visual controls. [Edge-aware wavelet helper](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/eaw.c#L111)

**C.** A Texture-like control could choose a constrained subset of these scales and a bounded gain response. That is a design possibility, not a claim that Adobe Texture uses this code, these wavelets or these thresholds. Noise and fine real texture overlap in scale, so indiscriminate band gain will amplify both.

## Diffuse or sharpen: an iterative multiscale computation

**S.** This module operates over a B-spline wavelet representation and iteratively updates detail using diffusion-related parameters. Controls include iteration count, spatial radius/scale extent, first-through-fourth-order speed terms and directional/anisotropy behavior. Negative and positive responses support sharpening and diffusion effects. It is substantially more involved than subtracting a single Gaussian blur. [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340)

**S.** The CPU implementation allocates working images and per-scale high-frequency buffers, then repeats the multiscale processing for the requested number of iterations. Radius/scale extent changes the decomposition workload; iterations multiply repeated work. Memory-allocation failure has an explicit fallback/logging path. [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340)

**S.** The fast pixelpipe path can immediately copy the input through and return, omitting the effect for that rendering. A very responsive preview is therefore not proof that this module ran at full quality. **P.** Any Luxforge equivalent should make intermediate quality and final settled state observable to agents as well as to the interface. [Diffuse or sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/diffuse.c#L1340)

## Sharpen: thresholded unsharp masking

**S.** The conventional sharpening module blurs the lightness channel with a separable Gaussian kernel. In its interior-pixel path, the essential calculation is:

```text
d = L - gaussian_blur(L)
detail = sign(d) * max(abs(d) - threshold, 0)
out_L = L + amount * detail
```

The chromatic components are preserved. The implementation also handles image borders, small images and a radius adjusted to the pipeline's scale; reproducing only the equation would miss those choices. [Sharpen](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/sharpen.c#L250)

**C.** Radius chooses which neighborhood contributes to the residual, amount scales that residual, and threshold suppresses weak differences. The threshold can reduce noise amplification but cannot reliably distinguish noise from all fine texture. This operator is not a lens-deconvolution model merely because the image looks sharper.

## Haze removal: estimating and inverting an atmospheric model

**S.** The source identifies dark-channel haze estimation and guided image filtering as its algorithmic basis. It estimates atmospheric light `A`, builds/refines a transmission map, and reconstructs each color component using an expression of this form:

```text
t_floor = clamp(exp(-distance * estimated_max_distance), 1/1024, 1)
t = max(filtered_transmission, t_floor)
out = (in - A) / t + A
```

Strength affects the transmission estimate; it is not merely the opacity of a fixed contrast curve. The floor limits extreme amplification. Window/scale behavior and retained compatibility settings also matter. [Haze removal](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/hazeremoval.c#L559)

**S.** Atmospheric estimates are image-wide dependencies. The implementation can cache statistics obtained through the preview so the full view's restricted region does not choose inconsistent atmospheric light merely because the user zoomed. High-quality processing has its own estimation behavior. This is a concrete case where a pixel's result depends on more than the visible local crop. [Haze removal](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/hazeremoval.c#L559)

**C.** Haze inversion boosts both useful signal and noise when transmission is small; incorrect atmospheric assumptions can produce color shifts or excessive contrast. A negative slider value is not evidence of a mathematically exact inverse of positive removal, particularly when estimation, clipping and floors intervene. Lightroom's similarly named Dehaze remains a separate proprietary implementation.

## Experiments suggested by these implementations

**P — not executed.** Use ramps with isolated edges, sinusoidal frequency sweeps, textured patches and noise at several amplitudes. Compare full resolution, Fit and export; include border/ROI changes, tile boundaries, negative values and extreme slider settings. Record halo width, frequency gain, noise amplification, peak memory and CPU/GPU differences. A screenshot of a pleasing photograph is insufficient to establish numerical behavior or scale consistency.
