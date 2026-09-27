//! One sRGB reference: the transfer function's named variants and the output quantizer, written
//! directly from the standard's constants (threshold `0.04045` / `0.0031308`, `12.92`, `1.055`,
//! `0.055`, exponent `2.4`). Every study and every core test that needs an sRGB oracle computes
//! through this module; no other private transcription of the transfer function belongs anywhere
//! in the workspace (`cargo xtask check-repository`'s `srgb-transfer-function` rule holds this,
//! with the one allowed exception being the core's own production copy,
//! `crates/luxforge-core/src/colour.rs`).
//!
//! Decoding has one shape: the sRGB EOTF is defined, unclamped, over the whole real line, so
//! [`decode_encoded`] serves every caller and [`decode`] is its convenience for one 8-bit
//! channel code.
//!
//! Encoding has three shapes, all built from the same unclamped formula
//! ([`encode_extended`]), differing only in what happens to a linear value outside `[0, 1]`
//! before it is applied:
//! * [`encode_clamped`] clamps both ends to `[0, 1]`: the host's final output boundary.
//! * [`encode_nonnegative`] clamps only below zero, so a linear value above 1 still encodes past
//!   white.
//! * [`encode_extended`] does not clamp at all: the analytic continuation a working colour domain
//!   (Presence, Tone, range, vignette) operates in.
//!
//! [`quantize`] applies the host's output rounding rule, `floor(255 * encoded + 0.5)`, to an
//! already-encoded value clamped to `[0, 1]`; [`code`] is the full pipeline from linear light to
//! an 8-bit output code.

/// The sRGB EOTF's linear-branch threshold, in encoded units.
const DECODE_THRESHOLD: f64 = 0.040_45;
/// The sRGB OETF's linear-branch threshold, in linear units.
const ENCODE_THRESHOLD: f64 = 0.003_130_8;

/// One 8-bit sRGB channel code, decoded to linear light.
pub fn decode(code: u8) -> f64 {
    decode_encoded(f64::from(code) / 255.0)
}

/// The sRGB EOTF (encoded to linear), the analytic continuation defined on the whole real line:
/// not clamped to `[0, 1]`.
///
/// Hand check: `decode_encoded(128.0 / 255.0)` decodes `0.501960...`. That value exceeds the
/// decode threshold (`0.04045`), so the nonlinear branch applies:
/// `((0.501960... + 0.055) / 1.055) ^ 2.4 = 0.2158605...`.
pub fn decode_encoded(encoded: f64) -> f64 {
    if encoded <= DECODE_THRESHOLD {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB OETF (linear to encoded), clamped to `[0, 1]` on both sides before encoding: the
/// host's final output boundary.
pub fn encode_clamped(linear: f64) -> f64 {
    encode_extended(linear.clamp(0.0, 1.0))
}

/// The sRGB OETF, clamped only below zero: a linear value above 1 still encodes past white.
pub fn encode_nonnegative(linear: f64) -> f64 {
    encode_extended(linear.max(0.0))
}

/// The sRGB OETF (linear to encoded), the analytic continuation defined on the whole real line:
/// not clamped.
pub fn encode_extended(linear: f64) -> f64 {
    if linear <= ENCODE_THRESHOLD {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// An encoded value, clamped to `[0, 1]`, quantized to its 8-bit output code:
/// `floor(255 * encoded + 0.5)`.
pub fn quantize(encoded: f64) -> u8 {
    (255.0 * encoded.clamp(0.0, 1.0) + 0.5).floor() as u8
}

/// One linear-light value to its 8-bit sRGB output code: [`encode_clamped`] then [`quantize`].
///
/// Any production quantizer must agree with this function on both sides of every threshold code
/// boundary (see `luxforge_reference::code_threshold` and the
/// `quantization_thresholds_match_encode_on_both_sides` test).
pub fn code(linear: f64) -> u8 {
    quantize(encode_clamped(linear))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_of_every_code_is_identity() {
        for value in 0u8..=255 {
            assert_eq!(
                code(decode(value)),
                value,
                "code {value} failed to round trip"
            );
        }
    }

    #[test]
    fn decode_is_zero_and_one_at_the_ends() {
        assert_eq!(decode(0), 0.0);
        assert_eq!(decode(255), 1.0);
    }

    #[test]
    fn the_three_encode_variants_agree_inside_the_unit_range() {
        for step in 0..=1000 {
            let linear = f64::from(step) / 1000.0;
            assert_eq!(encode_clamped(linear), encode_extended(linear));
            assert_eq!(encode_nonnegative(linear), encode_extended(linear));
        }
    }

    #[test]
    fn the_three_encode_variants_differ_outside_the_unit_range() {
        // Above 1: clamped stays at white's encoded value, nonnegative and extended do not.
        assert_eq!(encode_clamped(2.0), encode_extended(1.0));
        assert!(encode_nonnegative(2.0) > encode_clamped(2.0));
        assert_eq!(encode_nonnegative(2.0), encode_extended(2.0));

        // Below 0: clamped and nonnegative both floor at black's encoded value (0.0), extended
        // does not.
        assert_eq!(encode_clamped(-1.0), 0.0);
        assert_eq!(encode_nonnegative(-1.0), 0.0);
        assert!(encode_extended(-1.0) < 0.0);
    }

    #[test]
    fn quantize_clamps_before_rounding() {
        assert_eq!(quantize(-1.0), 0);
        assert_eq!(quantize(2.0), 255);
    }
}
