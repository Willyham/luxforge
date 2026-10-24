//! Independent f64 reference for grain: the frozen hash, noise field, Size and Roughness mappings,
//! tone weight, application and reduced-stage rule of `docs/design/grain-study.md`.
//!
//! This module shares no code with production. It exists so the later `luxforge.grain` unit, in
//! f32 Rust and WGSL, has an oracle it cannot influence. The study writes every equation out in
//! the form and order a production unit transcribes; this file is their literal transcription and
//! the two must be read together. Every constant below is named as the study names it.
//!
//! The hash and the lattice coordinates are exact `u32` arithmetic, so both renderers address the
//! same lattice values bit for bit; only the interpolation, the modulation and the application are
//! floating point. The sRGB transfer function comes from [`crate::srgb`], the luminance weights
//! from [`crate::tone`], the one copy of each in this crate.
//!
//! Domain: finite linear-sRGB input, values outside `[0, 1]` accepted. Non-finite input is out of
//! scope, as it is for every reference here: production refuses it before this stage.

use crate::srgb::{decode_encoded, encode_extended};
use crate::tone::luminance;

// ---- Frozen constants ----------------------------------------------------------------------------

/// The one frozen seed every photograph's grain is generated from ("LXGR").
pub const SEED: u32 = 0x4C58_4752;
/// The three independent noise components, each its own hash stream.
pub const FINE: u32 = 0;
pub const COARSE: u32 = 1;
pub const CLUMP: u32 = 2;
/// Each component's cell, as a multiple of the fine cell.
pub const COARSE_CELLS: f64 = 2.0;
pub const CLUMP_CELLS: f64 = 4.0;
/// The fine cell at Size 100, in full-resolution output pixels. Size 0 is one pixel.
pub const CELL_MAX: f64 = 6.0;
/// The coarse octave's weight relative to the fine one at Roughness 100.
pub const COARSE_MIX: f64 = 0.75;
/// The clumping modulation's depth at Roughness 100: `A^2` runs over `1 +- CLUMP_DEPTH`.
pub const CLUMP_DEPTH: f64 = 0.9;
/// The field is clamped to `+-FIELD_CLAMP` standard deviations before it is applied.
pub const FIELD_CLAMP: f64 = 4.0;
/// The tone-weighted amplitude Amount 100 reaches: `beta = AMOUNT_MAX * amount / 100`.
pub const AMOUNT_MAX: f64 = 0.05;
/// The mean correction's curvature, `decode''/decode' = CURVATURE / (E + CURVATURE_OFFSET)` on
/// the sRGB power branch, used at every non-negative encoded value.
pub const CURVATURE: f64 = 1.4;
pub const CURVATURE_OFFSET: f64 = 0.055;
/// The near-black rule's threshold, Tone's `EPSILON_L`.
pub const EPSILON_L: f64 = 1e-6;
/// The lattice coordinate's fixed point: 16 fractional bits.
pub const FIXED_ONE: u32 = 1 << 16;
/// The value noise's integral scale per cell, which sets the reduced-stage band limit and
/// attenuation.
pub const INTEGRAL_SCALE: f64 = 1.35;

// ---- The hash ------------------------------------------------------------------------------------

/// The 32-bit finaliser ("lowbias32"): xor-shift, multiply, xor-shift, multiply, xor-shift, in
/// `u32` wrapping arithmetic only. A bijection on `u32`.
pub fn mix(x: u32) -> u32 {
    let mut x = x;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x
}

/// A component's key, `mix(SEED + component)`: one constant per component.
pub fn component_key(component: u32) -> u32 {
    mix(SEED.wrapping_add(component))
}

/// The hash of lattice point `(i, j)` of `component`: `mix(i + mix(j + key))`.
pub fn hash(component: u32, i: u32, j: u32) -> u32 {
    mix(i.wrapping_add(mix(j.wrapping_add(component_key(component)))))
}

/// A hash's lattice value: the sum of its two 16-bit halves, centred, in `(-1, 1)`. Triangular,
/// symmetric about 0, variance `(1 - 2^-32) / 6`. Exact in f32 as in f64: the numerator has at
/// most 17 significant bits and the divisor is a power of two.
pub fn lattice_value(hash: u32) -> f64 {
    let low = hash & 0xFFFF;
    let high = hash >> 16;
    f64::from(low + high + 1) / 65536.0 - 1.0
}

// ---- The lattice ---------------------------------------------------------------------------------

/// A stage pixel index's lattice cell and fraction for a fixed-point `step` (lattice cells per
/// stage pixel, times `2^16`, at most `2^16`): `index * step` split into its integer part `i` and
/// its 16-bit fraction, without overflowing `u32`.
pub fn lattice(index: u32, step: u32) -> (u32, u32) {
    assert!(step <= FIXED_ONE, "a step of {step} passes 2^16");
    let high = index >> 16;
    let low = index & 0xFFFF;
    let low_product = low * step;
    (high * step + (low_product >> 16), low_product & 0xFFFF)
}

/// The two interpolation weights at a 16-bit fraction: `w1 = tau^2 (3 - 2 tau)`, `w0 = 1 - w1`.
pub fn weights(fraction: u32) -> (f64, f64) {
    let tau = f64::from(fraction) / 65536.0;
    let w1 = tau * tau * (3.0 - 2.0 * tau);
    (1.0 - w1, w1)
}

/// One component's value noise at a stage pixel: `(raw, normalised)`. `raw` is the smooth
/// interpolation of the four lattice values, in `(-1, 1)`; `normalised` divides it by the root sum
/// of squared weights, so its variance over the hash is `1/6` at every position.
pub fn noise(component: u32, x: u32, y: u32, step: (u32, u32)) -> (f64, f64) {
    let (i, fx) = lattice(x, step.0);
    let (j, fy) = lattice(y, step.1);
    let (w0x, w1x) = weights(fx);
    let (w0y, w1y) = weights(fy);
    let v00 = lattice_value(hash(component, i, j));
    let v10 = lattice_value(hash(component, i.wrapping_add(1), j));
    let v01 = lattice_value(hash(component, i, j.wrapping_add(1)));
    let v11 = lattice_value(hash(component, i.wrapping_add(1), j.wrapping_add(1)));
    let top = w0x * v00 + w1x * v10;
    let bottom = w0x * v01 + w1x * v11;
    let raw = w0y * top + w1y * bottom;
    let norm = ((w0x * w0x + w1x * w1x) * (w0y * w0y + w1y * w1y)).sqrt();
    (raw, raw / norm)
}

// ---- Parameters and mappings ---------------------------------------------------------------------

/// The three sliders, each in `0..=100`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrainParams {
    pub amount: f64,
    pub size: f64,
    pub roughness: f64,
}

impl GrainParams {
    /// The all-default payload `{}`: Amount 0, the neutral layer.
    pub const DEFAULT: Self = Self {
        amount: 0.0,
        size: 25.0,
        roughness: 50.0,
    };
}

/// The fine cell in full-resolution output pixels: `CELL_MAX^(size/100)`, 1 at Size 0.
pub fn cell(size: f64) -> f64 {
    CELL_MAX.powf(size / 100.0)
}

/// The tone-weighted amplitude at `amount`.
pub fn beta(amount: f64) -> f64 {
    AMOUNT_MAX * amount / 100.0
}

/// A stage's footprint: full-resolution output pixels per stage pixel on each axis,
/// `full / stage`, 1 at full resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Footprint {
    pub x: f64,
    pub y: f64,
}

impl Footprint {
    pub const FULL: Self = Self { x: 1.0, y: 1.0 };

    /// The footprint of a `stage` evaluated for a `full` output stage, both `(width, height)`:
    /// `max(1, full / stage)` on each axis, so a stage is never treated as finer than full
    /// resolution.
    pub fn of(stage: (u32, u32), full: (u32, u32)) -> Self {
        Self {
            x: (f64::from(full.0) / f64::from(stage.0)).max(1.0),
            y: (f64::from(full.1) / f64::from(stage.1)).max(1.0),
        }
    }
}

/// The band-limited cell on one axis, in full-resolution pixels:
/// `max(footprint, sqrt(cell^2 + (footprint^2 - 1) / INTEGRAL_SCALE^2))`, the cell itself at full
/// resolution. Its integral scale, `INTEGRAL_SCALE` times it, is the integral scale of the
/// component's area average over `footprint` pixels, `sqrt(Lambda^2 + footprint^2 - 1)`, wherever
/// that is at least `INTEGRAL_SCALE * footprint`; below that the cell is the footprint itself, one
/// lattice cell a stage pixel, whose values are independent pixel to pixel.
pub fn band_limited_cell(cell: f64, footprint: f64) -> f64 {
    let spread = (footprint * footprint - 1.0) / (INTEGRAL_SCALE * INTEGRAL_SCALE);
    footprint.max((cell * cell + spread).sqrt())
}

/// The fixed-point step on one axis: `round(2^16 * footprint / band_limited_cell)`, `2^16 / cell`
/// rounded at full resolution.
pub fn step(cell: f64, footprint: f64) -> u32 {
    (65536.0 * footprint / band_limited_cell(cell, footprint) + 0.5).floor() as u32
}

/// A component's integral scale on one axis, in full-resolution pixels: 1 when its
/// full-resolution step is `2^16` (one lattice cell a pixel, Size 0's fine component, whose
/// lattice values are independent pixel to pixel), `INTEGRAL_SCALE * cell` otherwise.
pub fn integral_scale(cell: f64) -> f64 {
    if step(cell, 1.0) == FIXED_ONE {
        1.0
    } else {
        INTEGRAL_SCALE * cell
    }
}

/// The attenuation on one axis: the standard deviation of a component's mean over `footprint`
/// pixels, relative to one pixel's, `(1 + (footprint^2 - 1) / Lambda^2)^(-1/4)` with `Lambda` its
/// [`integral_scale`]. 1 at full resolution.
pub fn attenuation(cell: f64, footprint: f64) -> f64 {
    let lambda = integral_scale(cell);
    (1.0 + (footprint * footprint - 1.0) / (lambda * lambda)).powf(-0.25)
}

// ---- The field -----------------------------------------------------------------------------------

/// The field compiled for one stage: each component's steps and the coefficients the per-pixel
/// equation reads. At full resolution every attenuation is 1 and `variance` is 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub fine_step: (u32, u32),
    pub coarse_step: (u32, u32),
    pub clump_step: (u32, u32),
    /// `sqrt(6) * alpha_fine / sqrt(1 + r^2)`.
    pub fine_gain: f64,
    /// `sqrt(6) * r * alpha_coarse / sqrt(1 + r^2)`, 0 at Roughness 0.
    pub coarse_gain: f64,
    /// `CLUMP_DEPTH * roughness / 100`, 0 at Roughness 0.
    pub clump_gain: f64,
    /// The field's expected variance, `(fine_gain^2 + coarse_gain^2) / 6`, which the mean
    /// correction reads.
    pub variance: f64,
}

impl Field {
    pub fn new(size: f64, roughness: f64, footprint: Footprint) -> Self {
        let c = cell(size);
        let rho = roughness / 100.0;
        let r = COARSE_MIX * rho;
        let mix_norm = 1.0 / (1.0 + r * r).sqrt();
        let steps = |cells: f64| (step(c * cells, footprint.x), step(c * cells, footprint.y));
        let alpha =
            |cells: f64| attenuation(c * cells, footprint.x) * attenuation(c * cells, footprint.y);
        let sqrt6 = 6f64.sqrt();
        let fine_gain = sqrt6 * alpha(1.0) * mix_norm;
        let coarse_gain = sqrt6 * r * alpha(COARSE_CELLS) * mix_norm;
        Self {
            fine_step: steps(1.0),
            coarse_step: steps(COARSE_CELLS),
            clump_step: steps(CLUMP_CELLS),
            fine_gain,
            coarse_gain,
            clump_gain: CLUMP_DEPTH * rho,
            variance: (fine_gain * fine_gain + coarse_gain * coarse_gain) / 6.0,
        }
    }

    /// The field at stage pixel `(x, y)`, before the clamp: `A * (a * n_fine + b * n_coarse)`
    /// with `A = sqrt(1 + k * m_clump)`. The coarse and clump components are not evaluated when
    /// their gain is 0, which changes no value.
    pub fn unclamped(&self, x: u32, y: u32) -> f64 {
        let (_, fine) = noise(FINE, x, y, self.fine_step);
        let mut f = self.fine_gain * fine;
        if self.coarse_gain != 0.0 {
            let (_, coarse) = noise(COARSE, x, y, self.coarse_step);
            f += self.coarse_gain * coarse;
        }
        if self.clump_gain != 0.0 {
            let (clump, _) = noise(CLUMP, x, y, self.clump_step);
            f *= (1.0 + self.clump_gain * clump).sqrt();
        }
        f
    }

    /// The field at stage pixel `(x, y)`, clamped to `+-FIELD_CLAMP`.
    pub fn at(&self, x: u32, y: u32) -> f64 {
        self.unclamped(x, y).clamp(-FIELD_CLAMP, FIELD_CLAMP)
    }
}

// ---- The application -----------------------------------------------------------------------------

/// Apply field value `g` (already clamped) at amplitude `beta` to one linear-sRGB pixel, with the
/// mean correction for a field of expected `variance`:
///
/// ```text
/// L = luminance(rgb), M = max(r, g, b)
/// unchanged if L <= 0 or M >= 1
/// E = enc(L), E_top = enc(L / M), t = E / E_top
/// s = 4 beta t (1 - t) E_top
/// E' = E + s g - 0.7 variance s^2 / (E + 0.055)
/// L' = dec(E')
/// rgb' = rgb + (L' - L)        if L < EPSILON_L
///      = rgb * (L' / L)        otherwise
/// ```
pub fn apply(rgb: [f64; 3], g: f64, beta: f64, variance: f64) -> [f64; 3] {
    if beta == 0.0 {
        return rgb;
    }
    let l = luminance(rgb);
    let m = rgb[0].max(rgb[1]).max(rgb[2]);
    if l <= 0.0 || m >= 1.0 {
        return rgb;
    }
    let e = encode_extended(l);
    let e_top = encode_extended(l / m);
    let t = e / e_top;
    let s = 4.0 * beta * t * (1.0 - t) * e_top;
    let bias = 0.5 * CURVATURE * variance * s * s / (e + CURVATURE_OFFSET);
    let e_out = e + s * g - bias;
    let l_out = decode_encoded(e_out);
    if l < EPSILON_L {
        rgb.map(|c| c + (l_out - l))
    } else {
        let ratio = l_out / l;
        rgb.map(|c| c * ratio)
    }
}

/// Grain at stage pixel `(x, y)` of a stage with `footprint`, on `rgb`: the whole unit.
pub fn grain_pixel(
    x: u32,
    y: u32,
    rgb: [f64; 3],
    params: &GrainParams,
    footprint: Footprint,
) -> [f64; 3] {
    let beta = beta(params.amount);
    if beta == 0.0 {
        return rgb;
    }
    let field = Field::new(params.size, params.roughness, footprint);
    apply(rgb, field.at(x, y), beta, field.variance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lattice_split_equals_the_wide_product() {
        for index in [0u32, 1, 65535, 65536, 65537, 123_456, u32::MAX] {
            for step in [1u32, 2731, 10923, 41876, 65535, 65536] {
                let wide = u64::from(index) * u64::from(step);
                assert_eq!(
                    lattice(index, step),
                    ((wide >> 16) as u32, (wide & 0xFFFF) as u32),
                    "index {index} step {step}"
                );
            }
        }
    }
}
