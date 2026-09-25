# Colour mixer mathematics

Status: frozen and implemented by the `lightwell.mixer` module, which is checked against it. No production equations live here: this document, the independent f64 reference under `crates/lightwell-core/tests/reference/mixer.rs` and the fixtures under `fixtures/mixer/mixer-cases.json` are the oracle the module's pointwise unit (`crates/lightwell-core/src/modules/mixer/unit.rs`) is checked against. Read it alongside [Presence, colour mixer and vignette](presence-mixer-vignette.md), whose "Colour mixer: one pointwise unit" section and verification items 1 and 2 this study answers, and [Saturation and Vibrance mathematics](basic-colour.md), whose Oklab conversion, achromatic-axis reasoning and gamut policy this study reuses unchanged.

Scope: the reference code under `tests/` never runs in a release build or against a real image row. The production unit answers the [performance rules](../engineering/performance-rules.md) checklist in its own commits.

## The unit

One pointwise function of one pixel:

```text
mix(rgb_linear: [f64; 3], params: &MixerParams) -> [f64; 3]
```

Input and output are linear sRGB (D65) f64, unclamped in both directions. `MixerParams` holds twenty-four numbers: hue, saturation and luminance for each of eight hue ranges, every one in `[-100, 100]`, indexed in the wheel order `red, orange, yellow, green, aqua, blue, purple, magenta`.

### Space

Oklab, exactly as [the colour study](basic-colour.md#conversion) accepts it: Björn Ottosson's published `M1`/`M2` matrices with a signed cube root, and the independently published `M2⁻¹`/`M1⁻¹` for the inverse. The reference **reuses** `crates/lightwell-core/tests/reference/colour.rs`'s `to_oklab`, `from_oklab`, `chroma` and `hue_degrees` rather than restating them, so the two colour studies cannot drift apart. Chroma is `C = sqrt(a² + b²)`, hue is `h = atan2(b, a)` in degrees, normalised here to `[0, 360)`.

Everything below happens between one `to_oklab` and one reconstruction (see [Exact greys](#exact-greys)). The conversion's own round-trip residual (~1e-7, measured over a 25³ sweep in the colour study) is therefore the floor under every exactness claim in this document, and is the identity tolerance stated below.

## The hue-range basis

### Centres

The eight range centres are the Oklab hue angles of the eight sRGB reference colours, decoded through the standard sRGB transfer function and converted as above. They are frozen as literals in the reference and re-derived from the 8-bit codes by `centres_match_the_reference_colours`, which holds each to 1e-9, so the constants are checked rather than asserted.

| Range | sRGB reference | Centre hue (degrees) | Its Oklab chroma | Its Oklab `L` |
| --- | --- | --- | --- | --- |
| Red | `(255, 0, 0)` | `29.2338851923` | 0.257683 | 0.627955 |
| Orange | `(255, 128, 0)` | `52.9846795940` | 0.185803 | 0.731895 |
| Yellow | `(255, 255, 0)` | `109.7692320765` | 0.211006 | 0.967983 |
| Green | `(0, 255, 0)` | `142.4953388878` | 0.294827 | 0.866440 |
| Aqua | `(0, 255, 255)` | `194.7689479320` | 0.154550 | 0.905399 |
| Blue | `(0, 0, 255)` | `264.0520206381` | 0.313214 | 0.452014 |
| Purple | `(128, 0, 255)` | `293.9376408145` | 0.293119 | 0.530457 |
| Magenta | `(255, 0, 255)` | `328.3634179235` | 0.322491 | 0.701674 |

The centres are **not** evenly spaced, because the sRGB reference colours are not evenly spaced in Oklab. The eight gaps `g_i` to the next centre counter-clockwise sum to exactly one turn:

| Gap | Degrees | Gap | Degrees |
| --- | --- | --- | --- |
| red → orange | `23.750794` | aqua → blue | `69.283073` |
| orange → yellow | `56.784552` | blue → purple | `29.885620` |
| yellow → green | `32.726107` | purple → magenta | `34.425777` |
| green → aqua | `52.273609` | magenta → red | `60.870467` |

The narrowest gap (red → orange) and the widest (aqua → blue) differ by a factor of 2.92. Every per-range quantity below is derived from these gaps rather than from a single global angle, for the reasons given under "Hue".

Every pixel is placed on the circle once: its **segment** is the centre most recently passed going counter-clockwise (the one with the smallest non-negative wrapped distance `d = (h − c_i) mod 360`, chosen by `argmin` so no hue is left unassigned whatever rounding does at an edge) and `t = clamp(d / g_i, 0, 1)` is how far across that gap it sits. The saturation and luminance weights and the hue warp are all evaluated at this one `(i, t)`.

### Weights

The saturation and luminance sliders are distributed by a piecewise raised cosine in `t`:

```text
w_i     = (1 + cos(pi * t)) / 2
w_{i+1} = 1 - w_i
w_j     = 0                        for the other six ranges
```

This is a smooth partition of unity in which each pixel is influenced by at most two adjacent ranges. The hue sliders do not use it; they drive the warp under "Hue".

- **Sum exactly one, everywhere.** Not merely close: exactly `1.0` in f64. `w_i ∈ [0, 1]`; if `w_i ≥ 0.5`, `1 − w_i` is exact by Sterbenz's lemma and the sum is exactly 1; if `w_i < 0.5`, `1 − w_i` lands in `(0.5, 1]` where half an ulp is `2⁻⁵⁴`, so `fl(1 − w_i) = (1 − w_i) + e` with `|e| ≤ 2⁻⁵⁴`, and the exact sum `1 + e` rounds to `1.0` in either adjacent binade (ties-to-even resolves `1 − 2⁻⁵⁴` to `1.0`). The other six weights are `+0.0`, and adding `+0.0` is exact. The same argument holds in f32. `weights_are_a_partition_of_unity` confirms `sum == 1.0` bit for bit at every one of 200 001 sampled hues, that each weight lies in `[0, 1]`, and that at most two are non-zero.
- **Exactly 1 at its own centre.** `t = 0` gives `cos(0) = 1` and `w_i = 1.0` exactly; the frozen centre constants are rounded to ten decimals, so a reference colour's own measured hue sits ~1e-11 from its centre, `t ≈ 4e-13` and `cos(pi t)` still returns exactly `1.0`. Measured: `w_i = 1.0` exactly for all eight.
- **Continuous and periodic, with a continuous first derivative.** `dw_i/dh = −(pi / (2 g_i)) sin(pi t)`, which is `0` at both ends of every segment, so the weights are C¹ across every centre and across the 360-degree seam. The Lipschitz bound is `pi / (2 g_min) = 0.066137` per degree; the measured largest change over a `0.0018` degree step is `1.1905e-4`, i.e. `0.066137` per degree, matching the bound to six decimals.

### Chroma ramp

```text
t      = clamp(C / C0, 0, 1)
w_c(C) = t² (3 − 2t)
```

`w_c(0) = 0` and `w_c(C ≥ C0) = 1`, both exactly. The ramp scales the hue rotation, the luminance amount and a **chroma increase**; it does not scale a chroma decrease (see "Saturation"). Its purpose is to keep a grey, a near-grey and shadow noise from being rotated, relit or coloured, because the hue of such a pixel is noise; it has no reason to keep them from being greyed.

| Constant | Value | Why |
| --- | --- | --- |
| `C0` | `0.02` | 6.2% of the largest Oklab chroma on the sRGB gamut surface (0.3225, at sRGB magenta). It sits below every ordinary colour the study measures — the least chromatic range centre is aqua at 0.1546 (7.7 × `C0`), the skin-like patches measure 0.0728, 0.0752 and 0.0976 — so ordinary photographic colour is affected at full strength, with `w_c = 1` exactly. It is also five orders of magnitude above the measured achromatic noise floor (below), so a grey cannot acquire a colour. |

## Hue

The hue sliders bend the hue circle with one monotone **warp** `H`, and each pixel is rotated by the warp's displacement at its own hue, scaled by the ramp:

```text
delta_h = w_c(C) * (H(h) − h)
h_out   = h + delta_h
```

### Reach

| Constant | Value | Meaning |
| --- | --- | --- |
| `KAPPA` (`HUE_REACH`) | `0.85` | At `±100` a range's centre colour moves 85% of the way to the neighbouring centre in the direction of travel. |

Range `i`'s centre is displaced by

```text
delta_i = (hue_i / 100) * KAPPA * g_i          when hue_i >= 0   (toward the next centre)
delta_i = (hue_i / 100) * KAPPA * g_{i-1}      when hue_i <  0   (toward the previous centre)
```

so `+100` means the same *fraction of the way to the next colour* everywhere, and the travel in degrees follows the local gap. `full_hue_slider_moves_a_centre_the_reach_of_the_way_to_its_neighbour` measures the travelled angle of each reference colour through `mix` and holds it to 1e-4 degrees; the unit test `a_full_hue_slider_moves_its_centre_the_reach_of_the_gap_in_the_direction_of_travel` measures the same through the f32 unit to 2e-3 degrees.

| Range | `+100` (degrees) | `−100` (degrees) | Range | `+100` | `−100` |
| --- | --- | --- | --- | --- | --- |
| Red | `+20.188` | `−51.740` | Aqua | `+58.891` | `−44.433` |
| Orange | `+48.267` | `−20.188` | Blue | `+25.403` | `−58.891` |
| Yellow | `+27.817` | `−48.267` | Purple | `+29.262` | `−25.403` |
| Green | `+44.433` | `−27.817` | Magenta | `+51.740` | `−29.262` |

### Limiting opposing neighbours

A single slider at `±100` closes one gap to `1 − KAPPA = 15%` of its width. Two neighbouring centres driven at each other (range `i` positive, range `i+1` negative) would together close gap `g_i` by `approach = delta_i − delta_{i+1}`, up to `2 KAPPA g_i`, and cross. The rule: **no combination closes a gap further than one slider at full strength does alone.** Where `approach > KAPPA g_i`, both displacements are scaled by `KAPPA g_i / approach`, so every gap keeps at least `(1 − KAPPA) g_i` and the eight displaced centres stay strictly increasing. Both displacements are measured in that same gap, so the rule reads in slider terms as "two opposing neighbours whose magnitudes sum past 100 are both scaled by `100 / (|hue_i| + |hue_{i+1}|)`": at `+100`/`−100` each moves 42.5% of their gap and they end 15% apart; at `+50`/`−50` the limit is exactly met and nothing is scaled. Only an opposing pair can exceed the limit (a same-direction pair closes a gap by at most one displacement), and a centre has one sign, so each centre belongs to at most one such pair; the rule is order-independent and needs no iteration. `opposing_neighbours_share_one_sliders_travel_and_never_cross` checks all eight pairs.

### The warp

`H` is the periodic monotone piecewise-cubic Hermite interpolant (PCHIP) through the eight knots `(c_i, c_i + delta_i)`, with `H(h + 360) = H(h) + 360`:

```text
m_i = 1 + (delta_{i+1} − delta_i) / g_i                                   secant of segment i
d_i = (w_a + w_b) / (w_a / m_{i-1} + w_b / m_i)                           slope at knot i
      w_a = 2 g_i + g_{i-1},   w_b = g_i + 2 g_{i-1}                      (Fritsch–Butland weights)
```

It is held and evaluated in **displacement form**, `D(h) = H(h) − h`, which is itself periodic: on segment `i` at fraction `t`,

```text
D = delta_i h00(t) + g_i (d_i − 1) h10(t) + delta_{i+1} h01(t) + g_i (d_{i+1} − 1) h11(t)
h00 = 2t³ − 3t² + 1,  h10 = t³ − 2t² + t,  h01 = −2t³ + 3t²,  h11 = t³ − t²
```

A knot whose displacement is zero and whose neighbouring secants are exactly one has `d_i − 1 = 0` exactly, so a segment between two such knots is displaced by exactly zero. The reference evaluates the Hermite basis above; production computes the same knots, slopes and the power-basis cubic `c0 + t (c1 + t (c2 + t c3))` of each segment in f64 once per unit, casts the 32 coefficients to f32, and per pixel evaluates one cubic on the segment the weights already found — no new transcendental call, no allocation.

### Why it is strictly monotone for any combination

After the limiting rule every secant is at least `1 − KAPPA = 0.15 > 0`. The weighted harmonic mean of two positive secants is positive and strictly less than three times either of them (`d_i < m_{i-1} · 3 (g_i + g_{i-1}) / (2 g_i + g_{i-1}) < 3 m_{i-1}`, and the same for `m_i`), so on every segment the normalised end slopes `α = d_i / m_i` and `β = d_{i+1} / m_i` lie in the open square `(0, 3)²`. The Hermite cubic's derivative is affine in `(α, β)` for each `t`; at the square's four corners it is `6t(1 − t)`, `3(1 − t)²`, `3t²` and `3(1 − 2t)²`, all non-negative and never zero at the same `t`, so strictly inside the square it is strictly positive on all of `[0, 1]` (the Fritsch–Carlson condition). Hence `H` is strictly increasing for every slider combination, and so is the per-pixel map `h + w_c (H(h) − h)`, whose slope `1 − w_c + w_c H'(h)` is a convex combination of `1` and `H'`.

`hue_warp_satisfies_the_fritsch_carlson_conditions_for_every_combination` checks the secant floor, the slope bounds and the knot interpolation for all 5⁸ = 390 625 combinations of the eight hue sliders at `−100, −50, 0, +50, +100`; `hue_map_is_strictly_monotone_for_every_slider_combination` takes the exact per-segment minimum of `dH/dh` (a quadratic in `t`) over the same lattice and over 50 000 pseudo-random whole-number combinations; `analytic_slope_floor_matches_a_dense_sweep` confirms the exact minimum against a 72 000-sample sweep. In production, `the_hue_warp_is_strictly_increasing_for_every_three_level_combination` samples the f32 cubics for all 3⁸ combinations. Measured slope floors (the minimum of `dH/dh`, where `1` is no compression):

| Setting | Slope floor |
| --- | --- |
| One slider at `±100`, per range (`+100` / `−100`) | red 0.1054 / 0.0717, orange 0.0730 / 0.1052, yellow 0.0998 / 0.0756, green 0.0855 / 0.0995, aqua 0.0797 / 0.0897, blue 0.0997 / 0.0755, purple 0.0922 / 0.0975, magenta 0.0747 / 0.0953 |
| Any single slider | `0.071703` (red at `−100`) |
| Every hue slider at `+100` / at `−100` | `0.307573` / `0.327065` |
| Opposing neighbours at `+100`/`−100` | from `0.069845` (magenta against red) to `0.105128` (red against orange) |
| Every five-level combination | `0.063955988` at `[−50, +100, 0, −100, −100, −100, −100, +100]`, frozen by the test |

A floor near 0.07 means the gap a full slider closes is compressed to about 7% of its local density at its tightest; that is what "85% of the way" costs, and it stays strictly positive.

### Support

A C¹ interpolant cannot keep a slope of one at a centre next to a gap compressed to 15%: the Fritsch–Carlson condition requires that centre's slope to be at most `3 × 0.15 = 0.45`. The slopes at the moved centre and at its two neighbours therefore change, and a single range's hue slider warps the **four segments** between the centres two ranges either side (for red: from purple to yellow). Every hue outside that arc is displaced by exactly zero (`a_single_hue_slider_warps_only_the_four_segments_around_its_centre`, and in production `a_hue_slider_leaves_every_segment_beyond_its_neighbours_exactly_unmoved`, which checks the coefficients are exact zeros). The largest displacement a single slider at `±100` gives the two outer segments, measured on a 1 000-point sweep:

| Range | `+100`: before side / after side | `−100`: before side / after side |
| --- | --- | --- |
| Red | purple–magenta `−0.65` / orange–yellow `−6.42` | `+3.67` / `+5.37` |
| Orange | magenta–red `−5.62` / yellow–green `−3.49` | `+6.90` / `+0.66` |
| Yellow | red–orange `−0.58` / green–aqua `−5.83` | `+2.50` / `+3.66` |
| Green | orange–yellow `−3.85` / aqua–blue `−7.68` | `+6.35` / `+2.28` |
| Aqua | yellow–green `−1.57` / blue–purple `−3.15` | `+3.51` / `+0.90` |
| Blue | green–aqua `−1.14` / purple–magenta `−3.79` | `+5.65` / `+2.62` |
| Purple | aqua–blue `−3.99` / magenta–red `−6.82` | `+7.83` / `+2.72` |
| Magenta | blue–purple `−1.84` / red–orange `−2.49` | `+3.25` / `+0.57` |

Degrees; a negative value moves those hues back toward the lower centre. The largest rotation any combination of the eight sliders at `−100, 0, +100` produces is `64.59` degrees.

### Lesson: half the gap

The first mixer rotated each pixel by the raised-cosine blend of the two neighbouring ranges' rotations, with `±100` reaching half the gap in the direction of travel. That kept one slider monotone (slope floor `1 − pi/4 = 0.2146`) but left red `+100` at 11.88 degrees and aqua `+100` at 34.64, which read as too little travel against the familiar behaviour of moving a colour well toward its neighbour; opposing neighbours folded (slope `1 − pi/2`). A blend of per-range rotations has slope exactly one at every centre, so its slope inside a gap falls to `1 − (pi/2)(delta / g)` and one slider folds the map once its reach passes `2/pi ≈ 0.64` of the gap; the warp moves the centres themselves and lets the slopes there follow.

Lightroom publishes no numbers for its hue sliders. Community estimates of its `±100` sweep range from about 30 to about 60 degrees, and every guide describes `±100` as moving a colour well toward the adjacent named colour (Blue `−100` toward aqua, Yellow `+100` toward green). Measured here, Blue `−100` moves `58.89` degrees toward aqua and Yellow `+100` moves `27.82` degrees, to within 5 degrees of the green centre. That is context for the constant, not an equivalence claim.

## Saturation

```text
s = sum_i w_i * (saturation_i / 100) * k_s          // in [-1, 1]
f = max(0, 1 + s)          when s <  0
f = max(0, 1 + w_c * s)    when s >= 0
C_out = f * C
```

| Constant | Value | Why |
| --- | --- | --- |
| `k_s` | `1.0` | `saturation_i = −100` at that range's own centre gives `f = 1 + 1 × (−1) = 0` exactly, so chroma is exactly zero — the same *exact* grey the Basic saturation unit produces at `−100`, and for the same reason. `+100` gives `f = 2`, doubling chroma, the same `[0, 2]` gain range Basic's saturation and vibrance already use, so the two controls behave comparably at the same slider value. |

**A decrease is applied in full; only an increase is ramped.** The two branches meet at `f = 1` when `s = 0`, so the factor is continuous in the sliders. Because the weights sum to exactly one in f32 as in f64, every saturation slider at `−100` makes `s = −(w_i + w_{i+1}) = −1` exactly and `f = 0` exactly for **every** pixel, near-greys and the darkest included, and the result reconstructs to three identical channels (see [Exact greys](#exact-greys)). `every_saturation_slider_at_minus_100_greys_every_colour_exactly` checks it over 7 `L` × 6 chroma levels down to `1e-9` × 720 hues; the unit test `every_saturation_slider_at_minus_100_makes_every_pixel_exactly_grey` checks `f = 0.0` at 36 000 hues × 4 ramp values and bit-equal channels over 200 000 pseudo-random pixels (saturated, near-grey, near-black and out of range); `every_saturation_slider_at_minus_100_renders_exact_greys` checks `R == G == B` on every output pixel of a 277 440-pixel colourful image through both render paths. `a_saturation_decrease_is_not_ramped_and_an_increase_is` holds the two branches.

Had the ramp also scaled the decrease, a pixel of chroma `C < C0` would keep `C (1 − w_c(C))` under a full desaturation. That was the first behaviour; measured residuals it left:

| Input (sRGB) | Oklab `C` | `w_c` | Residual chroma with a ramped decrease | Now |
| --- | --- | --- | --- | --- |
| near-black blue `(2, 2, 6)` | 0.017270 | 0.949 | 0.000878 | 0 (codes `(2, 2, 2)`) |
| near-black warm `(6, 4, 2)` | 0.010573 | 0.543 | 0.004833 | 0 |
| near-black green `(3, 5, 3)` | 0.009937 | 0.495 | 0.005016 | 0 |
| near-grey warm `(130, 128, 126)` | 0.003859 | 0.097 | 0.003484 | 0 |
| near-grey cool `(100, 101, 104)` | 0.004870 | 0.149 | 0.004144 | 0 |

`f` cannot be negative in exact arithmetic — `|s| ≤ 1` — but the clamp at `0` is kept as the frozen guard against a rounded sum landing a hair below `−1`. `chroma_factor_is_never_negative` checks `f ∈ [0, 2]` over 360 hues × 5 chroma levels × 10 slider patterns, and `saturation_extremes_zero_and_double_a_centre_colour` checks that `f` is exactly `0.0` and exactly `2.0` at the two extremes for each range's own centre.

## Luminance

```text
m     = w_c * sum_i w_i * (luminance_i / 100)          // in [-1, 1]
gamma = 2^(-m)                                          // in [0.5, 2]
core  = clamp(L, 0, 1)
L_out = core^gamma + (L − core)
```

| Constant | Value | Why |
| --- | --- | --- |
| Gamma base | `2.0` | `+100` is a square root and `−100` a square of Oklab `L`: one "stop" of gamma in each direction, symmetric in the exponent. Measured at each range's own centre colour, `L` moves from 0.4520 to 0.2043/0.6723 (blue) and from 0.6280 to 0.3943/0.7924 (red) at `∓100`/`±100` — a strong, clearly visible move without any parameter-dependent clipping. |

The response is a gamma curve on the `[0, 1]` part of `L`, with any excess passed through unchanged. That gives, for every `m`:

- **Identity at `m = 0`.** `gamma = 2^(−0.0) = 1.0` exactly and `core.powf(1.0) + (L − core) = L` exactly. `luminance_response_is_the_exact_identity_at_zero` checks bit equality for 501 sampled `L`, including values below 0 and above 1.
- **Exact fixed points at `L = 0` and `L = 1`**, so a black pixel stays black and an in-gamut pixel is never pushed above white.
- **`(0, 1)` maps into `(0, 1)`.** `−100` darkens toward black without reaching it for any non-black input; `+100` lightens without reaching white.
- **Strictly increasing in `L`** and **strictly increasing in `m` for `L ∈ (0, 1)`** (`d/dm (L^γ) = −ln2 · γ · L^γ · ln L > 0` because `ln L < 0`). `luminance_is_strictly_monotone_in_the_slider_for_a_centre_colour` walks all 201 integer slider values for each of the eight centre colours and requires a strict increase at every step; `luminance_response_is_bounded_and_monotone` checks the bounds and both monotonicities directly on the response.
- **Out-of-range `L` passes through unchanged** (`L > 1` or `L < 0`), which keeps the map increasing in `L` across the boundary and preserves above-white content rather than folding it back.

## Composition and identity

The rotation, the chroma factor and the luminance amount are evaluated **once, on the input pixel**, so `mix` is a single well-defined function of the input rather than a sequence whose later steps see their own output:

1. `lab = to_oklab(rgb)`; `C`, `h`, the segment `(i, t)`, then `w_c(C)`, `w_i`, `w_{i+1}` and `D(h)`.
2. Hue: rotate the `(a, b)` vector by `delta_h = w_c D(h)`.
3. Chroma: scale the rotated vector by `f`.
4. Luminance: apply the response to `L`.
5. Reconstruct (below).

The rotation and the scaling are applied to `(a, b)` directly — `a' = f (a cos Δ − b sin Δ)`, `b' = f (a sin Δ + b cos Δ)` — rather than by recomposing `C` and `h`, exactly as [the colour study](basic-colour.md#saturation) scales `a` and `b` rather than re-deriving chroma. A rotation and a non-negative scalar commute, so this realises the `(C, h)` equations above exactly; what it buys is that `mix` with every slider at zero is the **exact identity in Oklab**: `D = 0` gives `cos = 1`, `sin = 0`, `f = 1`, so `a' = a`, `b' = b` and `L_out = L`.

`neutral_parameters_are_bit_exactly_the_oklab_round_trip` proves this over a 21³ sweep of `[−0.2, 1.5]³` (9 261 points, including out-of-gamut and above-white): `mix(rgb, neutral)` equals `reconstruct(to_oklab(rgb))` **bit for bit**, every channel, every point.

**The identity tolerance is therefore the Oklab conversion's own round-trip residual, and nothing else.** Measured over the same sweep, `|mix(rgb, neutral) − rgb| ≤ 3.92e-7`. `identity_residual_is_the_oklab_round_trip_residual` holds it below `2e-6` and above `0`. The production module skips the unit entirely for a neutral payload and is bit-exact.

## Achromatic, zero-weight and near-black behaviour

### Exact greys

The reconstruction is the Oklab-to-linear conversion with one achromatic branch: an achromatic Oklab colour (`a = b = 0`, either sign of zero) reconstructs to `L³` in all three channels. That is what the exact matrices give it: the first column of `M2⁻¹` is exactly one, so `(L, 0, 0)` maps to `LMS' = (L, L, L)`, and each row of `M1⁻¹` sums to one to ten decimals. The published rows are rounded, though, and in f32 the three products `M1⁻¹ · (x, x, x)` round differently: measured over 1 000 001 evenly spaced `L` in `[0, 1]`, the matrix path alone returns channels up to `5.36e-7` apart and puts 8 of them on different sides of an output code threshold, a grey rendered with one channel a code off (`an_achromatic_colour_reconstructs_to_three_identical_channels`). The achromatic branch gives three bit-identical channels. It lives in the shared production `from_oklab` (`crates/lightwell-core/src/colour.rs`), so Basic's Saturation `−100` renders the same exact greys; the f64 reference applies the same rule in its own reconstruction.

### Achromatic invariance

A grey has `a = b = 0` in exact arithmetic, so `C = 0`, `w_c = 0`, and the rotation, the luminance amount and any chroma increase are exactly zero. In f64 a grey's chroma is not exactly zero — `M1`'s three rows do not sum to bit-identical values — but the measured noise floor over the 256 grey codes is `3.73e-8`, where `w_c ≤ 3 (C / C0)² = 1.04e-11`, so the arbitrary hue of that noise cannot matter and no epsilon guard of the kind [vibrance needs](basic-colour.md#near-black-and-achromatic-behaviour) is required: the ramp itself is the guard, because unlike vibrance's weight this one *vanishes* on the axis. A saturation decrease is not ramped, so it may shrink that noise chroma further, which only moves a grey closer to exact. `achromatic_ramp_is_invariant_under_every_slider` measures every one of the 256 grey codes against all 48 single-slider extremes and both combined extremes: the worst deviation is `2.61e-7`, below the conversion's own round-trip residual, and the test bound is `2e-6`. Through the render path, `greys_stay_byte_invariant_under_every_slider_on_a_rendered_ramp` requires every grey code to come out as the same code in all three channels under each of the 48 single-slider extremes. Measured example: sRGB `(128, 128, 128)` under the `all_ranges` fixture set (hue +25, saturation −40, luminance +30 on all eight ranges) reconstructs to output codes `(128, 128, 128)`.

### Zero-weight invariance

A range's weight is exactly `+0.0` outside the two segments adjacent to its centre, so its saturation and luminance sliders contribute exactly zero there, and its hue slider displaces exactly zero outside the four segments around its centre (see "Support"). The output is then **bit-identical**. `zero_weight_colours_are_bit_identical_under_that_range` proves this for every range, saturation and luminance two centres away and hue three centres away in each direction, at three slider values: equality, not a tolerance.

### Near-black chroma

The ramp guards the achromatic axis, not darkness. A genuinely coloured near-black pixel is treated as coloured: sRGB `(2, 2, 6)` has Oklab chroma `0.017270`, so `w_c = 0.949`, and under `all_ranges` it moves to output codes `(8, 8, 13)` — mostly the luminance response acting on `L = 0.0897`. This is intended (a dark blue is blue), but it means the mixer's luminance slider can visibly lift shadow content whose chroma is real, and that a noisy shadow whose chroma comes from sensor noise rather than from the scene is lifted with it. The `near_black_blue`, `near_black_warm` and `near_black_green` fixtures (measured `w_c` 0.949, 0.543 and 0.495) and the `near_grey_warm` and `near_grey_cool` fixtures (0.097 and 0.149) hold this behaviour frozen so a later change cannot pass unnoticed.

## Gamut policy

**No clamping inside the unit**, exactly as [the colour study](basic-colour.md#gamut-policy) specifies: a result outside `[0, 1]` is returned as-is and stays finite, and the host clamps once at the end of the colour run. `out_of_gamut_inputs_and_extreme_parameters_stay_finite` exercises six inputs (including `[1.5, −0.1, 0.7]`, `[−0.1, −0.1, −0.1]` and pure black) against all eight parameter corners at `±100`.

Two consequences carry over unchanged from the colour study and are not re-derived here: a strongly out-of-gamut result's hue is not preserved by the host's per-channel clamp, and above-white content is bounded only by that clamp. Frozen examples from the fixtures, unclamped linear then output codes:

| Colour and setting | Linear | Codes |
| --- | --- | --- |
| red at `red_hue_plus_100` | `[0.933779, 0.034478, −0.072126]` | `(247, 52, 0)` |
| aqua at `aqua_hue_minus_100` | `[0.267705, 0.978487, 0.395758]` | `(141, 253, 169)` |
| red at `red_plus_orange_minus_100` | `[0.979469, 0.013551, −0.042361]` | `(253, 31, 0)` |
| orange at `red_plus_orange_minus_100` | `[1.050494, 0.191135, 0.040698]` | `(255, 121, 57)` |
| yellow at `all_hue_plus_100` | `[0.401681, 1.204552, 0.212047]` | `(170, 255, 127)` |
| blue at `all_hue_plus_100` | `[0.130887, −0.037822, 0.886961]` | `(101, 0, 242)` |

The negative channels are the rotation pushing a saturated colour past the sRGB gamut boundary at constant Oklab `L` and chroma; the clamp then decides what is seen.

## Limitations

- **Two neighbours driven at each other share one slider's travel.** With red at `+100`, taking orange from `0` toward `−100` scales both back once their magnitudes sum past 100, so red's own colour moves back from 85% to 42.5% of the gap as orange is dragged. That interaction is what keeps the map from folding; it is bounded and continuous, and only occurs when both neighbours are pushed toward their shared boundary.
- **A hue slider bends two segments beyond its neighbours.** Red `+100` also moves hues between orange and yellow back toward orange by up to 6.4 degrees, and the largest such spill of any single slider is 7.8 degrees (purple `−100`, between aqua and blue). This is the cost of a smooth monotone warp at 85% reach (see "Support").
- **Hue differences compress hard next to a moved centre.** A full slider compresses the gap it closes to 15% of its width and the local slope to as little as 0.072; hues inside that gap become close to one another. The map stays strictly increasing, so hue order is never reversed.
- **A range whose neighbours are far away rotates further at `+100`.** Aqua's `+100` is 58.89 degrees and red's is 20.19, because their gaps differ by that much. The slider means "85% of the way to the next colour", not "a fixed angle".
- **Out-of-gamut results are common at high saturation and at full hue travel.** `+100` doubles chroma, which for an already-saturated colour lands well outside sRGB; the frozen `green_saturation_plus_100` fixture computes linear `[−0.555, 1.274, −0.352]` for sRGB green. A full hue move of a saturated colour lands outside too: blue `−100` carries sRGB blue's chroma 0.313 at `L = 0.452` to a teal hue sRGB cannot reach there, and the host's clamp renders it as a darker teal. The clamp's creases are visible on a saturated wheel as straight lines where a channel reaches 0 or 1.
- **The eight ranges are the eight sRGB reference hues, not a perceptual partition.** Nothing here claims the centres are perceptually equidistant or that they match Lightroom's ranges; [the Lightroom research](../research/lightroom/tone-and-color-tools.md) supplies the names and the familiar behaviour, and no equivalence is claimed anywhere in this document.

## Frozen tolerance: production versus this reference

**`1e-5 + 1e-5 × |reference|`, linear float, at most one output code of rounding difference where the design permits it** — adopted unchanged from [the colour study](basic-colour.md#frozen-tolerance-production-versus-this-reference), because the mixer's error sources are that study's plus strictly milder additions:

- It goes through the same Oklab round trip, with the same ~1e-7 residual from the independently rounded `M2⁻¹`/`M1⁻¹`, and the same cube-root/cube amplification near zero.
- Production runs in f32 with coefficients computed in f64. Beyond the conversion it adds one `cos` for the weights, the `sin`/`cos` of the rotation, one `powf` for the gamma and one cubic in `t`. `sin` and `cos` are well-conditioned on the bounded argument `|Δ| ≤ 64.6 degrees`; the cubic's coefficients are cast once from f64, so its f32 error is a few ulps of a displacement of at most that size; and the gamma `powf` has a bounded exponent in `[0.5, 2]` on a base in `[0, 1]`.
- Unlike the Basic layer, the mixer is a *single* unit: the reference composes one Oklab round trip, not two, so the measured f64-only noise floor here (`3.92e-7` at identity, `2.61e-7` on greys) is below the `~2e-6` the two composed Basic units reach.

Measured: production's largest deviation from the reference over all 576 fixture cases is `2.08e-6` (`skin_light` under `orange_combined`), and through the real render path every case renders the reference's exact output code.

## Fixtures

[`fixtures/mixer/mixer-cases.json`](../../fixtures/mixer/README.md) — 576 cases: 48 inputs (the eight range-centre reference colours, a six-step achromatic ramp, three near-black chromatic pixels, two near-grey pixels, a 24-point hue wheel at Oklab `L = 0.6` and chroma 0.05/0.12/0.20, three skin-like patches and two out-of-gamut linear inputs) under twelve parameter sets (neutral, each effect at each extreme on a single range, one range with all three sliders set, all twenty-four sliders set, every hue slider at `+100`, red `+100` against orange `−100`, and every saturation slider at `−100`). Generated by the reference itself —

```sh
cargo test --package lightwell-core --test mixer_reference -- --ignored generate_mixer_fixtures
```

— and reloaded by `mixer_fixtures_match_reference`, which recomputes every case from the same reference on every ordinary test run and fails the build if the file drifts from the frozen formulas. `expected_linear` is the unclamped linear sRGB output at full f64 precision; the reload tolerance is `1e-12`, covering only libm differences between the platform that generated the file and the platform re-verifying it.

The measured figures quoted throughout this document are printed by

```sh
cargo test --package lightwell-core --test mixer_reference -- --ignored --nocapture mixer_study_figures
```

## Files

| File | Purpose |
| --- | --- |
| `docs/design/mixer-study.md` | This document. |
| `crates/lightwell-core/tests/reference/mixer.rs` | The frozen f64 reference: constants, segments, weights, the hue warp and its limiting rule, the three equations, the reconstruction and `mix`. Reuses `reference/colour.rs`'s Oklab conversion unchanged. |
| `crates/lightwell-core/tests/mixer_reference.rs` | The property proofs and dense measurements above, the ignored figures test, and generation and re-verification of the fixtures. |
| `crates/lightwell-core/src/modules/mixer/unit.rs` | The production f32 unit and its tests against the fixtures. |
| `fixtures/mixer/mixer-cases.json` | Frozen expected values, reloaded on every test run. |
| `fixtures/mixer/README.md` | The fixture file's structure, coverage and declared precision. |

## References

- [Presence, colour mixer and vignette](presence-mixer-vignette.md) — the design this study freezes the numerics for, including the module boundary, the `PointwiseColor` integration and the acceptance items.
- [Saturation and Vibrance mathematics](basic-colour.md) — the accepted Oklab conversion, the achromatic-axis reasoning, the gamut policy and the tolerance this study adopts.
- [Global tone mathematics](basic-tone.md) — the near-black and luminance conventions of the delivered Basic layer; the mixer's luminance acts on Oklab `L` and does not use the tone curve's luminance-ratio reconstruction.
- [Björn Ottosson, "A perceptual color space for image processing" (Oklab)](https://bottosson.github.io/posts/oklab/) — source of the matrices reused through `reference/colour.rs`.
- F. N. Fritsch and R. E. Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J. Numer. Anal. 17(2), 1980; F. N. Fritsch and J. Butland, "A method for constructing local monotone piecewise cubic interpolants", SIAM J. Sci. Stat. Comput. 5(2), 1984 — the monotonicity condition and the weighted harmonic-mean slopes the hue warp uses.
- [Lightroom tone and colour research](../research/lightroom/tone-and-color-tools.md) — behavioural context only; no equivalence is claimed anywhere in this document.
