# Tone and color algorithms

[Knowledge base index](README.md) · All formulas describe identified paths in the pinned 5.6.1 implementation. They do not imply Adobe equivalence or a universal slider scale.

## Exposure: a useful exact starting point

**S.** In the prepared manual exposure path, let `e` be effective exposure in stops, `b` the module's black offset, and `x` an input color component. The code computes:

```text
white = 2^(-e)
scale = 1 / (white - b)
y = (x - b) * scale
```

With `b = 0`, this reduces to `y = x * 2^e`: one stop doubles the linear component. With a nonzero black adjustment it is an affine transform, and “black” is not equivalent to multiplying dark pixels only. This formula describes the module's pixel arithmetic, before later color/tone mapping and blending. [Exposure](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/exposure.c#L468)

**S.** The effective exposure can differ from the raw UI setting because of compensation and automatic deflicker processing. Deflicker derives a correction from a RAW histogram and the target level; it is not a second arbitrary tone curve. **C.** A twofold change in linear light need not double displayed sRGB code values, since later rendering and transfer functions intervene. [Exposure](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/exposure.c#L468)

## There are several distinct kinds of contrast

| Module | Main operation | Why the same numeric value is not portable |
| --- | --- | --- |
| Color balance RGB | Power contrast about a gray fulcrum within a larger color-grading pipeline | Works in a luminance-related domain with its own parameter mapping |
| Legacy contrast/brightness/saturation | Lab lightness lookup curve and chroma scaling | Different pivot, curve and encoding; deprecated |
| Filmic RGB, sigmoid, AgX | Scene-to-display tone rendering and associated color handling | Contrast interacts with black/white range, shoulder and color rendering |
| Local contrast | Bilateral or local-Laplacian processing | Output depends on surrounding pixels and scale |
| Contrast equalizer | Gains and thresholds over wavelet bands | Selects spatial scales instead of only tonal ranges |

The first three are detailed here; see [detail algorithms](detail-and-local-contrast.md) for the spatial operators. [Color balance RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorbalancergb.c#L579) [Legacy contrast/brightness/saturation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colisa.c#L179) [Filmic RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/filmicrgb.c#L888) [Sigmoid](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/sigmoid.c#L300) [AgX](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/agx.c#L1326) [Local contrast](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/bilat.c#L293) [Contrast equalizer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/atrous.c#L350)

### Color balance RGB's gray-pivot contrast

**S.** One identifiable stage computes `Y' = F * (Y / F)^(1+c)`, where `F` is the prepared gray fulcrum and `c` the stored contrast parameter. The actual runtime exponent is prepared as `1 + c`. This is applied to the `Y` component of the module's Yrg representation, within conversions and other adjustments. [Color balance RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorbalancergb.c#L579)

**C.** For positive values, `log(Y'/F) = (1+c) * log(Y/F)`. Thus the fulcrum stays fixed and the slope in log luminance changes. This is a useful explanation of the contrast stage, **not** a formula for the module's complete RGB output. The complete result also depends on hue/chroma changes, tonal masks, grading, saturation/brilliance and gamut handling.

### Legacy contrast/brightness/saturation

**S.** `colisa.c` exposes an unusually direct example of a conventional lightness curve. For Lab lightness `L` in the nominal 0–100 range and prepared contrast `C = 1 + parameter`, the contrast LUT uses:

```text
C <= 1: y = C * (L - 50) + 50
C > 1:  q = 20 * (C - 1)^2
        z = 2*L/100 - 1
        y = 50 * (sqrt(1+q)*z/sqrt(1+q*z*z) + 1)
```

Brightness then uses a power-law LUT, with `B = 2 * brightness_parameter` and `gamma = 1/(1+B)` for nonnegative `B`, otherwise `gamma = 1-B`. Its nominal lightness mapping is `100 * (L/100)^gamma`. Saturation multiplies the Lab `a` and `b` components by `1 + saturation_parameter`. The implementation uses 65,536-entry tables and extrapolation outside the table range; the formulas above do not reproduce those details automatically. [Legacy contrast/brightness/saturation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colisa.c#L179)

This is retained legacy code, not a recommendation for a new scene-referred editor. It also shows why “increase contrast by 20” has no cross-application mathematical meaning without identifying the algorithm and parameter conversion.

## Tone equalizer: exposure gains selected by an edge-aware mask

**S.** Tone equalizer estimates a luminance/energy mask, optionally smooths it using guided-filter variants, and indexes an exposure-dependent correction. Its control nodes are in stop space; Gaussian radial-basis interpolation produces a smooth response between them. The resulting gain multiplies the color channels together. It can therefore distinguish broad bright/dark regions without simply applying a fixed independent RGB curve. [Tone equalizer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/toneequal.c#L1167)

**S.** The mask controls and output gains do different jobs. Mask exposure/contrast moves pixels among the tonal control ranges; edge-aware smoothing changes the spatial regions that receive a gain. The default processing order puts this module before input color profile, after earlier RAW/geometry/exposure stages. Its source explicitly accounts for a linear camera-RGB context. Do not move its formula to display-encoded sRGB and expect the same result. [Tone equalizer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/toneequal.c#L1167) [Processing order](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/iop_order.c#L298)

**C.** This helps explain the appeal of a shadows/highlights interface: a user is often asking for regional exposure changes that preserve local detail. darktable makes the intermediate mask inspectable, while Lightroom's exact implementation remains proprietary. This is a functional comparison, not evidence of a shared algorithm.

## Filmic RGB

**S.** Filmic maps scene intensity into a logarithmic interval relative to middle gray and the selected black/white exposures, then uses spline-based rendering with toe, central contrast and shoulder behavior. Several historical color-science/processing versions remain in the same source. Color preservation and gamut-related behavior accompany the tone mapping; selecting the module name alone is insufficient to specify a rendition. [Filmic RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/filmicrgb.c#L888)

**C.** Separating the scene range from the display contrast is important: a large scene range must fit into a limited output range, but the user may still want a particular midtone slope. The toe/shoulder absorb that constraint. This operation does not restore sensor values that were never recorded; highlight reconstruction is a separate earlier task.

## Sigmoid

**S.** The scalar helper is a generalized log-logistic curve, written in a numerically safer form around zero:

```text
v = max(x, 0)
f = (film_fog + v)^film_power
y = magnitude * (f / (paper_exp + f))^paper_power
```

If extreme floating-point values produce NaN, this helper returns `magnitude`. `commit_params` derives coefficients from the user controls and target behavior; the UI sliders are not direct assignments to all the symbols above. Contrast and skew affect the solution. [Sigmoid](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/sigmoid.c#L300)

**S.** The complete module has different color-processing strategies, including per-channel processing and RGB-ratio handling, with hue/primary adjustments. A scalar curve by itself cannot reproduce the entire module. The configured default workflow at this release selects sigmoid; see [version caveats](versions-and-caveats.md) for the stale manual statements about filmic. [Sigmoid](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/sigmoid.c#L300) [Workflow defaults](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/data/darktableconfig.xml.in#L3898)

## AgX

**S.** This release also contains an AgX module. The processing path includes conversion from the working primaries, gamut handling, a rendering-primary transform, tone mapping/look processing and conversion back. Input sanitization explicitly handles invalid/extreme floating-point values. The implementation includes tunable tone-curve and look behavior; it is not adequately described as applying one fixed Blender LUT. [AgX](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/agx.c#L1326)

**D.** The official module reference describes exposure-range, contrast, primaries and look controls. **C.** Those controls make the relationship between scene encoding, rendering primaries, hue behavior and final tone rendition explicit. The particular implementation and defaults must accompany any comparison to another application's AgX. [AgX controls](https://docs.darktable.org/usermanual/5.6/en/module-reference/processing-modules/agx/)

## Color controls are not interchangeable either

**S.** Color calibration combines chromatic adaptation, channel mixing and calibration/profile-related controls. CAT16 and Bradford paths are visible in the source. This is conceptually separate from early sensor-channel white-balance gains and from later creative color grading. [Color calibration](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/channelmixerrgb.c#L104)

**S.** Color balance RGB uses smooth tonal weights, chroma/hue changes, grading and perceptual saturation/brilliance processing. Its vibrance stage depends on existing chroma; it does not simply apply one constant saturation multiplier to every pixel. Different retained saturation formulas and gamut processing further affect the result. [Color balance RGB](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/colorbalancergb.c#L579)

**P.** For Luxforge, specify each future control by input domain, neutral state, parameter units, output behavior and relevant spatial context. Familiar labels can help users, but tests should assert the selected algorithm's behavior instead of assuming Adobe or darktable numeric equivalence. No additional controls are adopted by this chapter.
