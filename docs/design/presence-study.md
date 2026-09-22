# Presence: the frozen Texture, Clarity and Dehaze algorithms

Status: frozen, not yet implemented. This document, the independent [`f64` reference](../../crates/lightwell-core/tests/reference/presence.rs) and the checked-in [oracle fixtures](../../fixtures/presence/README.md) are the complete specification the `lightwell.presence` spatial operation will be checked against. It answers the ["Presence: the spatial primitive"](presence-mixer-vignette.md#presence-the-spatial-primitive) section of the Presence, colour mixer and vignette design, inside the host contract frozen there: the operation receives linear-sRGB `f32` tiles, values outside `[0, 1]` and negative values are preserved between units, no unit clamps its own output, and the host clamps and quantizes once at the output boundary.

No formula below claims Lightroom or darktable numeric equivalence. The [Lightroom detail research](../research/lightroom/detail-and-local-contrast.md) and [darktable detail research](../research/darktable/detail-and-local-contrast.md) are context and candidate mechanisms only, as their own documents say.

## Working domains

**Luminance** is the tone study's: Rec. 709 coefficients on **linear** sRGB, not gamut-clamped.

```text
L = 0.2126*R + 0.7152*G + 0.0722*B
```

**Texture and Clarity** operate on one scalar, luminance passed through the sRGB OETF [analytically continued to every finite real value](basic-tone.md#working-tone-domain):

```text
encode(l) = 12.92 * l                       if l <= 0.0031308
          = 1.055 * l^(1/2.4) - 0.055        otherwise
decode(e) = e / 12.92                       if e <= 0.04045
          = ((e + 0.055) / 1.055)^2.4        otherwise
```

Both units work in this encoded domain for the same reason the tone curve does: a fixed-width excursion there has comparable visual meaning across the range, so one gain constant is meaningful from shadows to highlights, and the existing 8-bit output encoding is reused rather than a new perceptual model introduced. The reference transcribes these two functions from the standard sRGB constants rather than importing the tone study's copy, so a later change to that file cannot silently move this study's numbers.

**RGB is reconstructed by the luminance ratio** with the tone study's additive near-black rule:

```text
rgb_out = rgb_in + (L_out - L_in)     if |L_in| < EPSILON_L   (EPSILON_L = 1e-6)
        = rgb_in * (L_out / L_in)     otherwise
```

Every cross ratio between channels is preserved exactly by construction, so hue and linear saturation are unchanged and an achromatic pixel stays achromatic bit for bit. Both are asserted directly (`texture_and_clarity_preserve_channel_ratios`, worst cross-ratio residual `< 1e-15`, and `texture_and_clarity_keep_achromatic_pixels_achromatic`, exact equality of the three channels).

**Dehaze operates on linear-light RGB**, not on luminance, because a veil is chromatic: the atmospheric light has its own colour and the model's inverse is per channel.

## What the host contract forces

Four host rules shape every choice below, and the algorithms are selected to fit them rather than the other way round.

1. **Finite support only.** Production processes 512 × 512 output tiles, reading each tile plus the operation's summed halo from the input frame with edge clamping, and the summed halo is bounded at 512 pixels. Every filter here is therefore a box mean, a box minimum, or a box-based guided filter. No Gaussian appears anywhere: a Gaussian's support is infinite, so its halo would be a truncation choice rather than a bound.
2. **Tile invariance is structural, not hoped for.** Every filter is evaluated by direct summation over its window in a fixed order (horizontal mean first, then vertical mean of that), never by a running sum, so a result does not depend on where evaluation started. Every reduced grid is anchored at the frame origin. `the_declared_halos_make_tiled_evaluation_bit_identical` evaluates six image/parameter combinations at tile sizes 16, 37 and 64 and requires **exact** equality with the whole-frame evaluation, and the reference's planes panic on any read outside the rectangle they hold, so an insufficient halo fails the test instead of quietly changing a number. A production implementation that uses running sums instead will differ within the frozen tolerance below, which is the "up to float summation order" allowance; one that reads a smaller neighbourhood will not.
3. **Large radii are computed on a reduced base.** Clarity and Dehaze reduce by an integer factor of 4 per axis before filtering. Reduction cuts the work by 16, not the neighbourhood: the halo is still roughly twice the full-resolution radius.
4. **One global estimate, bounded.** Dehaze's atmospheric light is computed from the host's 1/16-per-side reduction of the whole stage and is 3 `f64` (24 bytes), far inside the host's 4 KiB limit. Texture and Clarity declare no global estimate.

### Reduction and upsample, defined exactly

**Downsample** by integer factor `s`: the reduced frame is `ceil(width/s) x ceil(height/s)`, and reduced pixel `(i, j)` is the mean of the full pixels in `[i*s, min((i+1)*s, width)) x [j*s, min((j+1)*s, height))`. A partial block at the right or bottom edge is averaged over **its actual pixels**, never over clamped copies. The block grid is anchored at the frame origin, so a tile's reduced pixels are the frame's reduced pixels.

**Upsample** bilinearly: a full pixel `x` samples reduced coordinate `u = (x + 0.5)/s - 0.5` and blends reduced indices `floor(u)` and `floor(u) + 1`, each clamped to the reduced frame. A full pixel therefore reaches at most one reduced index beyond its own block.

**Halo of a reduced-grid stage.** Let `reach` be how many reduced pixels beyond its own index a reduced result depends on (`2r` for one guided filter, `r` for one box or min filter, and the sum along a chain). Adding the upsample's one index and allowing the full pixel to sit at the far end of its own block:

```text
halo_full = (reach + 2) * s - 1
```

**Filters, on either grid.** The box mean over a `(2r+1)^2` window has halo `r`; the box minimum the same. The box-based guided filter (He, Sun and Tang) of input `I` under guide `G`:

```text
var = max(0, mean(G*G) - mean(G)^2)       cov = mean(G*I) - mean(G)*mean(I)
a   = cov / (var + eps)                   b   = mean(I) - a * mean(G)
q   = mean(a) * G + mean(b)
```

is two sequential box passes, so its halo is `2r`. `var` is floored at zero because `mean(G*G) - mean(G)^2` can be a tiny negative number in floating point on a constant window; the floor never changes a mathematically positive variance. Texture and Clarity use the self-guided case (`G = I`), where `cov = var`, so `a = var/(var+eps)` and `b = (1-a)*mean(I)` need no second set of box passes.

## Scales, radii and halos

Every radius is quoted in pixels at a reference long side of 6000 (24 MP) and scales with the stage's long side:

```text
scaled_radius(r_6000, S) = max(1, round(r_6000 * min(S, 10000) / 6000))
```

**The scale is capped at a long side of 10000.** The host accepts stages up to 16384 px per side; letting the radii keep growing there would push the summed halo to 732 px, past the host's 512 bound, and the whole operation would be refused with `resource-limit` on a wide panorama. Capping keeps Presence available at every stage size the host accepts, at the stated cost that above 60 MP the effect is slightly finer relative to the frame than at 24 MP. This is a deliberate trade recorded here, not an oversight.

| Quantity | At long side 6000 | Rule |
| --- | --- | --- |
| Texture fine radius `r_fine` | 1 px | `scaled_radius(1, S)` |
| Texture coarse radius `r_coarse` | 4 px | `max(scaled_radius(4, S), r_fine + 1)` |
| Clarity base radius | 96 px (1.6% of the long side) | `scaled_radius(96, S)`, then `r_red = max(1, round(that / 4))` on the reduced grid |
| Dehaze dark-channel radius | 12 px (0.2%) | `scaled_radius(12, S)`, then `max(1, round(that / 4))` reduced |
| Dehaze guided radius | 24 px (0.4%) | `scaled_radius(24, S)`, then `max(1, round(that / 4))` reduced |

**The band-nondegeneracy rule** on `r_coarse` matters: without it, both Texture radii round to 1 below a long side of about 750 px, the two smoothers become identical, the band is identically zero and Texture becomes a silent no-op that is neither the identity nor an effect. Forcing `r_coarse >= r_fine + 1` keeps the unit's behaviour continuous in image size.

**Clarity's base is 1.6% of the long side, not the 2% the design sketched.** 2% would make the summed halo 528 px at 60 MP, past the bound. 1.6% is the largest round value that leaves margin (448 px of 512), and the difference between a 96 px and a 120 px base at 24 MP is not a visible distinction in a broad local-contrast base.

**Halos**, as explicit functions of the long side `S`:

```text
halo_texture(S) = 2 * r_coarse(S)
halo_clarity(S) = (2 * r_red(S) + 2) * 4 - 1
halo_dehaze(S)  = (r_dark(S) + 2 * r_guide(S) + 2) * 4 - 1        (reduced radii)
halo_presence   = halo_dehaze + halo_texture + halo_clarity       (the units are sequential)
```

| Long side | Texture | Clarity | Dehaze | Summed | Host bound |
| --- | --- | --- | --- | --- | --- |
| 480 | 4 | 23 | 19 | **46** | 512 |
| 6000 (24 MP) | 8 | 199 | 67 | **274** | 512 |
| 10000 (60 MP) | 14 | 327 | 107 | **448** | 512 |
| 16384 (the widest stage the host accepts) | 14 | 327 | 107 | **448** | 512 |

`halo_formulas_match_the_study_note_and_fit_the_host_bound` asserts every row and the bound.

**Point-sample cost.** A `render.sample` through a Presence layer evaluates the compiled prefix over the point's neighbourhood: `(2 * halo_presence + 1)^2` input pixels, so 301 thousand at 24 MP and 805 thousand at 60 MP, plus one 1/16-scale reduction of the stage on a global-estimate cache miss. This is the O(halo² × layers) exception the design declares.

## The compressive gain

Texture and Clarity both turn a raw encoded excursion into the excursion actually applied:

```text
headroom = 1 - e          if raw > 0            headroom = e          if raw < 0
lim      = min(limit, max(0, headroom))
delta    = lim * tanh(raw / lim)                (exactly 0 when lim = 0 or raw = 0)
```

Four properties this buys, each of them used below:

- **Bounded excursion.** `|delta| < lim <= limit`, so overshoot at a step edge cannot exceed the unit's limit, whatever the input contrast.
- **No new clipping.** `lim <= headroom`, so an encoded luminance inside `[0, 1]` stays strictly inside `[0, 1]`. Neither unit can create a clipped highlight or a crushed black that was not already there. `texture_and_clarity_keep_encoded_luminance_inside_the_unit_range` checks this densely on four images at three extreme parameter combinations.
- **Out-of-range values pass through.** Outside `[0, 1]` the headroom is zero, so `delta = 0` and the pixel is returned untouched rather than clamped, as the host contract requires. `delta` is still continuous in `e` there, because `lim` falls to zero continuously.
- **Monotone and exact at zero.** `delta` is strictly increasing in `raw` (derivative `sech²`, value and slope 1 at the origin), and `raw = 0` gives exactly `0` with no rounding.

## Texture

```text
E     = encode(luminance(rgb))
F     = guided_self(E, r_fine,   EPS_TEXTURE)
C     = guided_self(E, r_coarse, EPS_TEXTURE)
band  = F - C
delta = soft_clip(gain_texture(amount) * band, E, LIMIT_TEXTURE)
out   = reconstruct(rgb, L, decode(E + delta))

EPS_TEXTURE  = 2.5e-3        LIMIT_TEXTURE = 0.10
gain_texture(a) = a/100 * 3.0    for a >= 0
                = a/100 * 1.0    for a <  0
```

**Why a difference of two edge-aware smoothers.** Structure finer than `r_fine` survives both smoothers and cancels in the difference; structure coarser than `r_coarse` is reproduced by both and cancels too; only the band between them survives. That is the medium-frequency selectivity Texture is asked for, obtained with two finite-support filters instead of a pyramid. `EPS_TEXTURE = 2.5e-3` puts the guided filter's half-way point at a window standard deviation of `0.05` encoded (about 13 of 255 codes): below that the filters behave as plain box means and the band is the full difference of boxes; above it both filters' `a` approaches 1, both reproduce the edge, and the band collapses toward zero. That is what keeps a strong edge from haloing, and it is visible in the measurements: the same `+100` produces a `0.038` encoded overshoot on a low-contrast step (linear 0.35 → 0.55) but only `0.014` on a high-contrast one (0.05 → 0.75).

**Gain criterion.** At `+100` the band is multiplied by `1 + 3 = 4` before compression, which measures as a peak band gain of **3.23** at a 10-pixel period (the difference of two box filters does not isolate the band perfectly, so the realized peak is below 4). At `-100` the band is removed (`gain = 1 - 1 = 0`), which measures as **0.26** of the input amplitude surviving at the same period. The absolute excursion is bounded at `LIMIT_TEXTURE = 0.10` encoded, about 25 of 255 codes.

## Clarity

```text
E      = encode(luminance(rgb))
E_red  = downsample(E, 4)
B_red  = guided_self(E_red, r_red, EPS_CLARITY)
B      = upsample(B_red, 4)
delta  = soft_clip(gain_clarity(amount) * (E - B), E, LIMIT_CLARITY)
out    = reconstruct(rgb, L, decode(E + delta))

EPS_CLARITY = 1.0e-2        LIMIT_CLARITY = 0.15
gain_clarity(a) = a/100 * 1.00    for a >= 0
                = a/100 * 0.75    for a <  0
```

The base is computed on the reduced grid because a full-resolution guided filter at 1.6% of the long side would need 321 × 321 window sums per pixel at 60 MP; on the reduced grid the same neighbourhood costs a sixteenth of that. `EPS_CLARITY = 1.0e-2` puts the half-way point at `0.1` encoded (about 26 codes), so the broad base follows genuine edges and flattens only the lower-contrast structure between them.

**Gain criterion.** At `+100` the residual is doubled, which measures as a band gain of **2.03** from 8 px to 2.0 at 64 px periods, falling to **1.11** at 1024 px, well above its base scale. At `-100` three quarters of the residual is removed, measuring **0.27** surviving. The negative gain is deliberately not 1: removing the residual entirely replaces the image with its own base, which measured as 3% of the input amplitude surviving on low-contrast content — a blur, not the soft rendering a negative Clarity is reached for. The absolute excursion is bounded at `LIMIT_CLARITY = 0.15` encoded, about 38 codes.

## Dehaze

The atmospheric model, per channel, in linear light:

```text
I = t*J + (1 - t)*A
```

### The global estimate: atmospheric light

Computed once from the host's 1/16-per-side box-average reduction of the whole stage (at most 0.25 megapixels at the largest stage the host accepts):

```text
d(p) = min(R_r(p), R_g(p), R_b(p))                 the pointwise channel minimum
N    = clamp(max(16, ceil(0.001 * pixels)), 1, pixels)
S    = the N pixels of R with the largest d, ties broken by the smaller row-major index
A_c  = max(mean over S of R_c, A_FLOOR)            A_FLOOR = 1e-3
```

The ordering is total (value then index), so the selection is deterministic for any input, including a constant frame. The pointwise channel minimum is used rather than a second windowed dark channel because the 16 × 16 box average has already removed isolated bright pixels and the host caps this input at 0.25 MP. `A_FLOOR` exists so the division by `A_c` below is always finite. The estimate is 3 `f64`, 24 bytes.

### Transmission

On the unit's own 4× reduced grid:

```text
d(p)      = min over channels of clamp(I_red_c(p) / A_c, 0, 1)
dark(p)   = min over the (2*r_dark+1)^2 window of d
t_raw     = 1 - omega * dark                       omega = OMEGA_MAX * |amount| / 100
G         = encode(luminance(I_red))
t_reduced = guided_filter(G, t_raw, r_guide, EPS_DEHAZE)
t         = clamp(upsample(t_reduced, 4), T_FLOOR, 1)

OMEGA_MAX = 1.0    EPS_DEHAZE = 1e-4    T_FLOOR = 0.1
```

Three clamps, each part of the estimator rather than a gamut clamp on pixel values:

- **`d` is clamped to `[0, 1]`.** Without it a pixel brighter than `A` would be read as opaque haze rather than as no haze, and `t_raw` could leave `[0, 1]` entirely. With it, `t_raw` lies in `[1 - omega, 1]` for every input.
- **`T_FLOOR = 0.1`** bounds the recovery gain at 10. Where the estimate is that small the dark channel is near 1, which means the pixel is nearly as bright as `A`, so `|I - A|` is small there and the bound is rarely approached in practice.
- **A ceiling of 1** because the guided refinement can overshoot slightly past 1 next to a transition, and a transmission above 1 would attenuate the scene rather than restore it.

**`OMEGA_MAX = 1.0`, not the 0.95 the literature uses** to leave a little aerial perspective: at `+100` this unit is the exact inverse of the forward model wherever the dark-channel estimate of `t` is exact, which both makes `+100` genuinely strong and makes the recovery measurement a sharp check on the estimator rather than on a retained-veil constant.

### Applying it

```text
amount > 0 :  out_c = (I_c - A_c) / t + A_c                              (the model inverted)
amount < 0 :  t_veil = t * (1 - VEIL_MAX * |amount| / 100)               VEIL_MAX = 0.5
              out_c  = t_veil * I_c + (1 - t_veil) * A_c                 (the model forward)
```

Both branches use the same estimated transmission and the same `A`, and both are continuous at `amount = 0`, where `omega = 0` gives `t = 1` and the veil factor gives 1.

**The extra uniform factor on the negative branch is a deviation, stated plainly.** A strict `out = t*I + (1-t)*A` with the estimated `t` would be an **exact no-op on a haze-free photograph**, because the dark-channel estimate of a haze-free scene is `t = 1` everywhere — a negative Dehaze that does nothing on a clear image is not a usable control. With the factor, `-100` blends a haze-free pixel half the way toward `A` and deepens an existing veil multiplicatively, which is what more atmosphere does. `negative_dehaze_adds_the_forward_models_veil` checks the consequence that matters: on a clear scene the implied transmission is identical across the three channels to within `1e-12` (so the veil really is one transmission applied to every channel, not a per-channel curve) and equals `1 - VEIL_MAX` exactly.

The forward branch is a convex combination of `I` and `A`, so it never leaves their span. The inverse branch is not clamped, so a recovered value may leave `[0, 1]` and is preserved, per the host contract.

### Monotonicity

For `amount > 0` the deviation from `A` is `(I - A)/t` with `t` decreasing in the amount; for `amount < 0` it is `t_veil * (I - A)` with `t_veil` decreasing in `|amount|`. So `(out - A)/(I - A)` is nondecreasing across the whole `-100..+100` range, which `every_unit_is_monotone_in_its_amount` asserts at sample points on the hazed fixture. Texture and Clarity are monotone by the compressive gain's own monotonicity in `raw`, checked the same way.

## Unit order

**Dehaze, then Texture, then Clarity** — the design's default, adopted unchanged.

1. **Dehaze first** because it is a physical inversion of a degradation: it should see the recorded scene, not a scene whose local contrast two other units have already amplified. Running it later would make the dark-channel estimate a function of Texture's and Clarity's settings.
2. **Texture before Clarity** so Clarity's broad base is computed over a frame whose medium-frequency content is already final; the reverse order would make the band Texture isolates depend on how much broad contrast Clarity had added, coupling two controls that should read as independent.

The order is frozen. The halos add along it, which is what the summed bound above accounts for.

## Identity, finiteness and refusal

Each unit at amount 0 returns its input untouched through an explicit branch, not through an encode/decode round trip, so the identity is exact with no rounding — `every_unit_at_zero_is_the_exact_identity` asserts bit equality on eight images. Any pixel whose `delta` is exactly zero takes the same path, so a pass-through is exact too. A neutral payload compiles to no operation at all.

Every stage is a composition of `+`, `-`, `*`, `/` by a value bounded away from zero (`var + eps`, the transmission floor, `A_FLOOR`), `tanh`, `exp` and `powf` with a positive base, so a finite input gives a finite output; `every_output_is_finite` exercises this over every fixture at the four extreme parameter corners. A non-finite **input** is out of scope for this reference and is the host's explicit `resource-limit` error, never a silently produced `NaN`.

## Per-tile scratch

Rough figures for one 512 × 512 output tile at a 60 MP stage (long side 10000, summed halo 448), all planes `f32`:

| Buffer | Extent | Planes | Bytes |
| --- | --- | --- | --- |
| Operation input region (the host's read) | 1408 × 1408 | 3 | 22.7 MiB |
| Output tile | 512 × 512 | 3 | 3.0 MiB |
| Dehaze scratch (reduced grid ≈ 351², plus the full-resolution transmission over 1194²) | — | ≈ 11 reduced + 1 full | ≈ 10.6 MiB |
| Texture scratch (encoded plane, one base, live filter temporaries over 1166²) | 1166 × 1166 | ≈ 6 | ≈ 31.1 MiB |
| Clarity scratch (encoded plane over 1166², reduced grid ≈ 290², upsampled base over the tile) | — | ≈ 1 full + 6 reduced + 1 tile | ≈ 8.1 MiB |

**A consequence the production task must resolve, not a figure to admire.** If the units free their scratch between them, the peak is about **57 MiB for a single tile** — essentially the whole 64 MiB process-wide `ScratchBudget`, so at 60 MP no two tiles could be in flight at once and the Rayon streaming the design assumes would serialize. The cause is the ratio, not the absolute size: a 512 × 512 output tile needs a 1408 × 1408 input region, 7.6 times more input than output. Larger tiles amortize the halo far better (a 2048 × 2048 output tile needs 2944² input, 2.1 times more), and tiling each unit separately rather than the operation as a whole reduces each pass's halo to that unit's own. Both options match this reference exactly, because the reference evaluates each unit over an arbitrary output rectangle and the tiled test composes those region evaluations; neither changes a number here. Choosing between them belongs to the host task, with measurements.

## Measured figures

Every figure below is recomputed from the reference on each test run and compared against [`fixtures/presence/measurements.json`](../../fixtures/presence/measurements.json) by `measurements_match_reference`, so the note and the code cannot drift apart silently. All measurements are at a stage long side of 6000; the fixtures are small because a tile of such a stage is small.

### Step edges

Maximum encoded excursion and the furthest affected pixel from the edge, on a 1024 × 8 step (and its 8 × 1024 transpose, which gives identical figures — the filters are isotropic).

| Fixture | Unit | Amount | Overshoot (encoded) | Halo width (px) | Declared halo |
| --- | --- | --- | --- | --- | --- |
| `step-high` (linear 0.05 → 0.75) | Texture | +100 | 0.0139 | 8 | 8 |
| `step-high` | Texture | −100 | 0.0047 | 7 | 8 |
| `step-high` | Clarity | +100 | 0.1397 | 176 | 199 |
| `step-high` | Clarity | −100 | 0.1273 | 171 | 199 |
| `step-low` (linear 0.35 → 0.55) | Texture | +100 | 0.0381 | 8 | 8 |
| `step-low` | Texture | −100 | 0.0133 | 7 | 8 |
| `step-low` | Clarity | +100 | 0.0621 | 171 | 199 |
| `step-low` | Clarity | −100 | 0.0478 | 167 | 199 |

Both units stay inside their frozen limits (0.10 and 0.15 encoded) and inside their declared halos, asserted by `step_edge_overshoot_stays_inside_the_frozen_limits`. Clarity's 176 px halo at 24 MP is the honest cost of a 96 px base: a broad local-contrast control produces a broad, low-amplitude gradient beside a strong edge. It is bounded, not absent.

### Band response

Encoded peak-to-trough gain on a sinusoid of the given period (encoded amplitude 0.005, measured away from the frame edges). Periods of 2 and 3 pixels are not usable probes on an integer grid — a period-2 sinusoid samples to a constant and a period-3 one is annihilated exactly by the three-tap fine filter — so 2.5 px is the finest period quoted.

| Period (px) | 2.5 | 4 | 6 | 8 | 10 | 12 | 16 | 32 | 64 | 128 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Texture +100 | 1.09 | 1.30 | 2.18 | 2.89 | **3.23** | 3.19 | 2.75 | 1.62 | 1.17 | 1.04 |
| Texture −100 | 0.97 | 0.90 | 0.61 | 0.37 | **0.26** | 0.27 | 0.41 | 0.79 | 0.94 | 0.99 |

| Period (px) | 8 | 32 | 64 | 128 | 256 | 512 | 1024 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Clarity +100 | 2.03 | 2.01 | 2.00 | 1.96 | 1.92 | 1.40 | 1.11 |
| Clarity −100 | 0.28 | 0.27 | 0.27 | 0.29 | 0.31 | 0.70 | 0.91 |

Texture peaks at a 10-pixel period and falls to 1.09 at the finest usable period and 1.04 at 128 px. Clarity is flat across everything below its base scale and falls away above it. `texture_is_a_medium_frequency_band` and `clarity_is_a_broader_band_than_texture` assert the shape.

**"Very fine detail nearly unchanged" is only approximately true.** At 4 px the gain is still 1.30, not 1.0. The three-tap fine filter suppresses the finest structure but does not null it; a true zero at the sampling limit would need a different fine filter and is not part of this freeze.

### Quantization on an 8-bit gradient

On a 512 × 8 ramp whose input steps are exactly one 8-bit code every four columns, the largest step between adjacent output codes:

| Setting | Maximum output code step |
| --- | --- |
| Texture +100 | 1 |
| Clarity +100 | 2 |
| Texture +100 and Clarity +100 | 3 |

This is the "extra quantization boundary" evidence the design asks for, and it behaves as amplification must: a unit that doubles local contrast doubles the quantization steps already present in an 8-bit source. Nothing here creates a boundary that was not in the input; it widens existing ones, which on a smooth 8-bit gradient is where banding would first become visible. The 3-code figure with both units maxed is the worst case measured and is asserted as the frozen bound.

### Noise amplification

Ratio of output to input standard deviation of encoded luminance, on 96 × 96 uniform noise about linear 0.2.

| Linear amplitude | Texture +100 | Texture −100 | Clarity +100 |
| --- | --- | --- | --- |
| 0.005 | 1.40 | 0.92 | 2.00 |
| 0.02 | 1.39 | 0.92 | 1.99 |
| 0.08 | 1.14 | 0.97 | 1.93 |

Texture amplifies broadband noise **less** than Clarity does, because its band is narrow and noise is not. At the largest amplitude Texture's edge awareness starts treating the noise as structure and its amplification falls to 1.14 — which is also a warning: at that amplitude the noise is being protected rather than boosted, so the control's effect on real texture at the same contrast is reduced too.

### Haze recovery

A synthetic scene built through the forward model with a known `t` and `A = [0.85, 0.88, 0.95]`, containing black cells so the dark-channel prior holds in every window and 32 rows of pure haze so the atmospheric-light estimator has something to find. Error is the largest linear-light difference between the `+100` result and the clear scene it was built from, over the region the refinement is not mixing across the sky boundary in (the top `32 + halo_dehaze` rows are excluded).

| Fixture | Transmission | Maximum recovery error |
| --- | --- | --- |
| `haze-flat` | constant 0.4 | **6.3e-15** (float noise) |
| `haze-ramp-gentle` | 0.38 → 0.42 across 128 px | **0.0156** |
| `haze-ramp` | 0.25 → 0.90 across 128 px | **0.303** |

The estimated `A` matches the true `A` to within `1e-12` on these fixtures. With a constant transmission the inversion is exact to float noise, which is the sharpest available statement that the estimator and the model inverse agree. With a varying transmission the dark channel's own min filter reports the **largest** transmission in its window, so `t` is over-estimated by roughly the gradient times the window reach and part of the veil is left in place. The steep fixture's gradient (0.65 of transmission over 128 px, evaluated with 24 MP radii) is about thirty times steeper than aerial perspective across a whole 6000 px frame, so 0.303 is a deliberate worst case; 0.0156 on the gentle ramp is the more representative figure. This is a property of the dark-channel prior, recorded rather than hidden.

### f32 filter error and the frozen tolerance

The self-guided filter evaluated in `f32` against the same filter in `f64`, on the encoded luminance of the textured patch:

| Window radius | Maximum difference |
| --- | --- |
| 1 (Texture fine) | 5.0e-7 |
| 4 (Texture coarse) | 3.5e-7 |
| 24 (Clarity, reduced grid) | 1.2e-6 |

The error grows with the window, as box sums over larger windows must: a 49 × 49 window accumulates about 2400 additions before dividing.

## Frozen tolerance

Production versus this `f64` reference: **`2e-4 + 2e-4 * |reference|`** in linear light, and at most one output code of difference after quantization.

How it was estimated, from the measured figures above rather than from first principles:

- The worst measured `f32` filter error at a frozen radius is `1.2e-6` in the encoded domain.
- **Through Texture and Clarity** it is multiplied by the gain (at most 3) and then by the encoded-to-linear slope, at most `2.4/1.055 = 2.28` at encoded 1.0: `1.2e-6 * 3 * 2.28 = 8.3e-6`.
- **Through Dehaze** an error in the transmission propagates through `(I - A)/t`, whose sensitivity is `|I - A| / t²`; at the transmission floor with a full-range veil that is `1.2e-6 / 0.01 = 1.2e-4`. This path, not the luminance units, is what sets the tolerance.
- `2e-4` covers the larger path with headroom and is still below one 8-bit code everywhere, including near black where a code spans about `3e-4` of linear light.

`the_frozen_production_tolerance_covers_the_measured_f32_filter_error` asserts both propagation paths against this number, so a later change to a radius or a gain that would invalidate it fails the build. No `f32` production implementation exists yet: this is a considered starting point for that implementation's own verification, to be tightened or loosened there against measured results, exactly as the [tone](basic-tone.md#frozen-tolerance) and [colour](basic-colour.md) studies say of theirs.

## Rejected candidates

- **Local Laplacian filtering** (the published foundation behind Lightroom-era Clarity, per the [Lightroom research](../research/lightroom/detail-and-local-contrast.md#published-local-laplacian-foundation)). Rejected: it cannot tile within the 512 px summed halo. The pinned darktable source is explicit that its own local-Laplacian path sets `process_tiling_ready = FALSE`, as the [darktable research](../research/darktable/detail-and-local-contrast.md#local-contrast-two-processing-paths) records. A pyramid's coarsest level depends on the whole frame, so there is no finite halo to declare; adopting it would mean either abandoning tiled execution or truncating the pyramid, which is a different algorithm with no stated bound.
- **Bilateral grid** (darktable's other local-contrast path). Rejected for the same reason in a weaker form: the grid is a full-frame structure, so a tile's slice depends on splats from outside its halo. A per-tile grid would not be tile-invariant, which the host requires.
- **Edge-aware wavelets** (darktable's contrast equalizer). Rejected: the à trous decomposition's support doubles per level, so a decomposition deep enough for a 96 px base reaches far past the halo bound, and a shallow one is a difference of box filters with more machinery.
- **Gaussian unsharp masking** for Texture. Rejected: infinite support, so any halo is a truncation choice rather than a bound; and without edge awareness the step-edge overshoot has no mechanism to collapse, which the measured `0.014` on a high-contrast step shows the guided pair does have.
- **A full-resolution guided filter for Clarity's base.** Rejected on cost, not correctness: 321 × 321 window sums per pixel at 60 MP, for a base that is by definition smooth. The reduced grid gives the same neighbourhood for a sixteenth of the work and the same halo.
- **`omega = 0.95` for Dehaze** (He et al.'s constant, kept to preserve a little aerial perspective). Rejected: it would make `+100` a systematically incomplete inverse and would blunt the recovery measurement into a check on a constant.
- **A per-channel dark channel with a spatial window for `A`.** Rejected: the host's 16 × 16 reduction already removes isolated bright pixels, and a second windowed estimator would add a parameter without a measurement to justify it.

## Limitations

Stated honestly, this is what the three frozen units cannot do.

- **Clarity's halo is 176 px wide at 24 MP.** A broad local-contrast control necessarily produces a broad gradient beside a strong edge. The compressive gain bounds its amplitude at 0.15 encoded and the guided base keeps it off genuine edges, but it is present and measurable, and at `+100` on a high-contrast edge it reaches 0.1397 of that bound.
- **Dehaze under-recovers a rapidly varying veil.** The dark channel's min filter reports the largest transmission in its window, so a transmission gradient leaves part of the veil in place: 0.0156 of linear light on a gentle ramp, 0.303 on a deliberately extreme one.
- **Dehaze's negative branch is not purely the estimated transmission.** As stated above, a uniform factor is required for the control to do anything on a haze-free photograph. The veil it adds on such an image is uniform, not depth-shaped, because a haze-free image carries no depth information to shape it with.
- **Texture still amplifies the finest structure by 1.30x at `+100`.** "Very fine detail unchanged" is approximate, not exact.
- **Neither luminance unit distinguishes noise from texture.** Texture amplifies 0.02-amplitude noise by 1.39, Clarity by 1.99. Noise and real texture overlap in scale; no threshold here separates them, and none is claimed. Detail and noise reduction remain a separate, later module with its own design.
- **Above a 60 MP stage the radii stop scaling,** so the effect is finer relative to the frame on a 16384 px panorama than on a 24 MP photograph. The alternative was refusing the operation entirely on such a stage.
- **Clarity's base is quantised to the 4× reduced grid.** Below a long side of about 1000 px the reduced radius rounds to 1 or 2 and the effective base is broader, relative to the frame, than 1.6% of the long side. The fixtures exercise that path deliberately; the scale-covariance claim is made for stages large enough for the rounding not to dominate.
- **No visual review on photographs was performed.** Every figure here is from synthetic fixtures. The design's native M4 rendered evidence, inspected for halos and shadow noise on real photographs, is a later task and may yet send a constant back here.

## Files

- `docs/design/presence-study.md` — this document.
- [`crates/lightwell-core/tests/reference/presence.rs`](../../crates/lightwell-core/tests/reference/presence.rs) — the literal `f64` transcription of the equations above: the plane/rectangle machinery that makes halo violations fail loudly, the finite-support filters, the reduction and upsample, the compressive gain, the three units, the atmospheric-light estimator and the whole-frame `apply_presence`.
- [`crates/lightwell-core/tests/reference/mod.rs`](../../crates/lightwell-core/tests/reference/mod.rs) — `pub mod presence;`, unchanged by this study.
- [`crates/lightwell-core/tests/presence_reference.rs`](../../crates/lightwell-core/tests/presence_reference.rs) — the deterministic image specs, the property proofs (exact identity, bit-identical tiled evaluation, halo table, achromatic and hue invariance, the encoded range bound, monotonicity, finiteness), the measurements, the `f32` comparison, the `#[ignore]`d fixture generator and the two reload checks.
- [`fixtures/presence/cases.json`](../../fixtures/presence/cases.json) — ten oracle cases on 24 × 24 images at two stage sizes, with full `f64` output.
- [`fixtures/presence/measurements.json`](../../fixtures/presence/measurements.json) — the 76 figures quoted above.
- [`fixtures/presence/README.md`](../../fixtures/presence/README.md) — what each fixture is and the frozen tolerance.
