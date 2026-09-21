# Basic Tone: the frozen global tone algorithm

Status: numerical study (TASK-011), frozen before production Tone controls are written. No production code exists yet for Contrast, Highlights, Shadows, Whites or Blacks; this document, the independent [`f64` reference](../../crates/lightwell-core/tests/reference/tone.rs) and the checked-in [oracle fixture](../../fixtures/basic/tone-cases.json) are the complete specification a later implementation of the `lightwell.basic.adjust` tone unit codes against. It answers the ["Tone contract to prove before shipping"](basic-and-histogram.md#tone-contract-to-prove-before-shipping) section of the Basic and histogram design, inside the frozen [integration contract](basic-and-histogram.md#integration-contract): the tone unit receives linear-sRGB `f32` rows after white balance and exposure and hands them to vibrance and saturation, with out-of-range and negative values preserved and no per-unit clamp; the host clamps and quantizes once, at the end of the colour-operation run.

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

**Why this domain, not linear or log luminance.** sRGB encoding is already the project's standard nonlinear tone mapping (`crates/lightwell-core/src/render.rs` uses the identical constants to decode/encode 8-bit code values), so reusing it needs no new perceptual model and no new dependency. It also does the job a tone curve needs: it compresses the linear range that photographic mid-and-high luminance occupies into a domain where a fixed-width tonal window (below) has comparable visual meaning across most of the range, unlike operating directly on `L`, where sRGB mid-grey (`L ≈ 0.18`, roughly an 18% grey card) sits far to the dark end of `[0, 1]` and most of the linear range is spent on highlights. A log domain was considered and rejected only for simplicity: it needs a floor constant for `l <= 0` (log is undefined there) and provides no benefit for photographic-range content that the already-available sRGB curve does not.

**Extrapolation is explicit and load-bearing, not incidental.** Two of the tests this domain must pass — "nondecreasing on an extended ramp" and "combined extremes stay finite" — exercise `l` outside `[0, 1]`, which happens routinely once white balance and exposure (which run before Tone) push values above 1 or, less commonly, slightly negative. `encode`/`decode` handle this by construction, with no separate extrapolation rule to maintain.

## Contrast

**Pivot.** The curve domain's mid-grey, `PIVOT = 0.5` (in encoded units), not the linear value of sRGB code 118. Reasoning: the working domain above is already the encoded domain, so its own numeric middle is the natural, dependency-free pivot; it also makes the Contrast formula below symmetric with no separate pivot-mapping step, and it is the same point the Highlights/Shadows windows are placed symmetrically around.

**Curve family.** A logistic S-curve normalized to fix `(0, 0)` and `(1, 1)`:

```text
g(u)     = 1 / (1 + exp(-alpha * (u - PIVOT)))
alpha    = ALPHA_MAX * contrast / 100          (ALPHA_MAX = 6.0)
curve(x) = (g(x) - g(0)) / (g(1) - g(0))        if contrast != 0
         = x                                    if contrast == 0
```

`contrast` is the slider value in the agreed −100..100 range. `contrast == 0` is an explicit branch (not just the formula's limit), because `alpha = 0` makes the ratio a `0/0` form; the branch also guarantees bit-exact identity, not a numerically-close approximation, at the neutral value.

**Why a logistic, not a pivot-affine or a rational-power S-curve.** A plain affine contrast (`y = pivot + c*(x - pivot)`) is unbounded: it has no toe or shoulder and pushes values arbitrarily far outside `[0, 1]` with a constant slope, so it does not "shape" the extremes the way a contrast control is expected to. A rational-power S-curve (`x^k / (x^k + (1-x)^k)`) was considered and rejected: it needs a fractional power of a possibly-negative base to extend below `0` or above `1`, so extending it to this design's unclamped domain needs a separate, ad hoc extrapolation rule. The logistic form needs none: `g` is defined and smooth for every real `u`, so `curve(x)` is defined and smooth for every real `x` with no piecewise extension.

**Monotonicity, proved, not just tested.**

```text
curve'(x) = alpha * g(x) * (1 - g(x)) / (g(1) - g(0))
```

For any finite `alpha != 0`: `g(x) in (0, 1)` strictly (the logistic never reaches its asymptotes at a finite argument), so `g(x)*(1-g(x)) > 0`. `g(1) - g(0)` is nonzero whenever `alpha != 0` (the logistic is strictly monotone in `u` for `alpha != 0`, so two different inputs give two different outputs), and its sign matches the sign of `alpha`. `alpha` and `(g(1)-g(0))` therefore always have the same sign, so their ratio — and hence `curve'(x)` — is **strictly positive for every finite `x` and every nonzero `alpha`**, with no numeric bound needed. `alpha = 0` is the identity, whose derivative is trivially `1`.

Consequence: at the extreme corners of the parameter cube (`contrast = ±100`, so `alpha = ±6`), the curve is provably monotone everywhere, though its slope can become extremely small (not negative — see "Smoothness and the extended domain" below) far from the pivot, where the logistic saturates. This is an intentional soft compression of extreme values, not a defect: those values are heading toward the output clamp anyway.

## Highlights and Shadows

**Form.** An additive offset in the same encoded domain, weighted by two raised-cosine "bump" windows — smooth tonal weights that are zero outside a window, rise smoothly (zero derivative at the edges) to `1` at the window's midpoint, and fall back to zero:

```text
bump(x; a, b) = 0                                                  if x <= a or x >= b
              = 0.5 * (1 - cos(2*PI * (x - a) / (b - a)))           otherwise

highlights_shadows(x) = x + h * bump(x; HIGHLIGHT_WINDOW) + s * bump(x; SHADOW_WINDOW)
  where h = A_HS * highlights / 100, s = A_HS * shadows / 100
  HIGHLIGHT_WINDOW = (0.45, 0.95),  SHADOW_WINDOW = (0.05, 0.55),  A_HS = 0.075
```

**Why these windows.** They are placed to give each control a clean, provable identity:

- `HIGHLIGHT_WINDOW`'s far edge (`0.95`) sits strictly below the encoded white point `1.0`, so **Highlights leaves pure white exactly unchanged** (`bump(1.0; ...) = 0` by the "zero at and above `b`" clause, not an approximation). `SHADOW_WINDOW`'s near edge (`0.05`) is strictly above the encoded black point `0.0`, so **Shadows leaves pure black exactly unchanged**, symmetrically.
- `HIGHLIGHT_WINDOW`'s near edge (`0.45`) sits just below the `0.5` pivot, giving a small, deliberate overlap with `SHADOW_WINDOW` (which is what "smooth overlapping tonal weights" in the design means concretely: both windows are simultaneously nonzero on `(0.45, 0.55)`), while keeping the leakage on the "wrong" side of the pivot small: at `x = 0.5`, `bump` is only `≈0.086` of its peak for either window (verified in `crates/lightwell-core/tests/basic_tone_reference.rs`'s `highlights_change_the_ramp_above_midtone_far_more_than_below` and its Shadows counterpart, which measure a ≈10.7x ratio between the two sides).
- The two windows are exact mirror images of each other about the pivot (`HIGHLIGHT_WINDOW = (0.45, 0.95)`, `SHADOW_WINDOW = (1 - 0.95, 1 - 0.45) = (0.05, 0.55)`), so Highlights and Shadows behave identically in shape, reflected around mid-grey — one control is not implemented more strongly than the other by construction.

**Why an additive offset, not a multiplicative gain.** An additive offset composes with the crossing-prevention-guaranteed Whites/Blacks stage (below) and the logistic Contrast stage (above) by simple function composition, and its own derivative bound (next) is a clean closed form. A multiplicative gain would need its own, separate positivity argument near zero and does not obviously combine more simply.

**Derivative bound: the maximum slope reduction, and why the composed curve stays positive.**

`bump`'s derivative is `bump'(x; a, b) = (PI / (b - a)) * sin(2*PI*(x-a)/(b-a))`, so its maximum magnitude is `PI / (b - a)`. Both windows here have width `0.5`, so:

```text
max |bump'| = PI / 0.5 = 6.2832
```

`highlights_shadows`'s derivative is `1 + h*bump'(x; HIGHLIGHT_WINDOW) + s*bump'(x; SHADOW_WINDOW)`. For any `x`, each term is bounded in magnitude by `A_HS * 6.2832` (since `|h|, |s| <= A_HS` over the agreed ±100 range), so the conservative union bound — treating both terms as simultaneously at their most negative, which they are not (their extremal locations do not coincide; see below) — is:

```text
min derivative >= 1 - 2 * A_HS * 6.2832 = 1 - 12.566 * A_HS
```

This is positive for `A_HS < 0.0796`. `A_HS = 0.075` gives a proven minimum slope of **`≈0.0575`**, strictly positive by this closed-form argument alone, independent of any numeric sampling. `A_HS` was set close to this bound (rather than left conservative) after the [visual review](#visual-evidence) below showed a materially more useful shadow lift at this amplitude than at a more conservative `0.06` (a `1.25x` stronger effect at the same safety margin's order of magnitude), while remaining analytically guaranteed positive.

The union bound is intentionally conservative: `HIGHLIGHT_WINDOW`'s extremal-slope points are at `0.45 + 0.125 = 0.575` and `0.45 + 0.375 = 0.825`; `SHADOW_WINDOW`'s are at `0.05 + 0.125 = 0.175` and `0.05 + 0.375 = 0.425`. All four are distinct `x` values, so the two terms' most-negative contributions never actually coincide, and the true worst-case slope is higher than `0.0575` in practice. The reference's monotonicity tests sample all 32 corners, all 80 edge midpoints and 200 fixed-seed random points of the 5-parameter cube, over a 1024-step neutral ramp on `[0, 1]` and a 512-step ramp on the extended domain `[-0.5, 2.0]`, and observe **zero monotonicity violations**, consistent with (and going beyond) this proof.

**Composition with the other stages stays positive.** Because Tone applies Whites/Blacks, then Highlights/Shadows, then Contrast as one function composition (see "Composition order" below), and each stage's own derivative is strictly positive everywhere on its input (Whites/Blacks: proved below; Highlights/Shadows: proved here; Contrast: proved above), the chain rule gives a strictly positive derivative for the complete composed curve at **every point in the extended domain and every combination of the five parameters in ±100**, not only the tested samples. This is why the reference's "combined extremes" test (32 corners, extended ramp) observes zero violations: it is confirming an analytic guarantee, not searching for a coincidental one.

## Whites and Blacks

**Form.** An endpoint-anchored linear remap, distinct from the Highlights/Shadows tonal weights above: it moves the white and black *points* of the encoded domain, rather than reshaping a bounded range around them.

```text
wp = 1.0 - (whites / 100) * K_W                 (K_W = 0.25)
bp =       (blacks / 100) * K_B                 (K_B = 0.25)
wp = bp + EPSILON_GAP   if wp - bp < EPSILON_GAP  (EPSILON_GAP = 0.05)
whites_blacks(x) = (x - bp) / (wp - bp)
```

Positive Whites moves `wp` below `1.0` (less input is needed to reach the encoded white point, extending highlight clipping); negative Whites moves it above `1.0` (protective, more input is needed, retaining highlight detail longer). Positive Blacks moves `bp` above `0.0` (crushing more of the dark end to encoded black); negative Blacks moves it below `0.0` (lifting, protective). This is an explicit Lightwell convention, not a claim about any other application's slider direction.

**Crossing-prevention clamp, stated exactly.** `wp` can never be reached below `bp + EPSILON_GAP`: if the raw `wp - bp` gap is smaller than `0.05`, `wp` is clamped up to `bp + 0.05`. The clamp is defensive: with `K_W = K_B = 0.25`, the worst corner (`whites = blacks = +100`) leaves `wp = 0.75`, `bp = 0.25`, a gap of `0.5`, ten times the clamp threshold — **the clamp is provably never active over the agreed ±100 range**. It exists so that a future change to `K_W`, `K_B` or the parameter range cannot silently divide by zero or invert the mapping; it is exercised directly by unit tests on the stage function even though the frozen ranges never reach it.

**Monotonicity.** `whites_blacks'(x) = 1 / (wp - bp)`, a constant. The clamp guarantees `wp - bp >= EPSILON_GAP = 0.05 > 0` always, so the derivative is a strictly positive constant — never conditional on `x` or on which corner of the parameter cube is chosen.

**Why endpoint-anchored, not a shared mechanism with Highlights/Shadows.** Whites/Blacks and Highlights/Shadows answer different questions ("where do black and white sit" versus "how much do near-black and near-white tones move without touching the endpoints"), and the [Basic controls table](basic-and-histogram.md#basic-controls-and-interaction) requires them to be controllable "separately". A shared curve family could not give Highlights/Shadows their exact-zero-at-the-opposite-endpoint property (needed above) while also letting Whites/Blacks move that same endpoint; keeping them as two composed stages keeps both properties provable independently.

## Composition order

**Whites/Blacks, then Highlights/Shadows, then Contrast** — the order the Basic and histogram design already recommends, adopted as-is:

1. **Whites/Blacks first**, so the endpoints Highlights/Shadows are defined relative to (`0.0` and `1.0` exactly) are already final before the tonal windows are evaluated. Running it later would mean a window drawn against `[0, 1]` no longer lines up with where black and white actually are once Whites/Blacks has moved them.
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

`L_in = 0` exactly is a subset of the near-black rule above (`0 < EPSILON_L`), so it is handled the same way, with no special case. Every stage of the curve (`encode`/`decode`, the linear remap, the raised-cosine bump, the logistic) is a composition of `+`, `-`, `*`, `/` (by a value bounded away from zero, per the crossing-prevention clamp), `exp` and `powf`, each of which returns a finite result for a finite input across the domains this document restricts them to (`exp`'s argument is bounded because `alpha` and `x` are both bounded on the tested/allowed domain; `powf`'s base is always positive where it is used, per the encode/decode derivation above). Given a finite input pixel and finite parameters, this reference never produces `NaN` or `±inf`; this is exercised, not merely asserted, by `combined_extremes_stay_finite_and_monotone_on_an_extended_ramp` over all 32 parameter-cube corners on the `[-0.5, 2.0]` extended ramp. The frozen algorithm has no notion of "invalid" finite input — a non-finite *input* pixel (which should not reach this unit; upstream units are responsible for that contract) is out of scope for this reference, matching [the host's own rule](basic-and-histogram.md#pointwise-colour-processing) that a non-finite value after any unit is an explicit render/sample error, never a silently-produced `NaN`.

## Frozen tolerance

Production versus this `f64` reference: **`1e-5 + 1e-5 * |reference|`** in linear float, and at most one output code of rounding difference after quantization. This is looser than the [default per-algorithm tolerance](basic-and-histogram.md#pointwise-foundation-and-exposure) (`1e-6 + 1e-6 * |reference|`), which the design explicitly allows a numerical task to freeze differently ("Freeze additional per-algorithm tolerances before implementation"): Tone composes three stages that each use `powf` or `exp` in `f32` (the encode/decode transfer function, the logistic), so its accumulated single-precision error is larger than a single conversion's. `1e-5` was not measured against an actual `f32` production implementation (none exists yet); it is a considered starting point for that implementation's own verification, to be tightened or loosened there against measured `f32` results, not re-derived from first principles.

## Limitations

This is one global, pointwise, per-pixel luminance curve. Stated honestly, it cannot do:

- **Local contrast retention on strong backlighting.** A silhouette against a bright sky, lifted by Shadows or Blacks, moves every pixel of that luminance the same way regardless of its neighbours (proved explicitly by `equal_luminance_patches_in_different_surroundings_map_identically` in the reference tests — two patches of identical colour in different surroundings always map identically). A real photograph's backlit subject often needs *more* lift near its edge (where it meets the bright background) than in its interior to look natural; this algorithm cannot give it that, by construction.
- **Highlight recovery that is already near true white tapers to nothing.** `HIGHLIGHT_WINDOW`'s far edge is `0.95`, deliberately short of the encoded white point `1.0` (so Highlights leaves pure white provably unchanged, see above). The [visual review](#visual-evidence) below found this means a background that is already close to clipping (linear ≈0.8–0.9, encoded ≈0.9–0.95) gets only a small correction from Highlights; Whites (which does reach the endpoint) is the more effective control there. This is an intentional consequence of keeping Highlights' and Whites' identities distinct and provable, not an oversight, but it is a real, user-visible limit on what Highlights alone can recover.
- **Flat or noisy shadows may still change their local contrast when lifted**, in either direction, since the curve's local slope in the lifted region is not held at exactly `1`. The [visual review](#visual-evidence) measured this directly on synthetic sensor-like shadow noise and found the change was a modest *expansion*, not a flattening, at the tested amplitude (see below) — a better outcome than the generic risk this bullet warns about, but not a guarantee for every input.

**Recorded proposal, not a decision:** an edge-aware local stage (for example, decomposing luminance into a broad base plus detail before Highlights/Shadows, adjusting the base, then recombining, as sketched for context in the [Lightroom research](../research/lightroom/tone-and-color-tools.md#highlightsshadows-versus-whitesblacks)) could address the backlit-silhouette case above. [Global tone stays global for this task](../decisions.md#basic-adjustments-and-histogram); a global pointwise curve ships with this limitation documented, and edge-aware processing remains a separate, later, separately-measured proposal, not something this document or its reference introduces.

## Visual evidence

Produced by the `#[ignore]` test `visual_review_writes_tone_sweeps_to_a_temp_dir` in `crates/lightwell-core/tests/basic_tone_reference.rs`, which writes PNGs to a temporary directory (not committed; deleted after this review) and is never run by `cargo test` or `cargo xtask check`. Three inputs, each swept at Contrast/Highlights/Shadows/Whites/Blacks = neutral and ±100 (a subset also at ±50):

**A 512x96 linear-light step wedge (0 to 1).** Every sweep stayed a smooth, monotone, hue-neutral grey ramp: a pixel-level check of one row found 198 distinct 8-bit values across 512 pixels (no large flat "steps"), a maximum adjacent-pixel jump of 7 codes, and zero decreasing steps (no inversion), for `shadows = +100`. No sweep showed banding, posterization or a colour cast.

**A 480x320 synthetic backlit subject**: a bright sky-like gradient background (linear ≈0.72–0.90) around a dark, mildly noisy foreground disc (linear ≈0.02–0.05, ±0.012 uniform noise, simulating sensor shadow noise), generated in the same test. Measured on the disc's interior (8-bit luma, `code == 0.2126*R + 0.7152*G + 0.0722*B` rounded per channel then reduced to grey for measurement):

| Sweep | Subject code range | Distinct values | Reading |
| --- | --- | --- | --- |
| Neutral | 41–60 | 20 | Baseline: noise is already visible and not flattened |
| `blacks = -100` | 84–99 | 16 | A clear, usable lift (+~40 codes); the endpoint remap dominates near-black content |
| `shadows = +100` | 48–76 | 29 | A smaller lift (+~9–16 codes) but an *expanded* range (19 to 28 codes) — local contrast in the shadow noise was not flattened at this amplitude, and the distinct-value count rose, not fell |

No posterization was visible or measured in either lifted case. `highlights = ±100` produced only a small, hard-to-see change on this background specifically, consistent with the "Limitations" finding above: this background's brightness (linear ≈0.8, encoded ≈0.90) sits near `HIGHLIGHT_WINDOW`'s far edge (`0.95`), past most of its effective range. `whites = +100` visibly clipped the background toward white; `whites = -100` visibly pulled it down to a mid grey, protecting it from clipping — both clearly legible at a glance. `contrast = ±100` visibly separated/compressed the subject-versus-background brightness, as expected, with no hue shift.

**`fixtures/s0/orientation-1.jpg`** (the existing flat-quadrant synthetic fixture: red, green, blue, gold blocks). Every sweep preserved each quadrant's hue with no visible shift, no banding within a flat block, and no inversion; `blacks = -100` visibly lifted the darkest (blue) quadrant. This fixture's flat colour blocks are a weak test of gradation (there is little tonal range within a block to show posterization or local-contrast loss), so the step wedge and backlit synthetic image above carry the bulk of this evidence; this fixture mainly confirms hue preservation and absence of gross artifacts on a non-synthetic-ramp image.

## Files

- `docs/design/basic-tone.md` — this document.
- [`crates/lightwell-core/tests/reference/tone.rs`](../../crates/lightwell-core/tests/reference/tone.rs) — the literal `f64` transcription of the equations above (`tone_pixel`, `tone_curve`, `luminance`, `ToneParams`, and this file's own private, extended sRGB encode/decode helpers).
- [`crates/lightwell-core/tests/reference/mod.rs`](../../crates/lightwell-core/tests/reference/mod.rs) — `pub mod tone;`, kept minimal so a parallel reference task's own additions merge cleanly.
- [`crates/lightwell-core/tests/basic_tone_reference.rs`](../../crates/lightwell-core/tests/basic_tone_reference.rs) — the independent proofs (identity, monotonicity over the full parameter cube plus 200 random samples, smoothness, distinct Highlights/Shadows/Whites/Blacks effects, hue preservation, the explicit global/pointwise claim), the oracle-fixture loader/checker, a `#[ignore]`d fixture regenerator, and the `#[ignore]`d visual-review PNG writer.
- [`fixtures/basic/tone-cases.json`](../../fixtures/basic/tone-cases.json) — ~90 input/parameter/expected-output cases computed by the reference, reloaded and checked bit-close (`1e-12`, to absorb JSON float round-trip only) against a fresh computation on every `cargo test` run, so production has an oracle it cannot influence.
