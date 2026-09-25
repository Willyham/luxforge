# White balance: the relative transform and the neutral picker

Status: frozen and implemented. The numerical study froze every constant and formula here ahead of implementation; TASK-014 then coded Temperature, Tint and the neutral picker against this document, unguessed. Production lives in `crates/lightwell-core/src/modules/basic/white_balance.rs` (the pointwise unit and the solver) and `modules/basic/mod.rs` (the parameters and the `neutral-sample` query), and is checked against the independent reference below within the pointwise contract's default `1e-6 + 1e-6 × |reference|`. See [Basic and histogram](basic-and-histogram.md) for the surrounding product and integration contract, and [product decisions](../decisions.md#basic-adjustments-and-histogram) for the picker default and rejection rules this document freezes.

Nothing here is a claim of equivalence to Lightroom, Adobe Camera Raw or any camera's Kelvin metadata. Temperature and Tint are **relative corrections of an already-rendered SDR JPEG**, not a reconstruction of scene illuminant. [Lightroom research](../research/lightroom/tone-and-color-tools.md#white-balance) found "no exact Adobe gain computation was recovered" and warned that "Kelvin and tint are user-facing descriptions, not directly portable RGB gains" — this document does not attempt to recover or match them.

## Summary

- **Transform:** von Kries chromatic adaptation in Bradford LMS space. Linear sRGB (D65) → CIE XYZ → Bradford LMS → per-channel gain → back. The gain is `LMS(D65) / LMS(target)`, where `target` is a chromaticity Temperature/Tint select relative to the true sRGB D65 white point.
- **Temperature** moves the target along a mired shift referenced to the Planckian locus (Kim et al. 2002 approximation); **Tint** moves it perpendicular to the locus in CIE 1960 `(u, v)`. Both are anchored so that `(0, 0)` reproduces the sRGB D65 white point exactly, not approximately.
- **Picker:** average a (≤ 5×5, edge-clipped) patch in linear sRGB, reject near-black/clipped/non-finite samples, then invert the same target map with a bounded 2-D Newton solver (tolerance `1e-6` in `(u, v)`, ≤ 64 iterations), reject an out-of-range result rather than clamp it.
- Verified in `crates/lightwell-reference/src/white_balance.rs` and `crates/lightwell-reference/tests/studies/white_balance.rs`; expected values are in [`fixtures/basic/white-balance-cases.json`](../../fixtures/basic/white-balance-cases.json).

## Why Bradford von Kries, not a simpler channel gain

The [parent design](basic-and-histogram.md#white-balance-and-neutral-picker) allows either a chromatic-adaptation transform or a simpler linear-sRGB channel-gain model, provided the choice is justified against fixtures. A plain per-channel gain (`R,G,B` each scaled independently) was tried first, informally, while shaping this study: it is a one-line formula but has no principled way to derive a *pair* of gains from a *single* chromaticity target without an extra free parameter (the third channel's gain is then either redundant or has to be fixed by a separate rule), and it processes each channel identically regardless of the eye's differing sensitivity in each cone response — a saturated colour's hue shifts differently than a chromatic-adaptation model shifts it. Bradford LMS is the standard, widely published cone-response space color-managed workflows already use for exactly this operation (display white-point adaptation), it has a natural closed relationship between "a target chromaticity" and "a diagonal gain," and — as the worked examples and the visual review below show — it produces plausible, monotone warm/cool and green/magenta behaviour on both a synthetic step-wedge and a real photo. It was kept.

## Colour spaces and constants

All colour math is IEEE 754 `f64` in the reference; production code runs the pointwise pipeline in `f32` per the [pointwise contract](basic-and-histogram.md#pointwise-foundation-and-exposure), with coefficients computed in `f64` as that contract requires.

### Forward matrices

Linear sRGB (D65) to CIE XYZ, the IEC 61966-2-1 primaries and white point:

```text
RGB_TO_XYZ = [ 0.4124564  0.3575761  0.1804375 ]
             [ 0.2126729  0.7151522  0.0721750 ]
             [ 0.0193339  0.1191920  0.9503041 ]
```

CIE XYZ to Bradford cone-response space, the 1985 Bradford matrix:

```text
XYZ_TO_LMS = [  0.8951000  0.2664000 -0.1614000 ]
             [ -0.7502000  1.7135000  0.0367000 ]
             [  0.0389000 -0.0685000  1.0296000 ]
```

### The inverse matrices must be computed, not copied

`RGB_TO_XYZ` and `XYZ_TO_LMS` are the only forward constants this document states. An implementation must compute their inverses at `f64` precision with the exact 3×3 adjugate/determinant formula (`mat3_inverse` in the reference), **not** substitute separately-published "XYZ to RGB" or "Bradford inverse" constants. This is not a style preference: two independently-rounded 7-digit matrices compose to identity only to about `1e-7` (measured while building this reference), which fails the `1e-12` identity requirement below outright. Computing the inverse of the *same* forward matrix the code already uses composes to identity to double-precision (about `1e-15`) regardless of how many digits the forward matrix itself was published with, because the arithmetic is then exactly self-cancelling up to floating-point rounding. For reference/sanity-checking only (an implementation must still compute these itself), the exact inverses of the matrices above are:

```text
XYZ_TO_RGB ≈ [  3.2404548360 -1.5371388501 -0.4985315469 ]
             [ -0.9692663899  1.8760109288  0.0415560823 ]
             [  0.0556434196 -0.2040258543  1.0572251625 ]

LMS_TO_XYZ ≈ [  0.9869929055 -0.1470542564  0.1599626517 ]
             [  0.4323052697  0.5183602715  0.0492912282 ]
             [ -0.0085286646  0.0400428217  0.9684866958 ]
```

### The anchor white point

`D65_X = 0.3127`, `D65_Y = 0.3290` — the IEC 61966-2-1 sRGB reference white, the same chromaticity `RGB_TO_XYZ` was built from. This is the point the transform reproduces **exactly** at Temperature 0 / Tint 0. It is deliberately kept separate from the "nominal CCT" below: D65 is a daylight illuminant, not a blackbody, and sits a small, well-known distance off the Planckian locus. Anchoring the transform at the Planckian point for "6504 K" instead of at the true D65 chromaticity was tried first and produced a mid-grey output about 2% off input at `(0, 0)` — nowhere near `1e-12` — which is why the anchor and the locus-direction reference below are two different, explicitly distinguished constants.

`D65_NOMINAL_CCT = 6504.0` — the commonly cited correlated colour temperature of the sRGB/D65 white point. Used only as the zero of the mired shift and the point at which the locus tangent direction is read; the transform never assumes this point's own `(x, y)` equals `D65_X`/`D65_Y`.

## Temperature: mired shift along the Planckian locus

```text
mired(t) = 1e6 / D65_NOMINAL_CCT − t · K_T          K_T = 1.0375 (mired per unit)
kelvin(t) = 1e6 / mired(t)
```

Positive Temperature **decreases** the mired value, i.e. raises the nominal correlated colour temperature (bluer). `K_T` is chosen so `t = +100` reaches a nominal `kelvin(100) ≈ 19999.4 K` (≈ 20000 K). Because mired and Kelvin are reciprocally related, a single linear-in-mired mapping anchored at D65's own mired (≈153.75) cannot be symmetric in Kelvin: `t = -100` reaches a nominal `kelvin(-100) ≈ 3883.5 K` (≈ 3900 K), not 2000 K. This asymmetry is inherent to the mired construction, not a bug; a camera or editor's own Temperature slider is not symmetric in Kelvin either. Both endpoints stay comfortably inside the Kim et al. approximation's valid domain (1667 K–25000 K, see below).

The **direction** Temperature moves the target chromaticity, not the exact Kelvin figure above, is what matters to the transform. That direction is read from the Planckian locus:

```text
planckian_locus_xy(kelvin):  (Kim, Kim, Tamatsu, Konig, Kim 2002 cubic approximation, valid 1667 K–25000 K)
  if kelvin <= 4000:
    x = -0.2661239e9/kelvin^3 - 0.2343589e6/kelvin^2 + 0.8776956e3/kelvin + 0.179910
  else:
    x = -3.0258469e9/kelvin^3 + 2.1070379e6/kelvin^2 + 0.2226347e3/kelvin + 0.240390
  if kelvin <= 2222:      y = -1.1063814 x^3 - 1.34811020 x^2 + 2.18555832 x - 0.20219683
  elif kelvin <= 4000:    y = -0.9549476 x^3 - 1.37418593 x^2 + 2.09137015 x - 0.16748867
  else:                   y =  3.0817580 x^3 - 5.87338670 x^2 + 3.75112997 x - 0.37001483
```

This is a **model of a blackbody radiator**, reproduced from its widely published coefficients (e.g. the "Planckian locus" article's Approximation section). It is used only to obtain a smooth, well-known warm/cool direction in chromaticity space — not a claim that any JPEG illuminant is a blackbody, and not a reproduction of any camera or Lightroom Kelvin value.

CIE 1931 `(x, y)` converts to CIE 1960 `(u, v)` by the standard formula `u = 4x / (-2x + 12y + 3)`, `v = 6y / (-2x + 12y + 3)` (and back by `x = 3u / (2u - 8v + 4)`, `y = 2v / (2u - 8v + 4)`).

## Tint: perpendicular offset in CIE 1960 (u, v)

```text
K_TINT = 0.00020   (uv units per Tint unit)
```

At the nominal Kelvin the temperature step selected, take the locus's unit tangent `(tu, tv)` by a 1 K forward difference, then rotate it -90° to `(tv, -tu)`. That is the perpendicular direction; Tint moves the target by `tint · K_TINT · (tv, -tu)`. The rotation sign is not a free choice stated without proof: it is the one that makes positive Tint raise R and B while lowering G (magenta), locked in and checked by `positive_tint_is_magenta` in the reference tests. The other rotation sign produces green for positive Tint, which would contradict the required "positive tint = more magenta" convention.

## Assembling the target chromaticity and the gains

```text
target_uv(t, tint):
  mired = 1e6/D65_NOMINAL_CCT - t·K_T ;  kelvin = 1e6/mired
  (u_p0, v_p0) = locus_uv(D65_NOMINAL_CCT)      # the locus point at the anchor Kelvin
  (u_p,  v_p)  = locus_uv(kelvin)               # the locus point at the shifted Kelvin
  (tu, tv) = tangent_unit(kelvin)               # locus unit tangent at the shifted Kelvin
  (u_d65, v_d65) = xy_to_uv(D65_X, D65_Y)
  u = u_d65 + (u_p - u_p0) + tint·K_TINT·tv
  v = v_d65 + (v_p - v_p0) + tint·K_TINT·(-tu)
```

At `t = 0, tint = 0`: `u_p == u_p0` and `v_p == v_p0` **by construction** — they are the same function evaluated at the same input twice — and the tint term is zero, so `target_uv(0, 0) == (u_d65, v_d65)` exactly, algebraically, independent of floating-point rounding anywhere else in the formula. This is why the anchor/locus separation above is load-bearing: the offsets are zero *by cancellation*, not by the Planckian curve happening to pass through D65.

```text
gains(t, tint):
  if t == 0 and tint == 0: return [1, 1, 1]                          # exact special case, see below
  (u, v) = target_uv(t, tint) ;  lms_target = LMS_of_uv(u, v)
  lms_d65 = LMS_of_uv(xy_to_uv(D65_X, D65_Y))
  return [ lms_d65[i] / lms_target[i] for i in L, M, S ]

apply(t, tint, rgb):
  if t == 0 and tint == 0: return rgb                                # exact special case
  xyz  = RGB_TO_XYZ · rgb ;  lms = XYZ_TO_LMS · xyz
  lms2 = lms * gains(t, tint)                     # componentwise
  xyz2 = LMS_TO_XYZ · lms2 ;  return XYZ_TO_RGB · xyz2
```

`LMS_of_uv(u, v)` converts back to `(x, y)`, builds `XYZ = (x/y, 1, (1-x-y)/y)` (unit luminance — chromaticity only, since a gain ratio is luminance-independent), and applies `XYZ_TO_LMS`.

### The `(0, 0)` identity: proved to `1e-12`, special-cased for exactness

Because `target_uv(0, 0)` is algebraically the anchor point, `gains(0, 0)` computed by the general formula is `[1, 1, 1]` to within floating-point rounding of the `uv → xy → XYZ → LMS` round trip, and the whole matrix sandwich composes to identity to about `1e-15` (using the *computed* inverses above) — the reference's `identity_at_zero_zero_is_exact_to_1e12` test checks this on seven representative colours including out-of-gamut ones, and `identity_on_all_256_greys_through_quantizer` checks it through the 8-bit round trip for every grey code. Both pass at roughly `1e-15`–`1e-16`, three orders of magnitude inside the `1e-12` requirement.

The implementation must **still** special-case `temperature == 0.0 && tint == 0.0` to skip the general formula and return the input unchanged (`gains` returns `[1.0, 1.0, 1.0]` literally). This is not needed for correctness — the general formula already proves close enough — but it is required so a neutral Basic layer's white-balance unit is bit-identical to no unit at all (the parent contract: "zero-valued Basic must not introduce round-trip changes") and costs nothing when neutral.

## Sign conventions

- **Positive Temperature warms** the image: R rises, B falls, relative to Temperature 0 at the same Tint. **Negative cools**: R falls, B rises. Proved monotone (strictly, at every integer step from -100 to 100, at five different Tint values) by `temperature_monotonic_over_full_range`.
- **Positive Tint is magenta**: R and B both rise, G falls. **Negative Tint is green**: R and B fall, G rises. Proved by `positive_tint_is_magenta` and, at every integer step, by `tint_monotonic_over_full_range` (using the proxy `(R+B)/2 - G`, which is monotone strictly increasing in Tint at every temperature tested).

These conventions match the ordinary photo-editor sense of "warmer"/"cooler" and "green"/"magenta," which is what the parent contract asks for ("matching the familiar UI convention"); they are not derived from or checked against Lightroom's own slider behaviour.

## Worked examples

Applying `apply(t, tint, [0.5, 0.5, 0.5])` (a mid-grey linear triple), full-precision output, tint or temperature held at 0:

| t | tint | R | G | B |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 0 | 0.500000 | 0.500000 | 0.500000 |
| +50 | 0 | 0.585155 | 0.493096 | 0.359174 |
| -50 | 0 | 0.405768 | 0.512416 | 0.703270 |
| +100 | 0 | 0.657367 | 0.491680 | 0.265694 |
| -100 | 0 | 0.304845 | 0.529113 | 0.996086 |
| 0 | +50 | 0.566099 | 0.475589 | 0.559511 |
| 0 | -50 | 0.432305 | 0.526576 | 0.448909 |
| 0 | +100 | 0.631024 | 0.452916 | 0.629875 |
| 0 | -100 | 0.362466 | 0.555823 | 0.404454 |

A saturated, near-blue patch `[0.05, 0.1, 0.8]` warmed hard (`t = -100`, i.e. cooled — negative temperature) pushes red **negative** (`R ≈ -0.0139`) while staying finite: this is preserved, not clamped, inside the unit, per the pointwise contract's "negative channels preserved, no clamp inside the unit." The same patch magenta-shifted (`tint = +100`) and warmed (`t = +100`) both stay finite and in a plausible range. `fixtures/basic/white-balance-cases.json`'s `transform_cases` carries 45 such cases (5 input colours × 9 parameter pairs) at full `f64` precision.

## The neutral picker

### Patch averaging and edge clipping

The picker samples up to 5×5 = 25 pixels centred on the picked input-stage pixel, at pixel centres, before the target Basic layer (so it sees the same stage the parent contract specifies). A pixel position outside `[0, width) × [0, height)` is dropped rather than clamped to the edge or wrapped — "clipped at the image edges" — so a corner sample can be as small as a single pixel (never zero, since the centre itself is always in bounds). Averaging is the arithmetic mean, per channel, of the pixels' **decoded linear-sRGB** values (not of the encoded bytes).

### Rejection

Checked in this order:

1. **Clipped**: any sampled pixel has any channel at code 0 or 255. Checked on the raw bytes, before decoding or averaging — a single blown highlight or crushed shadow pixel in the patch rejects the whole sample.
2. **Near-black**: the patch's mean relative luminance (`Y`, the same `0.2126729·R + 0.7151522·G + 0.0721750·B` row `RGB_TO_XYZ` already uses) is below `NEAR_BLACK_LUMINANCE = 0.02`. That is about sRGB code 39 for a neutral grey — dark enough that colour information is unreliable, and it also protects the solver's `XYZ → xy` division from a near-zero denominator.
3. **Non-finite**: any sampled or computed value is not finite. Unreachable from real 8-bit input by construction (`srgb_decode` of any byte is finite), but checked defensively since the same solver machinery may later be reused for a drafted linear buffer that isn't 8-bit-quantized.

### Solving

The patch average's own chromaticity — `xy` from `RGB_TO_XYZ · patch`, normalized by `X+Y+Z` — is the same kind of point `target_uv(t, tint)` produces. Solving for `(t, tint)` is therefore inverting `target_uv`: find the pair whose `target_uv` equals the patch's `(u, v)`. Applying that pair's gains to a patch whose own chromaticity is `target_uv(t, tint)` maps it back onto the D65 anchor by construction (the same identity the `(0, 0)` proof rests on, generalized) — this is what makes the picker "reproducible": the solver and the forward transform share one function, not two independently-tuned halves.

Bounded 2-D Newton's method, seeded at `(0, 0)`:

```text
h = 1e-3 (finite-difference step, parameter units)
repeat up to 64 times:
  r = target_uv(t, tint) - (patch_u, patch_v)
  if |r| < 1e-6: converged
  J = finite-difference Jacobian of target_uv at (t, tint), step h
  if J singular or non-finite: not converged
  (t, tint) -= J^-1 · r
  if t or tint non-finite: not converged
```

`SOLVER_TOLERANCE = 1e-6` (Euclidean distance in CIE 1960 `(u, v)`) and `SOLVER_MAX_ITERATIONS = 64` are the values the parent contract's own example suggested, adopted as frozen. In practice every representable correction converges in well under ten iterations: the round-trip grid below converges in 1–7 iterations everywhere on the required `-100..100` step-20 grid. A patch whose required correction is far outside the representable range (a strongly saturated, clearly non-neutral colour) either fails to converge within 64 iterations or lands on a wildly out-of-range value; both are rejected, never silently accepted.

**Rounding and its residual.** The converged continuous `(t, tint)` is rounded to the nearest integer (the parameters' step is 1, standard round-half-away-from-zero). Measured directly: `target_uv` moves by at most about `4×10⁻⁴` CIE 1960 `(u, v)` units per Temperature step (varying slightly over the range) and exactly `2×10⁻⁴` per Tint step, so rounding moves the target chromaticity by at most half of that — call it `2×10⁻⁴` combined, an order of magnitude below the solver's own `1e-6` convergence tolerance's neighbourhood of relevance and far below any visible chromaticity difference. In output terms: re-applying the rounded parameters to the original (unrounded) patch, across two thousand randomized `(t, tint)` pairs at a representative `0.18` relative luminance, left a worst-case channel spread (`max − min` of the three corrected channels) of `0.0018` in linear sRGB; **this design accepts a residual channel spread of up to `0.005`** as "the rounded parameters, re-applied, leave a residual the design accepts." No exception near the range ends was found beyond that bound at the continuous level — see the next section for why an 8-bit *patch* is a different story.

**Out-of-range is reported, never clamped.** After rounding, if either axis exceeds `-100..100`, the picker returns a structured `OutOfRange { temperature, value }` reason rather than clamping to ±100. `rejects_out_of_range_correction_without_clamping` constructs a patch whose true required correction is `temperature = 120` and checks the solver converges (the map is well-defined there) and is then rejected, not silently truncated.

### Why a 5×5 patch, not one pixel

One finding from building this reference is worth documenting explicitly, because it explains a design choice the parent contract states but does not derive: **a single 8-bit pixel is not enough signal to recover Tint reliably.** One Tint unit moves the target chromaticity by only `2×10⁻⁴` `(u, v)` units — comparable in magnitude to the chromaticity noise a single ±1-code rounding error introduces after decode. Solving from one quantized pixel (verified directly) can misplace the recovered Tint by several units, occasionally even for a plausible, in-range true value. Averaging the full patch — 25 independent samples, which in a real photograph carry ordinary pixel-to-pixel texture rather than being bit-identical — reduces that noise by roughly the square root of the sample count and recovers the true integer parameters exactly across the required round-trip grid (next section). The reference's synthetic test patches use an explicit ordered dither across their 25 samples for the same reason a real patch would show one naturally: it demonstrates the averaging benefit the 5×5 sample size exists to provide, rather than testing a patch no real picker click would ever produce (25 bit-identical pixels).

### Round-trip verification

For every `(t, tint)` on the `-100..100` step-20 grid (121 pairs): construct the target chromaticity via `target_uv(t, tint)`, build a synthetic 0.5-relative-luminance patch whose 25 dithered 8-bit samples average to that chromaticity, solve, and round. `solver_round_trips_over_grid_within_one_unit` checks every one of the 121 pairs recovers **exactly**, not merely within one unit — the stated one-unit bound left headroom that a dithered patch did not need. `fixtures/basic/white-balance-cases.json`'s `solver_cases` stores all 121 patches and their recovered parameters, plus a near-black rejection, a clipped-pixel rejection, an out-of-range (`temperature = 120`) rejection, and one edge-clipped 3×3 corner patch (9 of 25 samples) that still solves.

## Finite and gamut behaviour

- Negative and above-1.0 channel values are preserved between white balance and any later unit; nothing inside `apply` clamps. `gains_finite_and_positive_over_full_range` checks every gain stays finite and strictly positive on a `5`-step grid over the whole `-100..100 × -100..100` domain, and `saturated_patch_produces_predictable_finite_output` checks four saturated/out-of-gamut inputs at the four range extremes stay finite (one case is asserted negative-and-finite, matching the pointwise contract's stated behaviour).
- A non-finite **output** can only arise from a non-finite **input**, since gains are always finite and strictly positive across the entire representable range — consistent with the surrounding pointwise contract's rule that a non-finite result after any unit is an explicit `resource-limit` error, never a silently-clipped or NaN value.
- The neutral-picker solver treats a non-finite intermediate (only reachable while chasing a patch whose chromaticity is far enough from any representable correction that the Newton iterate has left the locus's well-behaved region) as immediate non-convergence rather than continuing to spend iteration budget on it.

## Visual review

Ignored tests (`crates/lightwell-reference/tests/studies/white_balance_visual.rs`, not run by `cargo test`, no images committed) render the reference at `(-100,0)`, `(-50,0)`, `(50,0)`, `(100,0)`, `(0,-100)`, `(0,100)` over `fixtures/s0/orientation-1.jpg` (the synthetic red/green/blue/gold quadrant fixture) and over a synthetic 64×64 grey checkerboard with a known cast (linear channels multiplied by `[1.25, 1.05, 0.65]`, simulating a warm, slightly green light source), writing PNGs to a temp directory. Findings from inspecting them directly:

- **Temperature -100** (cool) on the quadrant photo: the red quadrant shifts toward crimson/magenta, the green quadrant toward teal, the blue quadrant becomes more saturated blue, and the gold quadrant becomes dull olive-yellow — everything visibly pulled toward blue, including saturated colours, which is expected of a global chromatic-adaptation gain rather than a neutrals-only correction.
- **Temperature +100** (warm): the reverse — red stays red but slightly more orange, green becomes a punchier yellow-green, blue shifts toward indigo/violet (less pure, since its dominant channel is the one being suppressed), and gold becomes a warmer amber. Both directions are smooth and match "warm"/"cool" as an ordinary viewer would name them.
- **Tint -100** (green) and **+100** (magenta): the green and gold quadrants (the ones with the most to show on this axis) visibly shift toward pure green and toward orange/magenta respectively; the effect is present but visually gentler than the temperature extremes at the same magnitude, consistent with `K_TINT` being chosen smaller than a naive reading of "spans the same visual range" might suggest — it was picked, as stated above, to avoid an implausibly extreme cast at ±100, not to match Temperature's visual magnitude.
- **Synthetic cast recovery**: the checkerboard's original tiles render as a warm tan/beige, as the `[1.25, 1.05, 0.65]` gain implies. Sampling a 5×5 patch from one tile and solving recovered `(temperature, tint) = (-57, 20)` — a cooling, slightly-magenta correction, exactly the sign combination the cast's gains imply (R and G boosted, B suppressed is a warm-with-a-touch-of-green cast, and the recovered correction is cool-with-a-touch-of-magenta, its antidote). Applying that correction back to the whole checkerboard produces a visibly neutral grey image; the sampled patch's own centre pixel became exactly `(209, 209, 209)` — R = G = B to the code. The corrected image is not claimed to reproduce the *original* pre-cast image exactly (the gain-triple cast and the Bradford model are different constructions, as the design's summary already states no equivalence claim), only that the picker finds a correction that visibly neutralizes the sampled region, which is what the contract asks of it.

No exceptions to the expected warm/cool or green/magenta direction were found at any of the six settings on either image.

## What an implementer must reproduce exactly

1. The two forward matrices (`RGB_TO_XYZ`, `XYZ_TO_LMS`) verbatim, and their inverses computed programmatically via 3×3 adjugate/determinant — never hard-coded separately-published inverse constants.
2. `D65_X = 0.3127`, `D65_Y = 0.3290` as the anchor; `D65_NOMINAL_CCT = 6504.0` as the locus-direction reference point only.
3. `K_T = 1.0375` (mired/unit, subtracted), `K_TINT = 0.00020` (uv-units/unit), the Kim et al. 2002 locus polynomial exactly as given, and the locked tangent/perpendicular sign (`(tv, -tu)`).
4. The explicit `temperature == 0.0 && tint == 0.0` special case returning the input/identity unchanged, in addition to (not instead of) the general formula proving `1e-12`.
5. Patch averaging in linear sRGB over up to 25 edge-clipped samples; the three rejection checks in the stated order (clipped on raw bytes, near-black on `Y < 0.02`, non-finite); the bounded 2-D Newton solver with `1e-6`/`64` and round-to-nearest-step; out-of-range reported, never clamped.

## References

- [Basic and histogram](basic-and-histogram.md) — the surrounding product, module and integration contract; this document fulfils its ["White balance and neutral picker"](basic-and-histogram.md#white-balance-and-neutral-picker) section.
- [Product decisions: Basic adjustments and histogram](../decisions.md#basic-adjustments-and-histogram) — the picker default and rejection-rule decisions this document freezes.
- [Lightroom research: tone and colour tools](../research/lightroom/tone-and-color-tools.md#white-balance) — context on what "Temperature/Tint" describe in a familiar tool, with an explicit note that no Adobe gain computation was recovered.
- [Lightroom research: rendering and colour](../research/lightroom/rendering-and-color.md) — context on why a rendered JPEG's colour boundaries are not a single well-specified pipeline; supports treating this as a relative, self-contained correction rather than an attempted reconstruction.
- Reference implementation: [`crates/lightwell-reference/src/white_balance.rs`](../../crates/lightwell-reference/src/white_balance.rs), proved by [`crates/lightwell-reference/tests/studies/white_balance.rs`](../../crates/lightwell-reference/tests/studies/white_balance.rs) and visually reviewed by [`crates/lightwell-reference/tests/studies/white_balance_visual.rs`](../../crates/lightwell-reference/tests/studies/white_balance_visual.rs) (ignored, no committed images).
- Expected values: [`fixtures/basic/white-balance-cases.json`](../../fixtures/basic/white-balance-cases.json), generated by the reference and reloaded by `fixtures_match_reference_recomputation`.
