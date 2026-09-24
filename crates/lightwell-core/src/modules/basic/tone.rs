//! The Basic module's Tone unit: Contrast, Highlights, Shadows, Whites and Blacks, composed as one
//! frozen global luminance curve (`docs/design/basic-tone.md`, TASK-011/TASK-012). This file is the
//! `f32` production transcription of that document and of the independent `f64` reference at
//! `crates/lightwell-core/tests/reference/tone.rs`; the three must be read together, and every
//! constant here is named identically to the constant of the same name there.
//!
//! Everything that does not depend on the pixel is precomputed once, in `f64`, in the constructor:
//! the Contrast logistic's slope, its endpoint normalization and negative Contrast's reflection
//! weight `kappa`, the Highlights/Shadows odds-bias exponentials `e^(+-k)`, and the Whites/Blacks
//! endpoint remap's intercept and inverse gap. The per-pixel work in `apply_row` is plain `f32`
//! arithmetic plus, unavoidably, one `exp` for Contrast's pixel-dependent logistic argument (on
//! either side of the slider) and the sRGB encode/decode of the pixel's own luminance; nothing here
//! is clamped, matching the pointwise colour contract that the host clamps once at the end of a
//! run, not each unit.
use crate::modules::PointwiseColor;

/// Rec. 709 / sRGB luma coefficients on linear sRGB. See "Luminance" in the design doc.
///
/// Written as `f64` and narrowed once, the pattern `colour.rs` already uses for the Oklab matrices:
/// the units below multiply the `f32` values, and the host's value-based mask components
/// (`crate::mask::range`) multiply the `f64` ones, so the editor has **one** definition of
/// luminance rather than two that could drift.
pub(crate) const LUMA_R_F64: f64 = 0.2126;
pub(crate) const LUMA_G_F64: f64 = 0.7152;
pub(crate) const LUMA_B_F64: f64 = 0.0722;

const LUMA_R: f32 = LUMA_R_F64 as f32;
const LUMA_G: f32 = LUMA_G_F64 as f32;
const LUMA_B: f32 = LUMA_B_F64 as f32;

/// The curve domain's pivot: encoded mid-grey. See "Contrast" in the design doc.
const PIVOT: f32 = 0.5;

/// Contrast steepness scale: `alpha = ALPHA_MAX * |contrast| / 100`; the sign selects the curve,
/// not the exponent's sign. See "Contrast" in the design doc.
const ALPHA_MAX: f64 = 6.0;

/// Whites/Blacks endpoint range and the crossing-prevention clamp's minimum gap. See "Whites and
/// Blacks" in the design doc.
const K_W: f64 = 0.25;
const K_B: f64 = 0.25;
const EPSILON_GAP: f64 = 0.05;

/// Highlights/Shadows odds-bias steepness scale: `k = +-K_HS * amount / 100`. See "Highlights and
/// Shadows" in the design doc.
const K_HS: f64 = 1.5;

/// Below this linear luminance, RGB reconstruction switches to the additive near-black rule. See
/// "Luminance ratio and gamut policy" in the design doc.
const EPSILON_L: f32 = 1e-6;

/// Contrast, Highlights, Shadows, Whites and Blacks, composed Whites/Blacks, then
/// Highlights/Shadows, then Contrast (the frozen order) on encoded sRGB luminance, with RGB
/// reconstructed by the luminance ratio. All five parameters are stored in the agreed -100..100 UI
/// range; the module never constructs this unit when every one of them is neutral.
#[derive(Debug)]
pub(super) struct Tone {
    contrast: f64,
    highlights: f64,
    shadows: f64,
    whites: f64,
    blacks: f64,
    /// Contrast: `alpha = ALPHA_MAX * |contrast| / 100`, computed once. Only read when
    /// `contrast != 0.0`; the identity branch in `contrast_stage` never reaches it otherwise.
    alpha: f32,
    /// Contrast's endpoint normalization, `g(0)` and `1 / (g(1) - g(0))`, computed once in `f64`.
    contrast_g0: f32,
    contrast_inv_gap: f32,
    /// Negative Contrast's reflection weight, `kappa = 1 / sigma` with `sigma = S'(PIVOT)` the
    /// normalized logistic's pivot slope, computed once in `f64`. Only read when `contrast < 0.0`.
    contrast_kappa: f32,
    /// The odds-bias exponential `e^(-k)`, precomputed once per stage so the per-pixel path needs
    /// no `exp` call for either Shadows or Highlights.
    shadows_exp_neg_k: f32,
    highlights_exp_neg_k: f32,
    /// Whites/Blacks' endpoint-anchored linear remap, `y = (x - bp) / (wp - bp)`, precomputed as an
    /// intercept (`bp`, after the crossing-prevention clamp) and an inverse gap.
    blacks_bp: f32,
    whites_blacks_inv_gap: f32,
}

impl Tone {
    pub(super) fn new(
        contrast: f64,
        highlights: f64,
        shadows: f64,
        whites: f64,
        blacks: f64,
    ) -> Self {
        // Contrast: a logistic S-curve normalized to fix (0, 0) and (1, 1), built from the
        // slider's magnitude. Negative Contrast reflects its deviation from the identity, scaled by
        // kappa = 1 / sigma, sigma = S'(PIVOT) = alpha * g(PIVOT) * (1 - g(PIVOT)) / (g1 - g0) with
        // g(PIVOT) = 1/2 (see `contrast_stage`).
        let alpha = ALPHA_MAX * (contrast.abs() / 100.0);
        let g = |u: f64| 1.0 / (1.0 + (-alpha * (u - 0.5)).exp());
        let g0 = g(0.0);
        let g1 = g(1.0);
        // contrast == 0.0 makes alpha == 0.0 and so g1 - g0 == 0.0; the identity branch in
        // `contrast_stage` never reads `contrast_inv_gap` or `contrast_kappa` in that case, so 0.0
        // here is a safe, finite placeholder rather than an unused NaN/inf from dividing by zero.
        let (contrast_inv_gap, contrast_kappa) = if contrast == 0.0 {
            (0.0, 0.0)
        } else {
            let sigma = (alpha / 4.0) / (g1 - g0);
            (1.0 / (g1 - g0), 1.0 / sigma)
        };

        // Highlights/Shadows: the odds-bias curve's exponential, one per stage.
        let k_s = K_HS * (shadows / 100.0);
        let k_h = -K_HS * (highlights / 100.0);
        let shadows_exp_neg_k = (-k_s).exp();
        let highlights_exp_neg_k = (-k_h).exp();

        // Whites/Blacks: the endpoint-anchored linear remap, with the crossing-prevention clamp
        // (provably never active over the agreed +-100 range, but evaluated unconditionally so a
        // future range change cannot silently divide by zero or invert the mapping).
        let wp = 1.0 - (whites / 100.0) * K_W;
        let bp = (blacks / 100.0) * K_B;
        let wp = if wp - bp < EPSILON_GAP {
            bp + EPSILON_GAP
        } else {
            wp
        };

        Self {
            contrast,
            highlights,
            shadows,
            whites,
            blacks,
            alpha: alpha as f32,
            contrast_g0: g0 as f32,
            contrast_inv_gap: contrast_inv_gap as f32,
            contrast_kappa: contrast_kappa as f32,
            shadows_exp_neg_k: shadows_exp_neg_k as f32,
            highlights_exp_neg_k: highlights_exp_neg_k as f32,
            blacks_bp: bp as f32,
            whites_blacks_inv_gap: (1.0 / (wp - bp)) as f32,
        }
    }

    /// Stage 1: Whites/Blacks, `y = (x - bp) / (wp - bp)`, both precomputed. A constant-slope
    /// remap, so its derivative never depends on `x`.
    fn whites_blacks(&self, x: f32) -> f32 {
        (x - self.blacks_bp) * self.whites_blacks_inv_gap
    }

    /// The odds-bias curve `B_k(x) = x / (x + (1 - x) * e^-k)` on `[0, 1]`, passed straight through
    /// outside it: no pole, no amplification of an already-stretched extended-domain input (see
    /// "Highlights and Shadows" in the design doc). `exp_neg_k` is the precomputed `e^(-k)`.
    fn odds_bias(x: f32, exp_neg_k: f32) -> f32 {
        if x <= 0.0 || x >= 1.0 {
            x
        } else {
            x / (x + (1.0 - x) * exp_neg_k)
        }
    }

    /// The Shadows/Highlights blend weight: `1` at `x = 0`, `0` at `x = 1`, the input clamped to
    /// `[0, 1]` first. This clamp is defensive and applies only to the weight's own input, never to
    /// the pixel value the blend below actually carries forward.
    fn blend_weight(x: f32) -> f32 {
        let clamped = x.clamp(0.0, 1.0);
        let complement = 1.0 - clamped;
        complement * complement
    }

    /// Stage 2a: Shadows, `w(x) * B_ks(x) + (1 - w(x)) * x`. Exact at both ends for every `shadows`
    /// value: `w(0) = 1, B(0) = 0` gives `y(0) = 0`; `w(1) = 0` gives `y(1) = 1`.
    fn shadows_stage(&self, x: f32) -> f32 {
        let w = Self::blend_weight(x);
        w * Self::odds_bias(x, self.shadows_exp_neg_k) + (1.0 - w) * x
    }

    /// Stage 2b: Highlights, `shadows_stage`'s construction mirrored about the pivot's midpoint
    /// (`x <-> 1 - x`), so it lifts/crushes the upper end with the same shape and the same
    /// exact-endpoint property.
    fn highlights_stage(&self, x: f32) -> f32 {
        let mirrored = 1.0 - x;
        let w = Self::blend_weight(mirrored);
        let inner = w * Self::odds_bias(mirrored, self.highlights_exp_neg_k) + (1.0 - w) * mirrored;
        1.0 - inner
    }

    /// Stage 3: Contrast. Positive Contrast is the normalized logistic `S`; negative Contrast is
    /// `x - kappa * (S(x) - x)`, the same curve's deviation from the identity reflected and scaled
    /// so its pivot slope is the reciprocal of the positive curve's. `contrast == 0.0` is an
    /// explicit identity branch, matching the reference (the formula is a `0/0` form at
    /// `alpha = 0`, and the branch also guarantees bit-exact identity rather than a
    /// numerically-close approximation).
    fn contrast_stage(&self, x: f32) -> f32 {
        if self.contrast == 0.0 {
            return x;
        }
        let g = 1.0 / (1.0 + (-self.alpha * (x - PIVOT)).exp());
        let s = (g - self.contrast_g0) * self.contrast_inv_gap;
        if self.contrast > 0.0 {
            s
        } else {
            x - self.contrast_kappa * (s - x)
        }
    }

    /// The complete curve in the encoded working domain: Whites/Blacks, then Shadows, then
    /// Highlights, then Contrast — the frozen composition order (Shadows before Highlights, both
    /// before Contrast).
    fn tone_curve(&self, x: f32) -> f32 {
        let x = self.whites_blacks(x);
        let x = self.shadows_stage(x);
        let x = self.highlights_stage(x);
        self.contrast_stage(x)
    }
}

/// The sRGB OETF (linear -> encoded), analytically continued to every finite value, not clamped to
/// `[0, 1]`. See "Working tone domain" in the design doc.
///
/// `pub(crate)`: the vignette module's positive-amount branch reuses this exact function (see the
/// `mod tone` doc comment in `basic/mod.rs`); the formula is unchanged for that reuse.
pub(crate) fn encode_srgb_extended(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The inverse of `encode_srgb_extended`, equally extended.
pub(crate) fn decode_srgb_extended(e: f32) -> f32 {
    if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

impl PointwiseColor for Tone {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let l_in = LUMA_R * pixel[0] + LUMA_G * pixel[1] + LUMA_B * pixel[2];
            let l_out = decode_srgb_extended(self.tone_curve(encode_srgb_extended(l_in)));
            // Near-black rule: below EPSILON_L, divide by (near) zero is numerically unsafe, so
            // reconstruction switches to the additive rule. See "Luminance ratio and gamut policy"
            // in the design doc.
            if l_in.abs() < EPSILON_L {
                let delta = l_out - l_in;
                pixel[0] += delta;
                pixel[1] += delta;
                pixel[2] += delta;
            } else {
                let ratio = l_out / l_in;
                pixel[0] *= ratio;
                pixel[1] *= ratio;
                pixel[2] *= ratio;
            }
        }
    }

    /// A non-finite stored value, or a coefficient that overflowed computing it, is refused at
    /// compilation before any frame is touched.
    fn is_finite(&self) -> bool {
        self.contrast.is_finite()
            && self.highlights.is_finite()
            && self.shadows.is_finite()
            && self.whites.is_finite()
            && self.blacks.is_finite()
            && self.alpha.is_finite()
            && self.contrast_g0.is_finite()
            && self.contrast_inv_gap.is_finite()
            && self.contrast_kappa.is_finite()
            && self.shadows_exp_neg_k.is_finite()
            && self.highlights_exp_neg_k.is_finite()
            && self.blacks_bp.is_finite()
            && self.whites_blacks_inv_gap.is_finite()
    }

    /// The five stored values. The host compares compiled operations by this string, so two units
    /// that describe themselves identically must process identically: every coefficient above is a
    /// pure function of these five numbers.
    fn describe(&self) -> String {
        format!(
            "tone(contrast={:+}, highlights={:+}, shadows={:+}, whites={:+}, blacks={:+})",
            self.contrast, self.highlights, self.shadows, self.whites, self.blacks
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic/tone-cases.json")
    }

    #[derive(Deserialize)]
    struct FixtureParams {
        contrast: f64,
        highlights: f64,
        shadows: f64,
        whites: f64,
        blacks: f64,
    }

    #[derive(Deserialize)]
    struct ToneCase {
        label: String,
        input_linear_rgb: [f64; 3],
        params: FixtureParams,
        expected_linear_rgb: [f64; 3],
    }

    /// The frozen tolerance from `docs/design/basic-tone.md` "Frozen tolerance":
    /// `1e-5 + 1e-5 * |reference|`, in linear float.
    fn tolerance(reference: f64) -> f64 {
        1e-5 + 1e-5 * reference.abs()
    }

    /// The production `f32` unit against every case of the frozen oracle fixture
    /// `fixtures/basic/tone-cases.json`, applied to the decoded linear inputs the fixture already
    /// carries.
    ///
    /// Maximum observed error across all 89 committed cases and all three channels:
    /// `~4.99e-7` (well inside the frozen `1e-5 + 1e-5 * |reference|` tolerance), measured by this
    /// test's own `max_error` tracking and printed with `cargo test -p lightwell-core --lib --
    /// --nocapture modules::basic::tone::tests::production_matches_the_frozen_oracle_fixture_within_tolerance`.
    /// The 26 negative-Contrast cases stay within `~4.3e-7`.
    #[test]
    fn production_matches_the_frozen_oracle_fixture_within_tolerance() {
        let raw = fs::read_to_string(fixture_path()).expect("the tone-cases fixture");
        let cases: Vec<ToneCase> = serde_json::from_str(&raw).expect("valid JSON");
        assert!(cases.len() >= 89, "the committed fixture has 89 cases");
        let mut max_error = 0.0_f64;
        for case in &cases {
            let unit = Tone::new(
                case.params.contrast,
                case.params.highlights,
                case.params.shadows,
                case.params.whites,
                case.params.blacks,
            );
            let mut row = [[
                case.input_linear_rgb[0] as f32,
                case.input_linear_rgb[1] as f32,
                case.input_linear_rgb[2] as f32,
            ]];
            unit.apply_row(0, 0, &mut row);
            for (channel, expected) in case.expected_linear_rgb.iter().enumerate() {
                let actual = f64::from(row[0][channel]);
                let expected = *expected;
                let error = (actual - expected).abs();
                max_error = max_error.max(error);
                assert!(
                    error <= tolerance(expected),
                    "{}: channel {channel} actual {actual} expected {expected} error {error}",
                    case.label
                );
            }
        }
        println!("maximum observed error against the frozen fixture: {max_error}");
    }

    #[test]
    fn finiteness_follows_the_stored_values_and_their_coefficients() {
        assert!(Tone::new(50.0, -50.0, 25.0, -25.0, 10.0).is_finite());
        assert!(Tone::new(100.0, 100.0, 100.0, 100.0, 100.0).is_finite());
        assert!(Tone::new(-100.0, -100.0, -100.0, -100.0, -100.0).is_finite());
        assert!(!Tone::new(f64::NAN, 0.0, 0.0, 0.0, 0.0).is_finite());
        assert!(!Tone::new(0.0, f64::INFINITY, 0.0, 0.0, 0.0).is_finite());
        assert!(!Tone::new(0.0, 0.0, f64::NEG_INFINITY, 0.0, 0.0).is_finite());
    }

    #[test]
    fn the_description_lists_all_five_values() {
        let unit = Tone::new(20.0, -10.0, 5.0, 0.0, -100.0);
        assert_eq!(
            unit.describe(),
            "tone(contrast=+20, highlights=-10, shadows=+5, whites=+0, blacks=-100)"
        );
        assert_ne!(
            Tone::new(1.0, 0.0, 0.0, 0.0, 0.0).describe(),
            Tone::new(1.001, 0.0, 0.0, 0.0, 0.0).describe(),
            "a value below the display precision still describes itself apart"
        );
        assert_ne!(
            Tone::new(1.0, 0.0, 0.0, 0.0, 0.0).describe(),
            Tone::new(2.0, 0.0, 0.0, 0.0, 0.0).describe(),
            "two different stored values never describe themselves the same way"
        );
    }

    /// The module never constructs this unit when every field is neutral (compile drops it
    /// entirely so the identity byte path is kept), but the unit itself is still mathematically the
    /// identity if it were: every stage's identity branch or fixed point applies. `1e-5` absorbs the
    /// extended sRGB encode/decode round trip's float noise, not an approximation of the algorithm.
    #[test]
    fn all_neutral_parameters_are_the_identity_up_to_float_noise() {
        let unit = Tone::new(0.0, 0.0, 0.0, 0.0, 0.0);
        let input = [[0.0_f32, 0.25, 1.0], [0.5, 0.02, 0.9], [2.0, -0.1, 0.003]];
        let mut row = input;
        unit.apply_row(0, 0, &mut row);
        for (actual, expected) in row.iter().zip(input.iter()) {
            for (a, e) in actual.iter().zip(expected.iter()) {
                assert!((a - e).abs() < 1e-5, "actual {a} expected {e}");
            }
        }
    }

    /// A pointwise, per-pixel curve applied to an achromatic pixel (equal channels) must return
    /// another achromatic pixel: the same scalar ratio or the same additive delta is applied to all
    /// three equal channels.
    #[test]
    fn grey_pixels_stay_grey_for_a_combined_parameter_set() {
        let unit = Tone::new(40.0, -30.0, 20.0, 10.0, -15.0);
        let mut row: Vec<[f32; 3]> = (0..=64)
            .map(|i| {
                let l = i as f32 / 64.0;
                [l, l, l]
            })
            .collect();
        unit.apply_row(0, 0, &mut row);
        for pixel in &row {
            assert!(
                (pixel[0] - pixel[1]).abs() < 1e-5 && (pixel[1] - pixel[2]).abs() < 1e-5,
                "{pixel:?}"
            );
        }
    }
}
