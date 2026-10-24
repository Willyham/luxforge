//! The grain module's one positional colour unit: the frozen hash, noise field, Size and Roughness
//! mappings, tone weight, application and reduced-stage rule of `docs/design/grain-study.md`,
//! compiled against the stage a finish-stage layer receives.
//!
//! This file is the production transcription of that study and of the independent `f64` reference
//! at `crates/luxforge-reference/src/grain.rs`; the three must be read together. Every constant is
//! named as the study names it, and every expression is written in the same form and order.
//!
//! The unit runs in two parts, as the study does:
//!
//! 1. **Compile, in `f64`, once per unit** ([`GrainField::new`]): from the three strengths and the
//!    stage's footprint, the integer lattice steps of the three noise components, their gains, the
//!    field's variance and the amplitude `beta`. These are exactly what a GPU program receives as
//!    uniform words; it never recomputes them.
//! 2. **Per pixel, in `f32`** ([`Grain::apply_row`]): the stage pixel's integer position is hashed
//!    at a few lattice points (exact `u32` arithmetic, bit for bit the reference's), three value
//!    noises are interpolated and combined into the field value `G`, and `G` moves the pixel's
//!    luminance in the tone study's encoded domain.
//!
//! Position. The unit is evaluated at stage pixel `(X, Y) = (x0 + offset, y)`, the integer position
//! `apply_row` is handed in the stage the unit was compiled for. At full resolution that stage is
//! the output stage, so the grain at an output pixel depends on that pixel's position, the three
//! strengths and the frozen seed alone: not on the tile, the window, the thread or the photograph.
//! A stage reduced from full resolution (a reduced compile's `CompileStage::full` larger than its
//! `stage`) takes the study's reduced-stage rule through its footprint, `max(1, full / stage)` per
//! axis.
//!
//! Precision. The frozen tolerance against the reference is `1e-5` absolute on `G` and
//! `1e-5 + 1e-5·|reference|` on the linear output, at most one output code at quantization; the
//! integer parts (steps, lattice coordinates, hashes and lattice values) are equal exactly.
//!
//! Memory. The unit holds a few words: no table proportional to the frame or to a side of it, no
//! scratch and no cache beyond the four lattice values of the cell a row is in, kept on the stack
//! of one `apply_row` call.
use crate::{
    OperationIdentity,
    colour::{
        luma,
        srgb::{decode_f32, encode_f32},
    },
    modules::{PointwiseColor, Stage},
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};

/// The grain unit's GPU program (`unit.wgsl`): the six lattice steps and the five coefficients the
/// per-pixel path multiplies by, eleven words. A finish-stage unit, so the plan places it in output
/// space after the geometry tail, at the stage pixel the CPU unit is handed; the steps carry the
/// stage's footprint, so a reduced stage evaluates the study's reduced-stage field there.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_grain_film",
    source: include_str!("unit.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 11,
    enabled: true,
};

// ---- Frozen constants (the study's "Constants" table) --------------------------------------------

/// The one frozen seed every photograph's grain is generated from ("LXGR").
pub(super) const SEED: u32 = 0x4C58_4752;
/// The three independent noise components, each its own hash stream.
const FINE: u32 = 0;
const COARSE: u32 = 1;
const CLUMP: u32 = 2;
/// Each component's cell, as a multiple of the fine cell.
const COARSE_CELLS: f64 = 2.0;
const CLUMP_CELLS: f64 = 4.0;
/// The fine cell at Size 100, in full-resolution output pixels. Size 0 is one pixel.
const CELL_MAX: f64 = 6.0;
/// The coarse octave's weight relative to the fine one at Roughness 100.
const COARSE_MIX: f64 = 0.75;
/// The clumping modulation's depth at Roughness 100.
const CLUMP_DEPTH: f64 = 0.9;
/// The field is clamped to `±FIELD_CLAMP` standard deviations before it is applied.
const FIELD_CLAMP: f32 = 4.0;
/// The tone-weighted amplitude Amount 100 reaches: `beta = AMOUNT_MAX * amount / 100`.
const AMOUNT_MAX: f64 = 0.05;
/// The mean correction's curvature of the sRGB decode, `CURVATURE / (E + CURVATURE_OFFSET)`.
const CURVATURE: f64 = 1.4;
const CURVATURE_OFFSET: f32 = 0.055;
/// The lattice coordinate's fixed point: 16 fractional bits.
const FIXED_ONE: u32 = 1 << 16;
/// The value noise's integral scale per cell, which sets the reduced-stage band limit and
/// attenuation.
const INTEGRAL_SCALE: f64 = 1.35;

// ---- The hash: exact u32 arithmetic, identical in WGSL -------------------------------------------

/// The 32-bit finaliser ("lowbias32"): xor-shift, multiply, xor-shift, multiply, xor-shift, in
/// `u32` wrapping arithmetic only.
#[inline]
pub(super) fn mix(x: u32) -> u32 {
    let mut x = x;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x
}

/// A component's key, `mix(SEED + component)`.
pub(super) fn component_key(component: u32) -> u32 {
    mix(SEED.wrapping_add(component))
}

/// The hash of lattice point `(i, j)` of `component`: `mix(i + mix(j + key))`. The per-pixel path
/// hoists a row's `mix(j + key)`; this whole form is what the tests check against the vectors.
#[cfg(test)]
pub(super) fn hash(component: u32, i: u32, j: u32) -> u32 {
    mix(i.wrapping_add(mix(j.wrapping_add(component_key(component)))))
}

/// A hash's lattice value: the sum of its two 16-bit halves, centred, in `(-1, 1)`. Exact in `f32`:
/// the numerator has at most 17 significant bits and the divisor is a power of two.
#[inline]
pub(super) fn lattice_value(hash: u32) -> f32 {
    let low = hash & 0xFFFF;
    let high = hash >> 16;
    (low + high + 1) as f32 / 65536.0 - 1.0
}

/// A stage pixel index's lattice cell and 16-bit fraction for a fixed-point `step` (at most
/// `2^16`): `index * step` split into its integer part and its fraction without overflowing `u32`.
#[inline]
pub(super) fn lattice(index: u32, step: u32) -> (u32, u32) {
    debug_assert!(step <= FIXED_ONE, "a step of {step} passes 2^16");
    let high = index >> 16;
    let low = index & 0xFFFF;
    let low_product = low * step;
    (high * step + (low_product >> 16), low_product & 0xFFFF)
}

/// The two smoothstep interpolation weights at a 16-bit fraction: `w1 = tau^2 (3 - 2 tau)`,
/// `w0 = 1 - w1`. `tau` is exact in `f32`.
#[inline]
fn weights(fraction: u32) -> (f32, f32) {
    let tau = fraction as f32 / 65536.0;
    let w1 = tau * tau * (3.0 - 2.0 * tau);
    (1.0 - w1, w1)
}

// ---- The compile (f64, once per unit) ------------------------------------------------------------

/// A stage's footprint: full-resolution output pixels per stage pixel on each axis, 1 at full
/// resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Footprint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

impl Footprint {
    #[cfg(test)]
    pub(crate) const FULL: Self = Self { x: 1.0, y: 1.0 };

    /// The footprint of `stage` evaluated for the full-resolution stage `full`: `max(1, full /
    /// stage)` on each axis, so a stage is never treated as finer than full resolution.
    pub(crate) fn of(stage: Stage, full: Stage) -> Self {
        Self {
            x: (f64::from(full.width) / f64::from(stage.width)).max(1.0),
            y: (f64::from(full.height) / f64::from(stage.height)).max(1.0),
        }
    }
}

/// The fine cell in full-resolution output pixels: `CELL_MAX^(size/100)`, 1 at Size 0.
fn cell(size: f64) -> f64 {
    CELL_MAX.powf(size / 100.0)
}

/// The band-limited cell on one axis, in full-resolution pixels:
/// `max(footprint, sqrt(cell^2 + (footprint^2 - 1) / INTEGRAL_SCALE^2))`.
fn band_limited_cell(cell: f64, footprint: f64) -> f64 {
    let spread = (footprint * footprint - 1.0) / (INTEGRAL_SCALE * INTEGRAL_SCALE);
    footprint.max((cell * cell + spread).sqrt())
}

/// The fixed-point step on one axis: `round(2^16 * footprint / band_limited_cell)`.
fn step(cell: f64, footprint: f64) -> u32 {
    (65536.0 * footprint / band_limited_cell(cell, footprint) + 0.5).floor() as u32
}

/// A component's integral scale on one axis, in full-resolution pixels: 1 when its
/// full-resolution step is `2^16`, `INTEGRAL_SCALE * cell` otherwise.
fn integral_scale(cell: f64) -> f64 {
    if step(cell, 1.0) == FIXED_ONE {
        1.0
    } else {
        INTEGRAL_SCALE * cell
    }
}

/// The attenuation on one axis: `(1 + (footprint^2 - 1) / Lambda^2)^(-1/4)`, 1 at full resolution.
fn attenuation(cell: f64, footprint: f64) -> f64 {
    let lambda = integral_scale(cell);
    (1.0 + (footprint * footprint - 1.0) / (lambda * lambda)).powf(-0.25)
}

/// The field compiled for one stage, in `f64`: each component's fixed-point steps and the
/// coefficients the per-pixel equation reads. At full resolution every attenuation is 1 and
/// `variance` is 1 (to rounding).
///
/// These are the unit's GPU uniforms: the six steps as `u32` words and the gains, the variance and
/// `beta` narrowed to `f32`, so a program evaluates exactly the field this unit evaluates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GrainField {
    /// `[fine, coarse, clump]`, each `[x, y]`: lattice cells per stage pixel, times `2^16`.
    pub(crate) steps: [[u32; 2]; 3],
    /// `sqrt(6) * alpha_fine / sqrt(1 + r^2)`.
    pub(crate) fine_gain: f64,
    /// `sqrt(6) * r * alpha_coarse / sqrt(1 + r^2)`, 0 at Roughness 0.
    pub(crate) coarse_gain: f64,
    /// `CLUMP_DEPTH * roughness / 100`, 0 at Roughness 0.
    pub(crate) clump_gain: f64,
    /// The field's expected variance, `(fine_gain^2 + coarse_gain^2) / 6`, which the mean
    /// correction reads.
    pub(crate) variance: f64,
    /// The tone-weighted amplitude, `AMOUNT_MAX * amount / 100`; 0 is the exact identity.
    pub(crate) beta: f64,
}

impl GrainField {
    /// The compile of the three strengths for a stage of `footprint`, transcribed from the
    /// reference's `Field::new` and `beta`.
    pub(crate) fn new(amount: f64, size: f64, roughness: f64, footprint: Footprint) -> Self {
        let c = cell(size);
        let rho = roughness / 100.0;
        let r = COARSE_MIX * rho;
        let mix_norm = 1.0 / (1.0 + r * r).sqrt();
        let steps = |cells: f64| [step(c * cells, footprint.x), step(c * cells, footprint.y)];
        let alpha =
            |cells: f64| attenuation(c * cells, footprint.x) * attenuation(c * cells, footprint.y);
        let sqrt6 = 6f64.sqrt();
        let fine_gain = sqrt6 * alpha(1.0) * mix_norm;
        let coarse_gain = sqrt6 * r * alpha(COARSE_CELLS) * mix_norm;
        Self {
            steps: [steps(1.0), steps(COARSE_CELLS), steps(CLUMP_CELLS)],
            fine_gain,
            coarse_gain,
            clump_gain: CLUMP_DEPTH * rho,
            variance: (fine_gain * fine_gain + coarse_gain * coarse_gain) / 6.0,
            beta: AMOUNT_MAX * amount / 100.0,
        }
    }

    fn values(&self) -> [f64; 5] {
        [
            self.fine_gain,
            self.coarse_gain,
            self.clump_gain,
            self.variance,
            self.beta,
        ]
    }
}

// ---- The per-pixel noise (f32) -------------------------------------------------------------------

/// One component's value noise along one row: the row's lattice row, weights and hoisted inner
/// hashes, and the lattice values of the last cell a pixel fell in. A step is at most `2^16`, so
/// the cell advances by at most one a pixel, and the next cell's left column is the last one's
/// right column: at most two `mix` calls a pixel, none while the pixel stays in its cell.
struct RowNoise {
    step: u32,
    /// `mix(j + key)` and `mix(j + 1 + key)`: the inner hash of the cell's two lattice rows.
    rows: [u32; 2],
    /// The two vertical weights, `(w0y, w1y)`, and `w0y^2 + w1y^2`.
    wy: (f32, f32),
    norm_y: f32,
    /// The cell `i` the values below belong to, and its lattice values: column `i` on rows `j` and
    /// `j + 1`, then column `i + 1` on both.
    cell: Option<u32>,
    left: [f32; 2],
    right: [f32; 2],
}

impl RowNoise {
    fn new(component: u32, y: u32, [step_x, step_y]: [u32; 2]) -> Self {
        let key = component_key(component);
        let (j, fy) = lattice(y, step_y);
        let (w0y, w1y) = weights(fy);
        Self {
            step: step_x,
            rows: [
                mix(j.wrapping_add(key)),
                mix(j.wrapping_add(1).wrapping_add(key)),
            ],
            wy: (w0y, w1y),
            norm_y: w0y * w0y + w1y * w1y,
            cell: None,
            left: [0.0; 2],
            right: [0.0; 2],
        }
    }

    /// Column `i`'s lattice values on the cell's two rows.
    #[inline]
    fn column(&self, i: u32) -> [f32; 2] {
        [
            lattice_value(mix(i.wrapping_add(self.rows[0]))),
            lattice_value(mix(i.wrapping_add(self.rows[1]))),
        ]
    }

    /// The noise at stage column `x`: `(raw, norm)`, `raw` the smooth interpolation of the cell's
    /// four lattice values, in `(-1, 1)`, and `norm` the root sum of the squared weights, which
    /// divides it to the position-independent variance `1/6`.
    #[inline]
    fn at(&mut self, x: u32) -> (f32, f32) {
        let (i, fx) = lattice(x, self.step);
        match self.cell {
            Some(cell) if cell == i => {}
            Some(cell) if cell.wrapping_add(1) == i => {
                self.left = self.right;
                self.right = self.column(i.wrapping_add(1));
            }
            _ => {
                self.left = self.column(i);
                self.right = self.column(i.wrapping_add(1));
            }
        }
        self.cell = Some(i);
        let (w0x, w1x) = weights(fx);
        let (w0y, w1y) = self.wy;
        let [v00, v01] = self.left;
        let [v10, v11] = self.right;
        let top = w0x * v00 + w1x * v10;
        let bottom = w0x * v01 + w1x * v11;
        let raw = w0y * top + w1y * bottom;
        let norm = ((w0x * w0x + w1x * w1x) * self.norm_y).sqrt();
        (raw, norm)
    }

    /// The normalised noise at stage column `x`: `raw / norm`.
    #[inline]
    fn normalised(&mut self, x: u32) -> f32 {
        let (raw, norm) = self.at(x);
        raw / norm
    }
}

/// The three components along one row: the fine one always, the coarse and clump ones only when
/// their gain is non-zero, which changes no value.
struct RowField {
    fine: RowNoise,
    coarse: Option<RowNoise>,
    clump: Option<RowNoise>,
}

// ---- The unit ------------------------------------------------------------------------------------

/// Grain over the stage its layer receives. The module compiles a neutral payload (Amount 0) to no
/// unit at all; a GPU-shaped compile of one holds this unit with `beta` 0, which `apply_row`
/// leaves exactly as it is.
#[derive(Debug)]
pub(super) struct Grain {
    /// The three strengths, as the module's responses give them, and the footprint the field was
    /// compiled for: what `describe` names.
    amount: f64,
    size: f64,
    roughness: f64,
    footprint: Footprint,
    /// The `f64` compile: the steps, gains, variance and amplitude, which a GPU program's uniform
    /// words carry.
    field: GrainField,
    /// The compile's coefficients narrowed once to the `f32` the per-pixel path multiplies by.
    fine_gain: f32,
    coarse_gain: f32,
    clump_gain: f32,
    /// `4 * beta`, the tone weight's amplitude.
    four_beta: f32,
    /// `0.5 * CURVATURE * variance`, the mean correction's coefficient.
    correction: f32,
}

impl Grain {
    pub(super) fn new(amount: f64, size: f64, roughness: f64, footprint: Footprint) -> Self {
        let field = GrainField::new(amount, size, roughness, footprint);
        Self {
            amount,
            size,
            roughness,
            footprint,
            field,
            fine_gain: field.fine_gain as f32,
            coarse_gain: field.coarse_gain as f32,
            clump_gain: field.clump_gain as f32,
            four_beta: (4.0 * field.beta) as f32,
            correction: (0.5 * CURVATURE * field.variance) as f32,
        }
    }

    fn row(&self, y: u32) -> RowField {
        let [fine, coarse, clump] = self.field.steps;
        RowField {
            fine: RowNoise::new(FINE, y, fine),
            coarse: (self.coarse_gain != 0.0).then(|| RowNoise::new(COARSE, y, coarse)),
            clump: (self.clump_gain != 0.0).then(|| RowNoise::new(CLUMP, y, clump)),
        }
    }

    /// The clamped field value `G` at stage column `x` of `row`:
    /// `clamp(A * (a * n_fine + b * n_coarse), ±FIELD_CLAMP)` with `A = sqrt(1 + k * m_clump)`.
    #[inline]
    fn field_in(&self, row: &mut RowField, x: u32) -> f32 {
        let mut f = self.fine_gain * row.fine.normalised(x);
        if let Some(coarse) = &mut row.coarse {
            f += self.coarse_gain * coarse.normalised(x);
        }
        if let Some(clump) = &mut row.clump {
            let (raw, _) = clump.at(x);
            f *= (1.0 + self.clump_gain * raw).sqrt();
        }
        f.clamp(-FIELD_CLAMP, FIELD_CLAMP)
    }

    /// The clamped field value at stage pixel `(x, y)`, through the per-pixel path's own row
    /// evaluation. For the tests: the rendering path evaluates a whole row at once.
    #[cfg(test)]
    pub(super) fn field_at(&self, x: u32, y: u32) -> f32 {
        self.field_in(&mut self.row(y), x)
    }

    /// The application of field value `g` to one linear-sRGB pixel, already known to have a
    /// positive luminance `l` and its largest channel `m` below 1:
    ///
    /// ```text
    /// E = enc(L), E_top = enc(L / M), t = E / E_top
    /// s = 4 beta t (1 - t) E_top
    /// E' = E + s g - 0.7 variance s^2 / (E + 0.055)
    /// rgb' = reconstruct(rgb, L, dec(E'))
    /// ```
    #[inline]
    fn apply(&self, pixel: &mut [f32; 3], l: f32, m: f32, g: f32) {
        let e = encode_f32(l);
        let e_top = encode_f32(l / m);
        let t = e / e_top;
        let s = self.four_beta * t * (1.0 - t) * e_top;
        let bias = self.correction * s * s / (e + CURVATURE_OFFSET);
        let e_out = e + s * g - bias;
        *pixel = luma::reconstruct(*pixel, l, decode_f32(e_out));
    }
}

impl PointwiseColor for Grain {
    /// One contiguous run of row `y` of the stage, starting at column `x0`. A pixel at or below
    /// black, or with a channel at or above white, is left bit-identical and its field is never
    /// evaluated; Amount 0 leaves the whole row bit-identical.
    fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
        if self.field.beta == 0.0 {
            return;
        }
        let mut row = self.row(y);
        for (offset, pixel) in rgb.iter_mut().enumerate() {
            let l = luma::rec709(*pixel);
            let m = pixel[0].max(pixel[1]).max(pixel[2]);
            if l <= 0.0 || m >= 1.0 {
                continue;
            }
            let g = self.field_in(&mut row, x0 + offset as u32);
            self.apply(pixel, l, m, g);
        }
    }

    fn is_finite(&self) -> bool {
        [self.amount, self.size, self.roughness]
            .into_iter()
            .chain([self.footprint.x, self.footprint.y])
            .chain(self.field.values())
            .all(f64::is_finite)
    }

    /// The strengths, the footprint and every compiled coefficient as exact bits, then the six
    /// steps. The stage's own size is not part of it: the field depends on position and footprint
    /// alone, so two stages with one footprint process alike.
    fn identity(&self) -> OperationIdentity {
        OperationIdentity::new(
            "grain",
            [
                self.amount,
                self.size,
                self.roughness,
                self.footprint.x,
                self.footprint.y,
            ]
            .into_iter()
            .chain(self.field.values())
            .map(f64::to_bits),
        )
        .with_words(
            self.field
                .steps
                .iter()
                .flatten()
                .map(|step| u64::from(*step)),
        )
    }

    fn describe(&self) -> String {
        format!(
            "grain(amount={}, size={}, roughness={}, footprint={}x{})",
            self.amount, self.size, self.roughness, self.footprint.x, self.footprint.y
        )
    }

    /// The six steps as the compile computed them, then the gains, `4 beta` and the mean
    /// correction's coefficient as the `f32` values `apply_row` multiplies by: pure functions of
    /// the strengths and the footprint the description writes. A neutral GPU-shaped unit carries
    /// `4 beta = 0`, which the program returns untouched.
    fn gpu(&self) -> Option<GpuDescription> {
        let [fine, coarse, clump] = self.field.steps;
        Some(GpuDescription::new(
            &PROGRAM,
            [fine, coarse, clump]
                .into_iter()
                .flatten()
                .chain(
                    [
                        self.fine_gain,
                        self.coarse_gain,
                        self.clump_gain,
                        self.four_beta,
                        self.correction,
                    ]
                    .map(f32::to_bits),
                )
                .collect(),
        ))
    }
}

#[cfg(test)]
#[path = "unit_tests.rs"]
mod tests;
