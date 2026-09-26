# Geometry, masking and retouching

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## Geometry and optical corrections

**D.** Classic's crop interface includes aspect locking and straightening. [S08: Develop module tools](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-tools.html) Lens corrections address distortion, vignetting and chromatic aberration; manual transforms also include rotation, scale and constrained cropping. Post-crop vignetting follows the composition, unlike optical falloff correction. Grain is a synthetic effect with amount/size/roughness controls. [S23: Retouch photos](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html)

**D.** Upright analyzes perspective, with automatic and guided modes. Adobe recommends applying lens profiles before that analysis. Guided mode responds to user-drawn lines. Cropping can remove empty boundaries created by the transform; mode selection can reset prior crop/transform unless modified as documented. [S28: Guided Upright](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/guided-upright-perspective-correction.html)

**C.** A robust geometric renderer maps destination coordinates back into source coordinates and resamples there. Combining transforms can avoid cumulative resampling loss. But the retrieved sources do not reveal Adobe's exact composition order, interpolation kernel or crop-fitting algorithm. Lens distortion generally requires a nonlinear mapping; a simple affine matrix is insufficient.

**P.** Luxforge's accepted free crop, centered proportional Option resizing and composition-preserving straighten behavior remains defined by its own [editor/crop specification](../../specs/single-image.md). Similar Lightroom gestures do not settle edge clamping, mask-coordinate transforms, pixel-center conventions or rotation/crop composition.

## A mask is an editable selection plus an effect

**D.** Classic provides brush, linear/radial gradient, color/luminance/depth range and semantic selections such as subject, sky, objects and people. Mask components support inversion and intersection, with add/subtract refinement. AI masks may require updating after other changes. [S27: Masking](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/masking.html)

**C.** An abstract mask is a coverage field `M(x,y)` between zero and one. A basic illustrative local effect is:

```text
out = (1 − M) × input + M × effect(input)
```

This is a compositing model, not proof of Lightroom's exact local-tone implementation. The real calculation could modulate parameters, alter a nonlinear stage or use a different blending domain. Two overlapping local corrections do not necessarily equal one correction with summed slider values.

**D.** Brush Size, Feather, Flow and Density have separate roles: footprint, edge softness, application rate and stroke transparency. Auto Mask constrains painting using similar color. [S27: Masking](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/masking.html)

**C.** Persisting only an overlay screenshot loses useful semantics. A future editor may need brush samples, coordinate space, pressure/flow behavior, component composition and computed selection data. Whether Lightroom stores each selection as vectors, rasters, compressed fields or a mixture is not established here.

**P.** For Luxforge, distinguish:

- Editing the mask definition from editing the effect's settings.
- Temporarily showing/hiding an overlay from disabling an effect.
- Recomputing an automatic selection from preserving a user's accepted selection.
- Copying a semantic instruction such as “select subject” from copying fixed coordinates.

These distinctions belong in the common operation API if masks are scoped later. A pointer gesture cannot be the sole representation of the operation.

## Clone, heal and content generation

**C.** Conventional clone copies a chosen source region; heal seeks to match source detail with destination appearance; content-aware repair searches/synthesizes a plausible replacement; generative repair uses a trained model. These describe algorithm families, not recovered Lightroom implementations. Source-region selection, masks and ordering can affect reproducibility.

**D.** Adobe exposes separate Remove, Heal and Clone modes. Generative Remove uses Firefly, needs internet access and offers generated variants; content-aware removal has source-sampling controls. [S30: Remove tool](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html) **U.** No source here proves Lightroom Heal is a particular Poisson solver or that Content-Aware Remove is exactly the published PatchMatch algorithm. Avoid identifying algorithms solely from tool names.

**P.** Preserve the chosen generated result or repair dependency, not just a prompt or brush bounding box. If an effect asset disappears, indicate the missing dependency instead of silently exporting without it. That follows Luxforge's existing missing-provider principle.

## Depth and Lens Blur

**D.** Lens Blur estimates a depth map with Adobe Sensei, can use supported embedded device depth, and offers focus-range selection, bokeh shape/intensity and brush refinement of focus/blur. [S29: Lens Blur](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/lens-blur.html)

**C.** Depth-aware blur is spatially varying and must handle occlusion boundaries; a uniform Gaussian blur cannot recreate the same behavior. Hair, glass and overlapping edges are useful failure fixtures. A depth map is an estimate, not necessarily calibrated metric distance.

## Testing implications

**P.** Check masks and repairs across crop/rotate/flip, proxy/full resolution and export. Include strokes at image edges, intersected soft masks, overlapping repairs, unavailable derived assets and updates to automatic selections. Geometry correctness and the visual naturalness of a healed region need different evidence. None of these future tools was implemented or tested in this research task.
