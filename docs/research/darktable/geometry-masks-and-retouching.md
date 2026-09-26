# Geometry, masks, blending and retouching

[Knowledge base index](README.md) · Source baseline: 5.6.1.

## Geometry is a coordinate contract

**S.** Image operations can implement forward/backward point transforms and input/output region-of-interest callbacks. Crop changes the output bounds and coordinate mapping; perspective/rotation needs transformed sampling. The pipe walks requested regions backward through modules to determine what upstream pixels are needed. A screen-space rectangle alone does not describe a persistent edit. [Crop](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/crop.c#L463) [Rotate and perspective](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/ashift.c#L1001) [IOP callback contract](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/iop_api.h#L316) [Pixelpipe evaluator](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/pixelpipe_hb.c#L1757)

**S.** The perspective implementation and interpolation library expose actual resampling machinery, including bilinear, bicubic and Lanczos variants. Kernel selection affects support width, sharpness, ringing and cost. The crop module itself should not be assumed to resample with the same kernel as a rotated/perspective view. [Rotate and perspective](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/ashift.c#L1001) [Resampling kernels](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/interpolation.c#L161) [Crop](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/crop.c#L463)

**C.** A rotated output pixel usually corresponds to a noninteger input coordinate, so it needs reconstruction from neighboring samples. Pure integer cropping can instead select an existing region. This distinction helps avoid unnecessary resampling and makes geometry tests more precise.

**P.** Luxforge's accepted crop behavior—free handles, proportional Option scaling about a fixed center and composition-preserving straightening—remains its own specification. darktable's implementation is evidence for coordinate/ROI design, not a replacement for those product decisions.

## Lens correction has both metadata and model dependencies

**S.** `lens.cc` supports Lensfun-based correction and embedded-metadata methods. Depending on image and method, correction includes geometry and photometric effects such as vignetting/chromatic aberration. Profile availability and method choice are part of the result; correction is not reducible to one universal radial-distortion coefficient. [Lens correction](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/lens.cc#L73)

**C.** Distortion correction can change which source pixels are needed and introduce resampling. Vignetting correction can amplify corner noise. These consequences should be measured separately from file decoding and from the perceptual success of a profile.

## Shared blending applies a module locally

**S.** The blending layer is separate from each module's principal image algorithm. It handles drawn, parametric and raster-mask inputs, blend parameters and domain-specific processing. It checks geometric compatibility of input/output regions and scales. Modules therefore cannot assume an unrelated arbitrary mask can be indexed directly with their output pixel coordinates. [Shared blending](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/blend.c#L458)

| Mask concept | Source-level role | Dependency to preserve |
| --- | --- | --- |
| Drawn forms | Geometry such as paths, gradients and brush-related shapes, with group combination | Coordinates, feather/opacity and geometric transforms |
| Parametric mask | Selection based on image values in the relevant blending domain | Input/output values and color-domain interpretation |
| Raster mask reuse | Reuse a produced mask elsewhere in the pipe | Producer instance, ordering and compatible transformed data |
| AI-authored object mask | Inference followed by current-tool vectorization into path groups | Accepted paths versus disposable inference state |

The categories above describe the inspected implementation, not an assertion that all mask kinds use the same storage or recomputation rules. [Mask host](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/masks/masks.c#L30) [Shared blending](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/blend.c#L458) [AI mask finalization](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/masks/object.c#L1093)

**C.** For ordinary opacity mixing, the conceptual expression is `(1-m)*input + m*processed`, with `m` between zero and one. This illustrates a mask's role; darktable's complete blending system has additional modes, color-domain logic, combinations and opacity handling. Do not present that equation as its universal implementation. [Shared blending](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/blend.c#L458)

**S.** Raster reuse makes module order a data dependency as well as a visual preference. History/persistence includes mask state alongside parameter state. Reconstructing only slider numbers cannot recover an edit with missing forms or a missing upstream mask producer. [Shared blending](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/blend.c#L458) [History persistence](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/develop.c#L1769)

## Clone, heal, blur and fill are different retouch operations

**S.** Retouch supports these operation families and can work at wavelet scales. This permits frequency-selective retouching rather than treating every mark as replacement of the complete source pixel. The implementation tracks source/target forms and uses the appropriate processing path for the operation. [Retouch](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/retouch.c#L67)

**S.** Healing calls a solver whose source credits GIMP's healing work. It forms a difference between sample/reference data, solves a Laplace boundary-value problem with boundary values around the mask, then uses the smooth correction to reconcile the patch. The implementation uses a red/black Gauss–Seidel-style iterative relaxation with over-relaxation. This is concrete evidence of a boundary-matching algorithm, rather than an inference based on the tool name. [Healing solver](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/heal.c#L30)

**C.** Cloning copies chosen source structure; healing adjusts the patch to fit the target surroundings. A boundary solver helps explain why healing may fail or smear when the chosen source/target structure is unsuitable, even if their average color matches. It does not infer arbitrary missing objects like a generative inpainting model.

## Difficult cases for future tests

**P — not run.** Test masks under crop/rotation/reordering, strokes near image edges, overlapping clone/heal sources, repeated instances, scale-specific retouching, very small exports and tile boundaries. Compare Fit, 100% and export while recording interpolation, pipe quality and mask state. Undo/redo should restore both the visible effect and the stored geometry. Missing auxiliary resources must remain a visible recoverable condition; they must not silently become an empty mask or omitted effect.
