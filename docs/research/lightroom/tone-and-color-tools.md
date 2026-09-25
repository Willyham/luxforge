# Tone and color controls

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## Behavioral reference

**D.** Adobe documents intent, not reproducible formulas, for these controls. The distinctions below summarize the Basic/Tone documentation. [S16: Image tone and color](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html)

| Control | Main action |
| --- | --- |
| Exposure | Overall brightness, labelled in stops |
| Contrast | Separates darker/lighter midtones |
| Highlights | Adjusts bright regions while managing detail/clipping |
| Shadows | Adjusts dark regions while managing detail/clipping |
| Whites | Controls the bright-end clipping boundary |
| Blacks | Controls the dark-end clipping boundary |
| Temperature / Tint | White-balance axes: warm/cool and green/magenta |
| Tone Curve | Maps input tones to output tones; composite and channel curves |
| Saturation | Overall color intensity |
| Vibrance | Preferentially changes less-saturated colors and protects skin-like colors |

**D.** Adobe's PV3–5 procedure explicitly calls the tone controls image-adaptive. Its histogram regions are adjustment guidance, not hard mathematical thresholds. The same slider amount is not a universal input/output curve. [S17: Tone-control adjustment procedure](https://helpx.adobe.com/lightroom-classic/desktop/help/tone-control-adjustment.html)

**D.** Adobe's introductory lesson describes increased Contrast as widening light/dark differences. That explains the photographic objective; it does not establish a particular pivot, sigmoid, color space or adaptive rule. [S39: Basic editing techniques](https://www.adobe.com/learn/lightroom-classic/web/basic-photography-editing-techniques)

## What those descriptions mean computationally

The following are **C: illustrative models**, not Adobe equations.

### Exposure

For linear-light RGB, an ideal exposure change is:

```text
RGB_out = 2^EV × RGB_in
```

Thus +1 stop doubles scene-linear values. Multiplying gamma-encoded JPEG values by two is different. A complete renderer also has highlight rolloff, color transforms and clipping behavior. Lightroom's stop-labelled control does not imply that a screenshot's RGB values double.

Exposure cannot recover information absent from all recorded channels. Mapping a clipped white plateau to gray makes it darker, not detailed. Reconstructing partially clipped color, compressing retained highlights and inventing plausible content are different operations.

### Contrast and curves

A minimal pedagogical contrast transform is `y = p + c(x − p)`, where `p` is a pivot. A smooth S-curve can preserve endpoints more gracefully. Neither specifies Lightroom Contrast. Required design choices include the working transfer function, luminance versus per-channel processing, gamut treatment and out-of-range behavior.

A point curve expresses `y = f(x)`. Interpolation matters: unconstrained splines can overshoot; monotonic interpolation can preserve ordering. RGB-channel curves can shift hue as well as tone. “Linear” in the curve panel does not imply an unprocessed, scene-linear RAW image; upstream profiles and other stages still exist.

### Highlights/Shadows versus Whites/Blacks

Brightening a dark region while retaining its internal contrast requires more than lifting the black point. A conceptual local method decomposes luminance into a broad base plus detail, adjusts the base, then recombines. That helps explain why shadow controls can reveal structure without flattening it as much as a simple global curve. The [local Laplacian research](detail-and-local-contrast.md#published-local-laplacian-foundation) supplies a more rigorous related method; this base/detail sketch is not the shipped algorithm.

### White balance

A simple RAW model applies channel gains, `RGB_balanced = diag(gR,gG,gB) × RGB_camera`, alongside camera-profile/chromatic-adaptation decisions. Kelvin and tint are user-facing descriptions, not directly portable RGB gains. JPEG white balance works on an already rendered image and cannot undo every upstream tone/color/clipping decision. No exact Adobe gain computation was recovered.

### Saturation and Vibrance

A basic saturation model mixes a color away from an achromatic axis: `C_out = L + s(C_in − L)`. The result depends on the definition of `L` and color space. Vibrance requires a variable gain rather than one constant. Adobe educator Julieanne Kost describes it as biased by existing saturation and by color, with weaker changes to orange/red/yellow regions. This is not evidence that the global slider runs face detection. [S37: Lightroom Classic v13 reference notes](https://jkost.com/blog/wp-content/uploads/2024/02/2024_LrC_v13_Shortcuts.pdf)

## Color selection, grading and profiles

**D.** Color Mixer edits hue, saturation and luminance by color range. Point Color adds sampled colors with range/variance controls and range visualization. A global red adjustment affects matching reds anywhere, not only the object clicked. [S44: Color Mixer](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/color-mixer.html)

**H.** Color Grading applies tints to shadows, midtones and highlights, with additional luminance and global controls. Balance shifts their tonal weighting; Blending controls overlap. Even minimum Blending retains smooth transitions. This is tonal-range tinting rather than object recoloring. [S22: Introducing Color Grading](https://blog.adobe.com/en/publish/2020/10/20/introducing-color-grading)

**C.** A useful model for grading is smoothly weighted tint contributions, `Σ w_i(L)·tint_i`; it does not recover Adobe's weights, color mixing or processing order.

**D.** A profile supplies a rendering foundation without necessarily changing visible edit-slider values. Adaptive profiles can depend on image analysis and require refresh. [S16: Image tone and color](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html) **P.** Luxforge should distinguish a preset that assigns settings from a profile resource used during evaluation. It should never equate “all visible sliders at zero” with “no processing.”

## Validation ideas for future Luxforge tools

**P.** Use linear ramps, step wedges, saturated RGB patches, skin-like hues, a backlit face and a clipped highlight fixture. Compare equal-luminance patches in different surroundings to detect spatial adaptation. Check whether a change alters hue, black/white clipping, monotonicity and noise. Test RAW and rendered RGB separately. Select explicit mathematics and visual acceptance before reusing Lightroom's slider names.
