# Basic Tone: the frozen global tone algorithm

Status: frozen and implemented as the tone unit of the Basic module. This document, the independent [`f64` reference](../../crates/luxforge-reference/src/tone.rs) and the checked-in [oracle fixture](../../fixtures/basic/tone-cases.json) are the complete specification the `luxforge.basic.adjust` tone unit is checked against. It answers the ["Tone"](basic-and-histogram.md#tone) section of the Basic and histogram design, inside the frozen [integration contract](basic-and-histogram.md#integration-contract): the tone unit receives linear-sRGB `f32` rows after white balance and exposure and hands them to vibrance and saturation, with out-of-range and negative values preserved and no per-unit clamp; the host clamps and quantizes once, at the end of the colour-operation run.

No formula below claims Lightroom or darktable numeric equivalence. [Lightroom tone/colour research](../research/lightroom/tone-and-color-tools.md) and [darktable tone/colour research](../research/darktable/tone-and-color-tools.md) are context only, as their own documents say.

## Luminance

Relative luminance uses the Rec. 709 / sRGB coefficients on **linear** sRGB:

```text
L = 0.2126*R + 0.7152*G + 0.0722*B
```

sRGB shares Rec. 709's primaries and D65 white point, so these are exactly the correct relative-luminance weights for this working space; no alternative was considered. `L` is not gamut-clamped: a triple carrying negative or >1 components from an earlier unit (white balance, exposure) yields an `L` outside `[0, 1]` too, which every later step in this document is defined to accept.

## Working tone domain

The curve does not operate on `L` directly. It operates on `L` passed through the sRGB OETF (the encode direction of the standard transfer function), **analytically continued to every finite real value**, not clamped to `[0, 1]`:

```text
encode(l) = 12.92 * l                          if l <= 0.0031308
          = 1.055 * l^(1/2.4) - 0.055           otherwise
```

The two branches of the standard piecewise formula already extend correctly with no modification: the linear branch is defined for every real `l` (including negative `l`), and the power branch is defined for every `l > 0`, which is exactly its domain (`l > 0.0031308`). No clamp is applied. The result is monotone increasing and continuous on the whole real line (though not C1 at the breakpoint, which does not matter for anything proved below). The inverse, `decode`, extends the same way and is used to map the curve's output back to linear luminance:

```text
decode(e) = e / 12.92                          if e <= 0.04045
          = ((e + 0.055) / 1.055)^2.4           otherwise
```

**Why this domain, not linear or log luminance.** sRGB encoding is already the project's standard nonlinear tone mapping (`crates/luxforge-core/src/colour.rs` holds the one sRGB transfer function the renderers decode and encode 8-bit code values with), so reusing it needs no new perceptual model and no new dependency. It also does the job a tone curve needs: it compresses the linear range that photographic mid-and-high luminance occupies into a domain where a fixed-width tonal window (below) has comparable visual meaning across most of the range, unlike operating directly on `L`, where sRGB mid-grey (`L ≈ 0.18`, roughly an 18% grey card) sits far to the dark end of `[0, 1]` and most of the linear range is spent on highlights. A log domain was considered and rejected only for simplicity: it needs a floor constant for `l <= 0` (log is undefined there) and provides no benefit for photographic-range content that the already-available sRGB curve does not.

**Extrapolation is explicit and load-bearing, not incidental.** Two of the tests this domain must pass — "nondecreasing on an extended ramp" and "combined extremes stay finite" — exercise `l` outside `[0, 1]`, which happens routinely once white balance and exposure (which run before Tone) push values above 1 or, less commonly, slightly negative. `encode`/`decode` handle this by construction, with no separate extrapolation rule to maintain.

## Contrast

**Pivot.** The curve domain's mid-grey, `PIVOT = 0.5` (in encoded units), not the linear value of sRGB code 118. Reasoning: the working domain above is already the encoded domain, so its own numeric middle is the natural, dependency-free pivot; it also makes the Contrast formula below symmetric with no separate pivot-mapping step, and it is the same point Highlights and Shadows mirror each other around.

**Curve family.** One logistic S-curve `S`, normalized to fix `(0, 0)` and `(1, 1)` and built from the slider's magnitude. The sign chooses how `S` is applied: positive Contrast is `S` itself, negative Contrast reflects `S`'s deviation from the identity and scales it by `kappa`:

```text
g(u)     = 1 / (1 + exp(-alpha * (u - PIVOT)))
alpha    = ALPHA_MAX * |contrast| / 100            (ALPHA_MAX = 6.0)
S(x)     = (g(x) - g(0)) / (g(1) - g(0))
sigma    = S'(PIVOT) = (alpha / 4) / (g(1) - g(0))   (g(PIVOT) = 1/2)
kappa    = 1 / sigma

curve(x) = S(x)                                   if contrast > 0
         = x - kappa * (S(x) - x)                 if contrast < 0
         = x                                      if contrast == 0
```

`contrast` is the slider value in the agreed −100..100 range. `contrast == 0` is an explicit branch (not just the formula's limit), because `alpha = 0` makes `S` a `0/0` form; the branch also guarantees bit-exact identity, not a numerically-close approximation, at the neutral value. `sigma`, `kappa` and the normalization depend only on `contrast`, so they are computed once per layer; each pixel costs one `exp` on either side of the slider.

**What each sign does.** `S` steepens the midtones: its slope at the pivot is `sigma = (alpha/4) / tanh(alpha/4) > 1`, and it flattens toward the toe and shoulder. Negative Contrast's slope at the pivot is `1 - kappa * (sigma - 1) = 1 / sigma`, so `-c` flattens the midtones by exactly the factor `+c` steepens them. Measured pivot slopes (`negative_contrast_has_the_reciprocal_pivot_slope_of_positive_contrast`): `1.00749`, `1.18083` and `1.65719` at `+10`, `+50` and `+100`, and `0.99257`, `0.84687` and `0.60343` at the matching negative values, each product within `2e-10` of `1`. Both signs fix both endpoints exactly (`S(0) = 0` and `S(1) = 1` give `curve(0) = 0` and `curve(1) = 1`), and both tend to the identity as `contrast` approaches `0` (`S -> x` on any bounded range, and `kappa -> 1`).

Because the endpoints are fixed, flattening the midtones steepens the ends: at `-100` the slope rises from `0.603` at the pivot to `~1.42` at `x = 0` and `x = 1`, and approaches `1 + kappa` (`~1.603`) far outside `[0, 1]`. Negative Contrast therefore spreads values outside `[0, 1]`, which an earlier unit such as Exposure can leave, further out, where positive Contrast compresses them: a linear grey of `1.6` leaves Tone at `~1.96` at `-100` and `~1.09` at `+100`. Both clip to white at the output; a colour whose luminance is past white reaches the host clamp in more channels at negative Contrast.

The magnitude response is quadratic near zero (`sigma ≈ 1 + alpha^2 / 48`), so small settings are gentle on both sides: `±10` moves no grey code of an 8-bit wedge (its largest move is about a third of a code), while `-50` and `+50` take a 256-code grey wedge's output-code standard deviation from `73.9` to `69.9` and `78.8`, and `-100` and `+100` to `65.7` and `88.9` (`negative_contrast_narrows_a_grey_step_wedge_and_positive_contrast_widens_it`).

**Why the sign is not the exponent's sign.** Negating `alpha` was tried and does not flatten anything: `g` at `-alpha` is `1 - g` at `alpha`, and the endpoint normalization cancels that reflection exactly, so `-c` rendered `+c`'s S-curve and `-100` visibly increased contrast as `+100` does.

**Why reflect with `kappa`, not invert `S`.** The functional inverse `S^-1` would undo `+c` exactly, but `S` maps the whole real line onto a bounded interval (about `(-0.052, 1.052)` at `100`), so `S^-1` is undefined for extended-domain inputs outside it, and it needs a logarithm per pixel. The reflection reuses the same logistic and its single `exp`, is defined and smooth on the whole real line, and `kappa = 1 / sigma` makes the midtone slope at `-c` the reciprocal of `+c`'s.

**Why a logistic, not a pivot-affine or a rational-power S-curve.** A plain affine contrast (`y = pivot + c*(x - pivot)`) is unbounded: it has no toe or shoulder and pushes values arbitrarily far outside `[0, 1]` with a constant slope, so it does not "shape" the extremes the way a contrast control is expected to. A rational-power S-curve (`x^k / (x^k + (1-x)^k)`) was considered and rejected: it needs a fractional power of a possibly-negative base to extend below `0` or above `1`, so extending it to this design's unclamped domain needs a separate, ad hoc extrapolation rule. The logistic form needs none: `g` is defined and smooth for every real `u`, so `curve(x)` is defined and smooth for every real `x` with no piecewise extension, on both sides of the slider.

**Monotonicity, proved, not just tested.**

```text
S'(x)     = alpha * g(x) * (1 - g(x)) / (g(1) - g(0))
curve'(x) = S'(x)                                 if contrast > 0
          = 1 + kappa * (1 - S'(x))               if contrast < 0
```

For any finite `alpha > 0`: `g(x) in (0, 1)` strictly (the logistic never reaches its asymptotes at a finite argument), so `g(x)*(1-g(x)) > 0`, and `g(1) - g(0) = tanh(alpha/4) > 0`. `S'(x)` is therefore **strictly positive for every finite `x`**, which is positive Contrast's slope. `g*(1-g)` peaks at `1/4` where `g = 1/2`, at the pivot, so `0 < S'(x) <= sigma` everywhere, and negative Contrast's slope lies in `[1 + kappa * (1 - sigma), 1 + kappa) = [1/sigma, 1 + kappa)`: **strictly positive for every finite `x`, with a closed-form floor** (`0.603` at `-100`), confirmed by forward differences over the extended grid in `negative_contrast_alone_has_a_slope_between_its_pivot_slope_and_one_plus_kappa`. `contrast = 0` is the identity, whose derivative is trivially `1`.

Consequence: at `contrast = +100` (`alpha = 6`) the curve is provably monotone everywhere, though its slope can become extremely small (not negative — see ["Monotonicity: the dense proof and its scope"](#monotonicity-the-dense-proof-and-its-scope) below) far from the pivot, where the logistic saturates. This is an intentional soft compression of extreme values, not a defect: those values are heading toward the output clamp anyway. It is also, as that section explains, the actual bottleneck for the full composition's slope on the extended domain — not Highlights/Shadows. Negative Contrast never saturates.

## Highlights and Shadows

**Revision note.** An earlier version of this stage used an additive offset weighted by fixed-width raised-cosine windows. Reviewed and replaced: that family's strength was capped by its own derivative budget (a positivity bound on the window's slope), which limited the maximum lift to about `0.075` encoded (`~19` codes) — too weak for a useful shadow-lift or highlight-recovery control. The family below has no such cap: its strength is set by `K_HS`, traded off against how gently the curve bends, not against a hard positivity ceiling.

**Form.** A windowless odds-bias curve, blended toward the identity by a weight that fades smoothly from `1` at the control's own end of the domain to `0` at the far end — "windowless" in that, unlike the earlier family, there is no fixed interval outside which the effect is exactly zero; the blend just gets very small there.

```text
B_k(x) = x / (x + (1 - x) * exp(-k))                    on [0, 1]; pass through (B_k(x) = x) outside it

w(x) = (1 - clamp(x, 0, 1))^2                             the Shadows weight: 1 at x=0, 0 at x=1

shadows(x)    = w(x) * B_{k_s}(x) + (1 - w(x)) * x,        k_s =  K_HS * shadows / 100
highlights(x) = 1 - [ w(1-x) * B_k(1-x) + (1 - w(1-x)) * (1-x) ],  k = -K_HS * highlights / 100

highlights_shadows(x) = highlights(shadows(x))             (Shadows applied first, then Highlights)

K_HS = 1.5
```

`B_k` is the "odds bias" of `x`: writing `x`'s odds as `x / (1-x)`, `B_k(x)` is the value whose odds are `exp(k)` times `x`'s odds. `k > 0` lifts (`B_k(x) > x` on `(0, 1)`), `k < 0` crushes, `k = 0` is the identity. `highlights(x)` is exactly `shadows(x)` mirrored about the pivot's midpoint (`x <-> 1-x`, `k <-> -k`), so Highlights lifts/crushes the *upper* end with the same shape Shadows uses for the lower end; `highlights = +100` lifts near white, `highlights = -100` crushes near white (compresses the top end, as the design calls for).

**Why `B_k` is exact and pole-free on `[0, 1]`, for every finite `k`.** For `x in [0, 1]`, the denominator `x + (1-x)*exp(-k)` is a convex combination (weights `x` and `1-x`, both in `[0, 1]` and summing to `1`) of `1` and `exp(-k)`, both strictly positive for any finite `k` — so the denominator is always strictly positive there, and `B_k` has no pole on `[0, 1]`, regardless of how large `|k|` is.

**Domain extension: pass-through, not linear extrapolation.** Outside `[0, 1]` the convex-combination argument above no longer holds (one of `x`, `1-x` is negative), and `B_k` genuinely has a pole there for `k < 0`: solving `x + (1-x)*exp(-k) = 0` gives `x* = exp(-k) / (exp(-k) - 1)`, which is `> 1` whenever `k < 0`. At the frozen range's extreme (`k = -K_HS = -1.5`), `x* ≈ 1.287` — inside the `[-0.5, 2.0]` domain this design tests. A linear (tangent-slope) extrapolation was tried first and rejected: `B_k`'s slope at `x = 1` is `exp(-k)`, which reaches `exp(1.5) ≈ 4.48` at the extreme, so extrapolating linearly *amplifies* an already-stretched extended-domain value (for example, a pixel Whites/Blacks has already pushed past `1`) by up to `4.48x` on top of whatever Whites/Blacks did, which was observed to push the Contrast stage's input far enough to make its saturation (see "Contrast" above) far worse. Passing values outside `[0, 1]` straight through (`B_k(x) = x` there) avoids both problems: it is finite everywhere by construction (no pole to approach), it does not amplify, and it is exactly continuous at the boundary with no special-casing, because `B_k(0) = 0` and `B_k(1) = 1` for every finite `k` (substitute `x = 0` or `x = 1` into the formula above), which is exactly what pass-through also gives at those points.

**Both endpoints are exact, for every value of `shadows` and `highlights`.** `w(0) = 1` and `B_{k_s}(0) = 0` give `shadows(0) = 0`; `w(1) = 0` gives `shadows(1) = 1 * B_{k_s}(1) + 0 = 1` (and `B_{k_s}(1) = 1` regardless of `k_s`). So **Shadows leaves pure black *and* pure white exactly unchanged**, for every `shadows` value — not just its own far end. By the mirror construction, **Highlights leaves both endpoints exactly unchanged** too. (Through the full `tone_pixel` pipeline this holds to within about `1e-9`, not always bit-for-bit: `encode(1.0)` itself is one ULP below `1.0` in `f64`, a property of the sRGB OETF's floating-point evaluation unrelated to this stage, and the mirrored `1.0 - x` in `highlights` can amplify that sub-ULP residual by up to `exp(K_HS)` before mirroring back. `crates/luxforge-reference/src/tone.rs`'s internal tests prove the curve-domain functions are bit-exact at `x = 0.0` and `x = 1.0` directly; `studies/tone.rs`'s pipeline-level test documents the `~1e-9`-safe tolerance and why.)

**No hard window, so the "far side" is not exactly zero — quantified instead.** Because the blend weight `w` only *reaches* `0` or `1` exactly at the domain's own endpoints, Highlights has a small effect everywhere except exactly at `x = 0` and `x = 1`, not zero outside a fixed window as the earlier family had. Measured (via `tone_pixel` on grey patches, `K_HS = 1.5`):

| Point | Control | Effect there | Comparison |
| --- | --- | --- | --- |
| Near-black (linear `0.02`) | Blacks | moves it substantially (e.g. `blacks = -100`: `0.02 -> ~0.084`) | Blacks' effect is `18x`-`107x` Highlights' effect at the same point, across `-100`/`-50`/`50`/`100` |
| Near-black (linear `0.02`) | Highlights | small but nonzero (`highlights = -100`: `0.02 -> ~0.0194`) | (see above) |
| Near-white (linear `0.9`) | Whites | moves it substantially (e.g. `whites = +100`: `0.9 -> ~1.737`) | Whites' effect is `620x`-`5470x` Shadows' effect at the same point |
| Near-white (linear `0.9`) | Shadows | small but nonzero (`shadows = +100`: `0.9 -> ~0.9002`) | (see above) |

**Ratio on each control's own side of the pivot.** Measured directly on `tone_curve` (encoded domain, `[0, 1]` ramp, bucketed at the `0.5` pivot): Highlights (lifting, `highlights = +100`) changes the upper half `~1.31x` more than the lower; Highlights (crushing, `highlights = -100`) changes it `~2.68x` more. Shadows is the mirror: lifting (`shadows = +100`) gives `~2.68x`, crushing (`shadows = -100`) gives `~1.31x`. The asymmetry between lifting and crushing is a real, measured property of the odds-bias family (`B_k` is not itself symmetric in `k`'s sign on a fixed-size neighbourhood of `x`), stated here rather than smoothed over.

**Quantitative lift targets.** With `K_HS = 1.5`: `shadows = +100` lifts encoded `0.10` to `~0.288` (delta `~+0.188`); `highlights = -100` lowers encoded `0.90` to `~0.712` (delta `~-0.188`). Both clear a `0.12` floor with a comfortable margin, asserted directly in `shadows_and_highlights_meet_their_quantitative_lift_targets`.

**Why Shadows then Highlights, not one folded expression.** Composing them as two sequential stages keeps each half's endpoint-exactness and monotonicity argument independently simple (each is proved on its own, then the mirror relation carries the proof to the other); folding them into one expression would not change the result (function composition is what "then" means here) but would make the endpoint and monotonicity arguments harder to state separately.

## Monotonicity: the dense proof and its scope

The revised family's own strength is no longer capped by a small closed-form derivative bound the way the bump family was, so this design relies primarily on a dense numerical proof rather than a single clean inequality (a full closed-form bound for the weighted blend above, especially composed with Whites/Blacks and Contrast, was attempted and found intractable to reduce to one inequality; the numerical evidence below is the primary proof this design relies on, not a fallback).

**Method.** Forward differences of `tone_curve` (the full 3-stage composition: Whites/Blacks, then Highlights/Shadows, then Contrast) on a 4001-point grid over `[-0.5, 2.0]`, for all 32 cube corners, all 80 edge midpoints and 200 fixed-seed random combinations of the 5-parameter cube (312 combinations total) — `dense_forward_differences_prove_monotonicity_and_record_the_minimum_slope` in `studies/tone.rs`.

**Result, stated exactly, not rounded up.** Zero monotonicity violations across all 312 combinations and the full grid (the curve is nondecreasing everywhere tested). Three minimum slopes are worth recording separately:

- **Within the primary `[0, 1]` working domain** (still all 312 combinations, still the same grid density): minimum observed slope **`≈0.0328`**, near `x = 1` at the corner `contrast = 100, highlights = -100, shadows = -100, whites = 100, blacks = 100`, clearing a `0.02` floor.
- **Over the full extended domain `[-0.5, 2.0]`**: minimum observed slope **`≈2.03e-7`**, at the same corner, near the grid's right edge. This does **not** clear `0.02`.
- **Over the full extended domain, among the combinations with Contrast below zero**: minimum observed slope **`≈0.118`**, at `x ≈ 0.39` for the corner `contrast = -100, highlights = -100, shadows = 100, whites = -100, blacks = -100`. Negative Contrast's own slope never falls below `1 / sigma` (see "Contrast"), so with it the `0.02` floor holds over the whole extended domain.

**Why the extended-domain minimum is so much smaller, and why it is not a Highlights/Shadows problem.** Isolating the revised Highlights/Shadows family alone (Whites/Blacks and Contrast held neutral) over the same extended domain and grid, its own worst-case slope is **`≈0.224`** — comfortably strong, confirming the redesign fixed the weakness this revision was asked to fix (`highlights_shadows_stage_alone_has_a_strong_worst_case_slope`). The extended-domain bottleneck traces to positive Contrast instead: at the worst corner, Whites `= 100` and Blacks `= 100` leave an endpoint gap of only `0.5` (see "Whites and Blacks" below), a `2x` amplification: the domain's right edge (`x = 2.0`) reaches Contrast's input at `~3.5`, `3.0` above `PIVOT` (Highlights/Shadows passes such extended-domain values through unchanged, per the domain-extension rule above, so it does not make this worse). Contrast's logistic with `alpha = 6` (`contrast = +100`) is already saturated to within float noise of its asymptote that far from the pivot — the saturation this document's "Contrast" section describes as intentional, and present at the same order of magnitude (`2.055e-7`) in this design's original bump-windowed Highlights/Shadows family, confirming it does not come from that stage.

**This is recorded, not silently resolved.** Whether the extended-domain bound should also be brought to `0.02` for positive Contrast (for example by reducing `ALPHA_MAX`) is an open decision; `dense_forward_differences_prove_monotonicity_and_record_the_minimum_slope` asserts what is actually true — strict positivity (`> 1e-9`, a safety margin over the observed `2.03e-7`) over the full domain, the `0.02` floor within `[0, 1]`, and at least `0.1` over the full domain for every combination with Contrast below zero — rather than asserting the unmet `0.02` bound over the full domain.

## Whites and Blacks

**Form.** An endpoint-anchored linear remap, distinct from the Highlights/Shadows blend above: it moves the white and black *points* of the encoded domain by a constant amount everywhere, rather than reshaping a range around them with a position-dependent weight.

```text
wp = 1.0 - (whites / 100) * K_W                 (K_W = 0.25)
bp =       (blacks / 100) * K_B                 (K_B = 0.25)
wp = bp + EPSILON_GAP   if wp - bp < EPSILON_GAP  (EPSILON_GAP = 0.05)
whites_blacks(x) = (x - bp) / (wp - bp)
```

Positive Whites moves `wp` below `1.0` (less input is needed to reach the encoded white point, extending highlight clipping); negative Whites moves it above `1.0` (protective, more input is needed, retaining highlight detail longer). Positive Blacks moves `bp` above `0.0` (crushing more of the dark end to encoded black); negative Blacks moves it below `0.0` (lifting, protective). This is an explicit Luxforge convention, not a claim about any other application's slider direction.

**Crossing-prevention clamp, stated exactly.** `wp` can never be reached below `bp + EPSILON_GAP`: if the raw `wp - bp` gap is smaller than `0.05`, `wp` is clamped up to `bp + 0.05`. The clamp is defensive: with `K_W = K_B = 0.25`, the worst corner (`whites = blacks = +100`) leaves `wp = 0.75`, `bp = 0.25`, a gap of `0.5`, ten times the clamp threshold — **the clamp is provably never active over the agreed ±100 range**. It exists so that a future change to `K_W`, `K_B` or the parameter range cannot silently divide by zero or invert the mapping; it is exercised directly by unit tests on the stage function even though the frozen ranges never reach it.

**Monotonicity.** `whites_blacks'(x) = 1 / (wp - bp)`, a constant. The clamp guarantees `wp - bp >= EPSILON_GAP = 0.05 > 0` always, so the derivative is a strictly positive constant — never conditional on `x` or on which corner of the parameter cube is chosen.

**Why endpoint-anchored, not a shared mechanism with Highlights/Shadows.** Whites/Blacks and Highlights/Shadows answer different questions ("where do black and white sit" versus "how much do near-black and near-white tones move, with a much smaller effect near the endpoints themselves"), and the [Basic controls table](basic-and-histogram.md#basic-controls-and-interaction) requires them to be controllable "separately". A shared curve family could not give Whites/Blacks its unconditional move-the-endpoint behaviour while also giving Highlights/Shadows its own exact-endpoint-fixing property; keeping them as two composed stages keeps both properties provable independently.

## Composition order

**Whites/Blacks, then Highlights/Shadows, then Contrast** — the order the Basic and histogram design already recommends, adopted as-is:

1. **Whites/Blacks first**, so the endpoints Highlights/Shadows are defined relative to (`0.0` and `1.0` exactly) are already final before the smooth tonal reshaping is evaluated. Running it later would mean Highlights/Shadows' fixed points no longer line up with where black and white actually are once Whites/Blacks has moved them.
2. **Highlights/Shadows second**, so the broad tonal reshaping happens before Contrast's S-curve, matching the description of Contrast as changing "midtone separation": it separates the tones Highlights/Shadows has already placed, rather than the other way around (Contrast first would mean Highlights/Shadows chase a pivot that Contrast has already steepened around, changing what "the bright half" means).
3. **Contrast last**, closest to the final curve, so its documented, fixed pivot behaviour is not itself reshaped by a later stage.

Because every stage's derivative is proved strictly positive above, this order (or, by the same chain-rule argument, any order built from the same three positive-derivative stages) preserves monotonicity; the order is chosen for what it means to a person adjusting the sliders, not because another order would fail the proof.

## Luminance ratio and gamut policy

The curve above maps one scalar, encoded luminance, to another. Reconstructing RGB uses the luminance ratio:

```text
rgb_out = rgb_in * (L_out / L_in)
```

which exactly preserves each channel's ratio to the others — hue and (linear) saturation are unchanged by construction, not merely approximately.

**Near-black rule.** Below `EPSILON_L = 1e-6` (linear luminance, absolute value), dividing by `L_in` is numerically unsafe (a near-zero or exactly-zero denominator), so the reconstruction switches to an additive rule instead:

```text
rgb_out = rgb_in + (L_out - L_in)     if |L_in| < EPSILON_L
        = rgb_in * (L_out / L_in)     otherwise
```

For a genuinely near-black or zero pixel, the ratio rule and the additive rule agree in the limit (both move the pixel toward the achromatic `[L_out, L_out, L_out]`), so the switch is not a visible discontinuity for ordinary (non-negative-channel) photographic content; the two rules can disagree by more for a pathological pixel whose linear luminance is tiny because its channels partially cancel (a preserved negative channel from an earlier unit), which is an accepted, documented edge case, not a claim of perfect continuity there.

**Gamut policy.** No clamp is applied here. Negative and >1 channel values are preserved exactly as the [integration contract](basic-and-histogram.md#pointwise-colour-processing) requires: "values outside `[0, 1]` and negative values are preserved between units... the host clamps each channel to `[0, 1]`" only once, at the end of a run of colour operations. This unit never clamps its own output.

## Zero-luminance and non-finite behaviour

`L_in = 0` exactly is a subset of the near-black rule above (`0 < EPSILON_L`), so it is handled the same way, with no special case. Every stage of the curve (`encode`/`decode`, the linear remap, the odds-bias blend, the logistic) is a composition of `+`, `-`, `*`, `/` (by a value bounded away from zero, per the crossing-prevention clamp on Whites/Blacks and the pole-free argument for `B_k` on `[0, 1]`), `exp` and `powf`, each of which returns a finite result for a finite input across the domains this document restricts them to (`exp`'s arguments are bounded because `alpha`, `k`, `k_s` and `x` are all bounded on the tested/allowed domain; `powf`'s base is always positive where it is used, per the encode/decode derivation above). Given a finite input pixel and finite parameters, this reference never produces `NaN` or `±inf`; this is exercised, not merely asserted, by `combined_extremes_stay_finite_and_monotone_on_an_extended_ramp` over all 32 parameter-cube corners on the `[-0.5, 2.0]` extended ramp. The frozen algorithm has no notion of "invalid" finite input — a non-finite *input* pixel (which should not reach this unit; upstream units are responsible for that contract) is out of scope for this reference, matching [the host's own rule](basic-and-histogram.md#pointwise-colour-processing) that a non-finite value after any unit is an explicit render/sample error, never a silently-produced `NaN`.

## Frozen tolerance

Production versus this `f64` reference: **`1e-5 + 1e-5 * |reference|`** in linear float, and at most one output code of rounding difference after quantization. This is looser than the [default per-algorithm tolerance](basic-and-histogram.md#pointwise-foundation-and-exposure) (`1e-6 + 1e-6 * |reference|`), which the design explicitly allows a numerical task to freeze differently ("Freeze additional per-algorithm tolerances before implementation"): Tone composes several stages that each use `powf` or `exp` in `f32` (the encode/decode transfer function, the odds-bias curve, the logistic), so its accumulated single-precision error is larger than a single conversion's. Measured against the production `f32` unit over the oracle fixture's 89 cases, the largest error is `~4.99e-7` in linear float (`~4.3e-7` over its 26 negative-Contrast cases), at most `5%` of the tolerance.

## Limitations

This is one global, pointwise, per-pixel luminance curve. Stated honestly, it cannot do:

- **Local contrast retention on strong backlighting.** A silhouette against a bright sky, lifted by Shadows or Blacks, moves every pixel of that luminance the same way regardless of its neighbours (proved explicitly by `equal_luminance_patches_in_different_surroundings_map_identically` in the reference tests — two patches of identical colour in different surroundings always map identically). A real photograph's backlit subject often needs *more* lift near its edge (where it meets the bright background) than in its interior to look natural; this algorithm cannot give it that, by construction.
- **Lifting noisy shadows measurably compresses their local contrast**, unlike the earlier, weaker family (which happened to expand it slightly at its lower amplitude). The [visual review](#visual-evidence) below measured this directly on synthetic sensor-like shadow noise: the disc's code range narrowed from `19` codes (neutral) to `16` (`shadows = +100`) and `15` (`shadows = -100`) — a real, now-quantified example of the generic risk this bullet warns about, not a hypothetical one. It was not visible as banding or posterization at the tested amplitude (`16`-`17` distinct 8-bit values are still present in each lifted/crushed range), but the compression is real and should be expected to grow with stronger effective lifts (a bigger source dynamic range compressed into the same output range, or a future increase to `K_HS`).
- **Highlights/Shadows have no hard cutoff, so each has a small, nonzero effect even far from its own target region** (see "No hard window" above) — a deliberate trade for removing the earlier family's strength cap, but it means, for example, that a strong Highlights adjustment measurably touches near-black content too (`highlights = -100` at encoded `~0.02`: about `3%` relative change), even though Blacks still dominates there by more than an order of magnitude.

**Recorded proposal, not a decision:** an edge-aware local stage (for example, decomposing luminance into a broad base plus detail before Highlights/Shadows, adjusting the base, then recombining, as sketched for context in the [Lightroom research](../research/lightroom/tone-and-color-tools.md#highlightsshadows-versus-whitesblacks)) could address the backlit-silhouette case and the local-contrast compression above. [Global tone stays global for this task](../decisions.md#basic-adjustments-and-histogram); a global pointwise curve ships with these limitations documented, and edge-aware processing remains a separate, later, separately-measured proposal, not something this document or its reference introduces.

## Visual evidence

Produced by the `#[ignore]` test `visual_review_writes_tone_sweeps_to_a_temp_dir` in `crates/luxforge-reference/tests/studies/tone.rs`, which writes PNGs to a temporary directory (not committed; deleted after review), prints the measurements quoted below and is never run by `cargo test` or `cargo xtask check`. Three inputs, each swept at Contrast/Highlights/Shadows/Whites/Blacks = neutral and ±100, Contrast also at ±50 (±60 on the quadrant fixture). Every number below is what the current test prints. The Contrast sweeps were last inspected by eye after negative Contrast became the reflected curve described above; the other sweeps' formulas have not changed since they were last inspected by eye, and their numbers come from the same run.

**A 512x96 linear-light step wedge (0 to 1).** Every sweep stays a smooth, monotone, hue-neutral grey ramp: one row has zero decreasing steps (no inversion) in every sweep. The longest run of identical consecutive pixels is `4`-`6` at neutral, Contrast `±50` and `-100`, Shadows `±100`, Highlights `-100`, Whites `-100` and Blacks `-100`. The longer runs are compression at an end of the range, not banding: `14` at `contrast = +100` (the saturating toe and shoulder), `16` at `highlights = +100` (just below white), and `27` at `blacks = +100` and `246` at `whites = +100` (the clipped regions past the moved black and white points). Distinct 8-bit values per sweep range `168`-`247` across the 512-pixel row (`217` at neutral; `168` at `shadows = +100`, whose lift compresses part of the range elsewhere in the ramp). `shadows = +100` and `highlights = -100` noticeably shift most of the ramp, not just a subtle band of it. Contrast orders the row's code standard deviation by its sign: `52.9` at `-100`, `55.8` at `-50`, `58.7` at neutral, `62.3` at `+50` and `70.0` at `+100`; by eye, `-100` is a visibly flatter ramp with a lighter toe and a darker shoulder, and `+100` a visibly steeper one.

**A 480x320 synthetic backlit subject**: a bright sky-like gradient background (linear ≈0.72–0.90) around a dark, mildly noisy foreground disc (linear ≈0.02–0.05, ±0.012 uniform noise, simulating sensor shadow noise), generated in the same test. Measured as Rec. 709 luma of the output's 8-bit codes, on the disc's interior (more than 2 px inside its edge) and on the background (more than 2 px outside it):

| Sweep | Subject code range | Distinct values | Subject mean | Background mean | Reading |
| --- | --- | --- | --- | --- | --- |
| Neutral | 41–60 | 20 | 50.9 | 230.5 | Baseline: background minus subject `179.6` |
| `contrast = +100` | 19–34 | 16 | 26.7 | 245.2 | Subject and background pushed apart (`218.5`); the disc's own range narrows `19 -> 15` codes in the compressed toe |
| `contrast = +50` | 33–51 | 19 | 42.8 | 236.0 | Pushed apart (`193.2`) |
| `contrast = -50` | 47–67 | 21 | 57.8 | 225.8 | Pulled together (`168.0`) |
| `contrast = -100` | 54–75 | 22 | 65.5 | 221.6 | Pulled together (`156.1`); the disc's own range widens `19 -> 21` codes, because flattening the midtones steepens the toe (see "Contrast") |
| `shadows = +100` | 95–111 | 17 | 104.2 | 230.7 | A strong, clearly usable lift (`+53` codes); range narrowed `19 -> 16` codes, a real but modest local-contrast compression (see "Limitations") |
| `shadows = -100` | 19–34 | 16 | 27.0 | 229.9 | A strong crush (`-24` codes); range narrowed `19 -> 15` |
| `highlights = +100` | 43–64 | 22 | 54.3 | 245.4 | Small change on the subject itself (expected; the subject is far from Highlights' own target region); background `+15` |
| `highlights = -100` | 40–57 | 18 | 49.4 | 184.8 | Small change on the subject; background `-46`, clearly visible |
| `whites = +100` | 54–80 | 27 | 67.9 | 255.0 | Background clipped to white |
| `whites = -100` | 33–48 | 16 | 40.7 | 184.4 | Background pulled down to a mid grey, protected from clipping |
| `blacks = +100` | 0–0 | 1 | 0.0 | 222.3 | Disc crushed to black |
| `blacks = -100` | 83–99 | 17 | 91.7 | 235.4 | A strong lift of the disc |

No posterization was visible or measured: apart from `blacks = +100`, which clips the disc to black, `16`-`27` distinct values remain within each measured range. Highlights moves this background by `+15` (`highlights = +100`) and `-46` (`highlights = -100`) codes, versus Whites' `+25`/`-46` at the same amounts, so Highlights is a comparable lever to Whites here. Contrast moves the subject and background apart at positive values and together at negative ones, with no hue shift: by eye, `contrast = -100` visibly lifts the disc and dims the sky, and `contrast = +100` visibly darkens the disc and brightens the sky toward white.

**`fixtures/s0/orientation-1.jpg`** (the existing flat-quadrant synthetic fixture: red, green, blue, gold blocks). Every sweep preserved each quadrant's hue with no visible shift, no banding within a flat block, and no inversion; `blacks = -100` visibly lifted the darkest (blue) quadrant, and `highlights = -100` visibly darkens/desaturates the green and gold quadrants as well. `contrast = -60` lightens the darker red and blue quadrants and darkens the lighter green and gold ones (blue `(40, 70, 220) -> (44, 76, 236)`, gold `(234, 195, 30) -> (223, 186, 28)`), and `contrast = +60` does the opposite (blue `-> (35, 63, 200)`, gold `-> (247, 206, 32)`), hue preserved in both. This fixture's flat colour blocks are a weak test of gradation (there is little tonal range within a block to show posterization or local-contrast loss), so the step wedge and backlit synthetic image above carry the bulk of this evidence; this fixture mainly confirms hue preservation and absence of gross artifacts on a non-synthetic-ramp image.

## Files

- `docs/design/basic-tone.md` — this document.
- [`crates/luxforge-reference/src/tone.rs`](../../crates/luxforge-reference/src/tone.rs) — the literal `f64` transcription of the equations above (`tone_pixel`, `tone_curve`, `luminance`, `ToneParams`, and this file's own private, extended sRGB encode/decode helpers).
- [`crates/luxforge-reference/src/lib.rs`](../../crates/luxforge-reference/src/lib.rs) — `pub mod tone;`, kept minimal so a parallel reference task's own additions merge cleanly.
- [`crates/luxforge-reference/tests/studies/tone.rs`](../../crates/luxforge-reference/tests/studies/tone.rs) — the independent proofs (identity, monotonicity over the full parameter cube plus 200 random samples including the required dense 4001-point/`[-0.5, 2.0]` sweep, Contrast's sign (reciprocal pivot slopes, the negative side's slope bounds, a narrower grey wedge at `-c` and a wider one at `+c`), smoothness, the quantitative Highlights/Shadows lift targets, distinct Highlights/Shadows/Whites/Blacks effects and their measured ratios, hue preservation, the explicit global/pointwise claim), the oracle-fixture loader/checker, a `#[ignore]`d fixture regenerator, and the `#[ignore]`d visual-review PNG writer, which prints the measurements quoted above.
- [`fixtures/basic/tone-cases.json`](../../fixtures/basic/tone-cases.json) — 89 input/parameter/expected-output cases computed by the reference, including every Contrast magnitude of 10, 50 and 100 on both sides across the tonal range and past white, reloaded and checked bit-close (`1e-12`, to absorb JSON float round-trip only) against a fresh computation on every `cargo test` run, so production has an oracle it cannot influence.
