//! Conservative S0 recognition of standard RGB matrix/TRC sRGB profiles.
use crate::{Error, ErrorKind};
use moxcms::{ColorProfile, DataColorSpace, ParsingOptions, ProfileClass};

pub fn check(bytes: &[u8], components: u8) -> Result<(), Error> {
    let unsupported = || {
        Error::new(
            ErrorKind::UnsupportedProfile,
            "expected standard RGB matrix/TRC sRGB",
        )
    };
    if components != 3 {
        return Err(unsupported());
    }
    let profile = ColorProfile::new_from_slice_with_options(
        bytes,
        ParsingOptions {
            max_profile_size: 1024 * 1024,
            max_allowed_clut_size: 4096,
            max_allowed_trc_size: 4096,
        },
    )
    .map_err(|_| unsupported())?;
    if !matches!(
        profile.profile_class,
        ProfileClass::InputDevice | ProfileClass::DisplayDevice | ProfileClass::ColorSpace
    ) || profile.color_space != DataColorSpace::Rgb
        || profile.pcs != DataColorSpace::Xyz
        || profile.lut_a_to_b_perceptual.is_some()
        || profile.lut_a_to_b_colorimetric.is_some()
        || profile.lut_a_to_b_saturation.is_some()
        || profile.lut_b_to_a_perceptual.is_some()
        || profile.lut_b_to_a_colorimetric.is_some()
        || profile.lut_b_to_a_saturation.is_some()
    {
        return Err(unsupported());
    }
    // Reject conflicting CICP, rather than let it silently override the ICC matrix/TRCs.
    if let Some(cicp) = profile.cicp {
        let reference = ColorProfile::new_srgb().cicp.unwrap();
        if cicp.color_primaries != reference.color_primaries
            || cicp.transfer_characteristics != reference.transfer_characteristics
        {
            return Err(unsupported());
        }
    }
    let reference = ColorProfile::new_srgb();
    for (actual, expected) in [
        (profile.red_colorant, reference.red_colorant),
        (profile.green_colorant, reference.green_colorant),
        (profile.blue_colorant, reference.blue_colorant),
        (profile.white_point, reference.white_point),
    ] {
        for (a, b) in [
            (actual.x, expected.x),
            (actual.y, expected.y),
            (actual.z, expected.z),
        ] {
            if !a.is_finite() || (a - b).abs() > 0.0005 {
                return Err(unsupported());
            }
        }
    }
    for curve in [profile.red_trc, profile.green_trc, profile.blue_trc] {
        let evaluator = curve
            .ok_or_else(unsupported)?
            .make_linear_evaluator()
            .map_err(|_| unsupported())?;
        // All possible source codes are checked: this viewer accepts 8-bit JPEG only.
        for code in 0..=255 {
            let encoded = code as f32 / 255.;
            let expected = crate::colour::srgb::decode_f32(encoded);
            let actual = evaluator.evaluate_value(encoded);
            if !actual.is_finite() || (actual - expected).abs() > 0.0005 {
                return Err(unsupported());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_srgb_profile_and_metadata_variants() {
        let mut profile = ColorProfile::new_srgb();
        profile.cicp = None;
        let mut encoded = profile.encode().unwrap();
        check(&encoded, 3).unwrap();
        // ICC manufacturer and creator fields do not define color semantics.
        encoded[48..52].copy_from_slice(b"TEST");
        encoded[80..84].copy_from_slice(b"LWEL");
        check(&encoded, 3).unwrap();
        assert!(check(&encoded, 1).is_err());
        assert!(check(&encoded[..100], 3).is_err());
    }
    #[test]
    fn other_gamuts_gamma_and_malformed_profiles_are_rejected() {
        assert!(check(&ColorProfile::new_adobe_rgb().encode().unwrap(), 3).is_err());
        let mut profile = ColorProfile::new_srgb();
        profile.red_trc = Some(moxcms::curve_from_gamma(2.2));
        assert!(check(&profile.encode().unwrap(), 3).is_err());
        assert!(check(&[0], 3).is_err());
    }
}
