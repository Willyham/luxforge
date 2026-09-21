//! Independent f64 reference for the frozen global Tone algorithm (TASK-011).
//!
//! This module shares no code with production. It exists so a later production
//! implementation of the `lightwell.basic.adjust` tone unit has an oracle it
//! cannot influence. The frozen equations are written out in full in
//! `docs/design/basic-tone.md`; this file is their literal transcription, and
//! the two must be read together. Every constant below is named identically to
//! the constant of the same name in that document.
//!
//! Domain: this reference takes and returns linear-sRGB (D65) `f64` triples, with
//! no gamut clamp and no rounding, matching the "Pointwise colour processing"
//! section of `docs/design/basic-and-histogram.md`: values outside `[0, 1]` and
//! negative values are preserved, and the host quantizes only at the output
//! boundary, not here.

/// Rec. 709 / sRGB luma coefficients, applied to *linear* sRGB. sRGB and Rec. 709
/// share primaries and white point, so the same coefficients give the standard
/// relative-luminance weighting for this working space with no extra
/// justification needed. Sums to 1.0 exactly in the constants below.
const LUMA_R: f64 = 0.2126;
const LUMA_G: f64 = 0.7152;
const LUMA_B: f64 = 0.0722;

/// The curve domain's pivot: encoded mid-grey. See "Working tone domain" and
/// "Contrast" in the design doc.
const PIVOT: f64 = 0.5;

/// Contrast steepness scale: the logistic exponent at Contrast = +-100 is
/// +-ALPHA_MAX. See "Contrast" in the design doc for the derivative proof this
/// value does not affect (the derivative is provably positive for any finite,
/// nonzero alpha).
const ALPHA_MAX: f64 = 6.0;

/// Whites/Blacks endpoint range: at Whites = +-100 the white point moves by
/// -+K_W from 1.0; at Blacks = +-100 the black point moves by +-K_B from 0.0.
const K_W: f64 = 0.25;
const K_B: f64 = 0.25;

/// The minimum white-point-minus-black-point gap the crossing-prevention clamp
/// enforces. With K_W = K_B = 0.25 the worst corner (Whites = Blacks = +100)
/// leaves a gap of 0.5, so this clamp is provably never active over the agreed
/// +-100 range; it exists for defensive correctness if that range ever changes.
const EPSILON_GAP: f64 = 0.05;

/// Highlights tonal-weight bump window (curve domain): zero at and below 0.45,
/// zero at and above 0.95, peak 1.0 at the midpoint 0.7. Chosen so the window's
/// near edge sits close to the 0.5 pivot (a small, deliberate overlap with the
/// Shadows window) while its far edge stays strictly below the encoded white
/// point 1.0, so Highlights leaves pure white exactly unchanged (see
/// "Highlights and Shadows" in the design doc).
const HIGHLIGHT_WINDOW: (f64, f64) = (0.45, 0.95);
/// Shadows tonal-weight bump window (curve domain): zero at and below 0.05,
/// zero at and above 0.55, peak 1.0 at the midpoint 0.3. The mirror image of
/// `HIGHLIGHT_WINDOW` about the pivot: its far edge sits close to 0.5 and its
/// near edge stays strictly above the encoded black point 0.0, so Shadows
/// leaves pure black exactly unchanged.
const SHADOW_WINDOW: (f64, f64) = (0.05, 0.55);
/// Maximum additive offset each of Highlights/Shadows contributes at
/// parameter = +-100. See "Highlights and Shadows" in the design doc for the
/// derivative bound this value must satisfy (must stay under ~0.0796 for the
/// composed curve's derivative to stay positive by the conservative
/// union-sum bound; 0.075 keeps a proven-positive minimum slope of ~0.0575,
/// chosen close to that bound for a usable effect strength).
const A_HS: f64 = 0.075;

/// Below this linear luminance, the luminance-ratio reconstruction switches to
/// an additive rule to avoid dividing by (near) zero. See "Luminance ratio and
/// gamut policy" in the design doc.
const EPSILON_L: f64 = 1e-6;

/// The frozen algorithm's parameters, one field per Basic slider, each in the
/// agreed -100..100 UI range. All-zero is the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneParams {
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
}

impl ToneParams {
    pub const NEUTRAL: Self = Self {
        contrast: 0.0,
        highlights: 0.0,
        shadows: 0.0,
        whites: 0.0,
        blacks: 0.0,
    };

    fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }
}

/// Rec. 709 relative luminance of a linear-sRGB triple. Not gamut-clamped: a
/// triple with negative or >1 components (preserved from an earlier unit)
/// yields a luminance outside `[0, 1]` too, which the extended-domain encode
/// below accepts.
pub fn luminance(rgb: [f64; 3]) -> f64 {
    LUMA_R * rgb[0] + LUMA_G * rgb[1] + LUMA_B * rgb[2]
}

/// The sRGB OETF (linear -> encoded), analytically continued to every finite
/// real `l`, not just `[0, 1]`. The standard piecewise formula needs no
/// modification to do this: the linear branch (`12.92 * l`) is defined for
/// every real `l`, and the power branch (`1.055 * l.powf(1.0/2.4) - 0.055`) is
/// defined for every `l > 0`, which is exactly the branch's domain
/// (`l > 0.0031308`). No clamping is applied, so this is monotone increasing
/// and continuous (though not C1 at the breakpoint) on the whole real line.
/// This is the "working tone domain" the curve stages below operate in.
fn encode_srgb_extended(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The inverse of `encode_srgb_extended`, equally extended: the linear branch
/// (`e / 12.92`) is defined for every real `e`, and the power branch
/// (`((e + 0.055) / 1.055).powf(2.4)`) is defined for every `e > -0.055`, which
/// holds everywhere the branch is used (`e > 0.04045`). No clamping.
fn decode_srgb_extended(e: f64) -> f64 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// A raised-cosine bump: 0 outside `[a, b]`, 1 at the midpoint, C1 (zero
/// derivative at both ends, so it splices smoothly into the flat regions
/// outside). `b'(x) = (PI / (b - a)) * sin(2*PI*(x - a) / (b - a))`, so its
/// maximum slope magnitude is `PI / (b - a)`, attained a quarter and
/// three-quarters of the way through the window.
fn bump(x: f64, (a, b): (f64, f64)) -> f64 {
    if x <= a || x >= b {
        0.0
    } else {
        0.5 * (1.0 - (2.0 * std::f64::consts::PI * (x - a) / (b - a)).cos())
    }
}

/// Stage 1: Whites/Blacks. An endpoint-anchored linear remap `y = (x - bp) /
/// (wp - bp)`, with the white point `wp` and black point `bp` moved by the
/// Whites and Blacks parameters. The crossing-prevention clamp holds
/// `wp - bp >= EPSILON_GAP`.
fn whites_blacks_stage(x: f64, whites: f64, blacks: f64) -> f64 {
    let wp = 1.0 - (whites / 100.0) * K_W;
    let bp = (blacks / 100.0) * K_B;
    let wp = if wp - bp < EPSILON_GAP {
        bp + EPSILON_GAP
    } else {
        wp
    };
    (x - bp) / (wp - bp)
}

/// Stage 2: Highlights/Shadows. An additive offset weighted by two raised-
/// cosine bumps, one over the upper-tonal window and one over the lower-tonal
/// window: `y = x + h * bump(x, HIGHLIGHT_WINDOW) + s * bump(x, SHADOW_WINDOW)`.
fn highlights_shadows_stage(x: f64, highlights: f64, shadows: f64) -> f64 {
    let h = (highlights / 100.0) * A_HS;
    let s = (shadows / 100.0) * A_HS;
    x + h * bump(x, HIGHLIGHT_WINDOW) + s * bump(x, SHADOW_WINDOW)
}

/// Stage 3: Contrast. A logistic S-curve normalized to fix `(0, 0)` and
/// `(1, 1)`: `f(x) = (g(x) - g(0)) / (g(1) - g(0))` with
/// `g(u) = 1 / (1 + exp(-alpha * (u - PIVOT)))` and
/// `alpha = ALPHA_MAX * contrast / 100`. Contrast = 0 is the identity
/// (handled as an explicit branch, since alpha = 0 makes the logistic formula
/// a 0/0 form).
fn contrast_stage(x: f64, contrast: f64) -> f64 {
    if contrast == 0.0 {
        return x;
    }
    let alpha = ALPHA_MAX * (contrast / 100.0);
    let g = |u: f64| 1.0 / (1.0 + (-alpha * (u - PIVOT)).exp());
    let g0 = g(0.0);
    let g1 = g(1.0);
    (g(x) - g0) / (g1 - g0)
}

/// The complete tone curve in the encoded working domain: Whites/Blacks, then
/// Highlights/Shadows, then Contrast, exactly the frozen composition order.
pub fn tone_curve(l_encoded: f64, params: ToneParams) -> f64 {
    let y = whites_blacks_stage(l_encoded, params.whites, params.blacks);
    let y = highlights_shadows_stage(y, params.highlights, params.shadows);
    contrast_stage(y, params.contrast)
}

/// The complete frozen Tone algorithm: luminance in, luminance-ratio-scaled
/// RGB out. All-neutral parameters return the input unchanged (an explicit
/// identity fast path, not a round-trip through encode/decode, so zero-valued
/// Basic introduces no round-trip change at all, matching the host contract).
pub fn tone_pixel(rgb_linear: [f64; 3], params: ToneParams) -> [f64; 3] {
    if params.is_neutral() {
        return rgb_linear;
    }
    let l_in = luminance(rgb_linear);
    let l_out = decode_srgb_extended(tone_curve(encode_srgb_extended(l_in), params));
    if l_in.abs() < EPSILON_L {
        let delta = l_out - l_in;
        [
            rgb_linear[0] + delta,
            rgb_linear[1] + delta,
            rgb_linear[2] + delta,
        ]
    } else {
        let ratio = l_out / l_in;
        [
            rgb_linear[0] * ratio,
            rgb_linear[1] * ratio,
            rgb_linear[2] * ratio,
        ]
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn extended_srgb_encode_decode_round_trip_near_the_unit_range() {
        for i in -50..=306 {
            let l = f64::from(i) / 100.0;
            let e = encode_srgb_extended(l);
            let back = decode_srgb_extended(e);
            assert!(
                (back - l).abs() <= 1e-9 + 1e-9 * l.abs(),
                "l={l} e={e} back={back}"
            );
        }
    }

    #[test]
    fn bump_is_zero_outside_its_window_and_one_at_its_midpoint() {
        for window in [HIGHLIGHT_WINDOW, SHADOW_WINDOW] {
            let (a, b) = window;
            assert_eq!(bump(a, window), 0.0);
            assert_eq!(bump(b, window), 0.0);
            assert_eq!(bump(a - 10.0, window), 0.0);
            assert_eq!(bump(b + 10.0, window), 0.0);
            let mid = (a + b) / 2.0;
            assert!((bump(mid, window) - 1.0).abs() < 1e-12);
        }
    }
}
