//! The independent f64 references the core's tests and xtask's acceptance check production
//! against: the sRGB transfer function in [`srgb`], exposure here, and one module per study
//! (colour, the Tone curve, the DNG corrections, the RAW look, mask, mixer, Presence, the preview
//! error measure, range, tone, vignette, white balance), and the declared tolerance of each output
//! kind against the reference renderer in [`tolerance`].
//!
//! Every reference is written directly from its study's formulas — the standard sRGB transfer
//! function ([`srgb`] carries its constants) and each design's numerical contract — not by
//! reading or copying the production code. This crate depends on
//! nothing, and `cargo xtask check-repository` refuses a manifest of it that names
//! `luxforge-core`, so a reference cannot import the code it checks; the two implementations only
//! meet at test assertions. The studies that prove each reference's own properties and freeze the
//! committed fixture corpora are this crate's integration tests (`tests/studies/`).
//!
//! Declared precision (also recorded in `fixtures/basic/README.md`): histogram counts must
//! match exactly (they are integer reductions over exact bytes); production colour output
//! must match this reference within `1e-6 + 1e-6 * abs(reference)` in linear float, and at
//! most one output code of difference at a quantization boundary.

pub mod colour;
pub mod curve;
pub mod detail;
pub mod dng;
pub mod geometry;
pub mod look;
pub mod mask;
pub mod mixer;
pub mod presence;
pub mod preview_error;
pub mod range;
mod rng;
pub mod srgb;
pub mod tolerance;
pub mod tone;
pub mod vignette;
pub mod white_balance;

pub use rng::SplitMix64;

/// Exposure: multiply a linear-light channel value by `2^ev`. Not clamped; callers apply this
/// between decode and the final encode/quantize step, matching the design's "preserve finite
/// values outside `[0,1]` between color operations" rule.
pub fn exposure(linear: f64, ev: f64) -> f64 {
    linear * 2f64.powf(ev)
}

/// The linear-light value at which the output code changes from `k - 1` to `k`: the linear
/// value whose encoded value is exactly `(k - 0.5) / 255`. Valid for `k` in `1..=255`; `k = 0`
/// has no lower threshold (code 0 is the floor of the encoding, not a boundary between two
/// codes).
///
/// A production quantizer that agrees with [`srgb::code`] must, for every `k` in `1..=255`, map
/// every linear value below this threshold to `k - 1` and every value at or above it to `k` (see
/// the boundary-walk test below).
pub fn code_threshold(k: u8) -> f64 {
    assert!(k >= 1, "code_threshold is undefined for k = 0");
    srgb::decode_encoded((f64::from(k) - 0.5) / 255.0)
}

/// One stepwise colour operation. Later tasks add tone and white-balance ops here without
/// touching the pixel-evaluation shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RefOp {
    Exposure(f64),
    /// Runs `colour::apply_vibrance`, the frozen Oklab vibrance equation.
    Vibrance(f64),
    /// Runs `colour::apply_saturation`, the frozen Oklab saturation equation.
    Saturation(f64),
}

/// Decode `rgb` to linear light, apply `ops` in order without clamping between them, then
/// clamp/encode/quantize once at the end. Alpha is not represented here: callers that carry
/// alpha leave it untouched, matching the design's "preserve alpha" rule.
pub fn evaluate_pixel(rgb: [u8; 3], ops: &[RefOp]) -> [u8; 3] {
    let mut channels = [
        srgb::decode(rgb[0]),
        srgb::decode(rgb[1]),
        srgb::decode(rgb[2]),
    ];
    for op in ops {
        match *op {
            RefOp::Exposure(ev) => {
                for channel in &mut channels {
                    *channel = exposure(*channel, ev);
                }
            }
            RefOp::Vibrance(v) => {
                channels = colour::apply_vibrance(channels, v);
            }
            RefOp::Saturation(s) => {
                channels = colour::apply_saturation(channels, s);
            }
        }
    }
    [
        srgb::code(channels[0]),
        srgb::code(channels[1]),
        srgb::code(channels[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_of_every_code_is_identity() {
        for code in 0u8..=255 {
            assert_eq!(
                srgb::code(srgb::decode(code)),
                code,
                "code {code} failed to round trip"
            );
        }
    }

    #[test]
    fn zero_ev_is_identity_on_every_code() {
        for code in 0u8..=255 {
            let rgb = [code, code, code];
            assert_eq!(evaluate_pixel(rgb, &[RefOp::Exposure(0.0)]), rgb);
            // An empty op list must agree with an explicit 0 EV exposure.
            assert_eq!(evaluate_pixel(rgb, &[]), rgb);
        }
    }

    #[test]
    fn plus_one_then_minus_one_ev_returns_the_exact_decoded_value_before_quantization() {
        // 2f64.powf(1.0) and 2f64.powf(-1.0) are exact powers of two, so multiplying a
        // decoded channel by both in turn is an exact round trip in IEEE754 (no rounding),
        // independent of the final quantization step this test does not reach.
        for code in 0u8..=255 {
            let decoded = srgb::decode(code);
            let up = exposure(decoded, 1.0);
            let down = exposure(up, -1.0);
            assert_eq!(
                down, decoded,
                "code {code}: +1 EV/-1 EV did not invert exactly"
            );
        }
    }

    #[test]
    fn hand_checked_values() {
        // code 128 / 255 = 0.501960784313725..., above the encoded decode threshold
        // (0.04045), so the nonlinear branch applies:
        // ((0.501960784313725 + 0.055) / 1.055) ^ 2.4 = 0.21586050011389...
        let decoded_128 = srgb::decode(128);
        assert!(
            (decoded_128 - 0.215_860_500_113_89).abs() < 1e-7,
            "decoded_128 = {decoded_128}"
        );

        // +1 EV doubles the decoded value: 0.21586050011389... * 2 = 0.43172100022779...
        // Encoding (nonlinear branch, since it exceeds 0.0031308):
        // 1.055 * 0.43172100022779^(1/2.4) - 0.055 = 0.68845345...
        // floor(255 * 0.68845345... + 0.5) = floor(176.055...) = 176.
        assert_eq!(
            evaluate_pixel([128, 128, 128], &[RefOp::Exposure(1.0)]),
            [176, 176, 176]
        );

        // code 0 decodes to exactly 0.0 (linear branch: 0.0 divided by the slope is 0.0).
        assert_eq!(srgb::decode(0), 0.0);
        // code 255 decodes to exactly 1.0: 255/255 = 1.0, nonlinear branch,
        // ((1.0 + 0.055) / 1.055) ^ 2.4 = 1.0 ^ 2.4 = 1.0.
        assert_eq!(srgb::decode(255), 1.0);

        // code 64: 64/255 = 0.250980392156862..., nonlinear branch:
        // ((0.250980392156862 + 0.055) / 1.055) ^ 2.4 = 0.05126945...
        let decoded_64 = srgb::decode(64);
        assert!(
            (decoded_64 - 0.051_269_458_400).abs() < 1e-7,
            "decoded_64 = {decoded_64}"
        );

        // A strong negative exposure on a low code floors at output code 0: code 8 decodes to
        // 8/255 = 0.031372549..., linear branch (<=0.04045): dividing by the slope gives
        // 0.00242814... At -5 EV: 0.00242814... * 2^-5 = 0.0000758795...; encoding stays in the
        // linear branch: multiplying by the slope gives 0.00098036...;
        // floor(255*0.00098036...+0.5) = floor(0.7499...) = 0.
        assert_eq!(
            evaluate_pixel([8, 8, 8], &[RefOp::Exposure(-5.0)]),
            [0, 0, 0]
        );
    }

    #[test]
    fn quantization_thresholds_match_encode_on_both_sides() {
        for k in 1u8..=255 {
            let threshold = code_threshold(k);
            assert_eq!(
                srgb::code(threshold - 1e-12),
                k - 1,
                "k={k}: just below threshold {threshold} should encode to {}",
                k - 1
            );
            assert_eq!(
                srgb::code(threshold + 1e-12),
                k,
                "k={k}: just above threshold {threshold} should encode to {k}"
            );
        }
    }
}
