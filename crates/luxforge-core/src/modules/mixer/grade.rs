//! The colour mixer's grading unit: Shadows, Midtones, Highlights and Global tints with their
//! luminance treatments, Blending and Balance, evaluated after the HSL unit in the same colour run.
//!
//! These are the initial equations `docs/design/colour-grading.md` delegates to the implementation.
//! They are chosen for smoothness, bounded cost and exact neutrality rather than measured against
//! Lightroom; the design's final refinement task owns any change to them. Every constant is stated
//! once below with what it controls.
//!
//! Per pixel, in Oklab ([`crate::colour::oklab`], the conversion the HSL unit already uses):
//!
//! 1. **Tonal selection.** The pixel's post-HSL lightness `L`, clamped to `[0, 1]`, is the bounded
//!    coordinate `t` the weights read; it is read once and the clamp never touches the pixel
//!    itself. Two smoothstep transitions centred on the shadow/midtone boundary `c1` and the
//!    midtone/highlight boundary `c2` split `t` into three weights that always sum to one:
//!    `s1 = smooth((t - c1) / w + 1/2)`, `s2 = smooth((t - c2) / w + 1/2)`, shadows `1 - s1`,
//!    midtones `s1 - s2`, highlights `s2`. Both transitions share the width `w`, so `s1 >= s2`
//!    everywhere and the midtone weight is never negative. Balance moves both boundaries together:
//!    negative raises them, extending Shadows, and positive lowers them, extending Highlights.
//!    Blending sets the width `w`, from [`WIDTH_AT_NO_BLENDING`] to [`WIDTH_AT_FULL_BLENDING`];
//!    even the narrowest transition is a smooth ramp, never a step.
//! 2. **Luminance.** The three tonal luminance amounts, weighted by the same selection, form one
//!    exponent `m`, and `L` takes the gamma `2^(-m)` on its `[0, 1]` part with any excess passed
//!    through, exactly as the HSL unit's luminance response does. The luminance weights use the
//!    same boundaries but a width of at least [`LUMINANCE_WIDTH_FLOOR`]: at a narrower width two
//!    neighbouring ranges driven in opposite directions could fold the tone scale, and this floor
//!    keeps the mapping monotone for every combination of settings. A positive Shadows amount also
//!    lifts black by up to [`SHADOW_LIFT`] and a negative Highlights amount dims white by up to
//!    [`HIGHLIGHT_DIM`], through one increasing affine map of the `[0, 1]` part; at zero they leave
//!    black and white where they are. Global luminance then applies its own gamma, reading neither
//!    the weights nor Blending nor Balance.
//! 3. **Tint.** Each range's hue, on the familiar RGB colour wheel (red 0°, green 120°, blue
//!    240°), is converted to a direction in the Oklab `(a, b)` plane: the hue of the fully
//!    saturated sRGB colour at that wheel angle. Saturation scales that direction to an Oklab
//!    chroma of up to [`TINT_CHROMA`]. The three tonal tints are mixed as vectors by their weights,
//!    Global's is added at full weight, and the sum is added to the pixel's own `(a, b)` scaled by
//!    an endpoint envelope `1 - (2c - 1)^8` of the output lightness `c` clamped to `[0, 1]`. The
//!    envelope is zero at black and white, so an untouched black or white stays exactly neutral,
//!    while black lifted by the luminance treatment (or white dimmed) takes the tint. Greys are
//!    tinted on purpose: unlike HSL, grading has no achromatic suppression.
//!
//! **Extended input.** A RAW photograph's linear path can bring signed and above-white values.
//! Only the weight coordinate and the envelope read the clamped lightness; the luminance responses
//! pass any part of `L` outside `[0, 1]` through unchanged (after the lift's offset), and the tint
//! is zero there, so the unit is continuous across black and white and never truncates headroom.
//! Every output is finite for finite input, and the unit touches only the three colour channels the
//! host gives it, so alpha is never read or changed.
//!
//! **Neutrality.** The unit is compiled only when at least one of the four saturation or four
//! luminance amounts is non-zero ([`Grading::is_active`]): hue, Blending and Balance alone change
//! no pixel, so a dormant choice is kept as parameter state without any processing. With every
//! tint zero the `(a, b)` sums are exactly zero and the pixel's own `(a, b)` passes unchanged.
//!
//! Coefficients are computed once per compile in `f64` and cast into the fixed 18-word `f32` pack
//! [`Coefficients`] holds; the per-pixel path is `f32` and identical on the CPU and the GPU
//! (`grade.wgsl`).
use crate::{
    colour::{
        oklab::{self, Oklab},
        srgb,
    },
    modules::PointwiseColor,
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};

/// The grading unit's GPU program (`grade.wgsl`): the 18 words [`Coefficients::words`] packs.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_mixer_grade",
    source: include_str!("grade.wgsl"),
    kind: GpuProgramKind::Colour,
    words: WORDS,
    enabled: true,
};

/// The four wheels, in field order: the three tonal ranges and Global.
pub(super) const WHEEL_COUNT: usize = 4;

/// The wheels' names, as the field names and descriptions spell them.
pub(super) const WHEEL_NAMES: [&str; WHEEL_COUNT] = ["shadows", "midtones", "highlights", "global"];

/// The default Blending: the declared default of `grade-blending`.
pub(super) const DEFAULT_BLENDING: f64 = 50.0;

/// The shadow/midtone and midtone/highlight boundaries at Balance 0, in Oklab lightness: the
/// three ranges share the tone scale equally.
const SHADOW_BOUNDARY: f64 = 1.0 / 3.0;
const HIGHLIGHT_BOUNDARY: f64 = 2.0 / 3.0;

/// How far Balance at ±100 moves both boundaries, in Oklab lightness: at +100 the highlight range
/// begins at 5/12 and at -100 the shadow range reaches 7/12.
const BALANCE_REACH: f64 = 0.25;

/// The transition width at Blending 0 and at Blending 100, in Oklab lightness; Blending moves
/// linearly between them, and the default 50 gives 0.55.
const WIDTH_AT_NO_BLENDING: f64 = 0.1;
const WIDTH_AT_FULL_BLENDING: f64 = 1.0;

/// The narrowest transition the luminance weights use. The gamma response stays increasing while
/// the exponent's slope is below `e / (ln 2 · LUMINANCE_STRENGTH)` per unit of `L`; two neighbours
/// at ±100 give a slope of `3 · LUMINANCE_STRENGTH / width`, so 0.4 keeps every combination
/// monotone with margin (`grade_luminance_is_monotone_for_every_extreme_combination`).
const LUMINANCE_WIDTH_FLOOR: f64 = 0.4;

/// The exponent a luminance amount of ±100 gives at full weight: `L^(2^∓0.5)`.
const LUMINANCE_STRENGTH: f64 = 0.5;

/// The Oklab lightness black is lifted to by Shadows luminance +100, and the amount white is dimmed
/// by under Highlights luminance -100.
const SHADOW_LIFT: f64 = 0.15;
const HIGHLIGHT_DIM: f64 = 0.08;

/// The Oklab chroma a saturation of 100 adds at full weight and full envelope.
const TINT_CHROMA: f64 = 0.1;

/// The uniform words: two boundaries, two inverse widths, four `(a, b)` tints, four luminance
/// exponents, the lift and the dim.
const WORDS: usize = 18;

/// One wheel's three fields, as stored.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Wheel {
    /// Degrees on the RGB colour wheel, `[0, 360]`; 0 and 360 are the same direction.
    pub(super) hue: f64,
    /// Tint strength, `[0, 100]`.
    pub(super) saturation: f64,
    /// Brightness treatment, `[-100, 100]`.
    pub(super) luminance: f64,
}

/// The fourteen grading fields, as stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Grading {
    /// Shadows, Midtones, Highlights and Global, in [`WHEEL_NAMES`] order.
    pub(super) wheels: [Wheel; WHEEL_COUNT],
    /// Range overlap, `[0, 100]`.
    pub(super) blending: f64,
    /// Range balance, `[-100, 100]`.
    pub(super) balance: f64,
}

impl Default for Grading {
    fn default() -> Self {
        Self {
            wheels: [Wheel::default(); WHEEL_COUNT],
            blending: DEFAULT_BLENDING,
            balance: 0.0,
        }
    }
}

impl Grading {
    /// Whether any saturation or luminance amount is non-zero: the only fields that change a
    /// pixel. Hue, Blending and Balance alone are dormant settings.
    pub(super) fn is_active(&self) -> bool {
        self.wheels
            .iter()
            .any(|wheel| wheel.saturation != 0.0 || wheel.luminance != 0.0)
    }

    fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.wheels
            .iter()
            .flat_map(|wheel| [wheel.hue, wheel.saturation, wheel.luminance])
            .chain([self.blending, self.balance])
    }
}

/// The Oklab `(a, b)` direction, of unit length, of a hue on the RGB colour wheel: the fully
/// saturated sRGB colour at that angle (HSV with saturation and value 1), decoded to linear light
/// and converted. Periodic in 360 degrees, so the wheel's seam is continuous and 360 is 0.
fn tint_direction(hue_deg: f64) -> [f64; 2] {
    let sector = hue_deg.rem_euclid(360.0) / 60.0;
    let rising = 1.0 - ((sector % 2.0) - 1.0).abs();
    let encoded = match sector as u32 {
        0 => [1.0, rising, 0.0],
        1 => [rising, 1.0, 0.0],
        2 => [0.0, 1.0, rising],
        3 => [0.0, rising, 1.0],
        4 => [rising, 0.0, 1.0],
        _ => [1.0, 0.0, rising],
    };
    let [_, a, b] = oklab::lab_f64(encoded.map(srgb::decode));
    let length = a.hypot(b);
    [a / length, b / length]
}

/// The unit's whole per-pixel state: 18 `f32` values.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Coefficients {
    /// The shadow/midtone and midtone/highlight boundaries after Balance.
    boundaries: [f32; 2],
    /// `1 / width` for the tint weights.
    tint_inverse_width: f32,
    /// `1 / max(width, LUMINANCE_WIDTH_FLOOR)` for the luminance weights.
    luminance_inverse_width: f32,
    /// Each wheel's Oklab `(a, b)` tint at full weight, in [`WHEEL_NAMES`] order.
    tints: [[f32; 2]; WHEEL_COUNT],
    /// Each wheel's luminance exponent amount, `LUMINANCE_STRENGTH * luminance / 100`.
    luminance: [f32; WHEEL_COUNT],
    /// The lightness black is lifted to.
    lift: f32,
    /// The amount white is dimmed by.
    dim: f32,
}

impl Coefficients {
    fn new(grading: &Grading) -> Self {
        let balance = grading.balance / 100.0;
        let width = WIDTH_AT_NO_BLENDING
            + (WIDTH_AT_FULL_BLENDING - WIDTH_AT_NO_BLENDING) * (grading.blending / 100.0);
        let shift = BALANCE_REACH * balance;
        let tints = grading.wheels.map(|wheel| {
            let chroma = TINT_CHROMA * wheel.saturation / 100.0;
            if chroma == 0.0 {
                return [0.0; 2];
            }
            tint_direction(wheel.hue).map(|component| (chroma * component) as f32)
        });
        let [shadows, _, highlights, _] = grading.wheels;
        Self {
            boundaries: [
                (SHADOW_BOUNDARY - shift) as f32,
                (HIGHLIGHT_BOUNDARY - shift) as f32,
            ],
            tint_inverse_width: (1.0 / width) as f32,
            luminance_inverse_width: (1.0 / width.max(LUMINANCE_WIDTH_FLOOR)) as f32,
            tints,
            luminance: grading
                .wheels
                .map(|wheel| (LUMINANCE_STRENGTH * wheel.luminance / 100.0) as f32),
            lift: (SHADOW_LIFT * (shadows.luminance / 100.0).max(0.0)) as f32,
            dim: (HIGHLIGHT_DIM * (-highlights.luminance / 100.0).max(0.0)) as f32,
        }
    }

    /// The pack in the order `grade.wgsl` reads it. Each word is the coefficient plus `+0.0`, so
    /// a negative zero is written as the positive one it processes as.
    fn words(&self) -> impl Iterator<Item = f32> + '_ {
        self.boundaries
            .iter()
            .copied()
            .chain([self.tint_inverse_width, self.luminance_inverse_width])
            .chain(self.tints.iter().flatten().copied())
            .chain(self.luminance.iter().copied())
            .chain([self.lift, self.dim])
            .map(|value| value + 0.0)
    }

    /// The shadow, midtone and highlight weights of the bounded coordinate `t` at one inverse
    /// width. They sum to one.
    fn weights(&self, t: f32, inverse_width: f32) -> [f32; 3] {
        let [c1, c2] = self.boundaries;
        let rise = smoothstep((t - c1) * inverse_width + 0.5);
        let high = smoothstep((t - c2) * inverse_width + 0.5);
        [1.0 - rise, rise - high, high]
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let lab = oklab::to_oklab(rgb);
        let t = lab.l.clamp(0.0, 1.0);
        let [ws, wm, wh] = self.weights(t, self.tint_inverse_width);
        let [vs, vm, vh] = self.weights(t, self.luminance_inverse_width);
        let [ls, lm, lh, lg] = self.luminance;
        let amount = vs * ls + vm * lm + vh * lh;
        let mut l = lab.l;
        if amount != 0.0 {
            l = gamma(l, amount);
        }
        if self.lift != 0.0 || self.dim != 0.0 {
            let core = l.clamp(0.0, 1.0);
            l = self.lift + (1.0 - self.lift - self.dim) * core + (l - core);
        }
        if lg != 0.0 {
            l = gamma(l, lg);
        }
        let envelope = envelope(l);
        let [ts, tm, th, tg] = self.tints;
        let a = ws * ts[0] + wm * tm[0] + wh * th[0] + tg[0];
        let b = ws * ts[1] + wm * tm[1] + wh * th[1] + tg[1];
        oklab::from_oklab(Oklab {
            l,
            a: lab.a + envelope * a,
            b: lab.b + envelope * b,
        })
    }
}

/// The cubic smoothstep of `x` clamped to `[0, 1]`.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The gamma `2^(-amount)` on the `[0, 1]` part of `l`, any excess passed through: 0 and 1 are
/// fixed points.
fn gamma(l: f32, amount: f32) -> f32 {
    let core = l.clamp(0.0, 1.0);
    core.powf((-amount).exp2()) + (l - core)
}

/// `1 - (2c - 1)^8` of the lightness clamped to `[0, 1]`: one in the middle of the scale, zero at
/// black and white and outside them.
fn envelope(l: f32) -> f32 {
    let x = 2.0 * l.clamp(0.0, 1.0) - 1.0;
    let x2 = x * x;
    let x4 = x2 * x2;
    1.0 - x4 * x4
}

/// The grading unit: fourteen fields reduced to an 18-word coefficient pack.
#[derive(Debug)]
pub(super) struct Grade {
    grading: Grading,
    coefficients: Coefficients,
}

impl Grade {
    pub(super) fn new(grading: Grading) -> Self {
        Self {
            coefficients: Coefficients::new(&grading),
            grading,
        }
    }
}

impl PointwiseColor for Grade {
    /// The coefficient pack: a dormant hue, Blending or Balance that changes no pixel changes no
    /// identity either.
    fn identity(&self) -> crate::OperationIdentity {
        crate::OperationIdentity::new(
            "grade",
            self.coefficients
                .words()
                .map(|value| u64::from(value.to_bits())),
        )
    }

    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            *pixel = self.coefficients.apply(*pixel);
        }
    }

    fn is_finite(&self) -> bool {
        self.grading.values().all(f64::is_finite) && self.coefficients.words().all(f32::is_finite)
    }

    /// The unit and every field away from its default.
    fn describe(&self) -> String {
        let mut fields = Vec::new();
        for (name, wheel) in WHEEL_NAMES.iter().zip(&self.grading.wheels) {
            for (property, value) in [
                ("hue", wheel.hue),
                ("saturation", wheel.saturation),
                ("luminance", wheel.luminance),
            ] {
                if value != 0.0 {
                    fields.push(format!("{name}-{property}:{value:+}"));
                }
            }
        }
        if self.grading.blending != DEFAULT_BLENDING {
            fields.push(format!("blending:{}", self.grading.blending));
        }
        if self.grading.balance != 0.0 {
            fields.push(format!("balance:{:+}", self.grading.balance));
        }
        if fields.is_empty() {
            return "grade(neutral)".into();
        }
        format!("grade({})", fields.join(", "))
    }

    fn gpu(&self) -> Option<GpuDescription> {
        Some(GpuDescription::new(
            &PROGRAM,
            self.coefficients.words().map(f32::to_bits).collect(),
        ))
    }
}

#[cfg(test)]
#[path = "grade_tests.rs"]
mod tests;
