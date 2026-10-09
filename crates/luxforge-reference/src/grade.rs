//! f64 reference for the colour mixer's grading unit: the initial equations recorded in
//! `docs/design/colour-grading.md` ("Initial numerical implementation"), transcribed in `f64`
//! without the production unit's `f32` arithmetic, branches or coefficient pack.
//!
//! Per pixel, in Oklab: the post-HSL lightness, clamped to `[0, 1]`, selects smooth Shadows,
//! Midtones and Highlights weights that sum to one; the luminance amounts form one gamma exponent
//! from the same selection (with a minimum transition width that keeps the tone scale monotone),
//! Shadows +100 lifts black and Highlights -100 dims white through one increasing affine map, and
//! Global applies its own gamma; finally each range's tint, a direction on the RGB colour wheel
//! scaled by its saturation, is mixed by the weights, Global's added at full weight, and the sum is
//! added to the pixel's `(a, b)` through an envelope that is zero at black and white.
//!
//! The Oklab conversion is [`super::colour`]'s, as for every colour study.

use super::colour::{self, Oklab};
use super::srgb;

/// The four wheels: Shadows, Midtones, Highlights and Global.
pub const WHEEL_COUNT: usize = 4;

/// The wheels' names, as the payload fields spell them.
pub const WHEEL_NAMES: [&str; WHEEL_COUNT] = ["shadows", "midtones", "highlights", "global"];

/// The shadow/midtone and midtone/highlight boundaries at Balance 0, in Oklab lightness.
pub const SHADOW_BOUNDARY: f64 = 1.0 / 3.0;
pub const HIGHLIGHT_BOUNDARY: f64 = 2.0 / 3.0;

/// How far Balance ±100 moves both boundaries: positive lowers them, extending Highlights.
pub const BALANCE_REACH: f64 = 0.25;

/// The transition width at Blending 0 and 100; Blending moves linearly between them.
pub const WIDTH_AT_NO_BLENDING: f64 = 0.1;
pub const WIDTH_AT_FULL_BLENDING: f64 = 1.0;

/// The narrowest transition the luminance weights use.
pub const LUMINANCE_WIDTH_FLOOR: f64 = 0.4;

/// The gamma exponent amount a luminance of ±100 gives at full weight: `L^(2^∓0.5)`.
pub const LUMINANCE_STRENGTH: f64 = 0.5;

/// Black's lightness under Shadows luminance +100, and white's dimming under Highlights -100.
pub const SHADOW_LIFT: f64 = 0.15;
pub const HIGHLIGHT_DIM: f64 = 0.08;

/// The Oklab chroma a saturation of 100 adds at full weight and full envelope.
pub const TINT_CHROMA: f64 = 0.1;

/// One wheel: hue in degrees on the RGB colour wheel `[0, 360]`, saturation `[0, 100]` and
/// luminance `[-100, 100]`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wheel {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}

/// The fourteen grading fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradeParams {
    pub wheels: [Wheel; WHEEL_COUNT],
    pub blending: f64,
    pub balance: f64,
}

impl Default for GradeParams {
    fn default() -> Self {
        Self {
            wheels: [Wheel::default(); WHEEL_COUNT],
            blending: 50.0,
            balance: 0.0,
        }
    }
}

impl GradeParams {
    /// Whether the unit changes any pixel: some saturation or luminance amount is non-zero.
    pub fn is_active(&self) -> bool {
        self.wheels
            .iter()
            .any(|wheel| wheel.saturation != 0.0 || wheel.luminance != 0.0)
    }
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The Shadows, Midtones and Highlights weights of the lightness `l` at these settings, with the
/// transition width at least `floor`.
pub fn weights(l: f64, blending: f64, balance: f64, floor: f64) -> [f64; 3] {
    let width = (WIDTH_AT_NO_BLENDING
        + (WIDTH_AT_FULL_BLENDING - WIDTH_AT_NO_BLENDING) * blending / 100.0)
        .max(floor);
    let shift = BALANCE_REACH * balance / 100.0;
    let t = l.clamp(0.0, 1.0);
    let rise = smoothstep((t - (SHADOW_BOUNDARY - shift)) / width + 0.5);
    let high = smoothstep((t - (HIGHLIGHT_BOUNDARY - shift)) / width + 0.5);
    [1.0 - rise, rise - high, high]
}

/// The unit-length Oklab `(a, b)` direction of a hue on the RGB colour wheel: the hue of the fully
/// saturated sRGB colour (HSV value and saturation 1) at that angle.
pub fn tint_direction(hue_deg: f64) -> [f64; 2] {
    let h = hue_deg.rem_euclid(360.0);
    let channel = |offset: f64| {
        // The HSV primary channel ramp: 1 within 60° of its centre, falling to 0 at 120°.
        let distance = ((h - offset + 540.0).rem_euclid(360.0) - 180.0).abs();
        (2.0 - distance / 60.0).clamp(0.0, 1.0)
    };
    let encoded = [channel(0.0), channel(120.0), channel(240.0)];
    let lab = colour::to_oklab(encoded.map(srgb::decode_encoded));
    let length = lab.a.hypot(lab.b);
    [lab.a / length, lab.b / length]
}

fn gamma(l: f64, amount: f64) -> f64 {
    let core = l.clamp(0.0, 1.0);
    core.powf(2f64.powf(-amount)) + (l - core)
}

/// The grading unit applied to one linear sRGB pixel.
pub fn apply(rgb: [f64; 3], params: &GradeParams) -> [f64; 3] {
    let lab = colour::to_oklab(rgb);
    let w = weights(lab.l, params.blending, params.balance, 0.0);
    let v = weights(
        lab.l,
        params.blending,
        params.balance,
        LUMINANCE_WIDTH_FLOOR,
    );
    let amount = |index: usize| LUMINANCE_STRENGTH * params.wheels[index].luminance / 100.0;
    let exponent = v[0] * amount(0) + v[1] * amount(1) + v[2] * amount(2);
    let mut l = gamma(lab.l, exponent);
    let lift = SHADOW_LIFT * (params.wheels[0].luminance / 100.0).max(0.0);
    let dim = HIGHLIGHT_DIM * (-params.wheels[2].luminance / 100.0).max(0.0);
    let core = l.clamp(0.0, 1.0);
    l = lift + (1.0 - lift - dim) * core + (l - core);
    l = gamma(l, amount(3));
    let envelope = 1.0 - (2.0 * l.clamp(0.0, 1.0) - 1.0).powi(8);
    let (mut a, mut b) = (0.0, 0.0);
    for (index, weight) in [w[0], w[1], w[2], 1.0].into_iter().enumerate() {
        let wheel = params.wheels[index];
        let [da, db] = tint_direction(wheel.hue);
        let chroma = TINT_CHROMA * wheel.saturation / 100.0;
        a += weight * chroma * da;
        b += weight * chroma * db;
    }
    colour::from_oklab(Oklab {
        l,
        a: lab.a + envelope * a,
        b: lab.b + envelope * b,
    })
}
