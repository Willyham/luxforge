# Range-selection mathematics

Status: frozen, and transcribed. This document, the independent [`f64`
reference](../../crates/lightwell-core/tests/reference/range.rs) and its
[proofs](../../crates/lightwell-core/tests/range_reference.rs) are the complete specification the
luminance-range and colour-range units of the [masking design](masking.md) are checked against,
answering its [phase-D component section](masking.md#luminance-range-and-colour-range-phase-d). It
is the value-based companion to the [mask study](mask-study.md), which froze the position-based
components; that study named the range metrics as **not** frozen by it, and this one freezes them.
The production transcription is `crates/lightwell-core/src/mask/range.rs`, checked against the
reference **bit for bit** by `crates/lightwell-core/tests/mask_range.rs`.

What is frozen here: the luminance axis, the band and its shoulders, the Oklab colour metric, the
multi-sample combination, the refine mapping and the tolerance. **Not frozen here**, and named so
nothing assumes otherwise: the brush stroke's capsule profile and its accumulation rule, the
colour-constrained brush (which will reuse this study's metric but has its own gesture and its own
accumulation), and the masked colour and spatial blend itself. The reference under `tests/` never
runs in a release build or against a real image row, so the
[performance rules](../engineering/performance-rules.md) checklist applies to the production unit
rather than to it.

Three reuses, so the editor keeps one of each rather than two that differ for no reason a person
could name: the falloff is `smooth(s) = s²(3 − 2s)`, the shape the
[vignette](vignette-study.md#falloff-midpoint-and-feather) froze and the
[mask study](mask-study.md#the-easing) reuses; the luminance is the delivered Basic layer's Rec. 709
luminance and its analytically continued sRGB OETF from [the tone study](basic-tone.md); the colour
space is the Oklab the [colour study](basic-colour.md#conversion) accepted and the
[mixer study](mixer-study.md) reuses unchanged. No second colour space is introduced.

No formula below claims Lightroom or darktable numeric equivalence. The
[Lightroom](../research/lightroom/geometry-masks-and-retouching.md) and
[darktable](../research/darktable/geometry-masks-and-retouching.md) research supplies the names and
the behavioural context — darktable's "parametric mask" is the same idea, evaluated in its own
blending domain — and nothing here is derived from either.

## What a range selection reads

Both components read the **pixel value the operation they modulate receives**, in linear sRGB, as
the design states. That costs nothing during a render — the value is in the row being processed —
and costs one existing `O(layers)` sample at a point query. It also has four consequences that the
[compiled-mask contract](masking.md#the-compiled-mask) does not have today, which are escalated as
[P12 and P13](#proposals) rather than decided here:

- coverage is no longer a function of position alone, so `evaluate(x, y)` cannot answer it;
- a value-based component has no spatial support, so its conservative bounds rectangle is the whole
  stage and no colour span or spatial tile can be skipped for it;
- what it selects moves when a layer is reordered, because the operation's input changes;
- at proxy scale it reads a downscaled pixel, and the coverage of an average is not the average of
  the coverages. Measured below.

## The luminance band

### The axis

```text
Y = 0.2126*r + 0.7152*g + 0.0722*b        -- Rec. 709 on linear sRGB, in that order
e = encode_srgb_extended(Y)               -- the analytically continued sRGB OETF
```

`e` is the axis. It is **the domain the histogram bins**: the histogram describes the rendered SDR
sRGB output, and the encoded value is exactly what its quantizer rounds to a byte, so the number on
the slider is the number a person reads off the histogram's horizontal axis.
`the_luminance_axis_is_the_domain_the_histogram_bins` checks that for all 256 grey codes the axis
returns to the same bin the output quantizer produces, and that the axis and the delivered
quantizer agree on each.

The slider's `0..100` is `100 · e`, so one slider unit is `2.55` output codes and the conversion to
a code is exact and one line. The payload is **compiled onto the encoded axis** (`/100` once per
component), so the per-pixel path never multiplies a pixel's own value by 100.

The axis is **not clamped**. An earlier unit in the same colour run may legitimately hand on a
linear value below 0 or above 1, and the continued OETF is defined and strictly increasing on the
whole real line, so such a pixel is treated as darker than black or brighter than white rather than
folded back onto the axis (`the_axis_is_monotone_and_finite_outside_the_gamut`).

**Why this luminance and not another.** Two alternatives were implemented and measured over a dense
sweep of the sRGB cube, as the largest separation from the frozen axis on the `0..1` axis:

| Axis | Max separation | On sRGB red `(175, 54, 60)` |
| --- | --- | --- |
| Rec. 709 on linear light, then encoded (frozen) | — | `38.23` |
| Rec. 709 weights on the already-encoded channels | `0.277276` | `31.43` |
| `max(r, g, b)`, encoded | `0.683838` | `68.63` |

`the_rejected_luminance_axes_differ_from_the_frozen_one_by_the_recorded_amounts` holds all three.
The frozen one is the luminance the delivered [tone unit](basic-and-histogram.md#tone) already
computes, so the editor has one definition of luminance; the max-channel axis is the one the
delivered clipping predicate uses, and adopting it would make the band track clipping rather than
tone; luma on encoded channels is a different quantity with no unit in the product behind it.

### The band

Payload `{low, low_feather, high, high_feather}`, all on the slider's `0..100` axis, with
`0 ≤ low ≤ high ≤ 100`.

```text
-- once per compiled component
lo      = low  / 100
hi      = high / 100
lo_f    = low_feather  / 100
hi_f    = high_feather / 100

-- per pixel
Y    = 0.2126*r + 0.7152*g + 0.0722*b
e    = encode_srgb_extended(Y)
rise = if lo_f == 0 { if e >= lo { 1 } else { 0 } }
       else         { smooth(clamp((e - lo) / lo_f + 1, 0, 1)) }
fall = if hi_f == 0 { if e <= hi { 1 } else { 0 } }
       else         { smooth(clamp((hi - e) / hi_f + 1, 0, 1)) }
c    = min(rise, fall)
```

`min` is not an arbitrary choice of intersection: it is the
[frozen composition algebra](mask-study.md#composition)'s own intersection, and the band *is* an
intersection — brighter than the low edge **and** darker than the high edge. It introduces no
rounding at all, and it is what makes overlapping shoulders on a narrow band behave: for any
`low ≤ e ≤ high` both ramps are exactly 1, whatever the feathers.

**The `+ 1` spelling is frozen rather than the algebraically equal `(e − (lo − lo_f)) / lo_f`,**
because it is exact at both ends: at `e = lo` the numerator is exactly `0`, so the ratio is exactly
`1` and `smooth(1)` is exactly `1`; at `e = lo − lo_f` the ratio is exactly `−1 + 1 = 0`. The other
spelling rounds `lo − lo_f` first and lands a few ulps either side of both.
`the_band_is_exactly_one_inside_and_exactly_zero_outside` checks 500 randomized payloads: coverage
is exactly `1.0` at both band edges and anywhere between them, and exactly `0.0` past either
shoulder. At the shoulder's *rounded* outer end the caller's own `lo − lo_f` is itself a rounded
value, so coverage there is below `1e-28` rather than exactly zero; the tests assert that bound
rather than claiming an exactness the arithmetic does not have.

**The clamped form is bit-identical to the design's four-branch spelling**
(`0` below the shoulder, `1` inside the band, `smooth` between, plus the hard case), because
`smooth` returns exactly `0.0` and exactly `1.0` at the clamp's ends.
`the_clamped_band_form_equals_the_branch_form_bit_for_bit` compares them over 400 randomized
payloads at more than 120 000 points, by bit pattern. The clamped form is frozen because it is one
branch instead of four on the per-pixel path.

Two more properties are proved: coverage is nondecreasing up to `low` and nonincreasing after
`high`, so a band is one connected selection with no rings
(`the_band_rises_then_falls_and_never_leaves_the_unit_interval`); and every legal payload on every
finite input, including linear values from `−2` to `4`, gives a finite coverage in `[0, 1]`
(`legal_payloads_never_produce_non_finite_coverage`).

### The shoulder floor

A shoulder is legal when it is finite and

```text
feather == 0   or   1 <= feather <= 100
```

`feather = 0` is an explicit hard edge, taken on an exact zero and only there, following the
[radial's `hard_step` discipline](mask-study.md#the-radial-gradient)
(`a_zero_feather_is_a_hard_edge_and_no_vanishing_shoulder_is_divided_by`). Unlike the radial's
feather there is no second route to the hard branch by rounding, because everything between `0` and
`1` is refused.

**The floor is load-bearing, and it is the one place this study differs in kind from the mask
study.** A position-based component's input `(u, v)` is computed the same way by the reference and
by production; a value-based component's input is a *pixel*, computed in `f32` by production and in
`f64` here, so the shoulder's slope multiplies that difference straight into the coverage. The slope
is exactly `1.5 / feather` in the axis's own units (`the_shoulder_slope_is_bounded_by_the_frozen_floor`
measures it against that bound for six widths):

| Feather | max `|dc/de|` per unit of the axis | Coverage per output code of input |
| --- | --- | --- |
| `1` (the floor) | `150.0` | `0.588` |
| `2` | `75.0` | `0.294` |
| `5` | `30.0` | `0.118` |
| `15` | `10.0` | `0.039` |
| `50` | `3.0` | `0.012` |
| `100` | `1.5` | `0.006` |

So the floor bounds every reciprocal the band takes by `100`, which is what makes the frozen
tolerance below derivable. It excludes nothing a gesture can ask for: one unit is `2.55` output
codes, which is the smallest nonzero step a `0..100` feather slider offers, and anything narrower is
a hard edge asked for indirectly — which has its own exact branch.

That table is also the honest reading of what a narrow shoulder *does*: at the floor, two adjacent
output codes of input differ by more than half of full coverage. That is not a rounding artefact,
it is the selection being nearly binary, and it is what makes the noise measurement below come out
the way it does.

## The colour range

Payload `{samples: [[r, g, b], …], refine}`: up to **five** sampled colours as linear sRGB triples
in the domain of the operation the mask modulates, and one refine slider in `0..100`.

```text
-- once per compiled component
(a_k, b_k) = the Oklab a and b of each sample          -- to_oklab, unchanged
radius     = RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX)^(refine / 100)

-- per pixel
lab = to_oklab(rgb)
d2  = +infinity
for (a_k, b_k):
    da = lab.a - a_k
    db = lab.b - b_k
    d2 = min(d2, da*da + db*db)
d   = sqrt(d2)
r   = d / radius
c   = smooth(clamp((1 - r) / SPAN, 0, 1))
```

| Constant | Value | Why |
| --- | --- | --- |
| `MAX_SAMPLES` | `5` | A product bound — the panel shows five swatches — not a numerical one. The fold is `min`, which is exact and associative, so nothing in the mathematics changes if it is raised; the per-pixel cost of a sample is four flops beside one Oklab conversion. |
| `RADIUS_MAX` | `0.25` | 41% of the largest chromaticity distance between two in-gamut sRGB colours (`0.6165`, blue to yellow) and 78% of the largest Oklab chroma on the gamut surface (`0.3225`). The loosest setting takes a broad family and still leaves the opposite side of the wheel out. |
| `RADIUS_MIN` | `0.005` | Below the measured chromaticity spread of a single surface under half a stop of shading, so the tightest setting selects one flat patch and little else. It bounds the per-pixel reciprocal by `200`. |
| `PLATEAU` | `0.5` | Coverage is exactly `1` out to `PLATEAU · radius`. Sized so that at the default refine the plateau covers an ordinary surface's own chromaticity spread across a stop of shading: that spread measures `0.0157` for skin, `0.0159` for a sky and `0.0173` for foliage, against a plateau of `0.0177`. |
| `SPAN` | `0.5` | `1 − PLATEAU`. It is exactly a power of two, so this is the one division the [transcription rule](#transcription) allows to be written as a multiply: `x / 0.5` and `x * 2.0` are the same `f64`. |

Coverage is exactly `1.0` at a sampled colour and out to the plateau's edge, and exactly `0.0` past
the radius (`colour_coverage_is_exact_at_the_sample_at_the_plateau_and_at_the_radius`; at the radius
itself the probe's own distance is a rounded value, so coverage there is below `1e-28`). It is
nonincreasing with distance from the nearest sample, so a colour range is one connected selection in
colour space (`colour_coverage_is_nonincreasing_with_distance_from_the_nearest_sample`). An
**unsampled** colour range leaves `d2` at `+infinity`, so coverage is exactly `0`: it selects
nothing, exactly as an empty mask does.

### The metric is the chromaticity plane, and `L` does not appear

This is the study's largest choice and it is settled by measurement, not by taste. Three metrics
were implemented: the frozen `sqrt(da² + db²)`, and `sqrt((w·dL)² + da² + db²)` at `w = 0.5` and
`w = 1` (a full Oklab distance). Each is measured two ways on the same surface: how far a **stop of
shading** moves it (gains `0.5`, `0.7`, `1.4`, `2.0`, which change lightness and not chromaticity),
and how far away the nearest **intruder** a person would not want selected sits. Their ratio — the
margin — is the whole question, because a margin below 1 means *no radius exists* that takes the
surface and leaves the intruder.

| Surface / intruder | Metric | Shading | Separation | Margin |
| --- | --- | --- | --- | --- |
| blue sky / blue flower | chromaticity | `0.0159` | `0.0378` | **2.38** |
| blue sky / blue flower | weighted 0.5 | `0.0763` | `0.0449` | 0.59 |
| blue sky / blue flower | weighted 1.0 | `0.1501` | `0.0615` | 0.41 |
| light skin / orange | chromaticity | `0.0157` | `0.0845` | **5.39** |
| light skin / orange | weighted 0.5 | `0.0935` | `0.0862` | 0.92 |
| light skin / orange | weighted 1.0 | `0.1850` | `0.0911` | 0.49 |
| foliage / green | chromaticity | `0.0173` | `0.0712` | **4.11** |
| foliage / green | weighted 0.5 | `0.0676` | `0.0858` | 1.27 |
| foliage / green | weighted 1.0 | `0.1319` | `0.1193` | 0.90 |

The surfaces are the measured sRGB renderings of the 24-patch reflective colour chart, used because
they are real surface colours rather than numbers chosen to make a point.
`the_metric_is_blind_to_lightness_and_that_is_what_holds_a_shaded_surface` asserts that the frozen
metric's margin is above 2 on all three, and that a full Oklab distance puts a sky that has fallen
off by one stop **further from the sampled sky than a different blue flower is** — which is the
failure in one sentence. Any positive weight erodes the margin fast, because within one surface
lightness varies between eight and twelve times more than chromaticity does.

The second reason is architectural and is why the cost below is acceptable: **lightness already has
its own component.** A luminance range and a colour range intersect exactly under the frozen
algebra, so building lightness into the colour metric would duplicate the other component and make
the pair non-orthogonal. The price is real and is stated under [limits](#what-these-selections-do-not-select).

### The multi-sample combination

Folding the nearest sample by `min` on the **squared** distance and taking one square root is
bit-identical to taking the maximum of the per-sample falloffs, because the falloff is nonincreasing
in `d` and both spellings evaluate it on the same `f64`.
`the_nearest_sample_form_equals_the_max_of_falloffs_bit_for_bit` asserts that over 300 randomized
payloads at 60 000 points, by bit pattern. The frozen form is the cheap one: one `sqrt` per pixel
rather than one per sample.

The combination is therefore the same union the [mask study](mask-study.md#what-settles-p2) froze
for the component list, and it inherits the same properties, proved the same way by
`duplicating_and_reordering_samples_changes_nothing_bit_for_bit`:

| Property | `min` / `max` (frozen) | Probabilistic sum |
| --- | --- | --- |
| Sampling the same colour twice changes nothing | exact, bit for bit | up to `0.250000` of coverage |
| Reordering the swatches changes nothing | exact, bit for bit | not order-independent in `f64` |

A picker that samples a colour already in the list is an ordinary accident, and under a product
combination it would darken the selection; under the frozen one it is a no-op, which is also what
makes the swatch list safe to reorder and to re-run.

### The refine mapping

```text
radius = RADIUS_MAX * (RADIUS_MIN / RADIUS_MAX)^(refine / 100)
```

Strictly decreasing, so a higher refine is always a tighter selection, with `refine = 0` at
`RADIUS_MAX`, `refine = 100` at `RADIUS_MIN` and `refine = 50` at their geometric mean, `0.035355`.

**Geometric rather than linear**, because what a person judges is the *ratio* between the radius and
the distance to the colours they do not want. Both mappings share their ends, so the only difference
is where the travel is spent, and every measurement in this study puts the decisions between a
radius of about `0.05` (a whole colour family) and `0.005` (one flat patch):

| Mapping | Refine range covering radii `0.05` to `0.005` | Travel |
| --- | --- | --- |
| Geometric (frozen) | `41.1` to `100` | **58.9 points** |
| Linear | `81.6` to `100` | 18.4 points |

`the_refine_mapping_spends_its_travel_on_the_radii_that_separate_surfaces` holds both figures and
the 3× ratio.

**The default refine is 50, and that is a measured position rather than a midpoint.** The refine at
which a surface stops being held whole across ±1 stop of shading is `53.0` for skin, `52.7` for a
sky and `50.5` for foliage, so the geometric mean of the two radius ends lands within three points
of "hold an ordinary surface together" on all three. The same test asserts that at refine 50 each of
those surfaces is selected at exactly `1.0` at gains `0.5`, `0.7`, `1.0`, `1.4` and `2.0`.

A worked selection, from the figures: a single sampled sky at refine 50 has radius `0.035355` and
plateau `0.017678`; of the 24 chart patches it selects **only the sky itself**, and it holds that
sky at exactly `1.0` from gain `0.5` to gain `2.0`, falling to `0.6226` at gain `2.8`.

## What these selections do not select

This is the point of a non-AI selection, so it is stated with numbers rather than with adjectives,
and the user guide repeats it. Every figure is asserted by a named test.

**The product repeats it too, and reads it from the same table that makes a kind creatable.** Each
kind carries its own limit lines beside its parser — `LUMINANCE_LIMITS` and `COLOUR_LIMITS` in
`range.rs` — and `mask::component_kind_limits` returns them followed by the one line every value-based
kind shares, so a kind registered later carries that sentence with no panel edited. The Masks panel
draws them on the open component's row, above the numbers they apply to, and draws under the list of
kinds the thing no absent button can say: there is no Sky, Subject, People, Objects or Background
here. The [phase-D scenario](../../xtask/src/mask_range_smoke.rs) captures both.

**A luminance band cannot separate a blue sky from a grey card.** A photographed blue sky sits at
`47.25` on the axis and a mid-grey at `47.81` — `0.56` of a slider unit, `1.4` output codes apart.
Light skin sits at `62.53` and the next grey up at `62.75`. No band selects one without the other,
and `the_luminance_band_cannot_separate_a_blue_sky_from_a_grey_card` shows a band that takes both at
exactly `1.0`. The remedy is the component list: intersect the band with a colour range, or subtract
a brush.

**The axis is not a bar of the histogram.** The delivered histogram is three channel populations,
not a luminance trace, so on a coloured subject the band's number does not line up with a visible
hump: sRGB red reads `38.23` on the axis while its red channel bins at code 175, which is `68.6` on
the same `0..100` scale. The number means what the histogram's *axis* means, not what its tallest
bar is. Labelling that in the panel is [P15](#proposals).

**A narrow band speckles on noise.** A range selection is a per-pixel value test, so it inherits the
input's noise. On a shadow at output code 40 with about two codes of noise
(`a_hard_band_speckles_on_a_noisy_shadow_and_a_soft_one_does_not`):

| Feather | Coverage standard deviation | Neighbouring pixels flipping by more than half |
| --- | --- | --- |
| `0` (hard) | `0.4862` | `47.2%` |
| `1` (the floor) | `0.3602` | `31.5%` |
| `2` | `0.1896` | `11.6%` |
| `5` | `0.0422` | `0.0%` |
| `10` | `0.0116` | `0.0%` |

A hard band on a noisy shadow is salt and pepper, not a selection. A shoulder of about five units —
13 output codes, comfortably wider than the noise — removes it. That is the honest instruction, and
it is also why the floor of 1 is a floor and not a recommendation.

**A colour range cannot separate a face from an oak floor.** The two windows do not overlap: holding
a face whole across ±1 stop of shading needs refine at most `53.0`, and dropping an oak floor
(measured at `0.0290` from the skin) needs refine above `55.1`. At the default refine the face is
whole and the floor is `0.2955` selected, which under a masked `+1 EV` moves the floor from output
code 160 to 180 — 20 codes, plainly visible.
`no_refine_setting_holds_a_face_and_drops_the_oak_floor` asserts all of it, and also the case that
works: a terracotta pot at `0.0521` is out of the selection entirely at the same setting. The limit
is about how close a surface is, not about the metric failing on every warm colour, and the remedy
is the component list — subtract a brush over the floor — which is the design's whole thesis.

**A colour range cannot separate one person's skin from another's.** Dark skin is `0.0108` from
light skin, a third of what one face's own shading spans, so any setting that holds a lit face takes
both. For a *colour* range that is arguably correct, and it is stated so nobody discovers it by
surprise. The same fact is why sky and water, foliage and a green lawn, or a blue jacket and a blue
sky come together: the selection knows a colour, not a thing.

**Every neutral is one colour.** With no lightness term, white, mid grey and black are mutually
within `0.0015` of each other — a third of the tightest radius — so a sampled grey selects the whole
tonal range at any refine (`every_neutral_is_one_colour_to_the_frozen_metric`). A neutral subject
cannot be picked out by colour at all; it is what the luminance range is for, and the two intersect.

**The selection follows the operation's input, not the finished frame.** A preceding exposure layer
moves it: under a band drawn for a sky, a `+0.75 EV` lift ahead of it takes that sky from exactly
`1.0` to exactly `0.0` (`the_selection_follows_the_operations_input_not_the_finished_frame`). This
is correct — a mask modulates one operation, and an operation sees its own input — and it is
surprising, so the panel and the user guide say it. It also means reordering layers can change what
a range component selects, which the geometric components never do.

**A range selection at proxy scale is not the exact selection downscaled.** A value-based component
reads whatever pixel the stage hands it, so at Fit it reads a downscaled pixel, and the coverage of
an average is not the average of the coverages. Measured at a sky/roof edge over a 2 × 2
(`a_range_selection_at_proxy_scale_is_not_the_mean_of_the_exact_selection`):

| Component | Coverage on sky | On roof | Mean of the two | On the averaged pixel |
| --- | --- | --- | --- | --- |
| The study's luminance band | `0.4754` | `0.0000` | `0.2377` | `0.0000` |
| The study's colour range | `1.0000` | `0.0000` | `0.5000` | `0.1389` |

The proxy frame is still the exact recipe over the exact downscale, which is what the delivered
contract promises — the recipe is being evaluated on the downscaled image — so nothing about that
contract breaks. But the 2 × 2 mask supersample the [thin-feature rule](masking.md#point-queries-and-proxies)
applies cannot help here, because the full-resolution pixels are not there to read. Whether the
frame should nevertheless be marked approximate is [P13](#proposals); the recommendation is that it
should not, and that the overlay and the user guide should say the 100% view is the truth for a
range component.

## Frozen tolerance

The tolerance is frozen **on the metric**, at the repository default, and **derived on coverage**
through the shoulder's exact Lipschitz constant. That split is deliberate: unlike a geometric
component, a range component's input is a pixel that production computes in `f32` and this reference
computes in `f64`, and the shoulder multiplies that difference into the coverage by a factor the
payload floors bound.

**Metric: `1e-6 + 1e-6 × |reference|`** — on the axis value `e` and on the Oklab distance `d`. The
axis is a three-term dot product and one `powf`; the distance is one forward Oklab conversion (three
`cbrt` and two matrix products, the same conversion the delivered mixer performs) and a two-term
sum. There is no round trip and no chained transcendental stage, which is why this study takes the
[mask study's `1e-6`](mask-study.md#frozen-tolerance) rather than the
[mixer study's `1e-5`](mixer-study.md#frozen-tolerance-production-versus-this-reference), whose
looseness came from the inverse conversion this one never performs.

**Coverage: `1.5 · ε / w`,** where `ε` is the metric's tolerance and `w` the shoulder's width in the
metric's own units. `1.5` is the exact maximum slope of `smooth`. The two worst legal cases:

| Where | Shoulder width `w` | Bound on `|Δc|` | In output codes under a masked `+1 EV` |
| --- | --- | --- | --- |
| Luminance, at the feather floor | `0.01` of the axis | `3.0e-4` | `0.0265` |
| Colour, at the tightest refine | `RADIUS_MIN · SPAN = 0.0025` | `9.6e-4` | `0.0849` |
| Both, at the study's defaults | `0.15` and `0.0177` | `2.0e-5` and `1.4e-4` | below `0.013` |

`the_frozen_tolerance_is_under_one_output_code_at_every_legal_payload` asserts the derivation and
its conclusion: a coverage error of that size moves the blended output by at most **one code** at
every one of the 256 input codes, which is the repository's standing quantization promise.

**The delivered transcription takes the first of the stated remedies, so the tolerance above is
not what it is verified against.** `crates/lightwell-core/src/mask/range.rs` widens the incoming
`[f32; 3]` once, at the top of `CompiledMask::evaluate`, and every expression after it is this
reference's own `f64` expression — the luminance dot product, the continued OETF, the Oklab matrices
and the signed cube roots included. The whole coverage field was already `f64`, because the
[mask study](mask-study.md) froze it that way, so nothing is paid for this that the geometric
components were not already paying, and the result is **exact equality** with this reference rather
than a bound: `the_luminance_band_is_bit_identical_to_the_frozen_reference` and
`the_colour_range_is_bit_identical_to_the_frozen_reference` compare `f64` bit patterns over 24 000
randomized pixels each, including pixels outside the gamut and shoulders at the feather floor. The
derivation above stands as the justification for the payload floors, which are what keep the
arithmetic bounded, and as the bound anyone writing an `f32` variant later would have to meet.

## Transcription

**A production unit transcribes the blocks above expression for expression and in the same order, so
it is bit-identical to this reference rather than merely within tolerance of it.** The rule is the
[mask study's](mask-study.md#transcription), unchanged: **any value that does not depend on the
pixel may be precomputed, and no arithmetic on the per-pixel path may be rewritten.** Specifically
forbidden, each of which is within tolerance and not bit-identical:

- a precomputed reciprocal in place of a division — `(e - lo) * (1/lo_f)`, `d * (1/radius)`;
- a fused multiply-add, or any reassociation of the luminance dot product, of `da*da + db*db`, or of
  the Oklab matrix products;
- folding `smooth` into a different but algebraically equal polynomial, including Horner's form;
- folding the `+ 1` into the numerator as `(e - lo + lo_f) / lo_f` or `(e - (lo - lo_f)) / lo_f`;
- comparing `100 · e` against the payload's own `0..100` numbers instead of compiling the payload
  onto the encoded axis;
- clamping the luminance before encoding it, or clamping a colour into gamut before converting it;
- hoisting a component's `invert` into its falloff instead of the one `1 - c` the composition
  performs, or collapsing `(amount / 100) * m` into a scale applied earlier in the fold.

Permitted, and expected: compiling `lo`, `hi`, `lo_f`, `hi_f`, the samples' `(a, b)` pairs and
`radius`; writing `x / SPAN` as `x * 2.0`, which is the same `f64` because `SPAN` is exactly `0.5`;
taking the maximum of the per-sample falloffs instead of the minimum of the squared distances, which
is proved bit-identical and is simply slower; and reusing an Oklab conversion the same pixel already
required — but only when it is the same `to_oklab` on the same input value, which for a masked mixer
layer it is, because the mask reads the operation's input.

## Limitations and what this study does not settle

- **The luminance band has no local contrast.** It is a point function of one number, so it cannot
  tell a bright sky from a bright wall, and it has no edge awareness at all. An edge-aware
  refinement is already [P7](masking.md#proposals-with-recorded-defaults) in the design and needs
  its own study, because a guided filter is a neighbourhood operation and a mask is a point
  function.
- **The colour range's cost is one Oklab conversion per pixel**, the same conversion the delivered
  mixer performs, and it is the most expensive component kind. Unlike a geometric component it has
  no bounds rectangle to skip spans with, so the cost is paid over the whole stage. The
  transcription task measured it (`the_cost_of_a_whole_stage_rectangle`, release, M4 MacBook Pro,
  one thread, one component over a 6000 × 4000 stage): a linear gradient placed low in the frame
  bounds **40.0%** of the stage at **14.6 to 20.0 ns** per pixel inside it, while both range kinds
  bound **100%** at **28.9 to 44.2 ns** (luminance) and **28.1 to 39.2 ns** (colour). So a value-based
  component costs roughly **two to three times the per-pixel work over two and a half times the
  area**, and a 24 MP masked layer pays about **0.7 to 1.1 s** of coverage field on one thread.
  **Provisional, and only the shape is quotable**: the host's one-minute load average was 15.8 to
  21.6 across five runs, far above the 8.0 a baseline needs, and the spread between runs is itself
  larger than the difference between the two kinds — so no absolute number here is a baseline and
  the two range kinds are not separable by these figures. What every run agrees on is the shape: the
  rectangle is the whole stage where a placed gradient's is 40% of it, and the per-pixel cost stays
  within a small factor of a gradient's rather than an order of magnitude above it. The rectangle
  itself is exact rather than measured.
- **No photographic corpus.** The repository holds no photographs that can be decoded from a test —
  the JPEG fixtures are synthetic colour blocks — so every claim here is numerical, over measured
  surface colours and constructed scenes. **Phase D's acceptance pass closed as much of that as a
  synthetic fixture can and no more**: `fixtures/generated/range.jpg` is twelve flat patches of this
  study's own surfaces — the 24-patch chart's sRGB renderings — so the `mask-range` scenario renders
  the sky/grey-card failure, the two-skins failure and the every-neutral failure in the editor at the
  numbers measured here. What it is still not is a photograph: the patches are flat, so nothing there
  exercises the noise measurement above, and a real sky, a real face and a real noisy shadow remain
  uninspected. That is the honest remainder, and it needs a corpus rather than another scenario.
- **No fixture file is frozen.** Coverage is a pure function of a pixel and a handful of numbers, so
  the transcription task tests the production unit against this reference in process. A committed
  JSON oracle would be a third artefact to keep in step for no coverage the direct comparison does
  not already give — the same reasoning the mask study gives, and for the same reason: this study's
  inputs are payloads and single pixels, not images.
- **The colour-constrained brush is not frozen here**, and is now frozen in the
  [mask study](mask-study.md#the-colour-constraint), which is the brush's own study. It reuses this
  study's metric and its refine mapping exactly — the similarity *is* this study's colour range at one
  sample, proved bit for bit there — while its gesture, its stored seed and its accumulation are the
  brush's work.

## Proposals

Numbered continuing the [masking design's list](masking.md#proposals-with-recorded-defaults), which
runs to P11 and every row of which is now settled. **P12 to P15 are decided**, on the transcription
task and with the reasons below. P16 and P17 were raised by that task; **P17 is now decided and
built**, on the colour-constrained brush task, and **P16 is still the owner's and is untouched** —
phase D's own acceptance pass neither settled it nor worked around it, and what that pass measured
about the gap is recorded in its row.

| # | Question | Decision |
| --- | --- | --- |
| P12 | `CompiledMask::evaluate` is declared position-only, and a value-based component cannot be answered from position. What is the signature? | **Decided as recommended**: `evaluate(x, y, rgb) -> f32` and `coverage(x, y, rgb) -> f64`, with the pixel the operation receives. A geometric component ignores `rgb`, which `a_geometric_component_ignores_the_pixel_it_is_handed` proves is exact rather than approximate — the same `f64` bits for five very different pixels at every pixel of a stage, over all three position-only kinds, with both inversions and an amount applied — so the mask study's own bit-for-bit comparisons still hold unchanged. The colour run passes the snapshot it already takes to blend against, and the spatial tiling the tile-shaped snapshot it already cuts out, so `render.sample` and the rasterizing pass reach the mask through one call with the same arguments and a sampled byte still equals the rendered byte. The rejected alternative, a second entry point, doubles the contract for one argument |
| P13 | What does a value-based component answer for `bounds()` and `min_feature_px`, and is a proxy frame carrying one approximate? | **Decided as recommended**: whole stage, and `f32::INFINITY`, so the thin-feature rule never fires for one and no proxy frame is marked approximate for it. Supersampling cannot help, because the full-resolution pixels are not available at proxy scale, so the flag would name a condition nothing can fix; a mixed mask is still supersampled for its *geometric* components, and its range components answer the same value at all four subsample positions, because the supersample moves the position and not the pixel. The overlay and the [user guide](../user-guide.md) state instead that a range selection is evaluated on what the current view can see and that the 100% view is the truth. The cost of the whole-stage rectangle is measured by `the_cost_of_a_whole_stage_rectangle` |
| P14 | Which way does Refine go, and what is its default? | **Decided as recommended**: increasing refine narrows the selection, default `50`. The research does not establish Lightroom's behaviour, so this is Lightwell's own, and `50` is measured to be the setting that holds an ordinary surface across a stop of shading |
| P15 | The band's numbers are on the histogram's axis, but the delivered histogram is three channel populations rather than a luminance trace. How is that labelled? | **Decided as recommended**: the four band parameters are declared `0..100` with the unit `%`, and their notes say they are on the histogram's own axis and that one unit is 2.55 output codes; the user guide says the same. Adding a luminance trace to the delivered, verified histogram is a larger change to a shipped inspector and stays its own decision, unmade |
| P16 | The coverage overlay is a function of position over the finished frame, and a value-based component's coverage is a function of the pixel the masked *operation* receives. Where does the overlay get that pixel? | **Raised, not decided.** The frame the grid describes holds that operation's **output**, not its input, so painting it would draw a selection the render never makes; and the overlay addresses a mask, which several layers at different stages may share, so there is no single operation to ask. Today `analysis::coverage_grid` refuses a mask that reads pixels, naming the reason and that the 100% view is where such a selection can be read, and the preview reports no grid for it — honest, and a real gap in the panel for any mask holding a range component. **TASK-024 widened it and left it alone:** a brush stroke limited to a colour reads pixels too, so a mask a person *painted* can now lose its overlay where before only a typed one could. The refusal was not removed or worked around, and this decision is still the owner's. The options are to give the overlay request the layer whose input to read and pay one prefix evaluation per cell; to let it read the frame and label the grid as the finished picture's selection rather than the operation's; or to leave it refused. This needs the owner. **What phase D's acceptance pass found, offered as evidence and not as a decision:** `mask-range` asks the overlay four times on one mask — for the composition and for each of its three components — and gets one grid, the gradient's. The refusal is *legible*: the frame carries the host's own sentence, the step ends on it instead of waiting out the run, and the photograph is left exactly as it was rather than going black or drawing an empty texture. The gap is therefore smaller than it sounds for a **mixed** mask, where the geometric components can still be read one row at a time, and larger than it sounds for a mask that is only range components, where nothing can be. Two things make it more tolerable than when it was raised: every claim that scenario makes about coverage is read from the rendered photograph with an adjustment applied through the mask — which is what the design already tells a person to do, and it works — and the panel now says on the component's own row that there is no overlay and that the 100% view is the truth, so a person meets the limit before they look for the overlay. One thing makes it less: TASK-024's widening means a mask a person **painted** can lose its overlay, and a painted mask is the one a person most expects to be able to see |
| P17 | The colour range's swatches are picked off the photograph, and the delivered pick machinery (`CanvasInteraction::SampleApply`) runs a **module's** query and submits to a **module's** action. A mask command is neither. How does a click become a swatch? | **Decided and built on TASK-024, taking both halves of the second option.** `CanvasInteraction::SampleApply` may name a **host** pair as well as a module's, and the host declares one pick per sampling kind from the kind table, so registering a kind is still what makes it reachable; and the new read-only `mask.sample-input` answers the input pixel of the operation the mask modulates as `r`, `g` and `b`, which are exactly the parameters `mask.add-<kind>-sample` declares, so the delivered field-matching rule needs no exception and the client maps nothing. The rule a shared mask needed: the pixel is read at the stage **the first layer bound to that mask in evaluation order** receives, and a mask no layer is bound to is **refused by name** rather than answered from the source or the finished frame — a seed in the wrong domain would silently select nothing, which this study's own `+0.75 EV` measurement is the evidence for. The client still decodes nothing: the only source of a colour is the host's answer, and the colour-constrained brush goes further and takes no colour in its request at all, seeding itself from the stroke's own first position |

## Figures

Every measured number above comes from the reference's own tests. The dense figures are one ignored
test, because the tables do not belong in an ordinary run:

```sh
cargo test --release --locked --package lightwell-core --test range_reference \
    -- --ignored --nocapture range_study_figures
```

The smaller figures reprint with `-- --nocapture` on the ordinary tests that assert them. Every
randomized comparison uses a fixed SplitMix64 seed, so the figures are reproducible on any machine.

## Files

| File | Purpose |
| --- | --- |
| `docs/design/range-study.md` | This document. |
| [`crates/lightwell-core/tests/reference/range.rs`](../../crates/lightwell-core/tests/reference/range.rs) | The frozen `f64` reference: the luminance axis, the band, the legality rules, the colour metric, the refine mapping and the multi-sample combination. Reuses `reference/tone.rs`'s luminance and OETF, `reference/colour.rs`'s Oklab and `reference/mask.rs`'s `smooth` unchanged. |
| [`crates/lightwell-core/tests/range_reference.rs`](../../crates/lightwell-core/tests/range_reference.rs) | The proofs and measurements above, the scenes chosen to fail, and the ignored figures test. |
| [`crates/lightwell-core/tests/reference/mod.rs`](../../crates/lightwell-core/tests/reference/mod.rs) | Declares `pub mod range;` beside the other studies' references. |
| [`crates/lightwell-core/src/mask/range.rs`](../../crates/lightwell-core/src/mask/range.rs) | The production transcription: both kinds' payloads, legality rules, declared parameters, compiled terms and per-pixel coverage, and the two answers a value-based component gives about the frame. |
| [`crates/lightwell-core/tests/mask_range.rs`](../../crates/lightwell-core/tests/mask_range.rs) | The bit-identity sweeps, P12's condition on the geometric components, the byte and RAW linear renders, the sampled byte against the rendered byte, the composition with a gradient and a subtract brush, P13's answers, the whole-stage measurement, and `a_value_based_kind_is_exactly_one_that_reads_the_pixel`, which holds the kind table's own `value_based` column against every compiled kind's `reads_pixels` so a client can name the limits before a component has been drawn. |
| [`xtask/src/mask_range_smoke.rs`](../../xtask/src/mask_range_smoke.rs) | The `mask-range` smoke scenario: this study's own failures rendered in the editor on the M4 Mac, read from the photograph with an adjustment applied through the mask, with each range component's overlay refused and the gradient's painted. |
| `xtask/src/fixtures.rs`, `fixtures/generated/range.jpg` | The twelve flat patches that scenario is measured over, which are this study's own surfaces: the 24-patch chart's sRGB renderings, laid out so the sky and the grey card, the two skins, the five neutrals and a sky/foliage boundary are each one probe apart. |

## References

- [Masking](masking.md) — the design this study freezes the phase-D numerics for.
- [Mask coverage mathematics](mask-study.md) — the composition algebra, the `smooth` falloff, the transcription rule and the mask-space conventions this study continues.
- [Colour mixer mathematics](mixer-study.md) — the Oklab basis this study reuses, and the study format.
- [Saturation and Vibrance mathematics](basic-colour.md) — the accepted Oklab conversion and the gamut policy.
- [Global tone mathematics](basic-tone.md) — the Rec. 709 luminance and the analytically continued sRGB OETF the axis is built from.
- [Basic adjustments and histogram](basic-and-histogram.md) — the histogram's declared domain, which is the domain the band's numbers mean.
- [Instant previews](instant-preview.md) — the proxy contract the range components' value dependence is measured against.
- [Lightroom masking research](../research/lightroom/geometry-masks-and-retouching.md) · [darktable masking research](../research/darktable/geometry-masks-and-retouching.md) — behavioural context only; no equivalence is claimed anywhere in this document.
- [Performance rules](../engineering/performance-rules.md) — the point-query and per-pixel cost rules the frozen forms are chosen under.
