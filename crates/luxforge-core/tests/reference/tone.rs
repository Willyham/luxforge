//! Independent f64 reference for the frozen global Tone algorithm (TASK-011).
//!
//! This module shares no code with production. It exists so a later production
//! implementation of the `luxforge.basic.adjust` tone unit has an oracle it
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
/// ALPHA_MAX (the sign selects the curve, not the exponent's sign). See
/// "Contrast" in the design doc for the derivative proof this value does not
/// affect (the derivative is provably positive for any finite, nonzero alpha,
/// on both sides).
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

/// Highlights/Shadows odds-bias steepness scale: at parameter = +-100 the
/// family's exponent `k` is +-K_HS. See "Highlights and Shadows" in the design
/// doc: unlike a bump-weighted additive offset, this family's strength is not
/// capped by a fixed derivative budget, so K_HS trades off effect strength
/// against how gently the composed curve bends, not against a hard positivity
/// ceiling.
const K_HS: f64 = 1.5;

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
///
/// `pub` (not private) so the vignette reference (TASK-004) can reuse the same
/// analytically-continued sRGB OETF for its positive-amount mapping instead of
/// duplicating it; see `docs/design/presence-mixer-vignette.md`'s "Vignette:
/// one positional unit" and `tests/reference/vignette.rs`.
pub fn encode_srgb_extended(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The inverse of `encode_srgb_extended`, equally extended: the linear branch
/// (`e / 12.92`) is defined for every real `e`, and the power branch
/// (`((e + 0.055) / 1.055).powf(2.4)`) is defined for every `e > -0.055`, which
/// holds everywhere the branch is used (`e > 0.04045`). No clamping. `pub` for
/// the same cross-reference reason as `encode_srgb_extended` above.
pub fn decode_srgb_extended(e: f64) -> f64 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
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

/// The odds-bias curve `B_k(x) = x / (x + (1 - x) * exp(-k))` on `[0, 1]`,
/// extended by passing values outside `[0, 1]` through unchanged (identity).
/// `B_k` fixes `0` and `1` exactly for every finite `k` (both terms of the
/// denominator are nonnegative combinations of the positive values `1` and
/// `exp(-k)` when `x` is itself in `[0, 1]`, so the denominator is never zero
/// there, and the pass-through choice matches `B_k` exactly at the boundary:
/// `B_k(0) = 0`, `B_k(1) = 1`), so this extension is continuous with no
/// special-casing at `x = 0` or `x = 1`. `k > 0` lifts (`B_k(x) > x` on
/// `(0, 1)`), `k < 0` crushes, `k = 0` is the identity. See "Highlights and
/// Shadows" in the design doc for why pass-through was chosen over a linear
/// extrapolation (the tangent-slope extrapolation amplifies an
/// already-stretched extended-domain input, which a pointwise pass-through
/// does not).
fn odds_bias(x: f64, k: f64) -> f64 {
    if x <= 0.0 || x >= 1.0 {
        x
    } else {
        x / (x + (1.0 - x) * (-k).exp())
    }
}

/// The Shadows weight: `1` at `x = 0`, falling smoothly to `0` at `x = 1`, the
/// input clamped to `[0, 1]` first so the weight itself never leaves `[0, 1]`
/// outside the primary domain (a defensive clamp: `odds_bias`'s pass-through
/// already makes the blend below independent of this weight's value outside
/// `[0, 1]`, since `w * x + (1 - w) * x = x` for any `w`).
fn shadow_weight(x: f64) -> f64 {
    let clamped = x.clamp(0.0, 1.0);
    (1.0 - clamped).powi(2)
}

/// Shadows: a blend between the lifting/crushing odds-bias curve and the
/// identity, weighted by `shadow_weight`, so the effect is strongest near
/// black and fades smoothly (not abruptly) toward white:
/// `y = w(x) * B_{k_s}(x) + (1 - w(x)) * x`, `k_s = K_HS * shadows / 100`.
/// Exact at both ends: `shadow_weight(0) = 1` and `B_{k_s}(0) = 0` give
/// `y(0) = 0`; `shadow_weight(1) = 0` gives `y(1) = 1 * B + 0 = 1`. So Shadows
/// leaves pure black *and* pure white exactly unchanged, for every `shadows`.
fn shadows_stage(x: f64, shadows: f64) -> f64 {
    let k_s = K_HS * (shadows / 100.0);
    let w = shadow_weight(x);
    w * odds_bias(x, k_s) + (1.0 - w) * x
}

/// Highlights: `shadows_stage` mirrored about the pivot's midpoint
/// (`x` maps to `1 - x`, `k` maps to `-k`), so Highlights lifts/crushes the
/// *upper* end with the same shape Shadows uses for the lower end, and by the
/// same reasoning leaves both `0` and `1` exactly unchanged.
/// `highlights = +100` lifts near white (mirroring Shadows' lift near
/// black); `highlights = -100` crushes near white -- the "compresses the top
/// end" direction the design calls for.
fn highlights_stage(x: f64, highlights: f64) -> f64 {
    let k = -K_HS * (highlights / 100.0);
    let mirrored = 1.0 - x;
    let w = shadow_weight(mirrored);
    let inner = w * odds_bias(mirrored, k) + (1.0 - w) * mirrored;
    1.0 - inner
}

/// Stage 2: Highlights/Shadows, composed as Shadows then Highlights (not
/// folded into one expression, so each half keeps its own simple,
/// independently-provable monotonicity argument; see "Highlights and
/// Shadows" in the design doc).
fn highlights_shadows_stage(x: f64, highlights: f64, shadows: f64) -> f64 {
    highlights_stage(shadows_stage(x, shadows), highlights)
}

/// Stage 3: Contrast. `S(x) = (g(x) - g(0)) / (g(1) - g(0))` is a logistic
/// S-curve normalized to fix `(0, 0)` and `(1, 1)`, with
/// `g(u) = 1 / (1 + exp(-alpha * (u - PIVOT)))` and
/// `alpha = ALPHA_MAX * |contrast| / 100`, so it is built from the slider's
/// magnitude only. Positive Contrast is `S` itself. Negative Contrast reflects
/// `S`'s deviation from the identity, `x - kappa * (S(x) - x)`, scaled by
/// `kappa = 1 / sigma`, where `sigma = S'(PIVOT)` is the positive curve's
/// slope at the pivot: the pivot slope becomes `1 / sigma`, so `-c` flattens
/// the midtones by exactly the factor `+c` steepens them. (Negating `alpha`
/// instead is not a flattening: `g` at `-alpha` is `1 - g` at `alpha`, and the
/// endpoint normalization cancels that reflection, giving `+c`'s curve.)
/// Contrast = 0 is the identity (handled as an explicit branch, since
/// alpha = 0 makes the logistic formula a 0/0 form).
fn contrast_stage(x: f64, contrast: f64) -> f64 {
    if contrast == 0.0 {
        return x;
    }
    let alpha = ALPHA_MAX * (contrast.abs() / 100.0);
    let g = |u: f64| 1.0 / (1.0 + (-alpha * (u - PIVOT)).exp());
    let g0 = g(0.0);
    let g1 = g(1.0);
    let s = (g(x) - g0) / (g1 - g0);
    if contrast > 0.0 {
        return s;
    }
    // S'(x) = alpha * g(x) * (1 - g(x)) / (g1 - g0), and g(PIVOT) = 1/2.
    let sigma = (alpha / 4.0) / (g1 - g0);
    let kappa = 1.0 / sigma;
    x - kappa * (s - x)
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
    fn odds_bias_fixes_both_endpoints_and_passes_through_outside_them() {
        for k in [-3.0, -1.5, -0.1, 0.1, 1.5, 3.0] {
            assert_eq!(odds_bias(0.0, k), 0.0);
            assert_eq!(odds_bias(1.0, k), 1.0);
            assert_eq!(odds_bias(-1.0, k), -1.0);
            assert_eq!(odds_bias(2.0, k), 2.0);
        }
        // k > 0 lifts strictly inside (0, 1); k < 0 crushes.
        assert!(odds_bias(0.5, 1.5) > 0.5);
        assert!(odds_bias(0.5, -1.5) < 0.5);
        assert_eq!(odds_bias(0.5, 0.0), 0.5);
    }

    #[test]
    fn contrast_fixes_both_endpoints_exactly_on_both_sides() {
        for contrast in [-100.0, -50.0, -10.0, -0.5, 0.5, 10.0, 50.0, 100.0] {
            assert_eq!(contrast_stage(0.0, contrast), 0.0, "contrast={contrast}");
            assert_eq!(contrast_stage(1.0, contrast), 1.0, "contrast={contrast}");
        }
    }

    /// Negative Contrast tends to the identity as the slider approaches 0 from
    /// below, as positive Contrast does from above: no jump at the neutral
    /// value on either side.
    #[test]
    fn contrast_tends_to_the_identity_near_zero_on_both_sides() {
        for x in [-0.5, 0.1, 0.3, 0.5, 0.7, 0.9, 2.0] {
            for contrast in [-1e-3, 1e-3] {
                let y = contrast_stage(x, contrast);
                assert!((y - x).abs() < 1e-6, "contrast={contrast} x={x} y={y}");
            }
        }
    }

    #[test]
    fn shadows_and_highlights_leave_both_endpoints_exactly_unchanged() {
        for amount in [-100.0, -50.0, 50.0, 100.0] {
            assert_eq!(shadows_stage(0.0, amount), 0.0, "shadows={amount}");
            assert_eq!(shadows_stage(1.0, amount), 1.0, "shadows={amount}");
            assert_eq!(highlights_stage(0.0, amount), 0.0, "highlights={amount}");
            assert_eq!(highlights_stage(1.0, amount), 1.0, "highlights={amount}");
        }
    }
}
