//! Temperature/tint white balance for prepared camera-channel data.
//!
//! The returned gains are sensor multipliers: they are applied before demosaicing and are
//! normalized so green is exactly one. `cam_xyz` is the four-by-three LibRaw matrix whose first
//! three rows map XYZ to camera responses. The fourth row is intentionally ignored because this
//! helper only supports three developed camera channels.
//!
//! Temperature uses a documented blackbody/daylight locus approximation. The blackbody equations
//! cover the lower part of Lightwell's range, the daylight equations cover the upper part, and a
//! smooth transition keeps the two approximations continuous around their seam. Tint is an
//! explicit Lightwell unit: one unit is 1e-4 signed CIE 1960 `uv` distance (`Duv`). The
//! user-facing positive direction is magenta: it moves the assumed illuminant toward +Duv (the
//! green side), so the compensating sensor gains make a calibrated neutral more magenta.
//!
//! Provenance: RawTherapee's White Balance technical notes document the blackbody/daylight split
//! and its temperature ranges; the CIE 1960 `uv` coordinates define the signed locus offset.
//! LibRaw's API data structure and `cam_xyz_coeff` implementation establish the XYZ-to-camera row
//! direction used here. This helper does not infer or serialize an AsShot temperature.

use crate::{Error, ErrorKind};

pub const MIN_TEMPERATURE_K: f64 = 2_000.0;
pub const MAX_TEMPERATURE_K: f64 = 12_000.0;
pub const MIN_TINT: f64 = -100.0;
pub const MAX_TINT: f64 = 100.0;
pub const TINT_DUV_UNIT: f64 = 1.0e-4;

const PLANCK_DAYLIGHT_BLEND_START_K: f64 = 3_800.0;
const PLANCK_DAYLIGHT_BLEND_END_K: f64 = 4_500.0;
const MATRIX_DETERMINANT_RELATIVE_MIN: f64 = 1.0e-9;

fn validation(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, message)
}

fn planck_xy(temperature_kelvin: f64) -> [f64; 2] {
    // The two x(T) polynomials are the Kang, Moon, Hong, Lee, Cho, and Kim (2002)
    // Planckian-locus approximation ("Design of Advanced Color Temperature Control System
    // for HDTV Applications", Journal of the Korean Physical Society 41(6), 865-871).
    // over 1667..4000 K and 4000..25000 K. The matching y(T) polynomials are split at 2222 K
    // and 4000 K. Lightwell validates a narrower 2000..12000 K interval before calling this.
    let x = if temperature_kelvin <= 4_000.0 {
        -0.266_123_9e9 / temperature_kelvin.powi(3) - 0.234_358_0e6 / temperature_kelvin.powi(2)
            + 0.877_695_6e3 / temperature_kelvin
            + 0.179_910
    } else {
        -3.025_846_9e9 / temperature_kelvin.powi(3)
            + 2.107_037_9e6 / temperature_kelvin.powi(2)
            + 0.222_634_7e3 / temperature_kelvin
            + 0.240_390
    };
    let y = if temperature_kelvin <= 2_222.0 {
        -1.106_381_4 * x.powi(3) - 1.348_110_2 * x.powi(2) + 2.185_558_32 * x - 0.202_196_83
    } else if temperature_kelvin <= 4_000.0 {
        -0.954_947_6 * x.powi(3) - 1.374_185_93 * x.powi(2) + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x.powi(3) - 5.873_386_70 * x.powi(2) + 3.751_129_97 * x - 0.370_014_83
    };
    [x, y]
}

fn daylight_xy(temperature_kelvin: f64) -> [f64; 2] {
    // CIE daylight-locus x_D equations, split at 7000 K, followed by the standard
    // y_D = -3x_D^2 + 2.87x_D - 0.275 relation. The source design uses this locus
    // above the transition because 6504 K then lands on D65 rather than on the nearby
    // blackbody locus. The 3800..4500 K blend in `base_whitepoint_xy` is a Lightwell
    // continuity choice; it is not asserted to be RawTherapee's implementation.
    let x = if temperature_kelvin <= 7_000.0 {
        0.244_063 + 0.099_11e3 / temperature_kelvin + 2.967_8e6 / temperature_kelvin.powi(2)
            - 4.607_0e9 / temperature_kelvin.powi(3)
    } else {
        0.237_040 + 0.247_48e3 / temperature_kelvin + 1.901_8e6 / temperature_kelvin.powi(2)
            - 2.006_4e9 / temperature_kelvin.powi(3)
    };
    [x, -3.0 * x * x + 2.870 * x - 0.275]
}

fn smoothstep(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn base_whitepoint_xy(temperature_kelvin: f64) -> [f64; 2] {
    let planck = planck_xy(temperature_kelvin);
    let daylight = daylight_xy(temperature_kelvin);
    let blend = smoothstep(
        (temperature_kelvin - PLANCK_DAYLIGHT_BLEND_START_K)
            / (PLANCK_DAYLIGHT_BLEND_END_K - PLANCK_DAYLIGHT_BLEND_START_K),
    );
    [
        planck[0] + (daylight[0] - planck[0]) * blend,
        planck[1] + (daylight[1] - planck[1]) * blend,
    ]
}

fn xy_to_uv(xy: [f64; 2]) -> Result<[f64; 2], Error> {
    let [x, y] = xy;
    let denominator = -2.0 * x + 12.0 * y + 3.0;
    if !denominator.is_finite() || denominator.abs() < f64::EPSILON {
        return Err(validation("white-balance xy to uv conversion is singular"));
    }
    let uv = [4.0 * x / denominator, 6.0 * y / denominator];
    if uv.iter().all(|value| value.is_finite()) {
        Ok(uv)
    } else {
        Err(validation("white-balance uv conversion is non-finite"))
    }
}

fn uv_to_xy(uv: [f64; 2]) -> Result<[f64; 2], Error> {
    let [u, v] = uv;
    // Inverting u = 4x / (-2x + 12y + 3), v = 6y / (-2x + 12y + 3).
    let denominator = 2.0 * u - 8.0 * v + 4.0;
    if !denominator.is_finite() || denominator.abs() < f64::EPSILON {
        return Err(validation("white-balance uv to xy conversion is singular"));
    }
    let xy = [3.0 * u / denominator, 2.0 * v / denominator];
    let [x, y] = xy;
    if !xy.iter().all(|value| value.is_finite()) || x <= 0.0 || y <= 0.0 || x + y >= 1.0 {
        return Err(validation(
            "white-balance tint leaves the visible whitepoint domain",
        ));
    }
    Ok(xy)
}

fn locus_uv(temperature_kelvin: f64) -> Result<[f64; 2], Error> {
    xy_to_uv(base_whitepoint_xy(temperature_kelvin))
}

fn tinted_whitepoint_xy(temperature_kelvin: f64, tint: f64) -> Result<[f64; 2], Error> {
    let base_uv = locus_uv(temperature_kelvin)?;
    let lower = (temperature_kelvin - 1.0).max(MIN_TEMPERATURE_K);
    let upper = (temperature_kelvin + 1.0).min(MAX_TEMPERATURE_K);
    let lower_uv = locus_uv(lower)?;
    let upper_uv = locus_uv(upper)?;
    let tangent = [upper_uv[0] - lower_uv[0], upper_uv[1] - lower_uv[1]];
    let tangent_length = tangent[0].hypot(tangent[1]);
    if !tangent_length.is_finite() || tangent_length <= f64::EPSILON {
        return Err(validation("white-balance locus tangent is degenerate"));
    }
    // Along this locus temperature increases toward lower u and lower v. This normal points
    // toward the conventional positive-Duv/green side. Positive Lightwell tint moves the
    // assumed illuminant in that direction; its compensating gains then make the output more
    // magenta, matching the familiar control direction.
    let green_normal = [tangent[1] / tangent_length, -tangent[0] / tangent_length];
    let duv = tint * TINT_DUV_UNIT;
    uv_to_xy([
        base_uv[0] + green_normal[0] * duv,
        base_uv[1] + green_normal[1] * duv,
    ])
}

fn camera_matrix(cam_xyz: [[f32; 3]; 4]) -> Result<[[f64; 3]; 3], Error> {
    let mut matrix = [[0.0; 3]; 3];
    for (row, output) in matrix.iter_mut().enumerate() {
        for (column, value) in output.iter_mut().enumerate() {
            *value = f64::from(cam_xyz[row][column]);
        }
        if !output.iter().all(|value| value.is_finite()) {
            return Err(validation("camera XYZ matrix contains a non-finite value"));
        }
    }

    let norms = matrix.map(|row| row.iter().map(|value| value * value).sum::<f64>().sqrt());
    if !norms
        .iter()
        .all(|norm| norm.is_finite() && *norm > f64::EPSILON)
    {
        return Err(validation("camera XYZ matrix contains a zero row"));
    }
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    let scale = norms[0] * norms[1] * norms[2];
    if !determinant.is_finite()
        || !scale.is_finite()
        || determinant.abs() <= scale * MATRIX_DETERMINANT_RELATIVE_MIN
    {
        return Err(validation("camera XYZ matrix is degenerate"));
    }
    Ok(matrix)
}

/// Convert a temperature and Lightwell tint into green-normalized sensor gains.
///
/// `cam_xyz` is LibRaw's `[4][3]` XYZ-to-camera-response matrix; only its first three rows are
/// used. The fourth row is reserved for a fourth camera channel and is not part of this helper's
/// three-channel contract. The input controls are strict: endpoints are accepted, but values
/// outside 2000..12000 K or -100..100 tint are errors rather than silently coerced.
pub fn gains_from_temperature_tint(
    temperature_kelvin: f64,
    tint: f64,
    cam_xyz: [[f32; 3]; 4],
) -> Result<[f32; 3], Error> {
    if !temperature_kelvin.is_finite()
        || !(MIN_TEMPERATURE_K..=MAX_TEMPERATURE_K).contains(&temperature_kelvin)
    {
        return Err(validation(
            "RAW temperature must be finite and 2000..=12000 K",
        ));
    }
    if !tint.is_finite() || !(MIN_TINT..=MAX_TINT).contains(&tint) {
        return Err(validation(
            "RAW tint must be finite and -100..=100 Lightwell units",
        ));
    }
    let matrix = camera_matrix(cam_xyz)?;
    let [x, y] = tinted_whitepoint_xy(temperature_kelvin, tint)?;
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    if !xyz.iter().all(|value| value.is_finite() && *value > 0.0) {
        return Err(validation("RAW whitepoint XYZ is invalid"));
    }
    let response = matrix.map(|row| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]);
    if !response
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
    {
        return Err(validation(
            "camera matrix gives a non-positive white response",
        ));
    }
    let gains = [response[1] / response[0], 1.0, response[1] / response[2]];
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && *value <= super::MAX_RAW_GAIN)
    {
        return Err(validation(
            "temperature/tint gains exceed the finite 0..32 sensor range",
        ));
    }
    let gains = gains.map(|value| value as f32);
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && f64::from(*value) <= super::MAX_RAW_GAIN)
    {
        return Err(validation(
            "temperature/tint gains cannot be represented as f32",
        ));
    }
    Ok(gains)
}

#[cfg(test)]
mod tests {
    use super::super::MAX_RAW_GAIN;
    use super::*;

    const IDENTITY: [[f32; 3]; 4] = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
    ];

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} vs {expected}"
        );
    }

    #[test]
    fn fixed_locus_references_match_illuminant_a_and_d65() {
        let a = base_whitepoint_xy(2_856.0);
        close(a[0], 0.447_6, 0.001);
        close(a[1], 0.407_4, 0.001);

        let d65 = base_whitepoint_xy(6_504.0);
        close(d65[0], 0.3127, 0.0005);
        close(d65[1], 0.3290, 0.0005);
    }

    #[test]
    fn identity_matrix_has_independent_green_normalized_reference_gains() {
        let gains = gains_from_temperature_tint(6_504.0, 0.0, IDENTITY).unwrap();
        let [x, y] = base_whitepoint_xy(6_504.0);
        let z = 1.0 - x - y;
        close(f64::from(gains[0]), y / x, 1.0e-6);
        close(f64::from(gains[1]), 1.0, 1.0e-7);
        close(f64::from(gains[2]), y / z, 1.0e-6);
    }

    #[test]
    fn matrix_direction_and_tint_sign_are_explicit() {
        let matrix = [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5], [0.0; 3]];
        let neutral = gains_from_temperature_tint(6_504.0, 0.0, matrix).unwrap();
        let [x, y] = base_whitepoint_xy(6_504.0);
        let z = 1.0 - x - y;
        close(f64::from(neutral[0]), y / (2.0 * x), 1.0e-6);
        close(f64::from(neutral[2]), 2.0 * y / z, 1.0e-6);

        let neutral_uv = locus_uv(6_504.0).unwrap();
        let magenta_uv = xy_to_uv(tinted_whitepoint_xy(6_504.0, 100.0).unwrap()).unwrap();
        let green_uv = xy_to_uv(tinted_whitepoint_xy(6_504.0, -100.0).unwrap()).unwrap();
        let lower_uv = locus_uv(6_503.0).unwrap();
        let upper_uv = locus_uv(6_505.0).unwrap();
        let tangent = [upper_uv[0] - lower_uv[0], upper_uv[1] - lower_uv[1]];
        let length = tangent[0].hypot(tangent[1]);
        let green_normal = [tangent[1] / length, -tangent[0] / length];
        let projection = |point: [f64; 2]| {
            (point[0] - neutral_uv[0]) * green_normal[0]
                + (point[1] - neutral_uv[1]) * green_normal[1]
        };
        assert!(projection(magenta_uv) > 0.0);
        assert!(projection(green_uv) < 0.0);
    }

    fn calibrated_ratios(cam_xyz: [[f32; 3]; 4], rgb_cam: [[f32; 3]; 3], tint: f64) -> [f64; 2] {
        let gains = gains_from_temperature_tint(6_504.0, tint, cam_xyz).unwrap();
        // A fixed equal-response camera sample makes the sign check independent of a
        // demosaicer. The production path applies these gains before demosaic and then
        // the camera calibration matrix, so this is the corresponding compact reference.
        let response = rgb_cam.map(|row| {
            row.iter()
                .zip(gains)
                .map(|(coefficient, gain)| f64::from(*coefficient) * f64::from(gain))
                .sum::<f64>()
        });
        [response[0] / response[1], response[2] / response[1]]
    }

    #[test]
    fn positive_tint_makes_actual_camera_output_more_magenta() {
        // LibRaw matrices copied from the supplied Nikon Z6 and Fujifilm X100VI metadata.
        // These fixed references catch a user-facing sign inversion that a whitepoint-only
        // self-consistency test cannot see.
        let z6_cam_xyz = [
            [0.9943, -0.3269, -0.0839],
            [-0.5323, 1.3269, 0.2259],
            [-0.1198, 0.2083, 0.7557],
            [0.0; 3],
        ];
        let z6_rgb_cam = [
            [1.5967088, -0.40411508, -0.19259377],
            [-0.14100944, 1.504468, -0.3634585],
            [0.017567826, -0.40943876, 1.391871],
        ];
        let fuji_cam_xyz = [
            [1.1809, -0.5358, -0.1141],
            [-0.4248, 1.2164, 0.2343],
            [-0.0514, 0.1097, 0.5848],
            [0.0; 3],
        ];
        let fuji_rgb_cam = [
            [1.2560475, -0.04212498, -0.21392255],
            [-0.14963932, 1.5497811, -0.40014178],
            [0.004586819, -0.36177072, 1.3571839],
        ];
        // Supplied FC3411 DNG ColorMatrix2 is the fixed D65 XYZ-to-camera calibration.
        let dji_cam_xyz = [
            [0.8531, -0.3148, -0.0888],
            [-0.4071, 1.2492, 0.1265],
            [-0.0209, 0.0486, 0.5114],
            [0.0; 3],
        ];
        let dji_rgb_cam = [
            [1.457_020_3, -0.30071009, -0.15631016],
            [-0.19137614, 1.394_505_5, -0.20312932],
            [-0.00003927, -0.24614702, 1.246_186_3],
        ];
        for (cam_xyz, rgb_cam) in [
            (z6_cam_xyz, z6_rgb_cam),
            (fuji_cam_xyz, fuji_rgb_cam),
            (dji_cam_xyz, dji_rgb_cam),
        ] {
            let neutral = calibrated_ratios(cam_xyz, rgb_cam, 0.0);
            let positive = calibrated_ratios(cam_xyz, rgb_cam, 100.0);
            let negative = calibrated_ratios(cam_xyz, rgb_cam, -100.0);
            assert!(positive[0] > neutral[0] && positive[1] > neutral[1]);
            assert!(negative[0] < neutral[0] && negative[1] < neutral[1]);
        }
    }

    #[test]
    fn actual_camera_range_grid_is_bounded_and_directional() {
        // Exercise the public control contract at both advertised endpoints and immediately
        // around every piecewise-locus transition. The matrices are independent fixed metadata
        // fixtures; this test deliberately checks only gain invariants and control direction,
        // rather than reproducing the locus equations as an oracle.
        let z6_cam_xyz = [
            [0.9943, -0.3269, -0.0839],
            [-0.5323, 1.3269, 0.2259],
            [-0.1198, 0.2083, 0.7557],
            [0.0; 3],
        ];
        let fuji_cam_xyz = [
            [1.1809, -0.5358, -0.1141],
            [-0.4248, 1.2164, 0.2343],
            [-0.0514, 0.1097, 0.5848],
            [0.0; 3],
        ];
        let dji_cam_xyz = [
            [0.8531, -0.3148, -0.0888],
            [-0.4071, 1.2492, 0.1265],
            [-0.0209, 0.0486, 0.5114],
            [0.0; 3],
        ];
        let temperatures = [
            2_000.0, 2_000.001, 2_221.999, 2_222.0, 2_222.001, 3_799.999, 3_800.0, 3_800.001,
            3_999.999, 4_000.0, 4_000.001, 4_499.999, 4_500.0, 4_500.001, 6_504.0, 6_999.999,
            7_000.0, 7_000.001, 11_999.999, 12_000.0,
        ];

        for (camera_name, cam_xyz) in [
            ("Nikon Z6", z6_cam_xyz),
            ("Fujifilm X100VI", fuji_cam_xyz),
            ("DJI FC3411", dji_cam_xyz),
        ] {
            if camera_name == "DJI FC3411" {
                let matrix = camera_matrix(cam_xyz).unwrap();
                let mut maximum = [0.0_f64; 3];
                let mut at = [(0_u32, 0_i32); 3];
                for kelvin in (2_000..=12_000).step_by(10) {
                    for tint in -100..=100 {
                        let [x, y] = tinted_whitepoint_xy(kelvin as f64, tint as f64).unwrap();
                        let xyz = [x / y, 1.0, (1.0 - x - y) / y];
                        let response =
                            matrix.map(|row| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]);
                        let gains = [response[1] / response[0], 1.0, response[1] / response[2]];
                        for channel in 0..3 {
                            if gains[channel] > maximum[channel] {
                                maximum[channel] = gains[channel];
                                at[channel] = (kelvin, tint);
                            }
                        }
                    }
                }
                assert!(
                    maximum
                        .iter()
                        .all(|gain| gain.is_finite() && *gain > 0.0 && *gain <= MAX_RAW_GAIN),
                    "FC3411 full-grid gain maxima {maximum:?} at {at:?} exceed the RAW limit"
                );
            }
            for &temperature in &temperatures {
                let neutral = gains_from_temperature_tint(temperature, 0.0, cam_xyz)
                    .unwrap_or_else(|error| {
                        panic!("{camera_name} {temperature} K tint 0: {error}")
                    });
                let warm = gains_from_temperature_tint(temperature, -100.0, cam_xyz)
                    .unwrap_or_else(|error| {
                        panic!("{camera_name} {temperature} K tint -100: {error}")
                    });
                let magenta = gains_from_temperature_tint(temperature, 100.0, cam_xyz)
                    .unwrap_or_else(|error| {
                        let matrix = camera_matrix(cam_xyz).unwrap();
                        let [x, y] = tinted_whitepoint_xy(temperature, 100.0).unwrap();
                        let xyz = [x / y, 1.0, (1.0 - x - y) / y];
                        let response =
                            matrix.map(|row| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]);
                        panic!(
                            "{camera_name} {temperature} K tint +100: {error}; gains {:?}",
                            [response[1] / response[0], 1.0, response[1] / response[2]]
                        )
                    });

                for (tint, gains) in [(-100.0, warm), (0.0, neutral), (100.0, magenta)] {
                    assert!(
                        gains.iter().all(|gain| gain.is_finite()
                            && *gain > 0.0
                            && f64::from(*gain) <= MAX_RAW_GAIN),
                        "{camera_name} {temperature} K tint {tint} returned invalid gains {gains:?}"
                    );
                    assert_eq!(gains[1], 1.0, "green must be the normalized channel");
                }

                // Positive Lightwell tint is magenta: its compensating sensor gains increase
                // both chromatic channels, while negative tint moves them in the opposite way.
                assert!(magenta[0] > neutral[0] && magenta[2] > neutral[2]);
                assert!(warm[0] < neutral[0] && warm[2] < neutral[2]);
            }

            let low_kelvin = gains_from_temperature_tint(2_000.0, 0.0, cam_xyz).unwrap();
            let high_kelvin = gains_from_temperature_tint(12_000.0, 0.0, cam_xyz).unwrap();
            assert!(low_kelvin[0] < high_kelvin[0]);
            assert!(low_kelvin[2] > high_kelvin[2]);
            assert!(
                f64::from(low_kelvin[2]) / f64::from(low_kelvin[0])
                    > f64::from(high_kelvin[2]) / f64::from(high_kelvin[0]),
                "{camera_name} warm/cool balance direction inverted"
            );
        }
    }

    #[test]
    fn published_locus_polynomials_have_bounded_piecewise_discontinuities() {
        // The published splines are close but not exactly joined at 2222 and 4000 K; keep the
        // measured seam bounded instead of silently claiming mathematical continuity. The CIE
        // daylight branches likewise meet closely at 7000 K.
        for boundary in [2_222.0, 4_000.0] {
            let before = planck_xy(boundary - 1.0e-3);
            let after = planck_xy(boundary + 1.0e-3);
            close(before[0], after[0], 1.0e-4);
            close(before[1], after[1], 1.0e-4);
        }
        let before = daylight_xy(7_000.0 - 1.0e-3);
        let after = daylight_xy(7_000.0 + 1.0e-3);
        close(before[0], after[0], 1.0e-4);
        close(before[1], after[1], 1.0e-4);
    }

    #[test]
    fn locus_seam_is_continuous_and_controls_are_not_coerced() {
        let before = base_whitepoint_xy(PLANCK_DAYLIGHT_BLEND_START_K - 1.0e-6);
        let after = base_whitepoint_xy(PLANCK_DAYLIGHT_BLEND_START_K + 1.0e-6);
        close(before[0], after[0], 1.0e-7);
        close(before[1], after[1], 1.0e-7);
        let before = base_whitepoint_xy(PLANCK_DAYLIGHT_BLEND_END_K - 1.0e-6);
        let after = base_whitepoint_xy(PLANCK_DAYLIGHT_BLEND_END_K + 1.0e-6);
        close(before[0], after[0], 1.0e-7);
        close(before[1], after[1], 1.0e-7);

        for temperature in [1_999.999, 12_000.001] {
            assert!(gains_from_temperature_tint(temperature, 0.0, IDENTITY).is_err());
        }
        for tint in [-100.001, 100.001] {
            assert!(gains_from_temperature_tint(6_504.0, tint, IDENTITY).is_err());
        }
    }

    #[test]
    fn malformed_matrices_and_invalid_gains_fail_closed() {
        let mut nonfinite = IDENTITY;
        nonfinite[0][0] = f32::NAN;
        assert!(gains_from_temperature_tint(6_504.0, 0.0, nonfinite).is_err());

        let singular = [[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert!(gains_from_temperature_tint(6_504.0, 0.0, singular).is_err());

        let nonpositive = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert!(gains_from_temperature_tint(6_504.0, 0.0, nonpositive).is_err());

        let too_large = [[0.01, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert!(gains_from_temperature_tint(6_504.0, 0.0, too_large).is_err());
    }
}
