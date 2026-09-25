# Vignette study: the frozen mask and amount equations

Status: frozen and implemented by the `lightwell.vignette` module, which is checked against it. This
document, the independent [`f64` reference](../../crates/lightwell-reference/src/vignette.rs)
and the checked-in [oracle fixtures](../../fixtures/vignette/README.md) are the complete
specification a future `lightwell.vignette` unit is checked against. It answers the ["Vignette:
one positional unit"](presence-mixer-vignette.md#vignette-one-positional-unit) section of the
Presence, mixer and vignette design: a finish-stage layer evaluated after the geometry tail, in
output-stage pixel coordinates, so a later crop update recentres it exactly.

No formula below claims Lightroom or darktable numeric equivalence; the one style frozen here
(luminance-neutral, positive amount lightens toward white) is the Presence/mixer/vignette design's
own choice, not a claim about any other application's vignette.

## Coordinates: the output stage's normalised pixel centres

For a `width x height` output-stage frame and a zero-based pixel index `(x, y)` in `0..width x
0..height`:

```text
u = (x + 0.5 - width/2)  / (width/2)
v = (y + 0.5 - height/2) / (height/2)
```

`u, v` are in the open interval `(-1, 1)`: the pixel-centre convention (`+0.5`) means no finite
frame has a pixel whose `u` or `v` reaches exactly `-1` or `1` -- the corners only *approach*
`(+-1, +-1)` as the frame grows. This is not incidental; it is the reason the ["Corner saturation
condition"](#corner-saturation-condition) below exists at all, and it is proved in closed form
there rather than only observed.

## Shape radius: roundness

`s = roundness / 100`, in `[-1, 1]`.

**`s >= 0`** (ellipse toward circle):

```text
hd2 = (width/2)^2 + (height/2)^2                     -- squared half-diagonal
a   = (1 - s)/2 + s * (width/2)^2  / hd2
b   = (1 - s)/2 + s * (height/2)^2 / hd2
r^2 = a*u^2 + b*v^2
```

`a + b = 1` for every `s` in `[0, 1]`: the `(1-s)/2` terms sum to `1 - s` and the `s * (.)/hd2`
terms sum to `s * hd2/hd2 = s`, so the two sums add to exactly `1`. Substituting `u = v = 1` (the
continuous corner) therefore always gives `r^2 = a + b = 1`, regardless of `s` -- every shape in
this family, from the `s = 0` ellipse to the `s = 1` circle, passes through the same corner value.
`s = 0` gives `a = b = 1/2`, the plain ellipse `r^2 = (u^2 + v^2)/2`; `s = 1` gives `a =
(width/2)^2/hd2`, `b = (height/2)^2/hd2`, which is exactly `r` = (distance from the pixel centre
to the frame centre, in pixels) / (half-diagonal, in pixels) -- a true circle in pixel space, not
merely in normalised `(u, v)` space.

**`s < 0`** (ellipse toward rounded rectangle):

```text
p = 2 + 6 * (-s)                                      -- p runs from 2 (s=0) to 8 (s=-1)
r = ((|u|^p + |v|^p) / 2) ^ (1/p)
```

At `p = 2` this is exactly the `s = 0` ellipse above (the two families meet with no discontinuity
at `s = 0`). This form is homogeneous of degree 1 in `(u, v)` (`r(t*u, t*v) = t * r(u, v)` for `t
>= 0`, since `|t*u|^p = t^p |u|^p` and `((t^p X)/2)^{1/p} = t * (X/2)^{1/p}`), so it also reaches
exactly `r = 1` at the continuous corner for every `p`, the same corner-invariance the `s >= 0`
branch has by construction. `p = 8` is a **rounded rectangle, not a true rectangle** -- see
[Limitations](#limitations).

## Corner radius, in closed form

The discrete corner pixel `(0, 0)` (and, by the mirror/flip symmetry below, every one of the four
corner pixels) has coordinate magnitude:

```text
|u_corner| = (width - 1)  / width          -- substitute x=0 into the coordinate formula:
|v_corner| = (height - 1) / height            (0.5 - width/2)/(width/2) = 1/width - 1
```

so `r_corner(width, height, roundness) = shape_radius(u_corner, v_corner, ...)`, in the exact same
formula as above. `|u_corner| -> 1` as `width -> infinity`, but **`|u_corner| < 1` strictly for
every finite `width`** -- this is the precise version of "corners approach but do not reach
`(+-1, +-1)`". Measured at the study's three frozen fixture sizes:

| Frame | roundness=-100 | roundness=-50 | roundness=0 | roundness=50 | roundness=100 |
| --- | --- | --- | --- | --- | --- |
| 24x16 / 16x24 | 0.948317 | 0.948146 | 0.947974 | 0.949975 | 0.951972 |
| 20x20 | 0.95 | 0.95 | 0.95 | 0.95 | 0.95 |

(20x20 is exactly 0.95 for every roundness because `a = b = 1/2` for *every* `s` on a square
frame: `(width/2)^2/hd2 = (height/2)^2/hd2 = 1/2` when `width = height`, so `a = (1-s)/2 + s/2 =
1/2` regardless of `s`, collapsing the whole roundness family to the same ellipse on a square
canvas.) `reference::vignette::corner_radius` computes this directly; the internal test
`corner_radius_approaches_but_never_reaches_one` checks `r_corner < 1` at these and several other
sizes, including a synthetic 4000x3000 frame where it is far closer to 1.

## Falloff: midpoint and feather

`m = midpoint / 100` (`[0, 1]`), `f = feather / 100` (`[0, 1]`):

```text
r0 = m * (1 - f)
r1 = m + (1 - m) * f
t  = clamp((r - r0) / (r1 - r0), 0, 1)          -- only when r1 != r0
smoothstep(t) = 3*t^2 - 2*t^3
mask = smoothstep(t)                             if r1 != r0
     = 0 if r <= r0 else 1                        if r1 == r0   (hard step)
```

`r1 == r0` whenever `feather = 0` (both reduce to `r0 = r1 = m`), and, more generally, whenever
`m` and `f` are both `0` or both make `(1-f)` and `(1-m)` vanish together; the hard-step branch is
an explicit case, not a limit computed through division by a near-zero `(r1 - r0)`.

```text
mask(x, y, width, height, params) = falloff(shape_radius(pixel_uv(x, y, width, height), roundness), midpoint, feather)
```

The mask depends on nothing but `(x, y, width, height, params)` -- no other pixel, no `amount`.
This is what makes the post-crop recentring property hold: a crop changes `width`/`height` (and
the pixel's position within them), and the mask for the *new* stage is recomputed from those new
values alone, not inherited or shifted from the pre-crop mask.

## Corner saturation condition

**The literal claim "mask = 1 at every corner whenever feather < 100 and midpoint < 100" is not
universally true at the small sizes this study fixes fixtures at, and this is recorded honestly
rather than smoothed over.** The precise statement, derived directly from the two formulas above:

> The corner pixel's mask equals exactly `1` **if and only if** `r1 <= r_corner(width, height,
> roundness)`. When `r1 > r_corner`, the corner mask is `smoothstep(clamp((r_corner - r0)/(r1 -
> r0), 0, 1))`, strictly less than `1`.

Because `r_corner < 1` strictly (see above) while `r1` can be pushed arbitrarily close to `1` by
`midpoint` or `feather` alone approaching `100` (`r1 = m + (1-m)*f -> 1` as `f -> 1` for *any*
`m`, and as `m -> 1` for any `f`), there exist parameter combinations with both `midpoint < 100`
and `feather < 100` where the corner mask is measurably below `1` on a small frame. A concrete
example: `20x20`, roundness 0, midpoint 90, feather 90 gives `r1 = 0.99 > r_corner = 0.95`, and
the corner mask is `≈0.9942`, not `1` (worked in full below). `reference::vignette::mask`'s
`corner_mask_reaches_one_exactly_when_the_derived_saturation_condition_holds` test in
`studies/vignette.rs` asserts this "if and only if" directly, across every frozen aspect ratio,
every roundness and a `6x6` grid of `midpoint`/`feather` including this exact boundary case.

**The property is real and useful within a verified range.** For `midpoint <= 75` and `feather <=
75`, `r1 <= 0.75 + 0.25*0.75 = 0.9375`, strictly below every `r_corner` value in the table above
(minimum `0.947974`), so **corner mask = 1 is guaranteed** for that grid at every frozen aspect
ratio and every roundness. This is the grid `corner_mask_is_exactly_one_for_the_frozen_default_and_moderate_extreme_grid`
checks and `fixtures/vignette/mask-cases.json`'s `midpoint-feather/` cases record in full (`0, 25,
50, 75` safely inside the bound; `90, 99, 100` deliberately outside it, to make the transition
visible in the committed data rather than only in a test assertion). As the frame grows,
`r_corner -> 1`, so the safe grid widens toward the full `[0, 100)` range on a photograph-sized
canvas; the boundary is a small-canvas (and, by extension, small-fixture) effect, not a defect in
the frozen equations.

**Consequence for "corners go black at amount = -100".** Because `apply`'s negative branch is
`gain = 1 - |a| * mask`, the corner reaches exactly `0` gain (pure black) only when the corner
mask is exactly `1` -- i.e., under the same condition above. `corners_go_black_at_amount_minus_100_whenever_corner_mask_is_one`
states this precisely rather than as an unconditional claim.

**Midpoint = 100.** `r0 = 1*(1-f) = 1-f`, `r1 = 1 + 0*f = 1` regardless of `f`. At `feather = 0`:
`r0 = r1 = 1`, a hard step whose threshold is `1` itself -- since every discrete pixel's `r <
r_corner < 1 <= 1`, i.e. every real pixel satisfies `r <= r0`, **the entire frame reads mask `0`**
(the falloff "starts at the corner" and never actually transitions inside a finite frame). At
`feather > 0`: `r0 = 1-f < 1`, `r1 = 1`, so the transition runs from `1-f` to `1`; since the
discrete corner's `r_corner < 1 = r1`, the corner mask is `smoothstep(clamp((r_corner - r0)/(r1 -
r0), 0, 1))`, strictly below `1` again, by the same condition as above (this is just the `m = 1`
special case of the general rule, not a separate one). `fixtures/vignette/mask-cases.json`'s
`midpoint100/` cases record all three (`feather = 0, 50, 100`) at every frozen aspect ratio.

## Amount

`a = amount / 100`, in `[-1, 1]`.

**`a = 0`**: exact identity, for every mask value, including a mask outside `[0, 1]` (which the
frozen `mask` function never itself produces, but `apply` does not assume).

**`a < 0`** (darken), in linear light:

```text
gain    = 1 - |a| * mask
rgb_out = rgb_in * gain
```

At `a = -1` (amount = -100) and `mask = 1` exactly, `gain = 0`: the pixel goes to exact black.
This branch never clamps: an out-of-gamut input (negative or > 1, preserved from an earlier
stage) is scaled by the same `gain` and stays out-of-gamut in the same direction, matching the
project's "preserve values between colour operations, clamp once at the end" rule.

**`a > 0`** (lighten), per channel, in the Tone study's [analytically continued sRGB encoded
domain](basic-tone.md#working-tone-domain):

```text
E  = encode_ext(c)
E' = E + a * mask * (1 - E)          if E < 1
   = E                                otherwise (already at or above encoded white)
c' = decode_ext(E')
```

`encode_ext`/`decode_ext` are exactly Tone's `encode_srgb_extended`/`decode_srgb_extended`,
reused rather than re-derived a third time in this test tree (the sRGB OETF, analytically
continued to every finite real value, with no clamp -- see the Tone study for the full derivation
and why extrapolation, not clamping, is the right extension). Because `a` and `mask` are each
constrained to `[0, 1]` on this branch, `a * mask <= 1` always, so whenever `E < 1`:

```text
E' = E + a*mask*(1 - E) <= E + 1*(1 - E) = 1
```

with equality only when `a*mask = 1` exactly (`amount = 100` and `mask = 1`). **This proves "never
exceeds encoded 1" directly from the formula, for every input whose *own* encoded value starts
below `1`** -- it is not a blanket clamp. A channel whose input is *already* at or above encoded
white (an out-of-gamut value carried from an earlier stage) takes the `else` branch and is passed
through completely unchanged, staying exactly as far above white as it started; `apply` does not
bring it down to `1`. Both halves of this are tested directly:
`positive_amount_never_exceeds_encoded_one_for_a_below_white_input` (the proof, exercised on 2000
random below-white samples including negative, below-black channels) and
`a_channel_already_at_or_above_encoded_white_is_passed_through_unchanged` (the honest complement,
on several above-white samples including the literal random value, linear `1.5868993113623606`,
that an earlier, over-broad version of this test's property incorrectly flagged as a violation --
see the comment on that test for the full account).

## Symmetry and monotonicity

**Mirror/flip symmetry.** `pixel_uv(width - 1 - x, y, ...)` negates `u` (substitute and simplify:
`(width - 1 - x + 0.5 - width/2)/(width/2) = -(x + 0.5 - width/2)/(width/2)`) and leaves `v`
unchanged; `pixel_uv(x, height - 1 - y, ...)` negates `v` and leaves `u` unchanged. Every branch of
`shape_radius` depends on `u` and `v` only through `u^2`/`|u|^p` and `v^2`/`|v|^p`, both even in
sign, so `shape_radius(-u, v, ...) = shape_radius(u, -v, ...) = shape_radius(u, v, ...)` exactly,
and therefore `mask(width-1-x, y, ...) = mask(x, height-1-y, ...) = mask(x, y, ...)` for every
pixel, every frame size (odd or even) and every parameter combination. `mask_is_symmetric_under_horizontal_mirror_and_vertical_flip`
checks this exhaustively (every pixel, not a sample) on five frame sizes, including two odd-by-odd
sizes where a pixel sits exactly on the centre line on each axis.

**Nondecreasing in the shape radius.** `smoothstep` is nondecreasing on `[0, 1]` by construction
(`smoothstep'(t) = 6t(1-t) >= 0` there), and `t = clamp((r-r0)/(r1-r0), 0, 1)` is nondecreasing in
`r` for `r1 > r0` (the only case the smooth branch is used); the hard-step branch is trivially
nondecreasing (a single upward jump). So `mask` is a nondecreasing function of `r` alone. Combined
with the homogeneity properties in [Shape radius](#shape-radius-roundness) (`r` scales linearly
with `t` along any ray `(t*u, t*v)` from the centre, for both the `s>=0` and `s<0` branches), this
means **`mask` is nondecreasing along every ray outward from the frame's centre**, for every
parameter combination -- `mask_is_nondecreasing_as_a_function_of_shape_radius` samples many
directions on a large (`2000x1500`) frame to check this densely rather than only proving the two
lemmas separately.

**Corner is always the frame maximum.** `shape_radius` is coordinatewise nondecreasing in `|u|`
and `|v|` independently (both branches raise `|u|`/`|v|` to an even power or absolute-value
power), and the corner pixel maximises both `|u|` and `|v|` simultaneously among all pixels in the
frame. So the corner pixel's `r`, and hence its `mask` (mask is nondecreasing in `r`), is the
maximum over the whole frame -- **always true**, independent of whether that maximum happens to
equal `1` (which is the separate, conditional [corner saturation
condition](#corner-saturation-condition) above). `corner_mask_is_always_the_frame_maximum` checks
this on a coarse grid across every frozen aspect ratio and a range of parameters.

**Centre invariance, precisely.** The mask is `0` wherever `r <= r0` (the hard-floor of the
clamp), not only at one exact geometric centre pixel. At the default parameters (`midpoint = 50`,
`feather = 50`, so `r0 = 0.25`), every pixel near the centre of a `24x16`/`16x24`/`20x20` frame has
`r` well under `0.25` and reads mask `0` -- `mask_recentres_on_a_smaller_stage_after_a_crop` checks
this directly at the frozen sizes and at three larger "pre-crop" sizes, proving the mask is
recomputed independently rather than carried over. At the degenerate `midpoint = feather = 0`
(`r0 = 0`), only a pixel whose `r` is *exactly* `0` reads mask `0`; for an **odd-by-odd** frame one
pixel centre lands exactly there, but for an **even** frame (every one of this study's three
frozen aspect ratios) no pixel centre lands exactly at `r = 0`, so *every* pixel, including the
near-centre one, reads mask `1` under this specific combination --
`centre_invariance_is_r_less_or_equal_r0_not_an_exact_centre_pixel_guarantee` records both cases
explicitly rather than asserting a false universal "the centre pixel is always 0".

## Non-finite behaviour

This reference does not validate its inputs: given finite `width`, `height`, `params` and
`rgb_linear`, every arithmetic step (`+`, `-`, `*`, `/` by a `(r1 - r0)` guarded by the explicit
`r1 == r0` branch above, `sqrt` of a nonnegative sum of nonnegative terms, `powf` with a
nonnegative base) returns a finite result, matching the same reasoning [Tone's own non-finite
section](basic-tone.md#zero-luminance-and-non-finite-behaviour) gives for its own stages.
`finite_input_and_parameters_never_produce_non_finite_output` exercises this on 500 random
finite `(width, height, x, y, params, rgb)` combinations. Production refuses a non-finite input
before this stage is reached at all (the host's "non-finite after any unit is an explicit
render/sample error" rule); that refusal is production's concern, not this reference's, exactly
as `tone.rs`'s own module comment states for Tone.

## Worked examples

**Example 1 -- ellipse corner at default parameters (24x16).** `roundness = 0` gives `a = b =
0.5`. Corner `(0, 0)`: `u = (0 + 0.5 - 12)/12 = -0.958333...`, `v = (0.5 - 8)/8 = -0.9375`. `r^2 =
0.5*(-0.958333)^2 + 0.5*(-0.9375)^2 = 0.5*(0.918402... + 0.878906...) = 0.898654...`, `r =
0.947974...`. `midpoint = 50, feather = 50` give `r0 = 0.5*(1-0.5) = 0.25`, `r1 = 0.5 +
0.5*0.5 = 0.75`. `t = clamp((0.947974 - 0.25)/0.5, 0, 1) = clamp(1.395948, 0, 1) = 1`. `mask =
smoothstep(1) = 1`. (`hand_computed_example_one_ellipse_corner_at_defaults_is_one`.)

**Example 2 -- near-centre pixel, same frame and parameters.** Pixel `(11, 7)`, adjacent to the
geometric centre of the even-sized frame: `u = (11.5 - 12)/12 = -0.041667`, `v = (7.5 - 8)/8 =
-0.0625`. `r^2 = 0.5*(0.001736... + 0.003906...) = 0.002821...`, `r = 0.053115...`, well under
`r0 = 0.25`, so `t` clamps to `0` and `mask = 0` exactly.
(`hand_computed_example_two_ellipse_near_centre_at_defaults_is_zero`.)

**Example 3 -- the corner saturation boundary (20x20, midpoint=90, feather=90).** `r0 = 0.9*0.1 =
0.09`, `r1 = 0.9 + 0.1*0.9 = 0.99`. The discrete corner's `r = 0.95` (square frame, `r_corner =
0.95` for every roundness, per the table above) is less than `r1 = 0.99`, so `t = (0.95 -
0.09)/(0.99 - 0.09) = 0.86/0.9 = 0.955556`, and `mask = smoothstep(0.955556) = 3*0.955556^2 -
2*0.955556^3 = 0.994249657064...`, strictly below `1` -- the honest boundary case worked in full.
(`hand_computed_example_three_boundary_case_corner_mask_below_one`.)

**Example 4 -- amount application.** `rgb = [0.5, 0.3, 0.1]`. At `mask = 1, amount = -100`: `gain
= 1 - 1*1 = 0`, output `[0, 0, 0]`. At `mask = 1, amount = 100`: every channel's `E' = E + 1*(1-E)
= 1` exactly, decoding to `[1, 1, 1]`. At `mask = 0.5, amount = 50` (`a = 0.5`, `a*mask = 0.25`):
for `c = 0.5`, `E = encode_ext(0.5) = 1.055*0.5^{1/2.4} - 0.055 = 0.735356983...`, `E' = 0.735357 +
0.25*(1 - 0.735357) = 0.801518...`, `decode_ext(0.801518...) = 0.606403030961...`; the other two
channels follow the same steps to `0.430913342049...` and `0.225214369799...`.
(`hand_computed_example_four_amount_application_at_mask_one_and_mask_half`.)

## Frozen tolerance

Production versus this `f64` reference: **`1e-6 + 1e-6 * |reference|`** in linear float, and at
most one output code of rounding difference at quantization. This is the repository's default
per-algorithm tolerance, not the Tone study's loosened `1e-5 + 1e-5 * |reference|`: Tone earns its
looser bound by *composing* several `powf`/`exp`-bearing stages in sequence (the extended
encode/decode, the odds-bias curve, the logistic), each contributing its own `f32` rounding error
to the total. The vignette's positive-amount branch calls `encode_ext`/`decode_ext` **exactly
once each**, per channel -- the same shape as a single Basic exposure or colour operation, which
already use the tighter default -- and the mask geometry (`shape_radius`, `falloff`) is a handful
of arithmetic operations plus one `sqrt` or `powf` pair with no chained transcendental stages. No
production `f32` implementation exists yet to measure against; `1e-6` is the considered starting
point for that implementation's own verification (per the repository's stated policy for a
numerical task to freeze its own tolerance), to be tightened or loosened there against measured
results, not re-derived from first principles.

## Limitations

- **The rounded-rectangle shape (`roundness = -100`, `p = 8`) is a p-norm superellipse, not a true
  rectangle.** Its corners are rounded (the `p -> infinity` limit is the true rectangle; `p = 8`
  is a finite approximation the design chose over an unbounded `p`, which would need a separate
  hard-edge branch). This is a deliberate, stated choice, not an oversight.
- **Corner mask = 1 is a conditional guarantee on small canvases, not an unconditional one.** See
  [Corner saturation condition](#corner-saturation-condition): on the study's own `24x16`/`16x24`/
  `20x20` fixture sizes, `midpoint` and `feather` both need to stay at or below `75` (well inside
  the frozen `-100..100`/`0..100` ranges) for the corner to provably reach exactly `1`; pushing
  either close to `100` on a small frame can leave the corner mask measurably below `1`, and by
  the same mechanism leaves `amount = -100` short of exact black there. On a photograph-sized
  frame (thousands of pixels per side) `r_corner` is far closer to `1`, so the safe grid is far
  wider in practice; this limitation is a small-canvas effect, stated exactly rather than hidden
  behind a "sufficiently large image" hand-wave.
- **Positive amount above encoded white is unchanged, not clamped down.** An out-of-gamut input
  already at or above encoded white passes through the positive-amount branch untouched (see
  [Amount](#amount)); a vignette cannot be used to pull an already-blown highlight back toward
  white, only to lighten a channel that starts below it.
- **This is one global, position-dependent, per-pixel mapping with no per-pixel awareness of
  scene content** (no subject-aware or luminance-adaptive placement) -- exactly the "luminance
  positional" model the design's [proposals table](presence-mixer-vignette.md#proposals-with-recorded-defaults)
  chose over Lightroom's highlight-priority, paint-overlay and colour-priority styles, which are
  explicitly not selected here.

## Files

- `docs/design/vignette-study.md` -- this document.
- [`crates/lightwell-reference/src/vignette.rs`](../../crates/lightwell-reference/src/vignette.rs)
  -- the literal `f64` transcription of the equations above (`VignetteParams`, `mask`, `apply`,
  `vignette_pixel`, and the private `pixel_uv`/`shape_radius`/`smoothstep`/`falloff` helpers, plus
  the closed-form `corner_radius`).
- [`crates/lightwell-reference/src/tone.rs`](../../crates/lightwell-reference/src/tone.rs)
  -- unchanged except for making `encode_srgb_extended`/`decode_srgb_extended` `pub` so this
  study's positive-amount branch can reuse them instead of duplicating the sRGB OETF a third time
  in this test tree; no formula in that file changed.
- [`crates/lightwell-reference/src/lib.rs`](../../crates/lightwell-reference/src/lib.rs)
  -- unchanged; already declared `pub mod vignette;` ahead of this task.
- [`crates/lightwell-reference/tests/studies/vignette.rs`](../../crates/lightwell-reference/tests/studies/vignette.rs)
  -- the independent proofs (identity, mirror/flip symmetry, monotonicity along a ray, the corner
  saturation condition and its safe grid, centre invariance stated precisely, corners black at
  amount -100 under that same condition, positive amount never exceeding encoded white for a
  below-white input and passing an above-white input through unchanged, finiteness, four
  hand-computed worked examples), the oracle-fixture loaders/checkers and a `#[ignore]`d fixture
  regenerator.
- [`fixtures/vignette/README.md`](../../fixtures/vignette/README.md),
  [`fixtures/vignette/mask-cases.json`](../../fixtures/vignette/mask-cases.json) (202 cases),
  [`fixtures/vignette/amount-cases.json`](../../fixtures/vignette/amount-cases.json) (60 cases) --
  the committed oracle fixtures this study freezes.
