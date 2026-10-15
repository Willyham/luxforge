//! The Basic module's neutral picker, and the 3 x 3 linear-sRGB program a RAW photo's white-balance
//! draft runs on the GPU.
//!
//! Temperature and Tint are relative corrections of an already-rendered image, applied by the
//! scene unit (`scene.rs`, `docs/design/basic-scene.md`): each channel shifted by its frozen stops
//! on the scene scale. The picker finds the Temperature and Tint whose stops make a sampled patch's
//! channels equal on that scale, which is a neutral patch once rendered back.
use crate::{
    colour::{cielab::RGB_TO_XYZ, srgb::decode_u8},
    render::gpu::{GpuProgram, GpuProgramKind},
};

/// The one linear-sRGB 3 x 3 multiply (`white_balance.wgsl`), nine words: the leading step of a
/// RAW photo's white-balance draft (`crate::render::gpu`), which no layer's unit describes.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_basic_white_balance",
    source: include_str!("white_balance.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 9,
    enabled: true,
};

/// Mean linear relative luminance below which a picker patch is rejected as near-black.
const NEAR_BLACK_LUMINANCE: f64 = 0.02;

/// Neutral-picker solver: convergence tolerance, as the largest channel difference in stops.
const SOLVER_TOLERANCE: f64 = 1e-9;

/// Neutral-picker solver: iteration bound.
const SOLVER_MAX_ITERATIONS: u32 = 64;

/// Neutral-picker solver: the central-difference step the Jacobian uses, in slider units.
const SOLVER_JACOBIAN_STEP: f64 = 1e-3;

/// The inclusive range Temperature and Tint accept, which is also the range their descriptors
/// declare. A solved correction outside it is reported, never clamped.
pub(super) const PARAMETER_RANGE: f64 = 100.0;

/// Why a sampled patch produced no settings. The design requires an explicit reason rather than a
/// guess or a silent clamp, so every one of these reaches the caller as a structured refusal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Reason {
    /// At least one sampled pixel has a channel at code 0 or 255.
    Clipped,
    /// The patch's mean linear relative luminance is below [`NEAR_BLACK_LUMINANCE`].
    NearBlack,
    /// A sampled or computed value was not finite.
    NonFinite,
    /// The solver converged, but the correction this patch needs is outside the representable
    /// range. Carries which axis and the rounded value that would have been out of range.
    OutOfRange { temperature: bool, value: f64 },
    /// The solver did not converge within [`SOLVER_MAX_ITERATIONS`]: the patch is far enough
    /// from any representable correction that the iteration left the slider's range.
    DidNotConverge,
}

impl Reason {
    /// The refusal a client reads. Every message begins with the reason's own name, so a caller can
    /// branch on the prefix without parsing prose.
    pub(super) fn message(self) -> String {
        match self {
            Self::Clipped => "clipped: a sampled pixel is at code 0 or 255, so this patch carries \
                 no usable colour"
                .to_owned(),
            Self::NearBlack => format!(
                "near-black: the sampled patch's mean luminance is below {NEAR_BLACK_LUMINANCE}, \
                 where colour information is unreliable"
            ),
            Self::NonFinite => {
                "non-finite: a sampled or computed value was not a finite number".to_owned()
            }
            Self::OutOfRange { temperature, value } => {
                let axis = if temperature { "temperature" } else { "tint" };
                format!(
                    "out-of-range: this patch needs {axis} {value}, outside the \
                     -{PARAMETER_RANGE}..={PARAMETER_RANGE} these relative axes represent; \
                     reported rather than clamped"
                )
            }
            // Non-convergence and a wildly out-of-range solution are the same situation for the
            // person picking: this patch's correction is not something these relative axes can
            // represent. They share a prefix so a client branches on one reason, not two.
            Self::DidNotConverge => format!(
                "out-of-range: the neutral solver did not converge within \
                 {SOLVER_MAX_ITERATIONS} iterations, so this patch needs a correction outside the \
                 -{PARAMETER_RANGE}..={PARAMETER_RANGE} these relative axes represent"
            ),
        }
    }
}

fn relative_luminance(rgb: [f64; 3]) -> f64 {
    RGB_TO_XYZ[1][0] * rgb[0] + RGB_TO_XYZ[1][1] * rgb[1] + RGB_TO_XYZ[1][2] * rgb[2]
}

/// Decode and average up to 25 sampled 8-bit pixels in linear sRGB, or reject the patch.
///
/// The three checks run in the order the design freezes: clipping on the raw codes, before any
/// decoding or averaging, so one blown highlight or crushed shadow pixel rejects the whole sample;
/// then non-finiteness; then near-black on the mean's relative luminance.
pub(super) fn average_patch(pixels: &[[u8; 3]]) -> Result<[f64; 3], Reason> {
    if pixels.is_empty() {
        return Err(Reason::NonFinite);
    }
    if pixels
        .iter()
        .any(|pixel| pixel.iter().any(|channel| *channel == 0 || *channel == 255))
    {
        return Err(Reason::Clipped);
    }
    let count = pixels.len() as f64;
    let mut sum = [0.0; 3];
    for pixel in pixels {
        for (slot, code) in sum.iter_mut().zip(pixel) {
            *slot += decode_u8(*code);
        }
    }
    let mean = [sum[0] / count, sum[1] / count, sum[2] / count];
    if mean.iter().any(|value| !value.is_finite()) {
        return Err(Reason::NonFinite);
    }
    let luminance = relative_luminance(mean);
    if !luminance.is_finite() || luminance < NEAR_BLACK_LUMINANCE {
        return Err(Reason::NearBlack);
    }
    Ok(mean)
}

/// The Temperature and Tint, unrounded, whose stops make the patch's channels equal on the scene
/// scale, or in plain stops over a scene-referred patch: Newton's method from neutral with a
/// central-difference Jacobian.
fn neutral_settings(mean_linear_rgb: [f64; 3], scene_referred: bool) -> Result<(f64, f64), Reason> {
    if mean_linear_rgb.iter().any(|value| !value.is_finite()) {
        return Err(Reason::NonFinite);
    }
    if mean_linear_rgb.iter().any(|value| *value <= 0.0) {
        return Err(Reason::NonFinite);
    }
    let logs = if scene_referred {
        mean_linear_rgb.map(f64::log2)
    } else {
        super::scene::scene_logs(mean_linear_rgb)
    };
    let residual = |temperature: f64, tint: f64| {
        let stops = super::scene::white_balance_stops(temperature, tint);
        let z = [0, 1, 2].map(|c| logs[c] + stops[c]);
        (z[0] - z[1], z[2] - z[1])
    };
    let (mut temperature, mut tint) = (0.0_f64, 0.0_f64);
    let h = SOLVER_JACOBIAN_STEP;
    for _ in 0..SOLVER_MAX_ITERATIONS {
        let (a, b) = residual(temperature, tint);
        if !a.is_finite() || !b.is_finite() {
            return Err(Reason::DidNotConverge);
        }
        if a.abs().max(b.abs()) < SOLVER_TOLERANCE {
            return Ok((temperature, tint));
        }
        let (at1, bt1) = residual(temperature + h, tint);
        let (at0, bt0) = residual(temperature - h, tint);
        let (an1, bn1) = residual(temperature, tint + h);
        let (an0, bn0) = residual(temperature, tint - h);
        let j = [
            [(at1 - at0) / (2.0 * h), (an1 - an0) / (2.0 * h)],
            [(bt1 - bt0) / (2.0 * h), (bn1 - bn0) / (2.0 * h)],
        ];
        let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
        if !det.is_finite() || det.abs() < 1e-15 {
            return Err(Reason::DidNotConverge);
        }
        temperature -= (j[1][1] * a - j[0][1] * b) / det;
        tint -= (-j[1][0] * a + j[0][0] * b) / det;
        if !(temperature.is_finite() && tint.is_finite())
            || temperature.abs() > 2.0 * PARAMETER_RANGE
            || tint.abs() > 2.0 * PARAMETER_RANGE
        {
            return Err(Reason::DidNotConverge);
        }
    }
    Err(Reason::DidNotConverge)
}

/// The neutral picker's answer: the Temperature and Tint that neutralize the patch, each rounded
/// to the nearest whole step. A value outside the slider's range is reported as
/// [`Reason::OutOfRange`] rather than clamped.
pub(super) fn neutral_values(
    mean_linear_rgb: [f64; 3],
    scene_referred: bool,
) -> Result<(i64, i64), Reason> {
    let (temperature, tint) = neutral_settings(mean_linear_rgb, scene_referred)?;
    let (temperature, tint) = (temperature.round(), tint.round());
    if temperature.abs() > PARAMETER_RANGE {
        return Err(Reason::OutOfRange {
            temperature: true,
            value: temperature,
        });
    }
    if tint.abs() > PARAMETER_RANGE {
        return Err(Reason::OutOfRange {
            temperature: false,
            value: tint,
        });
    }
    Ok((temperature as i64, tint as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The neutral corrections of the independent reference, rounded, for casts made by every
    /// combination of Temperature and Tint on a grey and a light tan.
    #[test]
    fn a_cast_solves_to_the_references_correction() {
        for base in [[0.18, 0.18, 0.18], [0.45, 0.38, 0.30]] {
            for temperature in [-60.0, -20.0, 0.0, 15.0, 45.0] {
                for tint in [-40.0, 0.0, 25.0] {
                    let cast = luxforge_reference::scene::apply(temperature, tint, 0.0, base);
                    let reference = luxforge_reference::scene::neutral_settings(cast);
                    match (neutral_values(cast, false), reference) {
                        (Ok((t, n)), Some((rt, rn))) => {
                            assert_eq!((t, n), (rt.round() as i64, rn.round() as i64));
                        }
                        // The reference solves within twice the range and refuses beyond the
                        // range; the picker reports the value, or that it did not converge.
                        (Err(Reason::OutOfRange { .. } | Reason::DidNotConverge), None) => {}
                        (got, want) => panic!("{base:?} ({temperature}, {tint}): {got:?} {want:?}"),
                    }
                }
            }
        }
    }

    /// The rejection order and the messages a client branches on.
    #[test]
    fn rejections_name_themselves_in_the_frozen_order() {
        assert_eq!(average_patch(&[[0, 0, 0]]).unwrap_err(), Reason::Clipped);
        assert_eq!(
            average_patch(&[[255, 200, 200]]).unwrap_err(),
            Reason::Clipped
        );
        assert_eq!(average_patch(&[[8, 8, 8]]).unwrap_err(), Reason::NearBlack);
        assert_eq!(average_patch(&[]).unwrap_err(), Reason::NonFinite);
        assert_eq!(
            neutral_values([f64::NAN, 0.5, 0.5], false).unwrap_err(),
            Reason::NonFinite
        );
        for (reason, prefix) in [
            (Reason::Clipped, "clipped:"),
            (Reason::NearBlack, "near-black:"),
            (Reason::NonFinite, "non-finite:"),
            (Reason::DidNotConverge, "out-of-range:"),
            (
                Reason::OutOfRange {
                    temperature: true,
                    value: 120.0,
                },
                "out-of-range:",
            ),
        ] {
            assert!(
                reason.message().starts_with(prefix),
                "{reason:?}: {}",
                reason.message()
            );
        }
    }

    /// A patch no slider setting can neutralize is refused, never accepted at a clamped value.
    #[test]
    fn a_strongly_saturated_patch_is_refused_rather_than_clamped() {
        let refused = neutral_values([0.8, 0.05, 0.05], false).expect_err("no neutral correction");
        assert!(
            matches!(refused, Reason::OutOfRange { .. } | Reason::DidNotConverge),
            "{refused:?}"
        );
    }
}
