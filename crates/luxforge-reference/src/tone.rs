//! Independent f64 reference for the frozen global Tone algorithm.
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
/// -+K_W from 1.0; at Blacks = +-100 the black point moves by -+K_B from 0.0,
/// so positive Blacks lifts the dark end and negative Blacks crushes it.
const K_W: f64 = 0.25;
const K_B: f64 = 0.25;

/// The minimum white-point-minus-black-point gap the crossing-prevention clamp
/// enforces. With K_W = K_B = 0.25 the worst corner (Whites = +100, Blacks = -100)
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

/// The sRGB OETF (linear -> encoded), analytically continued to every finite real `l`, not just
/// `[0, 1]`: [`crate::srgb::encode_extended`]. This is the "working tone domain" the curve stages
/// below operate in.
///
/// `pub` (not private) so the vignette reference can reuse the same analytically-continued sRGB
/// OETF for its positive-amount mapping instead of duplicating it; see
/// `docs/design/presence-mixer-vignette.md`'s "Vignette: one positional unit" and
/// `crates/luxforge-reference/src/vignette.rs`.
pub fn encode_srgb_extended(l: f64) -> f64 {
    crate::srgb::encode_extended(l)
}

/// The inverse of `encode_srgb_extended`, equally extended: [`crate::srgb::decode_encoded`].
/// `pub` for the same cross-reference reason as `encode_srgb_extended` above.
pub fn decode_srgb_extended(e: f64) -> f64 {
    crate::srgb::decode_encoded(e)
}

/// Stage 1: Whites/Blacks. An endpoint-anchored linear remap `y = (x - bp) /
/// (wp - bp)`, with the white point `wp` and black point `bp` moved by the
/// Whites and Blacks parameters. The crossing-prevention clamp holds
/// `wp - bp >= EPSILON_GAP`.
fn whites_blacks_stage(x: f64, whites: f64, blacks: f64) -> f64 {
    let wp = 1.0 - (whites / 100.0) * K_W;
    let bp = -(blacks / 100.0) * K_B;
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
    if alpha < 0.1 {
        // Same normalized logistic, without subtracting two values near 0.5. This reference
        // uses f64's tanh directly; production uses a bounded f32 series near the origin.
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let tanhc = |z: f64| if z == 0.0 { 1.0 } else { z.tanh() / z };
        let kappa = tanhc(alpha / 4.0);
        let distance = x - PIVOT;
        let s = PIVOT + distance * tanhc((alpha * 0.5) * distance) / kappa;
        return if contrast > 0.0 {
            s
        } else {
            x - kappa * (s - x)
        };
    }
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
    tone_pixel_in(rgb_linear, params, Domain::RENDERED)
}

/// Highlights' and Whites' domain over a scene-referred input, in multiples of
/// the scene white: the linear luminance `C_HIGHLIGHTS * W` is the curve's
/// white. See "The operator" in `docs/design/basic-local-tone.md`.
pub const C_HIGHLIGHTS: f64 = 3.0;

/// Shadows' domain over a scene-referred input, in multiples of the scene
/// white.
pub const C_SHADOWS: f64 = 2.0;

/// The linear luminances the tone curve's white stands for: one for Whites,
/// Highlights, Blacks and Contrast, one for Shadows. A rendered input's domain
/// is `1` for both, which is the frozen curve exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Domain {
    pub highlights: f64,
    pub shadows: f64,
}

impl Domain {
    /// The rendered domain: the curve's white is linear 1.
    pub const RENDERED: Self = Self {
        highlights: 1.0,
        shadows: 1.0,
    };

    /// The domain over a scene-referred input whose scene white, after every gain
    /// before the curve, is `white` ([`exposed_white`]).
    pub fn scene(white: f64) -> Self {
        Self {
            highlights: C_HIGHLIGHTS * white,
            shadows: C_SHADOWS * white,
        }
    }
}

/// The tone curve on linear luminance `l` in `domain`: the encoded value of
/// `l / domain.highlights` passes Whites/Blacks; Shadows acts on the encoded
/// value of the same luminance over `domain.shadows`; Highlights and Contrast
/// act over `domain.highlights` again; the result is decoded and scaled back.
/// Where both domains are equal, no stage leaves the encoded domain, so the
/// rendered domain is [`tone_curve`] exactly.
pub fn tone_luminance_in(l: f64, params: ToneParams, domain: Domain) -> f64 {
    let (h, s) = (domain.highlights, domain.shadows);
    let y = whites_blacks_stage(encode_srgb_extended(l / h), params.whites, params.blacks);
    let y = if s == h {
        shadows_stage(y, params.shadows)
    } else {
        let over_s = encode_srgb_extended(h * decode_srgb_extended(y) / s);
        let shadowed = shadows_stage(over_s, params.shadows);
        encode_srgb_extended(s * decode_srgb_extended(shadowed) / h)
    };
    let y = highlights_stage(y, params.highlights);
    h * decode_srgb_extended(contrast_stage(y, params.contrast))
}

/// [`tone_pixel`] in `domain`: the luminance through [`tone_luminance_in`], RGB
/// rebuilt by the luminance ratio with the near-black rule. All-neutral
/// parameters return the input unchanged in every domain.
pub fn tone_pixel_in(rgb_linear: [f64; 3], params: ToneParams, domain: Domain) -> [f64; 3] {
    if params.is_neutral() {
        return rgb_linear;
    }
    let l_in = luminance(rgb_linear);
    let l_out = if domain == Domain::RENDERED {
        decode_srgb_extended(tone_curve(encode_srgb_extended(l_in), params))
    } else {
        tone_luminance_in(l_in, params, domain)
    };
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

/// The scene part of Basic's tone over a scene-referred input
/// (`docs/design/basic-local-tone.md`, "The operator"): Whites, Shadows and
/// Highlights in `domain`, with Blacks and Contrast left to [`rendered_tone`].
pub fn scene_tone(rgb_linear: [f64; 3], params: ToneParams, domain: Domain) -> [f64; 3] {
    tone_pixel_in(
        rgb_linear,
        ToneParams {
            contrast: 0.0,
            blacks: 0.0,
            ..params
        },
        domain,
    )
}

/// The rendered part of Basic's tone: Blacks, then Contrast, in the rendered
/// domain, after the Look on a RAW photograph and directly after the scene part
/// on a rendered one.
pub fn rendered_tone(rgb_linear: [f64; 3], params: ToneParams) -> [f64; 3] {
    tone_pixel(
        rgb_linear,
        ToneParams {
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            ..params
        },
    )
}

/// The largest number of block means the scene white is read from: 0.25 MP.
pub const SCENE_WHITE_SAMPLES: usize = 1 << 18;

/// The quantile of the block means' luminance that is the scene white.
pub const SCENE_WHITE_QUANTILE: f64 = 0.999;

/// The least scene white: a development darker than this is read as this, so
/// the domain never collapses.
pub const SCENE_WHITE_FLOOR: f64 = 1.0 / 256.0;

/// The side of the square blocks the scene white averages: the least `k` whose
/// grid of `ceil(width / k)` by `ceil(height / k)` blocks holds at most
/// [`SCENE_WHITE_SAMPLES`].
pub fn scene_white_block(width: usize, height: usize) -> usize {
    let mut k = 1;
    while width.div_ceil(k) * height.div_ceil(k) > SCENE_WHITE_SAMPLES {
        k += 1;
    }
    k
}

/// A development's scene white (`docs/design/basic-local-tone.md`): its linear
/// RGB averaged over [`scene_white_block`] blocks (a block at the right or bottom
/// edge over the pixels it holds), then the luminance of the block means at the
/// [`SCENE_WHITE_QUANTILE`] order statistic, index `floor(q * (n - 1))` of the
/// ascending means, no less than [`SCENE_WHITE_FLOOR`]. Sums run in `f64` in row
/// order within each block.
pub fn scene_white(width: usize, height: usize, rgb_linear: &[[f64; 3]]) -> f64 {
    assert_eq!(rgb_linear.len(), width * height, "one triple per pixel");
    if width == 0 || height == 0 {
        return SCENE_WHITE_FLOOR;
    }
    let k = scene_white_block(width, height);
    let (columns, rows) = (width.div_ceil(k), height.div_ceil(k));
    let mut means = Vec::with_capacity(columns * rows);
    for by in 0..rows {
        for bx in 0..columns {
            let (x0, y0) = (bx * k, by * k);
            let (x1, y1) = ((x0 + k).min(width), (y0 + k).min(height));
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += luminance(rgb_linear[y * width + x]);
                }
            }
            means.push(sum / ((x1 - x0) * (y1 - y0)) as f64);
        }
    }
    means.sort_by(f64::total_cmp);
    let index = (SCENE_WHITE_QUANTILE * (means.len() - 1) as f64).floor() as usize;
    means[index].max(SCENE_WHITE_FLOOR)
}

/// The scene white after a per-channel gain of `stops` (Basic's white balance and
/// Exposure, plain gains on a scene-referred input): `white` times the gain a
/// neutral grey's luminance takes.
pub fn exposed_white(white: f64, stops: [f64; 3]) -> f64 {
    white * (LUMA_R * stops[0].exp2() + LUMA_G * stops[1].exp2() + LUMA_B * stops[2].exp2())
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    /// Decimal evaluation of the original logistic equation at 430-digit precision supplies
    /// this fixture independently of the tanhc algebra used to avoid cancellation here.
    #[test]
    fn small_contrast_matches_high_precision_extended_cases() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/basic/contrast-small-cases.json"
        ))
        .expect("the independent high-precision contrast fixture");
        for case in fixture["cases"].as_array().expect("cases") {
            let contrast = case["contrast"].as_f64().expect("contrast");
            let input = case["input_encoded"].as_f64().expect("input");
            let expected = case["expected_encoded"].as_f64().expect("expected");
            let actual = contrast_stage(input, contrast);
            assert!(
                actual.is_finite() && (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
                "contrast={contrast} input={input}: actual {actual}, high precision {expected}"
            );
            assert_eq!(contrast_stage(0.0, contrast), 0.0);
            assert_eq!(contrast_stage(1.0, contrast), 1.0);
        }
    }

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

    #[test]
    fn the_rendered_domain_is_the_frozen_curve_and_an_equal_domain_is_a_scaled_one() {
        let params = ToneParams {
            contrast: 30.0,
            highlights: -60.0,
            shadows: 40.0,
            whites: 20.0,
            blacks: -10.0,
        };
        for i in 0..=40 {
            let l = f64::from(i) / 20.0;
            let frozen = decode_srgb_extended(tone_curve(encode_srgb_extended(l), params));
            assert_eq!(tone_luminance_in(l, params, Domain::RENDERED), frozen);
            // Both domains at 4: the curve of l / 4, scaled back.
            let scaled = Domain {
                highlights: 4.0,
                shadows: 4.0,
            };
            let expected =
                4.0 * decode_srgb_extended(tone_curve(encode_srgb_extended(l / 4.0), params));
            assert!((tone_luminance_in(l, params, scaled) - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn shadows_reads_its_own_domain_and_the_scene_part_leaves_its_whites_end_to_highlights() {
        // Shadows alone in the scene domain is Shadows on l / (C_SHADOWS * W).
        let white = 0.4;
        let domain = Domain::scene(white);
        let shadows = ToneParams {
            shadows: 80.0,
            ..ToneParams::NEUTRAL
        };
        for l in [0.001, 0.01, 0.05, 0.2, 0.5, 0.79, 1.5] {
            let s = C_SHADOWS * white;
            let expected =
                s * decode_srgb_extended(shadows_stage(encode_srgb_extended(l / s), 80.0));
            let actual = tone_luminance_in(l, shadows, domain);
            assert!(
                (actual - expected).abs() < 1e-12 * (1.0 + expected.abs()),
                "{l}: {actual} {expected}"
            );
        }
        // Past each stage's white, Shadows and Highlights pass the luminance through.
        let both = ToneParams {
            highlights: -100.0,
            shadows: 100.0,
            ..ToneParams::NEUTRAL
        };
        let past = 1.01 * C_HIGHLIGHTS * white;
        assert!((tone_luminance_in(past, both, domain) - past).abs() < 1e-12);
    }

    #[test]
    fn the_split_parts_take_only_their_own_fields() {
        let params = ToneParams {
            contrast: 50.0,
            highlights: -50.0,
            shadows: 30.0,
            whites: 25.0,
            blacks: 20.0,
        };
        let rgb = [0.3, 0.2, 0.1];
        let domain = Domain::scene(0.8);
        assert_eq!(
            scene_tone(rgb, params, domain),
            tone_pixel_in(
                rgb,
                ToneParams {
                    contrast: 0.0,
                    blacks: 0.0,
                    ..params
                },
                domain
            )
        );
        assert_eq!(
            rendered_tone(rgb, params),
            tone_pixel(
                rgb,
                ToneParams {
                    contrast: 50.0,
                    blacks: 20.0,
                    ..ToneParams::NEUTRAL
                }
            )
        );
        // On a rendered input, the two parts compose to the frozen curve when Blacks is neutral.
        let no_blacks = ToneParams {
            blacks: 0.0,
            ..params
        };
        let split = rendered_tone(scene_tone(rgb, no_blacks, Domain::RENDERED), no_blacks);
        let whole = tone_pixel(rgb, no_blacks);
        for c in 0..3 {
            assert!((split[c] - whole[c]).abs() < 1e-12, "{split:?} {whole:?}");
        }
    }

    #[test]
    fn the_scene_white_is_the_block_means_quantile() {
        // 1000 x 1 pixels: block 1, so the white is the luminance at index floor(0.999 * 999) = 998.
        let rgb: Vec<[f64; 3]> = (0..1000).map(|i| [f64::from(i) / 1000.0; 3]).collect();
        assert_eq!(scene_white_block(1000, 1), 1);
        assert!((scene_white(1000, 1, &rgb) - 0.998).abs() < 1e-12);
        // A frame past the sample budget is averaged in blocks, the edge blocks over what they hold.
        assert_eq!(scene_white_block(1024, 256), 1);
        assert_eq!(scene_white_block(1025, 256), 2);
        assert_eq!(scene_white_block(6000, 4000), 10);
        let (w, h) = (1025, 256);
        let rgb: Vec<[f64; 3]> = (0..w * h)
            .map(|i| if i % w == w - 1 { [8.0; 3] } else { [0.25; 3] })
            .collect();
        // The last column of blocks holds only the bright column: 128 of 65_664 means are 8.
        assert_eq!(scene_white(w, h, &rgb), 8.0);
        // A dark frame takes the floor.
        assert_eq!(scene_white(4, 4, &[[0.0; 3]; 16]), SCENE_WHITE_FLOOR);
    }

    #[test]
    fn exposure_scales_the_scene_white_exactly() {
        assert_eq!(exposed_white(0.5, [1.0; 3]), 1.0);
        assert_eq!(exposed_white(0.5, [0.0; 3]), 0.5);
        let gains = LUMA_R * 2.0 + LUMA_G + LUMA_B * 0.5;
        assert!((exposed_white(1.0, [1.0, 0.0, -1.0]) - gains).abs() < 1e-15);
    }
}
