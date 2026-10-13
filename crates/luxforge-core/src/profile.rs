//! Conservative S0 recognition of standard RGB matrix/TRC sRGB profiles, which the editor accepts
//! ([`check`]), and the conversion of any RGB matrix/TRC profile to linear sRGB/D65, which a
//! merge's JPEG frames take ([`RgbProfile`]).
use crate::{Error, colour::mat3};
use moxcms::{ColorProfile, DataColorSpace, ParsingOptions, ProfileClass};

/// An RGB matrix/TRC display, input or colour-space profile with an XYZ connection space and no
/// lookup-table transforms, which would take precedence over its matrix: what both readers accept.
fn matrix_trc(
    bytes: &[u8],
    components: u8,
    unsupported: impl Fn() -> Error,
) -> Result<ColorProfile, Error> {
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
    Ok(profile)
}

/// Each channel's linear value at every 8-bit code through `profile`'s tone curves, refused when a
/// curve is missing or answers a non-finite value.
fn code_tables(
    profile: &ColorProfile,
    unsupported: impl Fn() -> Error,
) -> Result<[[f32; 256]; 3], Error> {
    let mut tables = [[0.0_f32; 256]; 3];
    for (table, curve) in
        tables
            .iter_mut()
            .zip([&profile.red_trc, &profile.green_trc, &profile.blue_trc])
    {
        let evaluator = curve
            .as_ref()
            .ok_or_else(&unsupported)?
            .make_linear_evaluator()
            .map_err(|_| unsupported())?;
        for (code, value) in table.iter_mut().enumerate() {
            *value = evaluator.evaluate_value(code as f32 / 255.0);
            if !value.is_finite() {
                return Err(unsupported());
            }
        }
    }
    Ok(tables)
}

/// How a merge reads an 8-bit RGB JPEG's codes as float32 linear sRGB/D65
/// (`docs/design/panorama-merge.md`, "Working pixels"): each channel's code through its tone curve,
/// tabulated for the 256 codes, then the profile's linear RGB through its colorants into the D50
/// connection space and out through sRGB's (relative colorimetric, each side's white mapped to the
/// connection space's), keeping negative and above-one components. A file with no profile is
/// sRGB. Display P3, Adobe RGB and every other matrix/TRC profile are read as they declare.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RgbProfile {
    /// The linear value of each code, by channel.
    pub(crate) linear: [[f32; 256]; 3],
    /// The profile's linear RGB to linear sRGB, rows by output channel; the identity for sRGB
    /// assumed.
    pub(crate) to_srgb: [[f64; 3]; 3],
    /// Whether the file had no profile and was read as sRGB.
    pub(crate) assumed: bool,
}

impl RgbProfile {
    /// sRGB, assumed for a file with no profile: the sRGB decode and the identity.
    pub(crate) fn assumed_srgb() -> Self {
        let table = *crate::colour::srgb::decode_table();
        Self {
            linear: [table; 3],
            to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            assumed: true,
        }
    }

    /// The conversion of a JPEG with `components` components and the embedded ICC profile `bytes`,
    /// or [`Self::assumed_srgb`] with none. Refused as `unsupported-profile` unless it is an RGB
    /// matrix/TRC profile of a three-component frame with finite tone curves and an invertible
    /// matrix, or when it carries a CICP tag that names other primaries or another transfer than
    /// its matrix and curves, rather than let either silently win.
    pub(crate) fn of(bytes: Option<&[u8]>, components: u8) -> Result<Self, Error> {
        let Some(bytes) = bytes else {
            return Ok(Self::assumed_srgb());
        };
        let unsupported =
            || Error::unsupported_profile("expected an RGB matrix/TRC colour profile");
        let profile = matrix_trc(bytes, components, unsupported)?;
        let linear = code_tables(&profile, unsupported)?;
        let source = profile.rgb_to_xyz_matrix().v;
        if let Some(cicp) = profile.cicp {
            let declared = ColorProfile::new_from_cicp(cicp);
            let conflicting = || {
                Error::unsupported_profile(
                    "the colour profile's CICP tag disagrees with its matrix and tone curves",
                )
            };
            let curves = code_tables(&declared, conflicting)?;
            let matrix = declared.rgb_to_xyz_matrix().v;
            if matrix
                .iter()
                .flatten()
                .zip(source.iter().flatten())
                .any(|(a, b)| !a.is_finite() || (a - b).abs() > 0.0005)
                || curves
                    .iter()
                    .flatten()
                    .zip(linear.iter().flatten())
                    .any(|(a, b)| (a - b).abs() > 0.0005)
            {
                return Err(conflicting());
            }
        }
        let (srgb_from_xyz, determinant) =
            mat3::inverse_and_determinant(&ColorProfile::new_srgb().rgb_to_xyz_matrix().v);
        let to_srgb = mat3::mul(&srgb_from_xyz, &source);
        if !determinant.is_finite()
            || !source.iter().flatten().all(|value| value.is_finite())
            || mat3::determinant(&source).abs() < 1e-8
            || !to_srgb.iter().flatten().all(|value| value.is_finite())
        {
            return Err(unsupported());
        }
        Ok(Self {
            linear,
            to_srgb,
            assumed: false,
        })
    }

    /// One pixel's codes as linear sRGB/D65.
    #[inline]
    pub(crate) fn linear_srgb(&self, codes: [u8; 3]) -> [f32; 3] {
        let linear = [
            f64::from(self.linear[0][codes[0] as usize]),
            f64::from(self.linear[1][codes[1] as usize]),
            f64::from(self.linear[2][codes[2] as usize]),
        ];
        mat3::matvec_f64(&self.to_srgb, linear).map(|value| value as f32)
    }

    /// The largest channel's linear value at its own tone curve, the fraction of its encoding's
    /// white the pixel reached: what a JPEG frame's clipping evidence reads below white.
    #[inline]
    pub(crate) fn encoded_fraction(&self, codes: [u8; 3]) -> f64 {
        (0..3)
            .map(|c| f64::from(self.linear[c][codes[c] as usize]))
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

pub(crate) fn check(bytes: &[u8], components: u8) -> Result<(), Error> {
    let unsupported = || Error::unsupported_profile("expected standard RGB matrix/TRC sRGB");
    let profile = matrix_trc(bytes, components, unsupported)?;
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
        encoded[80..84].copy_from_slice(b"LFEL");
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
