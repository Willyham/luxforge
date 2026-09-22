# Colour mixer mathematics

Status: frozen. No production code changes here: this document, the independent f64 reference under `crates/lightwell-core/tests/reference/mixer.rs` and the fixtures under `fixtures/mixer/mixer-cases.json` are the oracle a later implementation of the `lightwell.mixer` module's pointwise unit is checked against. Read it alongside [Presence, colour mixer and vignette](presence-mixer-vignette.md), whose "Colour mixer: one pointwise unit" section and verification items 1 and 2 this study answers, and [Saturation and Vibrance mathematics](basic-colour.md), whose Oklab conversion, achromatic-axis reasoning and gamut policy this study reuses unchanged.

Scope: this is a numerical/design task. It does not implement `lightwell.mixer`, does not touch any crate's `src/`, and the reference code under `tests/` never runs in a release build or against a real image row. The [performance rules](../engineering/performance-rules.md) checklist therefore does not apply to the files this task adds; it will apply to the implementation task that turns this document into a `PointwiseColor` unit.

## The unit

One pointwise function of one pixel:

```text
mix(rgb_linear: [f64; 3], params: &MixerParams) -> [f64; 3]
```

Input and output are linear sRGB (D65) f64, unclamped in both directions. `MixerParams` holds twenty-four numbers: hue, saturation and luminance for each of eight hue ranges, every one in `[-100, 100]`, indexed in the wheel order `red, orange, yellow, green, aqua, blue, purple, magenta`.

### Space

Oklab, exactly as [the colour study](basic-colour.md#conversion) accepts it: Björn Ottosson's published `M1`/`M2` matrices with a signed cube root, and the independently published `M2⁻¹`/`M1⁻¹` for the inverse. The reference **reuses** `crates/lightwell-core/tests/reference/colour.rs`'s `to_oklab`, `from_oklab`, `chroma` and `hue_degrees` rather than restating them, so the two colour studies cannot drift apart. Chroma is `C = sqrt(a² + b²)`, hue is `h = atan2(b, a)` in degrees, normalised here to `[0, 360)`.

Everything below happens between one `to_oklab` and one `from_oklab`. The conversion's own round-trip residual (~1e-7, measured over a 25³ sweep in the colour study) is therefore the floor under every exactness claim in this document, and is the identity tolerance stated below.

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

The centres are **not** evenly spaced, because the sRGB reference colours are not evenly spaced in Oklab. The eight gaps to the next centre counter-clockwise sum to exactly one turn:

| Gap | Degrees | Gap | Degrees |
| --- | --- | --- | --- |
| red → orange | `23.750794` | aqua → blue | `69.283073` |
| orange → yellow | `56.784552` | blue → purple | `29.885620` |
| yellow → green | `32.726107` | purple → magenta | `34.425777` |
| green → aqua | `52.273609` | magenta → red | `60.870467` |

The narrowest gap (red → orange) and the widest (aqua → blue) differ by a factor of 2.92. Every per-range constant below is derived from these gaps rather than from a single global angle, for the reasons given under "Hue".

### Weights

For an input hue `h`, let `i` be the centre most recently passed going counter-clockwise (the one with the smallest non-negative wrapped distance `d = (h − h_i) mod 360`), let `g_i` be that centre's gap, and let `t = clamp(d / g_i, 0, 1)`. Then

```text
w_i     = (1 + cos(pi * t)) / 2
w_{i+1} = 1 - w_i
w_j     = 0                        for the other six ranges
```

This is a piecewise raised cosine: a smooth partition of unity in which each pixel is influenced by at most two adjacent ranges.

- **Sum exactly one, everywhere.** Not merely close: exactly `1.0` in f64. `w_i ∈ [0, 1]`; if `w_i ≥ 0.5`, `1 − w_i` is exact by Sterbenz's lemma and the sum is exactly 1; if `w_i < 0.5`, `1 − w_i` lands in `(0.5, 1]` where half an ulp is `2⁻⁵⁴`, so `fl(1 − w_i) = (1 − w_i) + e` with `|e| ≤ 2⁻⁵⁴`, and the exact sum `1 + e` rounds to `1.0` in either adjacent binade (ties-to-even resolves `1 − 2⁻⁵⁴` to `1.0`). The other six weights are `+0.0`, and adding `+0.0` is exact. `weights_are_a_partition_of_unity` confirms `sum == 1.0` bit for bit at every one of 200 001 sampled hues, that each weight lies in `[0, 1]`, and that at most two are non-zero.
- **Exactly 1 at its own centre.** `t = 0` gives `cos(0) = 1` and `w_i = 1.0` exactly; the frozen centre constants are rounded to ten decimals, so a reference colour's own measured hue sits ~1e-11 from its centre, `t ≈ 4e-13` and `cos(pi t)` still returns exactly `1.0` (the deviation is ~1e-24, far below one ulp). Measured: `w_i = 1.0` exactly for all eight.
- **Continuous and periodic, with a continuous first derivative.** `dw_i/dh = −(pi / (2 g_i)) sin(pi t)`, which is `0` at both ends of every segment, so the weights are C¹ across every centre and across the 360-degree seam. The Lipschitz bound is `pi / (2 g_min) = 0.066137` per degree; the measured largest change over a `0.0018` degree step is `1.1905e-4`, i.e. `0.066137` per degree, matching the bound to six decimals.

### Chroma ramp

All three effects are scaled by a chroma ramp that is exactly zero on the achromatic axis:

```text
t      = clamp(C / C0, 0, 1)
w_c(C) = t² (3 − 2t)
```

| Constant | Value | Why |
| --- | --- | --- |
| `C0` | `0.02` | 6.2% of the largest Oklab chroma on the sRGB gamut surface (0.3225, at sRGB magenta). It sits below every ordinary colour the study measures — the least chromatic range centre is aqua at 0.1546 (7.7 × `C0`), the skin-like patches measure 0.0728, 0.0752 and 0.0976, and the colour study's pastels are all above 0.01 — so ordinary photographic colour is affected at full strength, with `w_c = 1` exactly. It is also five orders of magnitude above the measured achromatic noise floor (below), so a grey cannot acquire a colour. |

`w_c(0) = 0` and `w_c(C ≥ C0) = 1`, both exactly.

## Hue

```text
delta_h = w_c * sum_i w_i * (hue_i / 100) * theta_i(sign)
h_out   = h + delta_h
```

`theta_i(sign)` is **per range and per direction**: half the gap to the neighbour in the direction of travel.

```text
theta_i(+) = gap(i -> i+1) / 2          theta_i(-) = gap(i-1 -> i) / 2
```

| Range | `theta(+)` | `theta(-)` | Range | `theta(+)` | `theta(-)` |
| --- | --- | --- | --- | --- | --- |
| Red | `11.875397` | `30.435234` | Aqua | `34.641536` | `26.136805` |
| Orange | `28.392276` | `11.875397` | Blue | `14.942810` | `34.641536` |
| Yellow | `16.363053` | `28.392276` | Purple | `17.212889` | `14.942810` |
| Green | `26.136805` | `16.363053` | Magenta | `30.435234` | `17.212889` |

**Why per range and per direction, and why half.** A single global constant would mean the same angle in the 23.75-degree red → orange gap and the 69.28-degree aqua → blue gap, so `+100` would cross a whole range in one place and barely leave the centre in another; scaling by the local gap makes `+100` mean the same *fraction of the way to the neighbouring colour* everywhere. Half, rather than the whole gap, is what keeps the hue map from folding. On the segment between centres `i` and `i+1`, writing `δ_i`, `δ_{i+1}` for the two signed rotations at those centres,

```text
delta_h(h) = δ_{i+1} + w_i(h) (δ_i − δ_{i+1})
d(h_out)/dh = 1 − (pi / (2 g_i)) sin(pi t) (δ_i − δ_{i+1})
```

Both bounds that matter are expressed in *this* gap: range `i` travelling positive uses `g_i / 2`, and range `i+1` travelling negative also uses `g_i / 2`. So `(δ_i − δ_{i+1}) ≤ g_i` always, and:

- **Single slider, or every hue slider pushed the same way**: one of `δ_i`, `δ_{i+1}` is zero or has the same sign, so `(δ_i − δ_{i+1}) ≤ g_i / 2` and the slope floor is `1 − pi/4 = +0.214602`. Strictly monotone: no two input hues can collapse onto one output hue. Measured minimum slope over a 36 000-sample sweep, for every single slider at `±100` and for several all-same-direction combinations: `0.214602`, matching the analytic floor to six decimals.
- **Two adjacent ranges driven at each other** (range `i` at `+100`, range `i+1` at `−100`): `(δ_i − δ_{i+1})` reaches `g_i` and the slope floor is `1 − pi/2 = −0.570796`. Measured for all eight adjacent pairs: `−0.570796`. This is the one fold, recorded under limitations.

Taking the whole gap instead of half would put the single-slider floor at `1 − pi/2`, i.e. a fold from one slider alone, which is why `+100` moves a centre colour **half way** to the neighbouring centre rather than all the way. `full_hue_slider_moves_a_centre_half_way_to_its_neighbour` measures the travelled angle for all eight ranges in both directions and holds it to 1e-4 degrees.

## Saturation

```text
f = max(0, 1 + w_c * sum_i w_i * (saturation_i / 100) * k_s)
C_out = f * C
```

| Constant | Value | Why |
| --- | --- | --- |
| `k_s` | `1.0` | `saturation_i = −100` at that range's own centre gives `f = 1 + 1 × (−1) = 0` exactly, so chroma is exactly zero — the same *exact* grey the Basic saturation unit produces at `−100`, and for the same reason. `+100` gives `f = 2`, doubling chroma, the same `[0, 2]` gain range Basic's saturation and vibrance already use, so the two controls behave comparably at the same slider value. |

`f` cannot be negative in exact arithmetic — the weights are a partition of unity and each term `w_i s_i` lies in `[−1, 1]`, so `|sum| ≤ 1` and `f ∈ [0, 2]` — but the reference clamps at `0` anyway, as the frozen guard against a rounded sum landing a hair below `−1`. `chroma_factor_is_never_negative` checks `f ∈ [0, 2]` over 360 hues × 5 chroma levels × 10 slider patterns, and `saturation_extremes_zero_and_double_a_centre_colour` checks that `f` is exactly `0.0` and exactly `2.0` at the two extremes for each range's own centre, and that the `−100` result reconstructs to R = G = B within 1e-9.

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
- **Exact fixed points at `L = 0` and `L = 1`**, so a black pixel stays black (the near-black rule this study needs: an achromatic pixel is already untouched because `w_c = 0`, and a chromatic pixel at `L = 0` stays at `L = 0` with no special case) and an in-gamut pixel is never pushed above white.
- **`(0, 1)` maps into `(0, 1)`.** `−100` darkens toward black without reaching it for any non-black input; `+100` lightens without reaching white.
- **Strictly increasing in `L`** and **strictly increasing in `m` for `L ∈ (0, 1)`** (`d/dm (L^γ) = −ln2 · γ · L^γ · ln L > 0` because `ln L < 0`). `luminance_is_strictly_monotone_in_the_slider_for_a_centre_colour` walks all 201 integer slider values for each of the eight centre colours and requires a strict increase at every step; `luminance_response_is_bounded_and_monotone` checks the bounds and both monotonicities directly on the response.
- **Out-of-range `L` passes through unchanged** (`L > 1` or `L < 0`), which keeps the map increasing in `L` across the boundary and preserves above-white content rather than folding it back.

## Composition and identity

Weights are evaluated **once, on the input pixel**, and all three effects are driven from them, so `mix` is a single well-defined function of the input rather than a sequence whose later steps see their own output:

1. `lab = to_oklab(rgb)`; `C`, `h`, then `w_i(h)` and `w_c(C)`.
2. Hue: rotate the `(a, b)` vector by `delta_h`.
3. Chroma: scale the rotated vector by `f`.
4. Luminance: apply the response to `L`.
5. `from_oklab`.

Hue before chroma before luminance. The rotation and the scaling are applied to `(a, b)` directly — `a' = f (a cos Δ − b sin Δ)`, `b' = f (a sin Δ + b cos Δ)` — rather than by recomposing `C` and `h`, exactly as [the colour study](basic-colour.md#saturation) scales `a` and `b` rather than re-deriving chroma. A rotation and a non-negative scalar commute, so this realises the `(C, h)` equations above exactly; what it buys is that `mix` with every slider at zero is the **exact identity in Oklab**, with no polar round-trip error at all: `Δ = 0` gives `cos = 1`, `sin = 0`, `f = 1`, so `a' = 1 × (a × 1 − b × 0) = a` and `b' = b`, and `L_out = L`.

`neutral_parameters_are_bit_exactly_the_oklab_round_trip` proves this over a 21³ sweep of `[−0.2, 1.5]³` (9 261 points, including out-of-gamut and above-white): `mix(rgb, neutral)` equals `from_oklab(to_oklab(rgb))` **bit for bit**, every channel, every point.

**The identity tolerance is therefore the Oklab conversion's own round-trip residual, and nothing else.** Measured over the same sweep, `|mix(rgb, neutral) − rgb| ≤ 3.92e-7`, consistent with the ~1e-7 order [the colour study measures](basic-colour.md#conversion) for the independently rounded `M2⁻¹`/`M1⁻¹` matrices. `identity_residual_is_the_oklab_round_trip_residual` holds it below `2e-6` and above `0` (a zero residual would mean the published matrices are bit-exact inverses, which is not claimed). A production unit is free to skip the conversion entirely for a neutral payload and be bit-exact; that is an implementation matter, not part of these equations.

## Achromatic, zero-weight and near-black behaviour

**Achromatic invariance.** A grey has `a = b = 0` in exact arithmetic, so `C = 0`, `w_c = 0`, and all three amounts are exactly zero. In f64 a grey's chroma is not exactly zero — `M1`'s three rows do not sum to bit-identical values — but the measured noise floor over the 256 grey codes is `3.73e-8`, where `w_c ≤ 3 (C / C0)² = 1.04e-11`. The arbitrary hue of that noise therefore cannot matter, and no epsilon guard of the kind [vibrance needs](basic-colour.md#near-black-and-achromatic-behaviour) is required here: the ramp itself is the guard, because unlike vibrance's weight this one *vanishes* on the axis instead of being maximal there. `achromatic_ramp_is_invariant_under_every_slider` measures every one of the 256 grey codes against all 48 single-slider extremes and both combined extremes: the worst deviation is `2.61e-7`, below the conversion's own round-trip residual, and the test bound is `2e-6`. Measured example: sRGB `(128, 128, 128)` under the `all_ranges` fixture set (hue +25, saturation −40, luminance +30 on all eight ranges) reconstructs to output codes `(128, 128, 128)`.

**Zero-weight invariance.** A range's weight is exactly `+0.0` outside the two segments adjacent to its centre, so its three sliders contribute exactly zero to every amount and the output is **bit-identical**. `zero_weight_colours_are_bit_identical_under_that_range` proves this for every range against two centres away in each direction, at three slider values, on all three sliders: equality, not a tolerance.

**Near-black chroma.** The ramp guards the achromatic axis, not darkness. A genuinely coloured near-black pixel is treated as coloured: sRGB `(2, 2, 6)` has Oklab chroma `0.017270`, so `w_c = 0.949`, and under `all_ranges` it moves to output codes `(8, 8, 13)` — mostly the luminance response acting on `L = 0.0897`. This is intended (a dark blue is blue), but it means the mixer's luminance slider can visibly lift shadow content whose chroma is real, and that a noisy shadow whose chroma comes from sensor noise rather than from the scene is lifted with it. The `near_black_blue`, `near_black_warm` and `near_black_green` fixtures (measured `w_c` 0.949, 0.543 and 0.495) hold this behaviour frozen so a later change cannot pass unnoticed.

## Gamut policy

**No clamping inside the unit**, exactly as [the colour study](basic-colour.md#gamut-policy) specifies: a result outside `[0, 1]` is returned as-is and stays finite, and the host clamps once at the end of the colour run. `out_of_gamut_inputs_and_extreme_parameters_stay_finite` exercises six inputs (including `[1.5, −0.1, 0.7]`, `[−0.1, −0.1, −0.1]` and pure black) against all eight parameter corners at `±100`.

Two consequences carry over unchanged from the colour study and are not re-derived here: a strongly out-of-gamut result's hue is not preserved by the host's per-channel clamp, and above-white content is bounded only by that clamp. One frozen example from the fixtures: sRGB red at `red_hue_plus_100` computes linear `[0.97320, 0.016704, −0.048432]`, which the host clamps to output codes `(252, 35, 0)` — the negative blue channel is the rotation pushing red past the sRGB gamut boundary on its way toward orange.

## Limitations

- **Two adjacent hue sliders driven at each other fold the hue map.** With range `i` at `+100` and range `i+1` at `−100` the slope reaches `1 − pi/2 = −0.5708`, so a band of input hues between the two centres collapses onto one output hue and the ordering of hues inside that band is reversed. This is the accepted cost of a `theta` large enough to be useful; it is bounded, it never affects continuity or finiteness, and it only occurs when the user explicitly asks both neighbouring ranges to move toward their shared boundary. `opposing_adjacent_sliders_fold_at_the_analytic_worst_case` asserts the measured worst case for all eight pairs so a future change cannot make it silently worse.
- **Hue differences compress and stretch near a range boundary.** Because weights are evaluated on the input, a colour halfway between two centres receives half of each range's rotation, while the centre itself receives all of one. A single slider therefore changes the *spacing* of hues inside its two segments, not only their position. This is inherent to a weighted-range mixer and is what makes a range slider local.
- **A range whose neighbours are far away rotates further at `+100`.** Aqua's `+100` is 34.64 degrees and red's is 11.88, because their gaps differ by that much. The slider means "half way to the next colour", not "a fixed angle".
- **Out-of-gamut results are common at high saturation.** `+100` doubles chroma, which for an already-saturated colour lands well outside sRGB; the host's clamp then decides what is seen. The frozen `green_saturation_plus_100` fixture computes linear `[−0.555, 1.274, −0.352]` for sRGB green.
- **The eight ranges are the eight sRGB reference hues, not a perceptual partition.** Nothing here claims the centres are perceptually equidistant or that they match Lightroom's ranges; [the Lightroom research](../research/lightroom/tone-and-color-tools.md) supplies the names and the familiar behaviour, and no equivalence is claimed anywhere in this document.
- **No visual review.** The Basic colour study could render PNGs through an existing unit; this study has no production unit to render through, so every claim here is numerical. Rendered inspection of hue continuity on a wheel and of noise in shadows is [acceptance item 4](presence-mixer-vignette.md#verification-and-acceptance) of the module task, on the M4 Mac, and is not claimed here.

## Frozen tolerance: production versus this reference

**`1e-5 + 1e-5 × |reference|`, linear float, at most one output code of rounding difference where the design permits it** — adopted unchanged from [the colour study](basic-colour.md#frozen-tolerance-production-versus-this-reference), because the mixer's error sources are that study's plus strictly milder additions:

- It goes through the same Oklab round trip, with the same ~1e-7 residual from the independently rounded `M2⁻¹`/`M1⁻¹`, and the same cube-root/cube amplification near zero. That alone is what widened the colour study's tolerance from the Basic design's `1e-6` default.
- Production runs in f32 with coefficients computed in f64, so it carries additional rounding from every `powf`, `cbrt`, `sin` and `cos` at f32 precision. The mixer adds three such calls (`sin`, `cos`, `powf` for the gamma) beyond the conversion. `sin` and `cos` are well-conditioned on the bounded argument `|Δ| ≤ 35 degrees`, and the gamma `powf` has a bounded exponent in `[0.5, 2]` on a base in `[0, 1]`, so neither adds an error of a different order from the ones `1e-5` was already sized for.
- Unlike the Basic layer, the mixer is a *single* unit: the reference composes one Oklab round trip, not two, so the measured f64-only noise floor here (`3.92e-7` at identity, `2.61e-7` on greys) is below the `~2e-6` the two composed Basic units reach.

A tighter tolerance is not frozen, because it would have to be justified against an f32 implementation that does not exist yet; `1e-5` is the tolerance the comparable, already-implemented colour unit meets.

## Fixtures

[`fixtures/mixer/mixer-cases.json`](../../fixtures/mixer/README.md) — 414 cases: 46 inputs (the eight range-centre reference colours, a six-step achromatic ramp, three near-black chromatic pixels, a 24-point hue wheel at Oklab `L = 0.6` and chroma 0.05/0.12/0.20, three skin-like patches and two out-of-gamut linear inputs) under nine parameter sets (neutral, each effect at each extreme on a single range, one range with all three sliders set, and all twenty-four sliders set). Generated by the reference itself —

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
| `crates/lightwell-core/tests/reference/mixer.rs` | The frozen f64 reference: constants, basis, the three equations and `mix`. Reuses `reference/colour.rs`'s Oklab conversion unchanged. |
| `crates/lightwell-core/tests/mixer_reference.rs` | The property proofs and dense measurements above, the ignored figures test, and generation and re-verification of the fixtures. |
| `fixtures/mixer/mixer-cases.json` | Frozen expected values, reloaded on every test run. |
| `fixtures/mixer/README.md` | The fixture file's structure, coverage and declared precision. |

## References

- [Presence, colour mixer and vignette](presence-mixer-vignette.md) — the design this study freezes the numerics for, including the module boundary, the `PointwiseColor` integration and the acceptance items.
- [Saturation and Vibrance mathematics](basic-colour.md) — the accepted Oklab conversion, the achromatic-axis reasoning, the gamut policy and the tolerance this study adopts.
- [Global tone mathematics](basic-tone.md) — the near-black and luminance conventions of the delivered Basic layer; the mixer's luminance acts on Oklab `L` and does not use the tone curve's luminance-ratio reconstruction.
- [Björn Ottosson, "A perceptual color space for image processing" (Oklab)](https://bottosson.github.io/posts/oklab/) — source of the matrices reused through `reference/colour.rs`.
- [Lightroom tone and colour research](../research/lightroom/tone-and-color-tools.md) — behavioural context only; no equivalence is claimed anywhere in this document.
