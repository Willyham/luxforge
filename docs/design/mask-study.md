# Mask coverage mathematics

Status: frozen. No production mask code exists: this document, the independent [`f64`
reference](../../crates/lightwell-core/tests/reference/mask.rs) and its
[proofs](../../crates/lightwell-core/tests/mask_reference.rs) are the complete specification the
mask units of the [masking design](masking.md) are checked against, answering its "What a mask is",
"Composition", "Mask space" and "Component kinds" sections. It settles that design's proposals P2
(the composition algebra), P3 (what a radial selects) and P4 (the brush's build-up rule, and the
Density control that follows from not having one) with the figures below.

Scope: this is a numerical/design task. It implements nothing, touches no crate's `src/`, and the
reference under `tests/` never runs in a release build or against a real image row, so the
[performance rules](../engineering/performance-rules.md) checklist applies to the transcription
tasks rather than to the files this one adds. **Not frozen here**, and named so nothing assumes
otherwise: the luminance and colour range metrics (their own study), the conservative bounds *pixel
rectangle* and `min_feature_px`, the brush's grid index and its occupancy cap, and the masked colour
and spatial blend itself. What is frozen is mask space, the legality of a stored distance, the
component composition algebra, the linear and radial coverage fields, and — added by TASK-018 — the
brush's capsule profile, its per-stroke maximum, its accumulation rules and its mask-space support
box.

No formula below claims Lightroom or darktable numeric equivalence. Where a choice differs from
Lightroom's — a radial selecting inside rather than outside — the difference is stated as
Lightwell's own, with its reason.

## Mask space

A component's geometry is stored in [content-stage](content-space-edits.md) coordinates: positions
as normalized fractions `x, y` of the stage's width and height, distances in **mask-space units**,
where one unit is the stage's *height*. Evaluation uses two spellings of one map, and they are not
interchangeable.

**A stored position** becomes mask space as

```text
u = x * (W / H)
v = y
```

with `W / H` computed once per compiled mask and reused, so the per-pixel path multiplies by one
stored ratio. `v` is the stored fraction unchanged, which is what makes the vertical axis the unit
axis and a stored distance a fraction of the height.

**A content-stage pixel centre** `(px, py)` becomes mask space as

```text
u = (px + 0.5) / H
v = (py + 0.5) / H
```

One mask-space unit is `H` pixels **on both axes**: the map from the pixel grid to mask space is a
single isotropic scale, so a circle in mask space is a circle in pixels at any aspect ratio, and a
stored radius means the same number of pixels across as down. Stepping one pixel moves exactly the
same mask-space distance on each axis (`mask_space_is_one_isotropic_scale_of_the_pixel_grid` asserts
that equality exactly, and the step against `1 / H` to within half an ulp of the two coordinates,
which is all a difference of quotients can promise).

| Stage | `W / H` | Mask space | Centre pixel's `(u, v)` | A stored radius of 0.2 |
| --- | --- | --- | --- | --- |
| 6000 x 4000 (landscape) | `1.5` | `(0, 1.5) x (0, 1)` | `(0.750125, 0.500125)` at `(3000, 2000)` | 1600 px across, 1600 px down |
| 4000 x 6000 (portrait) | `0.6666666666666666` | `(0, 0.6666…) x (0, 1)` | `(0.33341666666666665, 0.5000833333333333)` at `(2000, 3000)` | 2400 px across, 2400 px down |

The two rows are the same stored payload on a frame and its transpose:
`a_circle_is_a_circle_in_pixels_at_every_aspect_ratio` walks out from the centre on each axis and
counts covered pixels, measuring 1600/1600 and 2400/2400 against the nominal `0.4 * H`, rather than
arguing the property in prose.

**The two spellings agree far inside the frozen tolerance and not bit for bit.** Over 624 624
sampled pixel centres across both stages the largest disagreement is `2.220e-16` and 206 388 of them
differ in their last bits (`the_pixel_and_position_spellings_agree_within_tolerance_but_not_bit_for_bit`).
So each caller is frozen to one of them: a rasterizing pass and `render.sample` use the pixel
spelling, compiling a component's payload uses the position spelling. A production unit that mixes
them is not bit-identical to this reference even though it is within tolerance of it.

### Legal distances

A stored distance — the linear axis's mask-space length, a radial radius, later a brush radius — is
legal when it is finite and

```text
1e-4 <= distance <= 64
```

Both ends are load-bearing rather than decorative.

- **The floor replaces a runtime guard.** Every falloff divides by a stored distance, so a floor is
  what makes the divisor bounded instead of checked. `1e-4` mask-space units is below one pixel on
  every supported stage — 0.4 px at 4000 px of height, 1.6 px at the 16384 px per-side
  [admission limit](architecture.md#rendering-and-limits) — so nothing a gesture can draw is
  excluded by it, and it bounds every reciprocal the falloffs take by `1e4`.
- **The ceiling bounds the arithmetic.** A distance of `sqrt((W/H)² + 1)` from any point already
  covers the whole stage, so `64` covers every aspect ratio up to 63.99:1, far beyond anything the
  per-side limit can produce as a photograph. It keeps the largest representable squared ratio
  `(64 / 1e-4)² = 4.096e11`, which an `f32` accumulation of `(a/radius_x)² + (b/radius_y)²` carries
  with eleven orders of headroom below overflow.

The linear gradient's "zero-length axis is a validation error" is this same rule, not a separate
one: the axis's mask-space length *is* a distance, so it is compared as `l2 >= 1e-8` and no square
root is taken to reject it (`axis_is_legal`, exercised by
`a_degenerate_axis_is_rejected_by_the_frozen_rule`). The floor is a bound, not a representable
threshold — `0.4 + 1e-4 - 0.4` rounds a few ulps either side of `1e-4` — and nothing depends on
which side a payload exactly at the floor falls.

Stored precision is not this study's to fix, and nothing here depends on it: an `f32` normalized
coordinate already resolves `6e-8` of a side, which is 0.001 px on a 16384 px stage.

## Composition

Coverage is a field `m(u, v)` in `[0, 1]`, composed from the component list in order starting at
`m = 0`, then modified by the whole mask:

```text
m = 0
for component in components:
    c = the component's falloff at (u, v)
    c = 1 - c                       if component.invert
    m = max(m, c)                   if mode == add
    m = min(m, 1 - c)               if mode == subtract
    m = min(m, c)                   if mode == intersect
m = 1 - m                           if mask.invert
M = (amount / 100) * m
```

This is the Zadeh fuzzy-set algebra — union, complement, intersection — and it is **frozen**. An
empty component list composes to `m = 0`, so an empty mask selects nothing rather than everything
(`an_empty_component_list_selects_nothing`).

### What settles P2

The design's recorded default was Zadeh, for idempotence. The measured comparison confirms it, and
the margin is not close. The alternative implemented alongside it is the coherent product algebra —
the probabilistic sum `1 - (1 - m)(1 - c)` for add, `m(1 - c)` for subtract, `m * c` for intersect —
stated as a whole algebra rather than as the design's `m(1 - c)` alone, because a `max` union with a
product difference is neither family and has neither family's properties.

| Property the component list needs | Zadeh | Product |
| --- | --- | --- |
| Duplicating a component changes nothing | exact, bit for bit | up to `0.248209` of coverage |
| Reordering add components changes nothing | exact, bit for bit | up to `1.943e-16` |
| Composition is C¹ where two components cross | no (see below) | yes |

- **Idempotence.** `zadeh_composition_is_idempotent_bit_for_bit_in_every_mode` inserts a copy of
  each component of 300 randomized masks (1 to 6 components, every mode, random inversions) beside
  itself and asserts the composed coverage is bit-identical at every sampled point, in every mode.
  The product algebra fails this by up to `0.248209` over the same construction
  (`the_product_algebra_is_neither_idempotent_nor_exactly_order_independent`), and by `0.242687` on
  the ordinary three-component mask the figures below use. That failure is not a rounding artefact:
  it is what "subtract this brush twice" means under a product, and it makes duplicate, re-run and
  reorder into operations that change the picture.
- **Order independence.** `zadeh_add_components_are_order_independent_bit_for_bit` shuffles the
  add-only component lists of 300 randomized masks four times each and asserts bit-identical
  coverage: `max` is exact and commutative, so reordering is free. The product algebra is
  order-independent *mathematically* and not in `f64` — the accumulated product rounds differently —
  measuring `1.943e-16`. That is negligible in size and fatal in kind: `mask.reorder` could no
  longer promise that moving a component leaves the pixels alone.
- **Arithmetic cost.** `max` and `min` introduce no rounding at all
  (`the_algebras_own_rounding_is_orders_below_the_frozen_tolerance` asserts the result is one of the
  two operands, exactly). The only rounding the algebra itself contributes is one `1 - c` per
  inversion or subtraction, at most `5.551e-17`; at the design's 32-component limit that totals
  `1.776e-15`, nine orders below the frozen tolerance.

**The two algebras are not visually interchangeable, and the choice is made on properties rather
than on invisibility.** Over a 24 MP frame with three components — a vertical gradient, an added
soft radial and a subtracted soft radial, the shape of an ordinary local edit — the two fields
differ by up to `0.249982` coverage, `0.006113` on average; the overlay byte differs on 11.28% of
pixels, and a masked `+1 EV` on an 18% grey input differs by up to **12 output codes** on 9.03% of
pixels.

### What the frozen algebra costs

`max` and `min` are not differentiable where their arguments cross, so composed coverage is C⁰
there, not C¹. Measured on the same 24 MP field, horizontally, at `+1 EV` on 18% grey:

| Measurement | Zadeh | Product |
| --- | --- | --- |
| max adjacent-pixel `|ΔM|` | `2.273e-3` | `2.273e-3` |
| max second difference (slope change per pixel) | `2.218e-3` | `1.372e-5` |
| max adjacent-pixel `|Δcode|` | `1` | `1` |

The crease is real: at a crossing the slope changes by as much as the steeper component's whole
slope, within one pixel, and the product field's second difference is 162 times smaller. It is also
bounded: the largest step between adjacent pixels is one output code, so no edge appears — what a
person can see, where two soft components cross at full strength over a smooth sky, is a change of
gradient, not a band. That is the cost of the reorder, duplicate and re-run guarantees above, it is
recorded rather than smoothed over, and the remedies are already controls: a lower mask amount or
more feather on either component.

### A worked pixel

The 6000 x 4000 mask above, at pixel `(3800, 1800)` — `u = 0.950125`, `v = 0.450125` — where the
gradient is part way up its ramp and the subtracted radial is inside its feather band:

```text
c = [0.169506997, 0.000000000, 0.565287393]        (add, add, subtract)

Zadeh    m = max(0, 0.169506997)            = 0.169506997
             max(0.169506997, 0)            = 0.169506997
             min(0.169506997, 1 - 0.565287393 = 0.434712607)
                                            = 0.169506997
Product  m = 1 - (1 - 0)(1 - 0.169506997)   = 0.169506997
             1 - (1 - 0.169506997)(1 - 0)   = 0.169506997
             0.169506997 * 0.434712607      = 0.073686828
```

The subtraction removes 57% of the coverage under the product algebra and none of it under the
frozen one, because the pixel is only 17% covered to begin with and Zadeh's `min` leaves a value
already below the subtracting component's complement alone. At pixel `(1680, 2200)`, inside the
added radial, the same comparison is `0.977218724` against `0.977255913`: the add modes agree to
four decimals, and the whole disagreement between the algebras lives in `subtract`.

## The linear gradient

Payload `{x0, y0, x1, y1}`: the two ends of the axis as stored positions, `p0` at coverage 0 and
`p1` at coverage 1.

```text
(u0, v0) = position_uv(x0, y0)
(u1, v1) = position_uv(x1, y1)
du = u1 - u0
dv = v1 - v0
l2 = du*du + dv*dv
t  = clamp(((u - u0)*du + (v - v0)*dv) / l2, 0, 1)
c  = smooth(t)
```

`u0, v0, du, dv, l2` do not depend on the pixel and are computed once per compiled component. `l2`
is bounded below by the legality rule above, so the division needs no guard.

Three exactness properties hold and are proved rather than assumed:

- **`c = 0` exactly at `p0` and `c = 1` exactly at `p1`.** At `p1` the numerator is computed as
  `du*du + dv*dv` in the same order as `l2`, so the quotient is exactly `1.0` and `smooth(1.0)` is
  exactly `1.0`. `linear_coverage_is_exactly_zero_at_p0_and_exactly_one_at_p1` checks 2000
  randomized axes on both stages.
- **Constant perpendicular to the axis**, to `2e-16`, over 500 randomized axes sampled along and
  across (`linear_coverage_is_constant_perpendicular_to_the_axis`) — the projection is the only
  thing coverage depends on, so a gradient has no width.
- **Nondecreasing along the axis and flat outside it**, so the drag direction means what the
  gesture says and the clamp is exact at both ends
  (`linear_coverage_is_nondecreasing_along_the_axis_and_flat_outside_it`).

### The easing

`smooth(s) = s²(3 - 2s)` is frozen. It is the same falloff the delivered
[vignette](vignette-study.md#falloff-midpoint-and-feather) froze (written there as `3t² - 2t³`, the
same polynomial), so the editor has one falloff shape rather than two that differ for no reason a
person could name. Two alternatives were implemented and measured against it on one gradient down
4000 rows, at `+1 EV` on 18% grey:

| Easing | Max separation from `smooth` | In output codes | Max slope |
| --- | --- | --- | --- |
| `smooth(s) = s²(3 - 2s)` (frozen) | — | — | `1.5` |
| Raised cosine `(1 - cos(pi s))/2` | `0.0100085` at `s = 0.2786` | 1 code, 438 of 4000 rows | `1.5708` |
| Smootherstep `s³(6s² - 15s + 10)` | `0.0536656` at `s = 0.7236` | 3 codes, 1338 of 4000 rows | `1.875` |

`the_rejected_easings_differ_from_the_frozen_one_by_the_recorded_amounts` holds all three
separations and all three slopes to `1e-6`.

**What the difference looks like on a photograph.** Against the raised cosine, one output code over
a 4000 px gradient: the shoulder of the ramp sits a few hundred pixels higher or lower, and on a
smooth sky that is at or below the threshold of visibility. Against smootherstep, three codes and a
25% steeper middle: the same feather reads as a slightly harder edge. So the choice is not settled
by visibility, and it is not settled by curvature either — smootherstep is the only C² candidate,
but the slope discontinuity the composition algebra introduces at every component crossing
(`2.218e-3` per pixel, above) is two orders larger than anything the easing's own C¹ joins
contribute, so buying C² in the falloff while the algebra is C⁰ buys nothing. It is settled by the
three things that are decidable: one falloff shape in the product, the gentlest middle of the three,
and three flops with no per-pixel transcendental (the raised cosine needs a `cos` per pixel, which a
masked run cannot afford per the [performance rules](../engineering/performance-rules.md)).

## The radial gradient

Payload `{x, y, radius_x, radius_y, angle, feather}`: centre as a stored position, the two radii in
mask-space units, `angle` in degrees (`-180..180`), `feather` `0..100`.

```text
(cu, cv) = position_uv(x, y)
theta = angle * pi / 180
ca    = cos(theta)
sa    = sin(theta)
r0    = 1 - feather / 100
span  = 1 - r0
hard  = (span == 0)

du = u - cu
dv = v - cv
a  =  ca*du + sa*dv
b  = -sa*du + ca*dv
r  = sqrt((a / radius_x)^2 + (b / radius_y)^2)
c  = if hard { if r <= r0 { 1 } else { 0 } }
     else    { smooth(clamp((1 - r) / span, 0, 1)) }
```

Everything above the blank line is computed once per compiled component, including the one `cos`/`sin`
pair; the per-pixel path has no transcendental at all. `angle` rotates the ellipse clockwise as
drawn, because `v` increases down the frame.

**The clamped form is the frozen one, and it is bit-identical to the design's three-branch spelling**
(`1` for `r <= r0`, `0` for `r >= 1`, `smooth((1 - r)/span)` between), because `smooth` returns
exactly `0.0` and exactly `1.0` at the clamp's ends.
`the_clamped_radial_form_equals_the_branch_form_bit_for_bit` compares the two over 400 randomized
payloads on both stages at more than 100 000 points, by bit pattern rather than by tolerance. The
clamped form is frozen because it is one branch instead of three on the per-pixel path.

**`feather = 0` is an explicit hard edge, not a limit.** `hard` is set from the computed span being
exactly zero, following the vignette unit's own
[`hard_step` discipline](../../crates/lightwell-core/src/modules/vignette/unit.rs), so no division
by a vanishing span is ever evaluated. The test that this is a case and not an accident
(`feather_zero_is_a_hard_edge_and_no_vanishing_span_is_divided_by`) also covers the second route to
the same branch: a feather so small that `1 - feather/100` rounds to exactly `1.0` — `1e-16`, `1e-18`
— is the same hard edge, reached by rounding rather than by an equality against zero, and every
sampled coverage is exactly `0.0` or exactly `1.0`. A representable small feather (`0.01`) is the
smooth branch, checked in the same test so the boundary is not merely asserted from one side.

Two more properties are proved, over 2000 and 300 randomized payloads:

- **`c = 1` exactly at the centre** for every feather, and **`c = 0` exactly** just past the
  boundary on both rotated axes (`the_radial_selects_inside_exactly_at_the_centre_and_the_boundary`).
- **Nonincreasing outward along every ray**, so a radial is one connected selection with no rings
  (`radial_coverage_is_nonincreasing_outward_along_every_ray`).

### What settles P3

**Confirmed: a radial selects inside.** The design's default stands, and it is not only a taste
argument.

- The two readings are *exact* complements: a component's inversion and its coverage sum to exactly
  `1.0` at every sampled point, with no measured deviation at all
  (`a_components_inversion_is_the_exact_complement_of_its_coverage`), and inverting twice deviates by
  at most `5.551e-17`, exact for 55 906 of 58 800 sampled coverages. So the outside reading costs one
  toggle and no precision, and nothing numerical rides on which way round the default goes.
- What does ride on it is the bounds rectangle the design's processing contract depends on. An
  inside-selected radial has coverage zero outside its ellipse, so its conservative bounds is the
  axis-aligned box of that ellipse — for the added radial in the figures above, `0.30 x 0.22` mask
  units, 2400 x 1760 px, 17.6% of a 24 MP frame. Outside-selected, coverage is non-zero over the
  whole stage, so the bounds is the whole frame, every masked colour run is touched and every spatial
  tile is processed. Inside-by-default makes the ordinary case 5.7 times cheaper on that frame, and
  the expensive reading is the one a person has to ask for.
- It is also what drawing an ellipse around a face means, and the first component of a mask is always
  `add` over `m = 0`, so an outside-selected default would open every radial mask by selecting nearly
  the whole picture.

Lightroom's radial affects the outside until Invert is ticked. That difference is deliberate and is
stated in the design and in the user guide, not smoothed over.

## The brush

Payload `{strokes: [address, …]}`: the reserved [`strokes` field](../../crates/lightwell-core/src/path.rs) and
nothing else, holding the component's strokes **in order** as content addresses into the host's
stroke store. No coordinate is ever written into a component payload. Each referenced stroke carries
its own path and the brush it was drawn with:

```text
Stroke { points: [[x, y], …], size, feather, flow, erase }
```

`size` is the radius in mask-space units, so it is **a stored distance and takes the rule above**:
`1e-4 <= size <= 64`, refused by name rather than clamped. There is no second convention — the
floor is what bounds the divisor `band` below, exactly as it bounds a radius and an axis length —
and `smooth` is the same easing the gradients take.

### Per-stroke terms, computed once

```text
R      = size                            -- mask-space units
f      = feather / 100
band   = R * f                           -- the ramp's width
hard   = (band == 0)
amount = flow / 100
```

The stroke's positions become mask space through the **stored position** spelling, and each
consecutive pair becomes one segment:

```text
(ax, ay), (bx, by) = position_uv of the pair
ex   = bx - ax
ey   = by - ay
len2 = ex*ex + ey*ey
if len2 == 0:  ex = 0, ey = 0, len2 = 1
```

A stroke of `n >= 2` positions has `n - 1` segments. **A one-point stroke has the one degenerate
segment `A → A`**, and the normalization above is what makes "the distance to the point itself"
fall out of the same expression a real segment takes: with `ex = ey = 0` and `len2 = 1` the
projection evaluates `t = 0/1 = 0` and `q = A` exactly, so there is no branch on the per-pixel path
and no division by a vanishing length. It absorbs a repeated position too, which a stored path
cannot hold — [`decimate`](../../crates/lightwell-core/src/path.rs) drops repeats on the grid — but
which the mathematics must still be total on.

### The capsule profile

```text
wx = u - ax
wy = v - ay
t  = clamp((wx*ex + wy*ey) / len2, 0, 1)
qx = ax + t*ex
qy = ay + t*ey
dx = u - qx
dy = v - qy
d2 = dx*dx + dy*dy                                    -- per segment

d = sqrt(min over the stroke's segments of d2)
s = if hard { if d <= R { 1 } else { 0 } }
    else    { smooth(clamp((R - d) / band, 0, 1)) }
stroke = s * amount
```

**The minimum distance, not the maximum profile.** The design states one stroke's coverage as the
maximum over its segments of the capsule profile; the frozen spelling evaluates the profile once, at
the smallest distance. The two are the same number *bit for bit* — the profile is nonincreasing in
`d`, and `max` returns one of its operands exactly — and
`the_minimum_distance_form_equals_the_maximum_profile_form_bit_for_bit` compares them by bit pattern
over 48 000 points on both stages rather than arguing it. The frozen one is frozen because it takes
one `sqrt` and one `smooth` per stroke instead of one per segment, which at the 64-segments-per-pixel
cap below is the difference between one transcendental-free evaluation and 64 of them.

Squared distances are compared and the square root is taken once, of the minimum: `sqrt` is monotone
and correctly rounded, so the root of the smallest square is the smallest root, exactly.

**`feather = 0` is an explicit hard edge, not a limit**, taken from the computed `band` being exactly
zero, exactly as the radial's is and for the same reason: the divisor `band` is only reached on the
branch the compile has already proved is non-zero.
`feather_zero_is_a_hard_edge_and_no_vanishing_band_is_divided_by` covers the second route as well — a
feather so small that `R · f/100` underflows to exactly zero is the same edge, reached by rounding
rather than by an equality against the stored feather — and checks that a representable small feather
(`0.01`) is still the smooth branch, so the boundary is not asserted from one side only.

Two exactness properties carry the bounds rectangle and the grid index below, and are proved rather
than assumed (`the_profile_is_exactly_one_on_the_core_and_exactly_zero_past_the_radius`, 2000
randomized strokes):

- **Exactly `1.0` on the core** — at the centre for every feather, and at `d = R - band` on the
  feathered branch, because `smooth(1)` is exactly `1.0`.
- **Exactly `0.0` at and beyond `d = R`** on the feathered branch, because `smooth(0)` is exactly
  `0.0`; and exactly `0.0` beyond `d = R` on the hard branch, which is closed at `R` itself.

### Accumulation

Strokes accumulate inside the component **in stored order**, starting from `c = 0`:

```text
c = 0
for stroke in strokes:
    s = the stroke's coverage at (u, v)
    c = c * (1.0 - s)                    if stroke.erase
    c = c + (1.0 - c) * s                otherwise
```

That is the screen union across add strokes and the multiply-complement for erase strokes, and both
are **written in the one spelling that is an exact identity when a stroke contributes nothing**:
`c + (1 − c)·0` is `c` bit for bit and so is `c · (1 − 0)`, while the algebraically equal
`1 − (1 − c)(1 − s)` is not — it fails that identity on 195 983 of 200 000 log-sampled coverages, by
up to 100% of the coverage it was given, because a small `c` does not survive the round trip through
`1 − c` (`the_accumulation_is_an_exact_identity_at_zero_and_saturates_exactly_at_one`). Small
coverages are not a corner: the tail of a feather is most of a brush's area.

That exactness is the whole licence for the grid index below. A production unit may evaluate **only**
the strokes whose segments reach the pixel and still produce, bit for bit, the field the whole list
would have produced — `dropping_strokes_that_cover_nothing_is_bit_identical` states it as a property
of the fold rather than of any index. Both folds are exact at the other end too: `c + (1 − c)·1` is
exactly `1.0` and `c · (1 − 1)` is exactly `0.0`.

### What the rule is chosen for

**Maximum along a stroke, screen union across strokes.** One pass of the brush has one density
whatever the pointer's sampling rate: coverage depends on the path, not on how fast the hand moved or
on how many positions the desktop posted. Resampling a stroke's own polyline at eight times the
density moves its coverage by at most `2.759e-13`
(`a_strokes_density_does_not_depend_on_its_point_sampling`), which is seven orders below the frozen
tolerance and is the two expressions' rounding and nothing else. A doubled-back path — out and back
along the same line — covers *exactly* what the single pass covers, bit for bit over 20 000 sampled
points (`a_doubled_back_path_covers_exactly_what_the_single_pass_covers`), because the nearest
segment is the nearest segment however many times the path crosses it.

**A second pass is a second object and does build up**, which is the opposite of the component
algebra's idempotence and is deliberate:

| Flow | One pass reaches | Two passes reach |
| --- | --- | --- |
| 25 | `0.250000000` | `0.437500000` |
| 50 | `0.500000000` | `0.750000000` |
| 100 | `1.000000000` | `1.000000000` |

**Add strokes commute, to `2.220e-16`.** Permuting the strokes of 500 randomized add-only components
eight ways each, over 400 000 sampled points, leaves 99.46% of them bit-identical and moves the rest
by at most one ulp of `1.0` (`add_strokes_commute_under_the_frozen_rule`). The union is commutative
and associative in the reals; in `f64` the fold rounds differently, and that is recorded here rather
than claimed away. What it costs is nothing a picture can carry: through the masked blend at `+1 EV`
on 18% grey the deviation moves a channel by `3.997e-17` in linear light, against `3.922e-3` between
output codes, so no permutation of add strokes can change a byte. That is the difference from the
component algebra, where the same `1.943e-16` was called fatal in kind: `mask.reorder` promises that
moving a *component* leaves the pixels alone, while stroke order inside a component is not a promise
anyone makes about add strokes — it exists for the erase strokes, below.

**An erase stroke does not commute with an add.** Painting then erasing and erasing then painting
differ by up to `1.000000000` of coverage on the same two strokes
(`an_erase_stroke_does_not_commute_with_an_add`) — a whole edit, not a rounding artefact. That is why
the component stores its strokes in order and why the component list shows that order.

**Deleting one stroke is well defined.** The fold is over the stored order, so removing an entry from
the middle produces, bit for bit, the field the remaining strokes would have produced had the deleted
one never been made (`deleting_a_stroke_leaves_the_others_bit_identical`, 200 randomized components
with erase strokes among them). That is what makes `mask.delete-stroke` a forward edit rather than an
approximation of one.

### Density is not delivered

Lightroom's Density and Flow interact through a build-up model **along a single stroke**: coverage
accumulates from overlapping stamps, so it depends on the stamp spacing and therefore on the
resolution the stroke was stamped at. Nothing above has a stamp in it — one stroke is one geometric
path with one density — so there is no honest place to put a Density control that would mean what
Lightroom's means. It is named and left out rather than shipped meaning something else; the
[user guide](../user-guide.md) says so where a person would look for it. Flow is delivered and is
exactly what the rule above states: the per-stroke amount a single pass reaches.

### The conservative box

Coverage is exactly zero wherever `d > R` for every stroke, so a brush component's support is the
union of its **add** strokes' point boxes, each grown by that stroke's own radius. An erase stroke
contributes nothing to it — `c · (1 − s)` cannot raise `c` — and neither does a stroke whose flow is
exactly zero, because `s = profile · 0.0` is exactly `0.0` and the fold is an exact identity there.
`coverage_is_exactly_zero_outside_the_conservative_box` sweeps 600 randomized components on both
stages against it.

The box is over the stroke's *positions* rather than its segments because they are the same box: a
segment lies inside the box of its two endpoints.

### What this section does not settle

- **The grid index is a production concern, not a numerical one.** Its correctness is the exactness
  property above — a stroke the pixel cannot reach contributes exactly nothing — and the cell size,
  the occupancy cap and the refusal are the transcription task's to choose and to measure. Nothing
  about the field changes with them.
- **The pixel rectangle is not frozen here**, only the mask-space box. Turning a mask-space box into
  a conservative pixel rectangle is the same closed form the radial's already uses, with the same two
  slacks.
- **Per-stroke pressure is later work.** A per-position radius would change `R` from a stroke term
  into a segment term and nothing else in the frozen form, which is why the payload's point shape is
  the one it is.

## Frozen tolerance

**Production versus this reference: `1e-6 + 1e-6 * |reference|` in coverage units.** This is the
repository's default per-algorithm tolerance, taken for the same reason the
[vignette study](vignette-study.md#frozen-tolerance) takes it rather than Tone's looser `1e-5`: the
per-pixel path is a handful of arithmetic operations plus one `sqrt`, with no chained transcendental
stage — the `cos`/`sin` pair and every ratio that does not depend on the pixel are computed once per
compiled component, so a component's own error does not grow with the frame. The algebra adds
nothing measurable on top: `max` and `min` are exact, and 32 components contribute at most
`1.776e-15` through their `1 - c` steps.

**Quantization: at most one output code.** The mask overlay is `floor(255 * M + 0.5)` per display
cell. A coverage difference within the bound above can move that byte only where `M` lies within
`2e-6` of a code boundary, which is `5.1e-4` of the `1/255 = 3.92e-3` interval between codes, so the
difference is at most one code and only near a boundary. The same statement carries to a masked
pixel: a coverage difference of `2e-6` moves a blended channel by at most `2e-6 * |effect - input|`,
which cannot cross more than one code boundary.

No production `f32` implementation exists to measure against yet. `1e-6` is the considered starting
point for that implementation's own verification, to be tightened or loosened there against measured
results, exactly as the vignette study states for its own bound.

## Transcription

**A production unit transcribes the blocks above expression for expression and in the same order, so
it is bit-identical to this reference rather than merely within tolerance of it.** The reference's
module documentation says the same thing from the other side, as
[the vignette unit's does](../../crates/lightwell-core/src/modules/vignette/unit.rs). The rule that
makes this checkable: **any value that does not depend on the pixel may be precomputed, and no
arithmetic on the per-pixel path may be rewritten.** Precomputing changes nothing because the value
is identical; rewriting changes the last bits. Specifically forbidden, each of which is within
tolerance and not bit-identical:

- a precomputed reciprocal in place of a division — `a * (1/radius_x)`, `dot * (1/l2)`,
  `(1 - r) * (1/span)`, `(R - d) * (1/band)`, `dot * (1/len2)`;
- a fused multiply-add, or any reassociation of `du*du + dv*dv`, of the dot product, or of
  `ca*du + sa*dv`;
- folding `smooth` into a different but algebraically equal polynomial, including Horner's form;
- hoisting a component's `invert` into its falloff (as a sign flip or a swapped clamp) instead of the
  one `1 - c` the composition performs;
- collapsing `(amount / 100) * m` into a single scale applied earlier in the fold;
- spelling a brush's add as `1 - (1 - c) * (1 - s)` rather than `c + (1 - c) * s`, or its erase as
  `c - c * s` rather than `c * (1 - s)`: both are the same union in the reals and only the frozen
  spellings are exact identities at `s = 0`, which is what the grid index depends on;
- taking the square root per segment rather than once per stroke, or scaling a stroke's profile by
  `flow` before the accumulation's multiply rather than in the `s * amount` the profile ends with.

Permitted, and expected: hoisting `u0, v0, du, dv, l2`, `cu, cv, ca, sa, r0, span, hard`, a stroke's
`R, band, hard, amount` and its segments' `ax, ay, ex, ey, len2`, and `amount / 100` to compile time;
hoisting the row term of a component out of a row loop; skipping a span or tile whose coverage the
bounds rectangle proves is zero; and **skipping a stroke whose segments cannot reach the pixel**,
which the `s = 0` identity above makes bit-identical rather than merely close.

## Limitations and what this study does not settle

- **The position range is not settled.** The design stores `x, y` in `[0, 1]`, which forbids a
  gradient endpoint or a radial centre outside the frame — both of which the gesture naturally
  allows, and both of which are ordinary edits (a gradient whose coverage never reaches 0 inside the
  frame cannot be expressed with endpoints inside it). Every formula above is total on finite inputs
  and `legal_payloads_never_produce_non_finite_coverage` exercises coverage from `-2` to `4` in both
  axes, so **nothing in the frozen mathematics changes if the range is widened**; only the validation
  rule and the payload contract do. The recommendation is `[-1, 2]` — one stage extent of overshoot
  on each side, which keeps every stored coordinate bounded and every derived distance inside the
  ceiling above. This is a product and validation decision, not a numerical fact, so it is recorded
  here for the owner rather than decided. **Decided on 2026-09-23: widened to `[-1, 2]`**, as recommended; the
  design and the validation rule carry it and nothing in the mathematics above changed.
- **Composed coverage is C⁰ at component crossings**, measured above, and that is the price of the
  frozen algebra's exactness properties. If the crease turns out to matter in practice, the product
  algebra is the alternative and its cost is stated: reorder, duplicate and re-run stop being
  identities.
- **No fixture file is frozen.** Coverage is a pure function of a handful of numbers with no colour
  space or transfer function in it, so the transcription tasks test the production unit against this
  reference in process, pixel by pixel. A committed JSON oracle would be a third artefact to keep in
  step for no coverage the direct comparison does not already give. The vignette and mixer studies
  freeze fixtures because their inputs are pixels; this one's are payloads.
- **The range selections are outside this study.** They reuse the [mixer study](mixer-study.md)'s
  Oklab basis and are frozen by the [range study](range-study.md), not here. The brush *is* frozen
  here, above, and it reuses this study's `smooth` and its distance floor rather than introducing a
  second convention.

## Figures

Every measured number above comes from the reference's own tests. The 24 MP comparison is one
ignored test, because a 24 million pixel sweep does not belong in an ordinary run:

```sh
cargo test --release --locked --package lightwell-core --test mask_reference \
    -- --ignored --nocapture mask_study_figures
cargo test --release --locked --package lightwell-core --test mask_brush_reference \
    -- --ignored --nocapture brush_study_figures
```

The smaller figures reprint with `-- --nocapture` on the ordinary tests that assert them. Every
randomized comparison uses a fixed SplitMix64 seed, so the figures are reproducible on any machine.

## Files

| File | Purpose |
| --- | --- |
| `docs/design/mask-study.md` | This document. |
| [`crates/lightwell-core/tests/reference/mask.rs`](../../crates/lightwell-core/tests/reference/mask.rs) | The frozen `f64` reference: `Stage`, the distance rules, `smooth`, the linear and radial fields, both algebras, `coverage`, and the brush's segments, capsule profile, accumulation and support box. |
| [`crates/lightwell-core/tests/mask_reference.rs`](../../crates/lightwell-core/tests/mask_reference.rs) | The mask-space, composition and gradient proofs, and the ignored 24 MP figures test. |
| [`crates/lightwell-core/tests/mask_brush_reference.rs`](../../crates/lightwell-core/tests/mask_brush_reference.rs) | The brush proofs and measurements above, and the ignored `brush_study_figures` test. |
| [`crates/lightwell-core/tests/reference/mod.rs`](../../crates/lightwell-core/tests/reference/mod.rs) | Declares `pub mod mask;` beside the other studies' references. |

## References

- [Masking](masking.md) — the design this study freezes the numerics for, and whose proposals P2, P3 and P4 it settles.
- [Path primitives](../../crates/lightwell-core/src/path.rs) — the stored coordinate grid, the decimation contract and the content-addressed stroke store the brush's payload references.
- [Vignette study](vignette-study.md) — the positional coverage field, the `smooth` falloff, the explicit hard-step case and the tolerance this study follows.
- [Content-space edits](content-space-edits.md) — the content stage mask space is defined against.
- [Instant previews](instant-preview.md) — why normalized storage keeps a masked recipe proxy eligible.
- [Colour mixer mathematics](mixer-study.md) — the study format, and the Oklab basis the range selections will reuse.
- [Architecture](architecture.md) — the per-side and per-buffer limits the distance ceiling is reasoned against.
- [Performance rules](../engineering/performance-rules.md) — the point-query and per-pixel cost rules the frozen forms are chosen under.
