# Saturation and Vibrance mathematics

Status: frozen and implemented as the `colour-adjust` unit of the Basic module ([design](basic-and-histogram.md)). No production code changes here: this document, the independent f64 reference under `crates/lightwell-reference/src/colour.rs` and the fixtures under `fixtures/basic/colour-cases.json` are the oracle a later implementation of the `vibrance` and `saturation` Basic units is checked against. Read it alongside [Basic adjustments and histogram](basic-and-histogram.md), whose "Saturation and vibrance" section and integration contract this study answers, and [product decisions](../decisions.md#basic-adjustments-and-histogram) for the accepted ranges.

Scope: this is a numerical/design task. It does not implement `lightwell.basic.adjust`'s colour units, does not touch `crates/lightwell-core/src/`, and the reference code under `tests/` never runs in a release build or against a real image row. The [performance rules](../engineering/performance-rules.md) checklist therefore does not apply to the files this task adds; it will apply to the implementation task that turns this document into a `PointwiseColor` unit.

## Candidate: Oklab chroma scaling

[Oklab](https://bottosson.github.io/posts/oklab/) is the first and, after the property tests below, the accepted candidate: it did not fail any property test, so no fallback (for example chroma scaling about Rec. 709 luminance) was needed.

### Conversion

Linear sRGB (D65) to Oklab goes through an intermediate LMS-like cone space, a per-component cube root, and a second linear map. Both directions use Björn Ottosson's published matrices, reproduced here with every published digit (`crates/lightwell-reference/src/colour.rs` defines them as `f64` constants byte for byte):

Linear sRGB → LMS (`M1`):

```text
l = 0.4122214708 r + 0.5363325363 g + 0.0514459929 b
m = 0.2119034982 r + 0.6806995451 g + 0.1073969566 b
s = 0.0883024619 r + 0.2817188376 g + 0.6299787005 b
```

Signed cube root, componentwise: `l' = signed_cbrt(l)`, `m' = signed_cbrt(m)`, `s' = signed_cbrt(s)`, where

```text
signed_cbrt(x) = sign(x) · |x|^(1/3)
```

LMS' → Oklab (`M2`):

```text
L = 0.2104542553 l' + 0.7936177850 m' − 0.0040720468 s'
a = 1.9779984951 l' − 2.4285922050 m' + 0.4505937099 s'
b = 0.0259040371 l' + 0.7827717662 m' − 0.8086757660 s'
```

The inverse direction, Oklab → linear sRGB, uses independently published (not algebraically derived) matrices:

Oklab → LMS' (`M2⁻¹`):

```text
l' = L + 0.3963377774 a + 0.2158037573 b
m' = L − 0.1055613458 a − 0.0638541728 b
s' = L − 0.0894841775 a − 1.2914855480 b
```

Cube, componentwise: `l = l'³`, `m = m'³`, `s = s'³`.

LMS → linear sRGB (`M1⁻¹`):

```text
r = 4.0767416621 l − 3.3077115913 m + 0.2309699292 s
g = −1.2684380046 l + 2.6097574011 m − 0.3413193965 s
b = −0.0041960863 l − 0.7034186147 m + 1.7076147010 s
```

**The signed cube root is defined for negative inputs and is what makes this safe for out-of-range colour.** `f64::powf(1.0 / 3.0)` is *not* interchangeable with a real cube root: for a negative base it returns `NaN`. Because a preceding Basic unit (exposure, tone) is contractually allowed to leave a channel outside `[0, 1]`, including negative, `M1 · rgb` can land a negative LMS component even when the pixel is only mildly out of range (for example `rgb = [−0.1, −0.1, −0.1]` gives `l = m = s ≈ −0.1`). `signed_cbrt` (`x.signum() * x.abs().cbrt()` in the reference, equivalently Rust's own `f64::cbrt`, which already implements the signed real cube root) keeps every step finite. `out_of_gamut_linear_inputs_stay_finite` in `colour.rs` exercises exactly this path.

**Matrix round-trip residual.** `M2⁻¹` and `M1⁻¹` are independently rounded 10-significant-digit matrices, not algebraic inverses of `M2` and `M1`, so `from_oklab(to_oklab(rgb))` is not bit-exact. `matrices_round_trip_within_documented_residual` sweeps a 25×25×25 grid over `[−0.2, 1.5]³` (15,625 points, including out-of-gamut and above-white values) and measures the residual directly; the observed maximum is on the order of **5×10⁻⁷** (an independent Python double-precision check over 200,000 random points in `[0, 1]³` measured up to 2.6×10⁻⁷, and over `[−1, 2]³` up to 5.0×10⁻⁷), consistent with the ~10⁻⁷ order expected from independently rounded 10-digit constants. The test asserts the residual stays below `2e-6` (margin over the measured value) and above `0` (a residual of exactly zero would mean the published matrices are bit-exact inverses, which is not claimed). This residual is the reason saturation and vibrance are specified as scaling `a`/`b` directly (see below) rather than as "convert, scale, convert back, then separately re-derive hue" — the latter would needlessly launder every pixel through the residual twice.

One exact, load-bearing property of the published constants: **`M1⁻¹`'s three rows each sum to exactly `1.0` in f64, and `M2⁻¹`'s first column is exactly `[1.0, 1.0, 1.0]`.** Together these mean that for any Oklab colour with `a = b = 0`, `from_oklab` reconstructs `r = g = b` bit-for-bit (up to the shared rounding of a single scalar `L³` times three matrix rows that happen to sum identically) — not merely "close to equal." `saturation_negative_100_is_grey_on_a_6_bit_cube` measures this directly over a 64-level-per-channel cube and holds every channel to within 1×10⁻⁹. The f32 production conversion cannot rely on the rounded rows summing identically, so its `from_oklab` reconstructs `a = b = 0` as `L³` in all three channels directly; `saturation_minus_100_renders_three_equal_codes_for_every_pixel` holds every output pixel of a colour cube and a near-grey ramp to three equal codes.

### Saturation

```text
chroma_out = chroma_in × (1 + s / 100),  s ∈ [−100, 100]
```

Realized by scaling Oklab `a` and `b` by the same factor and leaving `L` untouched:

```text
k = 1 + s / 100                      // k ∈ [0, 2]
a_out = a_in × k
b_out = b_in × k
L_out = L_in
```

Because `sqrt((k·a)² + (k·b)²) = k · sqrt(a² + b²)` for `k ≥ 0`, this realizes the chroma formula exactly and, as a corollary, preserves `atan2(b, a)` exactly (for `k > 0`) — saturation cannot shift hue by construction. `s = −100` gives `k = 0.0` exactly in f64, so `a_out = a_in × 0.0 = 0.0` and `b_out = 0.0` exactly regardless of `a_in`, `b_in` (short of them being non-finite, which does not occur for finite input): saturation −100 is *exact* grey, not merely close, and combined with the `M1⁻¹`/`M2⁻¹` property above, produces bit-close equal R, G, B. `s = 100` doubles chroma. `saturation_chroma_is_monotone_in_s` confirms chroma is nondecreasing in `s` across the full range for a mixed set of colours (primaries, skin-like patches, an arbitrary saturated magenta), and `saturation_preserves_hue_for_in_gamut_results` confirms the hue angle is unchanged to within 1×10⁻⁹ — measured on the Oklab value the scaling step itself produces, before the round trip back through the independently rounded matrices (whose own ~10⁻⁷ residual would otherwise dominate a 10⁻⁹ hue tolerance and mask what the scaling formula guarantees, not what it fails to guarantee).

### Vibrance

```text
chroma_out = chroma_in × (1 + (v / 100) × w(C, h)),  v ∈ [−100, 100]
```

realized the same way as saturation (`k = 1 + (v/100)·w`, scale `a` and `b` by `k`, leave `L` untouched). `w(C, h) = w_c(C) · w_h(h)` is the product of a chroma weight and a hue weight, both in `[0, 1]`, so `k` always lands in `[0, 2]`, the same bound saturation's factor has.

**Chroma weight** — full gain near grey, smoothly falling to zero as chroma approaches the practical top of the sRGB gamut in Oklab:

```text
C_normalized = max(C / C_ref, 0)
w_c(C) = 1 − smoothstep(c0, c1, C_normalized)
smoothstep(e0, e1, x) = let t = clamp((x − e0) / (e1 − e0), 0, 1) in t² (3 − 2t)
```

with the frozen constants

| Constant | Value | Why |
| --- | --- | --- |
| `C_ref` | `0.32` | The Oklab chroma of the most saturated point on the sRGB gamut surface, `(255, 0, 255)` (sRGB magenta), measured by scanning the gamut surface at 8-bit resolution: `0.3225`. `C_ref` normalizes chroma to roughly `[0, 1]` without being a gamut boundary test itself. |
| `c0` | `0.10` | Below 10% of `C_ref` (Oklab chroma ≲ 0.032), vibrance runs at full gain. |
| `c1` | `0.70` | At and above 70% of `C_ref` (Oklab chroma ≳ 0.224), vibrance contributes no chroma gain; the sRGB primaries (chroma 0.21–0.32) fall at or past this edge. |

**Hue weight** — a colour heuristic, not skin detection or a promise about every skin tone: it weights every pixel whose Oklab hue falls in a fixed band the same way, regardless of what the pixel depicts, and a real skin tone whose hue falls outside the band gets no special treatment.

```text
w_h(h) = 1 − SKIN_PROTECTION × skin_response(h)
skin_response(h) = max(cos((h − h_center) / half_width × π/2), 0)   if |h − h_center| < half_width, wrapped to (−180°, 180°]
                  = 0                                                otherwise
```

| Constant | Value | Why |
| --- | --- | --- |
| `h_center` | `55°` | Centre of the skin-like hue band (Oklab `atan2(b, a)` degrees). |
| `half_width` | `35°` | Band covers 20°–90°. The three documented skin-like patches measure at 52.9° (sRGB 224,172,140), 58.7° (141,85,36) and 74.1° (255,219,172); sRGB red measures 29.2° and yellow 109.8°, both outside the band, so primaries keep the full chroma weight's worth of hue treatment. |
| `SKIN_PROTECTION` | `0.6` | Maximum fraction of gain removed at the band centre; `w_h(h_center) = 0.4`, `w_h` outside the band `= 1.0`. |

**Frozen numeric examples**, both proved as regression tests in `colour.rs`:

- *Low chroma vs. high chroma at the same v* — `w(C = 0.02, h = 200°) = 1.0` and `w(C = 0.20, h = 200°) ≈ 0.0430`: the low-chroma point gets **≈23.3×** the gain of the high-chroma point at the same `v`, both measured away from the skin band so only `w_c` differs.
- *Skin band centre vs. 180° away, same chroma* — `w(C = 0.08, h = 55°) = 0.3375` and `w(C = 0.08, h = 235°) = 0.84375`: the ratio is **exactly `0.4`**, i.e. `1 − SKIN_PROTECTION`, because `w_c` cancels (identical chroma on both sides) and `skin_response(235°) = 0` exactly (180° is past the 35° half-width).

**Negative vibrance** uses the same `w(C, h)`: `v < 0` gives `k < 1`, reducing chroma with the identical chroma- and hue-dependent weighting positive vibrance uses (less reduction on colours already near max chroma, less reduction — i.e. more restraint — in the skin band).

### Near-black and achromatic behaviour

A colour with Oklab chroma exactly `0` (`a = b = 0`) is unaffected by any `k`: `0 × k = 0`. `L` is never touched by either unit. `greys_stay_grey_for_every_s_and_v` checks every one of the 256 grey codes against every combination of `v` and `s` in `{−100, −50, 0, 50, 100}`.

In floating-point practice, achromatic input is not *exactly* zero chroma: `M1`'s three rows do not sum to bit-identical values, so a grey input's `a`, `b` are a tiny — measured up to ≈4×10⁻⁸ over the 256 grey codes — and essentially arbitrary-hue numerical noise rather than true zero. Two consequences are handled explicitly:

1. **Vibrance's hue weight is skipped below `CHROMA_EPSILON = 1e-4`.** Without this guard, the arbitrary hue of that noise can occasionally land inside the skin band, giving a near-black pixel a `w_h < 1` it has no perceptual basis for, and — worse — making `v = −100` land at `k` slightly above `0` instead of exactly `0` for that pixel (breaking the "vibrance −100 also produces exact grey" property vibrance shares with saturation −100). Below `CHROMA_EPSILON`, `apply_vibrance` uses `w = w_c(C) = 1.0` unconditionally (chroma this small is always below `c0`) and skips reading the hue. `CHROMA_EPSILON` sits several orders of magnitude above the measured noise floor and several below the smallest chroma of a genuinely coloured patch (the pastels in the fixture file are all above `0.01`), so it never affects a real colour.
2. **Composing two units compounds the residual.** `apply_basic_colour` (vibrance then saturation) round-trips through the Oklab matrices twice. Over the full grey ramp and the full `v`/`s` range, the worst measured per-channel spread after both units is **≈2×10⁻⁶** (at white, `v = s = 100`). `greys_stay_grey_for_every_s_and_v` holds this to `1e-5`, a 5× margin over the measured worst case, and notes this is far below one 8-bit output code across the range that matters (the linear step near white exceeds `1e-3`). The single-unit exact-zero property (saturation −100, or vibrance −100 via the `CHROMA_EPSILON` guard) is unaffected: it holds to `1e-9`.

### Gamut policy

**No clamping inside either unit.** A colour that leaves `[0, 1]` after `apply_vibrance` or `apply_saturation` is returned as-is and stays finite; `combined_extremes_stay_finite` and `out_of_gamut_linear_inputs_stay_finite` prove this for the full `v = s = ±100` grid and for out-of-range linear inputs (including `1.5` and `−0.1` components). The host clamps once, at the end of the run of colour operations, per the integration contract.

**This can shift hue for strongly out-of-gamut colours — a limitation, not a bug to fix here.** Scaling `a` and `b` preserves the Oklab hue angle exactly, but an independent per-channel clamp to `[0, 1]` afterward does not: it can push different channels different (relative) distances, changing the ratio between them and therefore the reconstructed hue. Measured example: an sRGB `(255, 80, 20)` orange-red has Oklab hue 36.41°; scaling by saturation's `k` for `s = 50` already pushes the raw linear result to `[1.42, −0.06, −0.07]`, and after the host's `[0, 1]` clamp (`[1, 0, 0]`, pure red) the reconstructed hue is 29.23° — a **7.18° shift**. Further increasing `s` to 100 clamps to the same `[1, 0, 0]` and the same 29.23°: once a channel clamps, chroma keeps climbing "underneath" the clamp with no further visible change until a wider color range or a different hue reaches the boundary. Both the shift itself and this saturating-at-the-clamp behaviour are accepted limitations of scaling-then-clamping in Oklab; a production implementation must not read a clamped result's hue as if it matched the unclamped input's hue.

**Above-white behaviour.** `L > 1` (over-bright, e.g. after a large positive Exposure) is preserved through both units exactly like any other finite value — neither unit reads or constrains `L`, and the chroma-scaling formulas do not depend on `L` being in `[0, 1]`. The host's output clamp is still the only place brightness or chroma actually gets bounded.

**Non-finite handling.** Neither unit sanitizes non-finite input: a `NaN` or `±∞` component propagates through the matrices and cube root/cube exactly as IEEE 754 arithmetic defines (multiplying, cubing or cube-rooting a `NaN` yields `NaN`; the reference does not special-case this). Producing a non-finite value from finite input is exactly the failure the "combined extremes" and "out-of-gamut" tests rule out for every input this study exercises. Rejecting a non-finite *input* before it reaches a colour unit is the host's job (per the integration contract, "a non-finite value after any unit fails the render or sample with `resource-limit`"), not this unit's.

### Frozen tolerance: production versus this reference

**`1e-5 + 1e-5 × |reference|`, linear float, at most one output code of rounding difference where the design permits it** — wider than the Basic/histogram design's general default (`1e-6 + 1e-6 × |reference|`), and frozen here as the algorithm-specific exception the design anticipates ("Freeze additional per-algorithm tolerances before implementation"). Two f64-specific effects justify the widening on their own, before any f32-vs-f64 production gap is even considered:

- The independently rounded `M2⁻¹`/`M1⁻¹` matrices carry a ~10⁻⁷ round-trip residual (measured above) purely in f64.
- `signed_cbrt` and its inverse cube amplify small input differences near zero (the cube root's derivative diverges as `x → 0`), which is exactly why composing two units on a near-grey pixel reaches a ~2×10⁻⁶ spread in f64 alone (see "Near-black and achromatic behaviour").

Production additionally runs in f32 with coefficients computed in f64 (per the integration contract), so it carries further rounding from every `powf`/`cbrt` call at f32 precision; `1e-5` leaves comfortable headroom above the f64-only noise floor measured here for that additional loss. "At most one output code" mirrors the general contract's existing allowance and is expected to matter only very close to a code boundary.

## Fixtures

`fixtures/basic/colour-cases.json` (144 cases: 15 colours × 8 `(vibrance, saturation)` combinations) is generated by the reference itself — `cargo test --package lightwell-reference --test studies -- --ignored generate_colour_fixtures` — and reloaded by `colour_fixtures_match_reference`, which recomputes every case from the same reference and fails the build if the file drifts from the frozen formulas. Each case records an 8-bit sRGB input (or, for the three out-of-gamut cases, a direct linear input outside `[0, 1]`), `vibrance`, `saturation`, and the resulting linear sRGB `expected_linear` at full f64 precision, computed in the frozen order (vibrance, then saturation). Colours covered: the sRGB primaries and secondaries, three pastels, the three skin-like patches named in the task (sRGB `224,172,140`, `141,85,36`, `255,219,172`), black/mid-grey/white, and three out-of-gamut linear inputs (`[1.5, 0.5, 0.2]`, `[−0.1, 0.3, 0.8]`, `[1.5, −0.1, 0.7]`). The eight `(v, s)` combinations per colour are `(0,0)`, `(0,±100)`, `(±100,0)`, `(50,50)`, `(−100,−100)`, `(100,100)`.

## Visual review

Produced by the ignored `saturation_and_vibrance_visual_review` test in `crates/lightwell-reference/tests/studies/colour_visual.rs`: `cargo test --package lightwell-reference --test studies -- --ignored --nocapture` writes PNGs to a temp directory it prints (no images are committed, per the task rules and [fixture policy](../../fixtures/README.md)). It applies saturation at −100/−50/+50/+100 and vibrance at −100/+50/+100 to `fixtures/s0/orientation-1.jpg` (flat red/green/blue/gold quadrants) and to a synthetic 480×240 image: a hue sweep (top third, fixed moderate chroma), a chroma sweep from grey to saturated orange-red (middle third), and the three skin-like patches (bottom third).

Findings from inspecting the rendered PNGs:

- **Saturation −100** desaturates every quadrant to a correctly ordered neutral grey (the red quadrant renders lighter than the blue quadrant, matching their relative luminances) with no banding or artifacts in the flat regions.
- **Saturation +100** visibly over-saturates the already-vivid quadrant image and, on the chroma sweep, reaches solid clipped red about halfway across instead of only at the far (already-saturated) end — the uniform doubling this study specifies, working as intended but confirming it clips readily on already-saturated source colour, consistent with the documented gamut-clamp limitation above.
- **Vibrance ±100** has a much smaller visible effect than saturation on the same quadrant image (the primaries' chroma is at or past `c1`, so `w_c` is small or zero there) — the intended chroma-dependent-versus-uniform contrast is clearly visible, not just true in the numbers.
- **Vibrance +100 on the chroma sweep** visibly lifts the low/mid-chroma portion of the ramp toward saturated colour much faster than the original, then visibly levels off before the high-chroma end, distinct in shape from saturation's uniform-doubling ramp.
- **Skin-like patches**: vibrance +100 leaves the three patches close to their original appearance; saturation +100 visibly pushes them toward a more orange, less natural tone. This is the qualitative effect the skin-hue weighting is designed to produce, and it is visible, not only present in the ratio computed above.
- No posterization, banding or unexpected artifacts were observed in any flat-colour region or gradient across the reviewed set.

Limitations recorded from this review (in addition to the gamut-clamp hue shift and the skin-heuristic scope already stated above): the chroma sweep and skin patches were only reviewed at one fixed lightness (`L = 0.70`) and one fixed base hue for the chroma sweep (30°, orange-red); a full portrait photograph and a wider set of lightness/hue combinations were not available inside this environment and are left to native rendered review once the units are implemented in the editor (acceptance item 5 of the Basic and histogram plan).

## Files

| File | Purpose |
| --- | --- |
| `docs/design/basic-colour.md` | This document. |
| `crates/lightwell-reference/src/lib.rs` | Shared private sRGB decode/encode/quantize helpers for numerical-task reference modules, plus `pub mod colour;`. |
| `crates/lightwell-reference/src/colour.rs` | The frozen Oklab conversion, saturation and vibrance reference, with the property tests proved above as `#[test]` functions. |
| `crates/lightwell-reference/tests/studies/colour.rs` | Generates and reloads `fixtures/basic/colour-cases.json`. |
| `crates/lightwell-reference/tests/studies/colour_visual.rs` | The ignored visual review; writes no committed output. |
| `fixtures/basic/colour-cases.json` | Frozen expected values, reloaded by `studies/colour.rs`. |

## References

- [Björn Ottosson, "A perceptual color space for image processing" (Oklab)](https://bottosson.github.io/posts/oklab/) — source of the matrices reproduced above.
- [Basic adjustments and histogram](basic-and-histogram.md) — integration contract (unit order, domain, output boundary, general float tolerance) and the "Saturation and vibrance" numerical task description this study answers.
- [Lightroom tone/color research](../research/lightroom/tone-and-color-tools.md) — behavioural context only; no equivalence is claimed anywhere in this document.
- [W3C CSS Color 4 conversion code](https://www.w3.org/TR/css-color-4/#color-conversion-code) — an independent secondary reference for the sRGB transfer function used by `code_to_linear`/`linear_to_code`, not a dependency.
