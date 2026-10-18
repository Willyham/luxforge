# White balance: the neutral picker

Status: implemented. This document specifies Basic's neutral picker: how it samples, averages, refuses and solves a patch. Basic's Temperature and Tint are per-channel shifts on the scene scale, and the picker solves through those same shifts ([Basic on the scene scale](basic-scene.md)). Production is `crates/luxforge-core/src/modules/basic/white_balance.rs`, with the query in `crates/luxforge-core/src/modules/basic/mod.rs`; the independent `f64` reference is `luxforge_reference::scene` (`crates/luxforge-reference/src/scene.rs`).

Temperature and Tint are relative corrections of the stage the Basic layer receives, not Kelvin values or a reconstruction of the scene's illuminant. The picker answers the slider values that make a patch neutral; it estimates no light source.

## Where the picker applies

Basic's picker is the module query `neutral-sample` (`query.neutral-sample`), bound to the canvas by a `sample-apply` interaction with the shortcut `W` and offered by the `picker` control at the end of the White balance group. A pick runs the query at the content pixel behind it and submits the answer to `set-basic` once; a refusal commits nothing. The query takes `x` and `y` in pixels of the stage the Basic layer receives (the content stage: the source after EXIF orientation plus any layer before it; `render.locate` maps a rendered pixel to it) and an optional `mask` target.

It serves a JPEG's global target and a mask on either kind. On a RAW photo's global target the White balance group's picker is the RAW development's sensor pick instead ([source-kind controls](source-controls.md#picker-and-shortcut)).

## The neutral picker

### The patch

The picker reads the stage before the target's Basic layer: the global layer, or the mask's own, or the place that layer would be inserted when there is none yet. It therefore evaluates before the layer's own correction, so picking the same patch twice gives the same answer whatever white balance is already set.

The patch is up to 5 × 5 pixel centres around the named pixel. A position outside the stage is dropped rather than clamped or wrapped, so a patch at a corner holds 3 × 3 samples and a patch is never empty for a centre inside the stage. A centre outside the stage is refused as `outside the stage`. The samples are that stage's 8-bit output codes, and the answer names the renderer that drew them as `renderer {record, reason}`, as `render.sample` does.

### Averaging

Each code is decoded to linear sRGB, and the patch's colour is the arithmetic mean of the decoded values per channel, never of the encoded codes.

### Rejection

A patch is refused, in this order:

1. **Clipped**: any sampled pixel has a channel at code 0 or 255. Checked on the codes before decoding or averaging, so one blown highlight or crushed shadow in the patch refuses it.
2. **Non-finite**: a decoded mean, or a value computed from it, is not finite.
3. **Near-black**: the mean's relative luminance, `0.2126729·R + 0.7151522·G + 0.0721750·B`, is below `0.02`, about code 39 on a neutral grey, where the colour is unreliable.

Every refusal's message begins with its reason (`clipped:`, `non-finite:`, `near-black:`, `out-of-range:`), so a client branches on the prefix without parsing prose.

### Solving

The picker finds the Temperature and Tint whose stops make the patch's three channels equal on the scene scale, which renders it neutral. For the mean `(R, G, B)`, take each channel's scene log `z_c = S(c) + stops_c(temperature, tint)` ([the scene scale](basic-scene.md#the-scene-scale)); on a RAW photo, whose patch is the development's scene-referred values, `S` is plain `log2`, as the unit applies plain gains there. The residual is `(z_R − z_G, z_B − z_G)`.

Newton's method solves it from neutral `(0, 0)`, with a central-difference Jacobian of step `1e-3` slider units, to a tolerance of `1e-9` stops on the larger residual, within 64 iterations. A singular or non-finite Jacobian, a non-finite iterate or one past ±200 is non-convergence. A mean with a channel at or below zero is refused as non-finite.

The converged values are rounded to the nearest whole step, halves away from zero. A rounded value past ±100 is refused as `out-of-range`, naming the axis and the value it would need, never clamped; non-convergence is refused as `out-of-range` too, since for the person picking both mean the patch needs a correction the sliders cannot represent.

### The answer

`{temperature, tint, patch: {x, y, width, height, pixels, mean_linear}, renderer}`: the rounded settings, the rectangle the patch covered, its codes, its linear mean and the renderer that drew it. The query is read-only, and two clients asking the same pixel get the same answer.

## Verification

- `crates/luxforge-core/src/modules/basic/white_balance.rs` tests the solver against the reference and the rejection order; `crates/luxforge-core/src/modules/basic/mod.rs` runs every full patch of the solver corpus through the query and holds its answer to `luxforge_reference::scene::neutral_settings`, rounded, or to an `out-of-range` refusal where the reference finds none within range.
- `crates/luxforge-core/tests/basic/white_balance.rs` drives the picker from an independent JSON client: discovery, a pick applied through `set-basic`, a mask's pick reading the stage with the global white balance, edge-clipped patches and every refusal committing nothing, `render.locate` coordinates and two clients agreeing. It also renders every colour input of the corpus through a real Basic layer against `luxforge_reference::scene::apply`.
- The corpus is [`fixtures/basic/white-balance-cases.json`](../../fixtures/basic/README.md#white-balance-corpus-white-balance-casesjson): colour inputs at Temperature and Tint pairs, and 8-bit patches with their refusal cases, checked against the scene reference.

## References

- [Basic and histogram](basic-and-histogram.md#white-balance-and-neutral-picker): the surrounding module and integration contract.
- [Basic on the scene scale](basic-scene.md): the unit and the stops the picker solves through.
- [Product decisions: Basic adjustments and histogram](../decisions.md#basic-adjustments-and-histogram): the picker's patch and rejection defaults.
