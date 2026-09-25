# Color and scene-referred processing

[Knowledge base index](README.md) · Source baseline: 5.6.1.

## Scene-linear is an interpretation, not the entire algorithm

**C.** A scene-linear sample is proportional to a modeled amount of light. It can exceed the eventual display white: a value above 1 need not be an error. Compressing these values into a bounded display range is a rendering choice. Applying exposure before that compression differs from brightening already compressed display RGB.

**S.** darktable exposes input/working profiles and defaults the working-profile parameter to linear Rec.2020. Modules declare their expected color spaces; the pipeline performs conversions where required. Sensor data, camera RGB, working RGB, Lab and output/display RGB are therefore distinct domains. Float processing is widespread, but “every stage is linear Rec.2020” is false. [Input color profile](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorin.c#L80) [IOP callback contract](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/iop_api.h#L316) [Pixelpipe evaluator](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/pixelpipe_hb.c#L1757)

**D.** The intended scene-referred workflow delays major tone compression and performs much of the work before a display-rendering module. Legacy display-referred arrangements remain available. [Pixelpipe and order](https://docs.darktable.org/usermanual/5.6/en/darkroom/pixelpipe/the-pixelpipe-and-module-order/)

## Boundaries to keep separate

| Boundary | What it does | Source entry point |
| --- | --- | --- |
| RAW normalization | Black subtraction, white range, sensor crop, optional gain maps | [RAW normalization](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/rawprepare.c#L46) |
| Sensor white balance | Channel compensation before later color treatment | `temperature` in the [order tables](pixelpipe-and-rendering.md#render-order-is-not-history-order) |
| Input profile | Interprets camera/rendered input color and selects working representation | [Input color profile](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorin.c#L80) |
| Color calibration | Chromatic adaptation and channel mixing, with CAT16 and other modes | [Color calibration](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/channelmixerrgb.c#L104) |
| Rendering transform | Compresses dynamic range and manages the resulting color appearance | [Filmic, sigmoid, AgX](tone-and-color-tools.md#filmic-rgb) |
| Output profile | Converts pixels for export/display/proofing | [Output color profile](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorout.c#L488) |
| File format | Encodes those samples at chosen size/precision with metadata | [Export implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/imageio/imageio.c#L991) |

**S.** Color calibration has explicit chromatic-adaptation modes, including CAT16 and Bradford-related paths, plus camera/working-profile matrices. This is more than applying a Kelvin-to-RGB multiplier. The sensor white-balance and calibration modules have cooperating responsibilities; disabling one casually can change the meaning of the other's assumptions. [Color calibration](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/channelmixerrgb.c#L104)

**C.** White balance, chromatic adaptation and creative warming are related but not interchangeable. For a JPEG, camera rendering and clipping already occurred; a RAW-specific correction cannot reconstruct all lost information from the JPEG.

## Tone mapping is not ICC profile conversion

**S.** Filmic, sigmoid and AgX are actual processing modules with their own curve/appearance controls. The configured new-image workflow selects sigmoid by default at this commit, with filmic and AgX choices. [Workflow defaults](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/data/darktableconfig.xml.in#L3898)

**C.** Tone mapping answers “how should this scene's brightness and color relationships fit the output?” An ICC transform answers “what coordinates describe this color in another device/profile?” A camera profile is not a complete creative look, and attaching an ICC tag without transforming samples does not perform color conversion.

**S.** The output module can use a fast matrix/curve path when the profile supports it, or Little CMS transforms. Soft-proof modes and a forced-Little-CMS export preference affect that choice. This is a concrete reason to record profile and rendering settings when comparing images rather than assuming one implementation path. [Output color profile](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorout.c#L488)

## Perceptual controls still need explicit domains

**S.** Color balance RGB's implementation moves among working RGB, luminance/chroma coordinates and perceptual representations for different operations. Its gray-fulcrum contrast is a power transformation of a luminance component; its saturation/chroma/vibrance behavior is not a single uniform multiplication of RGB. [Color balance RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorbalancergb.c#L579)

**C.** Increasing saturation around one achromatic axis and applying contrast independently per channel can produce different hue changes. A wide working gamut avoids some premature clipping but does not automatically solve gamut mapping, negative values, highlight desaturation or display limits.

**P.** For Luxforge, each future operation should specify input/output color meaning, treatment of negative/over-range values, and the location of clipping. Numerical fixtures should test saturated primaries, neutral ramps, near-black gradients and bright colored lights—not only ordinary photographs.

## What a color-correctness experiment must record

**P.** Record original and output hashes, input profile, working profile, rendering module/version, output profile/intent, optional proofing, image dimensions, preview mode and CPU/GPU selection. Use lossless exported pixels for numerical comparison; screenshots serve UI evidence and require their own profile/capture provenance.

**U.** No monitor calibration, HDR desktop presentation, CPU/GPU color equivalence or end-to-end M4 display validation was performed. Source availability tells us where transformations occur; it does not certify every supported device or driver.
