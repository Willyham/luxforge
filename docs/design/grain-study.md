# Grain study: the frozen hash, field, application and reduced-stage rule

Status: frozen, and implemented by the [`luxforge.grain` module](grain.md). This document, the independent [`f64` reference](../../crates/luxforge-reference/src/grain.rs) and the checked-in [oracle fixtures](../../fixtures/grain/README.md) are the complete specification the production unit, in f32 Rust and in WGSL, is checked against. It answers the [processing contract](grain.md#processing-contract) of the [grain design](grain.md): one `PointwiseColor` in the finish stage, a pure function of the output pixel's position, the three sliders and one frozen seed.

No value here claims Lightroom equivalence. The amplitudes and mappings are this study's own, chosen for clearly visible film grain at Amount 100; Lightroom's responses are fitted later by the [alignment round](grain.md#verification-and-acceptance).

## Overview

The unit runs in two parts, which the production unit transcribes in the same form and order:

1. **Compile, once per unit, in f64 on the CPU.** From the three sliders and the stage's footprint it computes the integer lattice steps of three noise components and five coefficients. The GPU receives them as uniform words; it never recomputes them.
2. **Per pixel, in f32.** From the stage pixel's integer position it hashes a few lattice points, interpolates three value noises, combines them into the field value `G`, and applies `G` to the pixel's luminance in the encoded tone domain.

Only the hash and the lattice coordinate are integer, and both are exact `u32` arithmetic, so the CPU and WGSL address the same lattice values bit for bit. Everything after is smooth floating point, so the GPU differs from the reference by f32 rounding of smooth functions alone.

## Constants

| Name | Value | Role |
| --- | --- | --- |
| `SEED` | `0x4C584752` ("LXGR") | The one frozen seed every photograph's grain comes from |
| `FINE`, `COARSE`, `CLUMP` | `0`, `1`, `2` | The three components, each its own hash stream |
| `CELL_MAX` | `6` | The fine cell at Size 100, in full-resolution output pixels |
| `COARSE_CELLS`, `CLUMP_CELLS` | `2`, `4` | The coarse and clump cells as multiples of the fine cell |
| `COARSE_MIX` | `0.75` | The coarse octave's weight at Roughness 100 |
| `CLUMP_DEPTH` | `0.9` | The clumping modulation's depth at Roughness 100 |
| `FIELD_CLAMP` | `4` | The field is clamped to ±4 standard deviations |
| `AMOUNT_MAX` | `0.05` | The tone-weighted amplitude at Amount 100 |
| `CURVATURE`, `CURVATURE_OFFSET` | `1.4`, `0.055` | The mean correction's curvature of the sRGB decode |
| `EPSILON_L` | `1e-6` | Tone's near-black threshold |
| `INTEGRAL_SCALE` (λ) | `1.35` | The fine noise's integral scale per cell, which sets the reduced-stage rule |
| `FIXED_ONE` | `2^16` | The lattice coordinate's fixed point |

## The hash

All arithmetic is `u32` with wrapping multiplication and addition, shifts and xor; WGSL's `u32` arithmetic wraps the same way.

```text
mix(x):                         -- "lowbias32", a bijection on u32
    x ^= x >> 16
    x  = x * 0x7FEB352D
    x ^= x >> 15
    x  = x * 0x846CA68B
    x ^= x >> 16
key(k)      = mix(SEED + k)                  -- one constant per component
hash(k,i,j) = mix(i + mix(j + key(k)))
value(h)    = ((h & 0xFFFF) + (h >> 16) + 1) / 65536 - 1
```

`value` is the sum of the hash's two 16-bit halves, centred: a triangular distribution on `(-1, 1)`, symmetric about 0 (a hash's bitwise complement maps to the exact negation), with variance `(1 - 2^-32)/6`. Its numerator has at most 17 significant bits and its divisor is a power of two, so it is exact in f32 as in f64. A row's `mix(j + key(k))` is shared by both columns of a lattice row, which a production unit may hoist.

Test vectors a transcription checks first (all are in [`hash-vectors.json`](../../fixtures/grain/hash-vectors.json), with more):

| Input | `mix` |
| --- | --- |
| `0x00000000` | `0x00000000` |
| `0x00000001` | `0x688990c0` |
| `0x00000002` | `0xd1132181` |
| `0x000000ff` | `0xb3443e84` |
| `0x00010000` | `0xdcafae10` |
| `0x7fffffff` | `0x8d29ffb8` |
| `0x80000000` | `0xcc4b4124` |
| `0xdeadbeef` | `0xe628c683` |
| `0xffffffff` | `0x6768824a` |

`key(FINE) = 0xa99c2dfc`, `key(COARSE) = 0x80d15dec`, `key(CLUMP) = 0x4ac8a78e`.

| Component | `i` | `j` | `hash` | `value` |
| --- | --- | --- | --- | --- |
| 0 | 0 | 0 | `0x5b3a2c9d` | -0.4693603515625 |
| 0 | 1 | 0 | `0xf7defc1c` | 0.9530487060546875 |
| 0 | 0 | 1 | `0xe9dbe01f` | 0.7889862060546875 |
| 0 | 3 | 5 | `0x2724ba6a` | -0.1189117431640625 |
| 0 | 65535 | 65536 | `0x7f685f54` | -0.1299285888671875 |
| 1 | 3 | 5 | `0x76c6a0f0` | 0.0926361083984375 |
| 1 | 4294967295 | 4294967295 | `0xb7752287` | -0.1484832763671875 |
| 2 | 0 | 0 | `0x9cb4d54f` | 0.44537353515625 |
| 2 | 123456 | 654321 | `0xff65cb59` | 0.7919769287109375 |

Over a 512 × 512 lattice of each component the measured mean is within 0.004 of 0, the variance within 0.002 of 1/6 and the neighbour correlations within 0.01 of 0 (`lattice_values_are_symmetric_with_variance_one_sixth`).

## Coordinates and the lattice

The unit is evaluated at stage pixel `(X, Y)`, the integer position in the stage it is compiled for: `x0 + offset` along the row `apply_row(y, x0, ..)` is handed. At full resolution the stage is the output stage, so `(X, Y)` is the output pixel's integer position, counted from the output stage's top-left corner. The footprint `f` on each axis is the full-resolution output pixels per stage pixel, `max(1, full / stage)` from `CompileStage`'s `full` and `stage` sizes (`1 / scale`), 1 at full resolution.

Each component has a fixed-point step on each axis (below), at most `2^16`, which is the lattice cells a stage pixel covers times `2^16`. The lattice coordinate is the exact product `X * step`, split without overflow:

```text
lattice(X, step):
    high = X >> 16;  low = X & 0xFFFF
    p    = low * step                      -- < 2^32
    i    = high * step + (p >> 16)         -- the lattice cell
    frac = p & 0xFFFF                      -- the 16-bit fraction
```

so `i = floor(X * step / 2^16)` and `frac = (X * step) mod 2^16` exactly, for every `u32` position. At full resolution the coordinate is `X / cell` to 16 fractional bits; at a reduced stage it is `X * f / cell'`, the stage pixel's centre in full-resolution coordinates less a fixed offset of `(f - 1)/2` pixels, which puts the centres on lattice points when the cell equals the footprint. The coordinate is an integer pair and a dyadic fraction, identical in both renderers.

## The noise

One component `k` at stage pixel `(X, Y)` with steps `(sx, sy)`:

```text
(i, fx) = lattice(X, sx);  (j, fy) = lattice(Y, sy)
tau  = frac / 65536                         -- exact, per axis
w1   = tau * tau * (3 - 2 * tau);  w0 = 1 - w1
v00 = value(hash(k, i, j));     v10 = value(hash(k, i + 1, j))
v01 = value(hash(k, i, j + 1)); v11 = value(hash(k, i + 1, j + 1))
top    = w0x * v00 + w1x * v10
bottom = w0x * v01 + w1x * v11
raw    = w0y * top + w1y * bottom           -- in (-1, 1)
norm   = sqrt((w0x^2 + w1x^2) * (w0y^2 + w1y^2))
normalised = raw / norm
```

`i + 1` and `j + 1` wrap. `raw` is smoothstep-interpolated value noise, a convex combination of four lattice values, so `|raw| < 1`. `normalised` divides by the root sum of the squared weights, so its variance over the hash is exactly `1/6` at every position, wherever the pixel falls in its cell; by Cauchy-Schwarz `|normalised| < 2`. Value noise is continuous across cell boundaries, so a cell index that f32 and f64 would place differently cannot occur here (the coordinate is integer), and the field is smooth in the fraction.

## The field and the sliders

**Size.** The fine cell is `cell = CELL_MAX^(size/100)` full-resolution output pixels: 1 at Size 0, `6^0.25 ≈ 1.565` at the default 25, `√6 ≈ 2.449` at 50, 6 at 100. Size is measured in full-resolution output pixels ([decided G1](grain.md#decided)), so a crop or a camera's resolution does not change it. The coarse and clump cells are 2 and 4 times it.

**Roughness.** `ρ = roughness/100`. The coarse octave's weight is `r = COARSE_MIX * ρ`, and the clumping modulation's depth `k = CLUMP_DEPTH * ρ`. At Roughness 0 the field is the fine noise alone: even, one feature size. Raising it mixes in noise of twice the cell, so features vary in size, and modulates the strength by a slow field four cells across, so grain gathers in stronger and weaker patches: uneven and clumped. Both keep the variance at 1.

**Compile (f64, once per unit).** For each component `m ∈ {1, COARSE_CELLS, CLUMP_CELLS}` and each axis with footprint `f`:

```text
c_m       = m * cell
c'_m      = max(f, sqrt(c_m^2 + (f^2 - 1) / λ^2))        -- the band-limited cell
step_m    = floor(65536 * f / c'_m + 0.5)                 -- 65536 / c_m rounded at f = 1
Λ_m       = 1 if floor(65536 / c_m + 0.5) == 65536 else λ * c_m    -- integral scale, pixels
α_m(axis) = (1 + (f^2 - 1) / Λ_m^2)^(-1/4)                -- 1 at f = 1
α_m       = α_m(x) * α_m(y)
```

and then

```text
fine_gain   = sqrt(6) * α_1 / sqrt(1 + r^2)
coarse_gain = sqrt(6) * r * α_2 / sqrt(1 + r^2)
clump_gain  = k
variance    = (fine_gain^2 + coarse_gain^2) / 6            -- 1 at full resolution
beta        = AMOUNT_MAX * amount / 100
```

The unit's uniforms are the six steps (`u32`), the four gains and variance, and `beta`. At full resolution every step is `round(65536 / c_m)`; [`field-cases.json`](../../fixtures/grain/field-cases.json) lists them for every integer Size, so a production compile can be checked word for word.

**Per pixel.**

```text
f = fine_gain * normalised(FINE)
if coarse_gain != 0:  f = f + coarse_gain * normalised(COARSE)
if clump_gain  != 0:  f = f * sqrt(1 + clump_gain * raw(CLUMP))
G = clamp(f, -FIELD_CLAMP, FIELD_CLAMP)
```

Skipping a component whose gain is 0 changes no value (`0 * x` adds exactly 0, `sqrt(1)` is 1), so Roughness 0 costs one component. The fine and coarse noises are independent with variance `1/6`, so `f` has variance `variance`; the clump factor's square has expectation exactly 1 (`raw(CLUMP)` is symmetric about 0) and lies in `(1 - k, 1 + k)`, so it scales the variance by 1 in expectation and leaves the mean at 0. At Size 0 and Roughness 0 the step is `2^16`, the fraction always 0, and `G = sqrt(6) * value(hash(FINE, X, Y))`: one independent value a pixel.

The unclamped field is bounded by `2 (fine_gain + coarse_gain) sqrt(1 + clump_gain)` (at most 9.6). The clamp at ±4 moves at most `4e-5` of the pixels ([measurements](#measurements)), so it bounds the application without changing the statistics.

## The application

On one linear-sRGB pixel, with `G` above:

```text
L = 0.2126 r + 0.7152 g + 0.0722 b                -- Tone's luminance
M = max(r, g, b)
if L <= 0 or M >= 1:  unchanged                 -- black, below black, at or above white
E     = enc(L)                                    -- Tone's extended sRGB encode
E_top = enc(L / M)                                -- the encoded luminance at which M reaches 1
t     = E / E_top                                 -- the pixel's tone within its own range, (0, 1)
s     = 4 * beta * t * (1 - t) * E_top            -- the encoded amplitude
E'    = E + s * G - 0.7 * variance * s^2 / (E + 0.055)
L'    = dec(E')                                   -- Tone's extended sRGB decode
rgb'  = rgb + (L' - L)        if L < EPSILON_L    -- Tone's near-black rule
      = rgb * (L' / L)        otherwise           -- the luminance ratio
```

`enc` and `dec` are the [Basic tone study's](basic-tone.md#working-tone-domain) extended transfer functions, and the reconstruction is its [luminance ratio and near-black rule](basic-tone.md#luminance-ratio-and-gamut-policy). Alpha is never read. Amount 0 compiles to no unit.

**Tone weight.** `t (1 - t)` vanishes at black and at the pixel's white, so grain never lifts a black or tints a highlight. `E_top` is the encoded luminance at which the pixel's largest channel reaches 1: for a grey it is 1 and `t = E`; a saturated colour has less room above it and receives proportionally less grain, which is what keeps its hue.

**Bounds, proved.** Since `L ≤ M` for any pixel whose largest channel is `M` (the weights are positive and sum to 1), `0 < E ≤ E_top`. For `|G| ≤ 4`, `beta ≤ AMOUNT_MAX = 0.05` and `0 ≤ variance ≤ 1`:

- Upper: `E' ≤ E + 4 s = E_top (t + 16 beta t (1 - t)) ≤ E_top (t + 0.8 t (1 - t)) < E_top` for `t < 1`, since the correction is never negative.
- Lower: `0.7 s^2 / (E + 0.055) ≤ 0.7 s^2 / E ≤ 11.2 beta^2 t (1 - t)^2 E_top`, so `E' ≥ E (1 - 16 beta (1 - t) - 11.2 beta^2 (1 - t)^2) ≥ E (1 - 0.8 - 0.028) > 0`.

So `0 < L' < L / M`: every channel of a pixel below white stays below white, a non-negative channel stays non-negative, and the ratio reconstruction keeps each channel's ratio to the others exactly. A pixel with any channel at or above 1, the RAW path's headroom, is unchanged.

**Mean preservation.** A zero-mean change in the encoded domain brightens linear light, because `dec` is convex. The term `0.7 variance s^2 / (E + 0.055)` is the second-order correction `½ (dec''/dec') s^2 E[G^2]` with the power branch's curvature, `dec''/dec' = 1.4 / (E + 0.055)`, used at every tone, so the correction is continuous in `E`. The field is symmetric, so the third-order term vanishes, and the fourth is below `1e-5` relative. Below the transfer function's linear branch (`E ≤ 0.04045`) the true curvature is 0 and the correction slightly over-corrects. The measured residual is at most 0.12 output code at Amount 100, against up to 0.9 code uncorrected ([measurements](#measurements)). Mean preservation is in linear light, the domain every reduction and resize averages in.

**Amplitude.** At Amount 100 a mid-grey (encoded 0.5) carries grain of standard deviation 12.9 output codes, its excursions within ±0.2 encoded of 0.5, so the grain never clips a mid-grey. The tone weight halves it near encoded 0.15 and 0.85 and takes it to about 1 code at encoded 0.02 and 0.98.

## The reduced-stage rule

A layer compiled at a stage reduced by `f` per axis evaluates the field at each stage pixel with the compile above at that footprint, and applies it with that compile's `variance`. Three things change:

1. **Band limit.** Each component's cell becomes `c'_m = max(f, sqrt(c_m^2 + (f^2 - 1)/λ^2))`. The area average of a field over `f` pixels has integral scale `sqrt(Λ^2 + f^2 - 1)` (box widths add in quadrature), and value noise of cell `c` has integral scale `λ c`, so this cell reproduces the averaged field's correlation length. Where the average would be correlated over less than `λ` stage pixels, value noise at a fractional cell cannot follow it, so the cell becomes the footprint itself: one independent lattice value a stage pixel, which is what an area average of a much finer field is.
2. **Attenuation.** Each component's gain is multiplied by `α_m`, the standard deviation of its area average relative to one pixel's. Per axis, `α^4 = 1 / (1 + (f^2 - 1)/Λ^2)` is exact for independent pixels (`Λ = 1`, `α^2 = 1/f`) and agrees to second order with a smooth field's `1 - (f^2 - 1)/(12 ℓ^2)` (a Gaussian correlation of width `ℓ` has `Λ = ℓ sqrt(2π)`). `Λ` is the measured integral scale: 1 for Size 0's fine component, `λ c_m` otherwise. Box averages of the fine noise along one axis, over 2 to 32 pixels, give `Λ / c` between 1.30 and 1.42 for every cell from 1.05 to 6, so only Size 0's cell of exactly 1 has independent pixels; λ = 1.35 fits them within the tolerance below.
3. **The mean correction** reads the compile's `variance`, the reduced field's own, so it corrects the reduced amplitude rather than the full one.

At `f = 1` every step, gain and `variance` is the full-resolution value, so a stage at full resolution renders the full field exactly. A stage larger than full resolution takes `f = 1`. The rule holds for any footprint, including Fit's non-integer one; it is measured at `f` = 2, 4 and 8.

The picture at rest, samples, the histogram and export render the full-resolution field and reduce it as [GPU-first](gpu-first.md#the-contract) reduces every stack; the reduced stage is motion, reported and not gated.

## Frozen tolerances

**Production against this reference, at full resolution and at reduced stages:** the field value `G` within `1e-5` absolute; the linear output within **`1e-5 + 1e-5 * |reference|`**, and at most one output code at quantization. This is the Basic tone study's tolerance rather than the default `1e-6`: the unit composes three square roots in the field, two extended encodes and one decode in the application, each rounded in f32. The integer parts (steps, lattice coordinates, hashes, lattice values) are equal exactly, and the f64 compile's steps are equal word for word to [`field-cases.json`](../../fixtures/grain/field-cases.json).

**The field's statistics** over a 1024 × 1024 window at the origin, at every Size and Roughness in {0, 25, 50, 75, 100}: `|mean| ≤ 0.06`, `|sd - 1| ≤ 0.02`, at most `1e-4` of the pixels past the clamp.

**Mean preservation:** the application's bias at Amount 100, measured with antithetic field values (each `G` and `-G`, which removes the window's odd moments) and the window's own second moment, at most **0.15 output code** in every channel at encoded tones 0.02 to 0.98 of a grey, an orange and a blue.

**Reduced stages,** on 256 × 256 stage pixels of flat encoded greys 0.2, 0.5 and 0.8 at Amount 100, against the full-resolution grain over the same area reduced by its linear-light area average: the standard deviation of the encoded values within **5%**, each of eight equal radial bands' share of the variance within **0.08** (the radially averaged periodogram over `[0, 0.5)` cycles a pixel), and the encoded means within **0.5 code**, at Size {0, 25, 50, 75, 100} × Roughness {0, 50, 100} and `f` = 2, 4 and 8.

## Measurements

From `print_the_study_measurements` in the study (`cargo test -p luxforge-reference --test studies -- --ignored --nocapture print_the_study_measurements`).

**Size and the spectrum** (Roughness 0, 256 × 256): the lag at which the autocorrelation falls to one half, and each radial band's share of the variance, lowest frequency first.

| Size | Cell | Fine step | Half-correlation lag (px) | Band shares |
| --- | --- | --- | --- | --- |
| 0 | 1.000 | 65536 | 0.50 | 0.017 0.047 0.078 0.111 0.136 0.173 0.201 0.238 |
| 25 | 1.565 | 41874 | 1.10 | 0.062 0.161 0.203 0.213 0.170 0.111 0.058 0.022 |
| 50 | 2.449 | 26755 | 1.69 | 0.145 0.311 0.293 0.171 0.052 0.008 0.006 0.013 |
| 75 | 3.834 | 17095 | 2.71 | 0.307 0.471 0.180 0.018 0.012 0.010 0.002 0.001 |
| 100 | 6.000 | 10923 | 4.16 | 0.591 0.369 0.021 0.015 0.002 0.001 0.001 0.001 |

Size 0 is white (its share grows with each band's area); each step of Size moves the energy down in frequency.

**Normalisation and Roughness** (1024 × 1024): the standard deviation is within 1.1% of 1 everywhere and the mean within 0.04 (the window's sampling, largest where the coarse and clump cells are widest). Roughness raises the kurtosis at fixed strength, from about 2.4 to 2.6 at Roughness 0 (lighter-tailed than a Gaussian) to 2.9 to 3.0 at 100, the signature of the clumping modulation; the share past the clamp is at most `4e-5`.

| Size | Roughness 0 | 25 | 50 | 75 | 100 |
| --- | --- | --- | --- | --- | --- |
| 0 (sd / kurtosis) | 1.0006 / 2.40 | 1.0006 / 2.45 | 1.0005 / 2.58 | 1.0003 / 2.74 | 1.0003 / 2.91 |
| 25 | 1.0014 / 2.57 | 1.0015 / 2.60 | 1.0014 / 2.70 | 1.0014 / 2.83 | 1.0014 / 2.96 |
| 50 | 1.0023 / 2.56 | 1.0026 / 2.60 | 1.0027 / 2.70 | 1.0025 / 2.83 | 1.0023 / 2.96 |
| 75 | 1.0018 / 2.57 | 1.0025 / 2.60 | 1.0033 / 2.70 | 1.0041 / 2.82 | 1.0049 / 2.95 |
| 100 | 1.0102 / 2.56 | 1.0093 / 2.59 | 1.0082 / 2.69 | 1.0071 / 2.82 | 1.0061 / 2.95 |

**Tone and mean** (grey, Size 25, Roughness 50, Amount 100):

| Encoded tone | Grain sd (codes) | Bias uncorrected (codes) | Bias corrected (codes) |
| --- | --- | --- | --- |
| 0.02 | 1.01 | -0.000 | -0.037 |
| 0.05 | 2.45 | +0.149 | -0.005 |
| 0.1 | 4.64 | +0.378 | +0.009 |
| 0.2 | 8.24 | +0.726 | +0.017 |
| 0.3 | 10.82 | +0.899 | +0.021 |
| 0.5 | 12.88 | +0.818 | +0.018 |
| 0.7 | 10.82 | +0.425 | +0.009 |
| 0.9 | 4.64 | +0.062 | +0.001 |
| 0.95 | 2.45 | +0.016 | +0.000 |

Over every tone, colour and the three field settings of the tolerance test the worst corrected bias is -0.117 code, a dark blue at encoded 0.08, in the linear branch.

**Reduced stages**, the worst over the three tones: the standard deviation ratio (reduced / target) lies in 0.991 to 1.032 everywhere. The worst band-share difference is 0.070 (`f` = 4, Size 50, Roughness 0), where the averaged field's correlation in stage pixels (1.27) falls in the gap between independent pixels (1) and the narrowest value noise (λ = 1.35); elsewhere it is below 0.06, and below 0.015 at every Size at `f` = 2. The encoded means agree within 0.11 code. A rule that kept the full-resolution cell, `sqrt(c^2 + f^2 - 1)`, matched the deviation equally but missed the bands by up to 2.2 times in power, its reduced grain blotchier than the true reduction; the integral-scale cell above replaced it.

## Worked examples

**1. A hash.** `key(FINE) = mix(0x4C584752) = 0xa99c2dfc`; `hash(FINE, 0, 0) = mix(0 + mix(0 + 0xa99c2dfc)) = 0x5b3a2c9d`, whose halves are `0x5b3a = 23354` and `0x2c9d = 11421`, so `value = (23354 + 11421 + 1)/65536 - 1 = -0.4693603515625`. (`the_hash_matches_its_frozen_test_vectors`.)

**2. The default compile.** Size 25, Roughness 50: `cell = 6^0.25 = 1.5650846`, steps `round(65536/1.5650846) = 41874`, `round(65536/3.1301692) = 20937`, `round(65536/6.2603383) = 10468`. `r = 0.375`, so `fine_gain = sqrt(6)/sqrt(1.140625) = 2.2935287`, `coarse_gain = 0.375 * 2.2935287 = 0.8600733`, `clump_gain = 0.45`, `variance = 1`. Pixel `X = 10` on the fine lattice: `10 * 41874 = 418740 = 6 * 65536 + 25524`, so `i = 6` and `tau = 25524/65536 = 0.389465`. (`worked_example_two_the_default_compile_and_a_lattice_coordinate`.)

**3. A mid-grey pixel.** Encoded 0.5 grey (linear 0.2140411), `G = 1`, Amount 100: `t = 0.5`, `s = 4 * 0.05 * 0.25 * 1 = 0.05`, `E' = 0.5 + 0.05 - 0.7 * 0.0025 / 0.555 = 0.5468468`, `L' = dec(E') = 0.2599923` on every channel. (`worked_example_three_a_mid_grey_pixel`.) At the default field's value at pixel (123, 45), `G = 0.6993226`, the same grey becomes `E' = 0.5318130` and linear 0.2446771 ([`pixel-cases.json`](../../fixtures/grain/pixel-cases.json), `flat/0.5/123x45`).

**4. A saturated colour.** `[0.8, 0.1, 0.05]` at the same pixel: `L = 0.24521`, `E = 0.53235`, `M = 0.8`, `E_top = enc(0.30651) = 0.58957`, `t = 0.90293`, so `s = 0.01033`, about a fifth of a grey's at the same luminance (0.04979); the output `[0.82341, 0.10293, 0.05146]` keeps the channel ratios exactly.

## Per-pixel cost

At Roughness above 0: three components, each four hashes of three `mix` calls (a production unit shares a row's inner `mix` between its two columns, leaving six `mix` a component), three square roots for the normalisations and one for the clump, then two encodes, one decode and a division. At Roughness 0, one component. No table, no scratch, nothing proportional to the frame; the compile is a few dozen f64 operations.

## Limitations

- **Value noise is axis-aligned.** At large Size the grain's blobs follow the lattice's rows and columns, visible as a faint squareness at Size 100 and 100%. Rotating the lattice would hide it but would break the per-axis reduced-stage rule, which is separable.
- **The reduced stage matches statistics, not pixels.** Its pattern differs from the reduction of the full-resolution pattern; where the averaged correlation falls between one and λ stage pixels its spectrum is slightly whiter than the target (band share up to 0.07 off).
- **The mean correction is second order.** In the transfer function's linear branch it over-corrects slightly, darkening deep shadows by up to 0.12 output code at Amount 100; a photograph's own window of the field has a sample mean of a few hundredths of a standard deviation, which no correction removes.
- **Saturated bright colours carry little grain.** The tone weight is measured against the pixel's own ceiling, so a colour whose largest channel is near 1 receives little grain, and one at or above 1 none, to keep its hue and never pass white.
- **One pattern for every photograph,** by the [recorded default](grain.md#proposals-with-recorded-defaults): the same output position carries the same grain on every photograph, anchored at the output stage's top-left, so a crop that moves the output's origin re-lays the pattern over the picture with the same statistics and the same size.
- **Footprints below 1 are treated as 1;** the rule is measured at integer footprints 2, 4 and 8, and specified, not measured, for Fit's non-integer ones.

## Files

- `docs/design/grain-study.md`: this document.
- [`crates/luxforge-reference/src/grain.rs`](../../crates/luxforge-reference/src/grain.rs): the `f64` transcription of every equation above (`mix`, `component_key`, `hash`, `lattice_value`, `lattice`, `weights`, `noise`, `cell`, `beta`, `Footprint`, `band_limited_cell`, `step`, `integral_scale`, `attenuation`, `Field`, `apply`, `grain_pixel`).
- [`crates/luxforge-reference/tests/studies/grain.rs`](../../crates/luxforge-reference/tests/studies/grain.rs): the proofs (hash vectors and lattice statistics, Size 0's independent pixels, normalisation, bounds and clamp, position-only dependence across tiles and windows, Size and Roughness responses, Amount 0 identity, black, white and headroom invariance, the white and black bounds, chroma, the near-black rule, mean preservation, finiteness, the full-resolution compile, the reduced-stage statistics at 2, 4 and 8, three worked examples), the fixture checks, the ignored fixture regenerator and the ignored measurement printer.
- [`fixtures/grain/`](../../fixtures/grain/README.md): the hash vectors, compiled fields and pixel cases this study freezes.
