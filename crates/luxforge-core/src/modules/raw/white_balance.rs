//! Temperature/tint white balance for prepared camera-channel data.
//!
//! The returned gains are sensor multipliers: they are applied before demosaicing and are
//! normalized so green is exactly one. `cam_xyz` is the four-by-three LibRaw matrix whose first
//! three rows map XYZ to camera responses. The fourth row is intentionally ignored because this
//! helper only supports three developed camera channels.
//!
//! Temperature uses a documented blackbody/daylight locus approximation. The blackbody equations
//! cover the lower part of Luxforge's range, the daylight equations cover the upper part, and a
//! smooth transition keeps the two approximations continuous around their seam. Tint is an
//! explicit Luxforge unit: one unit is 1e-4 signed CIE 1960 `uv` distance (`Duv`). The
//! user-facing positive direction is magenta: it moves the assumed illuminant toward +Duv (the
//! green side), so the compensating sensor gains make a calibrated neutral more magenta.
//!
//! Provenance: RawTherapee's White Balance technical notes document the blackbody/daylight split
//! and its temperature ranges; the CIE 1960 `uv` coordinates define the signed locus offset.
//! LibRaw's API data structure and `cam_xyz_coeff` implementation establish the XYZ-to-camera row
//! direction used here. Nothing here serializes an AsShot temperature: the payload keeps the
//! camera's gains, and [`temperature_tint_from_gains`] answers which temperature and tint would
//! reproduce them, for a client to show.

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
    // and 4000 K. Luxforge validates a narrower 2000..12000 K interval before calling this.
    let x = if temperature_kelvin <= 4_000.0 {
        -0.266_123_9e9 / temperature_kelvin.powi(3) - 0.234_358_9e6 / temperature_kelvin.powi(2)
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
    // blackbody locus. The 3800..4500 K blend in `base_whitepoint_xy` is a Luxforge
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

/// Where a temperature sits on the locus in CIE 1960 `uv`, with the unit tangent toward higher
/// temperature and the unit normal toward the green (+Duv) side that tint moves along. The forward
/// map and its inverse both read the locus through this one function, so they cannot disagree
/// about the frame a tint is measured in.
struct LocusFrame {
    base: [f64; 2],
    tangent: [f64; 2],
    green_normal: [f64; 2],
}

fn locus_frame(temperature_kelvin: f64) -> Result<LocusFrame, Error> {
    let base = locus_uv(temperature_kelvin)?;
    let lower = (temperature_kelvin - 1.0).max(MIN_TEMPERATURE_K);
    let upper = (temperature_kelvin + 1.0).min(MAX_TEMPERATURE_K);
    let lower_uv = locus_uv(lower)?;
    let upper_uv = locus_uv(upper)?;
    let tangent = [upper_uv[0] - lower_uv[0], upper_uv[1] - lower_uv[1]];
    let tangent_length = tangent[0].hypot(tangent[1]);
    if !tangent_length.is_finite() || tangent_length <= f64::EPSILON {
        return Err(validation("white-balance locus tangent is degenerate"));
    }
    let tangent = [tangent[0] / tangent_length, tangent[1] / tangent_length];
    // Along this locus temperature increases toward lower u and lower v. This normal points
    // toward the conventional positive-Duv/green side. Positive Luxforge tint moves the
    // assumed illuminant in that direction; its compensating gains then make the output more
    // magenta, matching the familiar control direction.
    Ok(LocusFrame {
        base,
        tangent,
        green_normal: [tangent[1], -tangent[0]],
    })
}

fn tinted_whitepoint_uv(temperature_kelvin: f64, tint: f64) -> Result<[f64; 2], Error> {
    let frame = locus_frame(temperature_kelvin)?;
    let duv = tint * TINT_DUV_UNIT;
    Ok([
        frame.base[0] + frame.green_normal[0] * duv,
        frame.base[1] + frame.green_normal[1] * duv,
    ])
}

fn tinted_whitepoint_xy(temperature_kelvin: f64, tint: f64) -> Result<[f64; 2], Error> {
    uv_to_xy(tinted_whitepoint_uv(temperature_kelvin, tint)?)
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

/// Convert a temperature and Luxforge tint into green-normalized sensor gains.
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
    let gains = gains_f64(temperature_kelvin, tint, &camera_matrix(cam_xyz)?)?;
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

/// The forward map in `f64`: the validated controls, the white point they select and the
/// green-normalized sensor gains that make that white neutral, before the `f32` conversion the
/// payload stores. [`temperature_tint_from_gains`] checks its answer through this same function.
fn gains_f64(
    temperature_kelvin: f64,
    tint: f64,
    matrix: &[[f64; 3]; 3],
) -> Result<[f64; 3], Error> {
    if !temperature_kelvin.is_finite()
        || !(MIN_TEMPERATURE_K..=MAX_TEMPERATURE_K).contains(&temperature_kelvin)
    {
        return Err(validation(
            "RAW temperature must be finite and 2000..=12000 K",
        ));
    }
    if !tint.is_finite() || !(MIN_TINT..=MAX_TINT).contains(&tint) {
        return Err(validation(
            "RAW tint must be finite and -100..=100 Luxforge units",
        ));
    }
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
    Ok(gains)
}

/// How far, in CIE 1960 `uv`, the white a solved temperature and tint select may sit from the white
/// the gains describe: half of one Luxforge tint unit. A white the forward map reaches is solved to
/// rounding; this bound matters only where the published locus polynomials do not quite join (a
/// gap of `2.7e-5` at 4000 K, `2.8e-6` at 2222 K and `2.7e-7` at 7000 K), so the nearest
/// temperature there is still an answer, and it stays below anything the controls can show.
pub const INVERSE_TOLERANCE_UV: f64 = 0.5 * TINT_DUV_UNIT;
/// A bisection of one interval, or the search for the closest white in one, stops on its own once
/// its bracket is one `f64` apart, after about 30 halvings of a scan step; this bounds it
/// regardless.
const INVERSE_MAX_ITERATIONS: usize = 64;
/// Where the forward map jumps in temperature: at each seam of the published locus polynomials,
/// where the base white jumps, and one kelvin either side, where the tangent's ±1 K difference
/// starts or stops straddling that jump. Between them it is continuous.
const INVERSE_BREAKPOINTS: [f64; 9] = [
    2_221.0, 2_222.0, 2_223.0, 3_999.0, 4_000.0, 4_001.0, 6_999.0, 7_000.0, 7_001.0,
];
/// The spacing of the scan for sign changes. Outside the Planckian/daylight blend the locus bends
/// gently (a radius of curvature of at least 0.087 `uv` away from the seams, against the 0.01 `uv`
/// a tint of ±100 reaches), so the tangent component falls steadily and a coarse scan brackets its
/// one crossing.
const INVERSE_SCAN_STEP_K: f64 = 100.0;
/// Inside the blend the locus bends sharply (a radius down to 0.008 `uv`, less than a ±100 tint
/// reaches, so the lines tint moves along cross and one white can have several temperatures), and
/// a fine scan finds each crossing that a coarse bracket would pair off and miss.
const INVERSE_FOLD_BAND_K: [f64; 2] = [3_700.0, 4_600.0];
const INVERSE_FOLD_STEP_K: f64 = 10.0;

/// The temperatures the inverse evaluates first, in order: the coarse scan, the fine scan of the
/// blend and the breakpoints. About 190 of them.
fn inverse_scan() -> Vec<f64> {
    let grid = |from: f64, to: f64, step: f64| {
        let steps = ((to - from) / step).round() as usize;
        (0..=steps).map(move |index| from + index as f64 * step)
    };
    let mut points: Vec<f64> = grid(MIN_TEMPERATURE_K, MAX_TEMPERATURE_K, INVERSE_SCAN_STEP_K)
        .chain(grid(
            INVERSE_FOLD_BAND_K[0],
            INVERSE_FOLD_BAND_K[1],
            INVERSE_FOLD_STEP_K,
        ))
        .chain(INVERSE_BREAKPOINTS)
        .collect();
    points.sort_by(f64::total_cmp);
    points.dedup();
    points
}

/// The temperature and Luxforge tint whose gains are `gains`: the inverse of
/// [`gains_from_temperature_tint`] over the same camera matrix and the same declared ranges.
///
/// The forward map sets `gains[i] = response[1] / response[i]` for the camera's response to the
/// selected white, so that response is proportional to the reciprocal gains; the inverse camera
/// matrix turns it into the white's XYZ and so its CIE 1960 `uv`. Tint moves a white along the
/// locus normal at its temperature, so the temperature is where the white's offset from the locus
/// has no component along the locus tangent, and the tint is the normal component in tint units.
/// That tangent component is continuous between the [`INVERSE_BREAKPOINTS`], so each interval
/// whose ends differ in sign is bisected, in at most [`INVERSE_MAX_ITERATIONS`] steps, and the
/// temperature whose white lands closest wins. Everything is `f64` and nothing is clamped into
/// range: a white outside 2000..12000 K or ±100 tint, gains that describe no visible white, and a
/// white no temperature and tint reach within [`INVERSE_TOLERANCE_UV`] are refused with
/// `out-of-range:` and the reason, and the answer is checked through the forward map before it is
/// returned. It costs a few hundred locus evaluations and reads no pixel.
pub fn temperature_tint_from_gains(
    gains: [f32; 3],
    cam_xyz: [[f32; 3]; 4],
) -> Result<[f64; 2], Error> {
    temperature_tint_from_gains_f64(gains.map(f64::from), &camera_matrix(cam_xyz)?)
}

fn temperature_tint_from_gains_f64(
    gains: [f64; 3],
    matrix: &[[f64; 3]; 3],
) -> Result<[f64; 2], Error> {
    if !gains.iter().all(|gain| gain.is_finite() && *gain > 0.0) {
        return Err(validation("RAW gains must be finite and positive"));
    }
    let inverse = mat3_inverse(matrix)?;
    let response = [gains[1] / gains[0], 1.0, gains[1] / gains[2]];
    let xyz = inverse.map(|row| row[0] * response[0] + row[1] * response[1] + row[2] * response[2]);
    let sum = xyz[0] + xyz[1] + xyz[2];
    let xy = [xyz[0] / sum, xyz[1] / sum];
    if !sum.is_finite()
        || sum <= 0.0
        || !xy.iter().all(|value| value.is_finite() && *value > 0.0)
        || xy[0] + xy[1] >= 1.0
    {
        return Err(validation(
            "out-of-range: the gains describe no visible white",
        ));
    }
    let target = xy_to_uv(xy)?;
    // One temperature read as an answer: the tangent component, whose sign changes where a white
    // lies on that temperature's tint line, the tint, and how far the white the answer selects,
    // with its tint held to the range, is from the gains' white.
    let answer = |temperature: f64| -> Result<Answer, Error> {
        let frame = locus_frame(temperature)?;
        let offset = [target[0] - frame.base[0], target[1] - frame.base[1]];
        let along = offset[0] * frame.tangent[0] + offset[1] * frame.tangent[1];
        let tint =
            (offset[0] * frame.green_normal[0] + offset[1] * frame.green_normal[1]) / TINT_DUV_UNIT;
        let beyond = (tint.abs() - MAX_TINT).max(0.0) * TINT_DUV_UNIT;
        Ok(Answer {
            temperature,
            along,
            tint,
            distance: along.hypot(beyond),
        })
    };
    let points = inverse_scan();
    let scanned = points
        .iter()
        .map(|temperature| answer(*temperature))
        .collect::<Result<Vec<_>, _>>()?;
    let (at_low, at_high) = (scanned[0].along, scanned[scanned.len() - 1].along);
    // Every scanned temperature is a candidate, so a white in a seam's gap still finds the nearest.
    let mut best = scanned
        .iter()
        .copied()
        .reduce(Answer::closer)
        .expect("the scan is not empty");
    for pair in scanned.windows(2) {
        let (mut low, mut high) = (pair[0].temperature, pair[1].temperature);
        let low_sign = pair[0].along.is_sign_positive();
        if pair[0].along == 0.0
            || pair[1].along == 0.0
            || low_sign == pair[1].along.is_sign_positive()
        {
            continue;
        }
        for _ in 0..INVERSE_MAX_ITERATIONS {
            let middle = 0.5 * (low + high);
            if middle <= low || middle >= high {
                break;
            }
            if answer(middle)?.along.is_sign_positive() == low_sign {
                low = middle;
            } else {
                high = middle;
            }
        }
        best = best.closer(answer(low)?).closer(answer(high)?);
    }
    // Where two temperatures meet at a fold of the map, the tangent component touches zero without
    // changing sign between two scanned temperatures. The closest white on either side of the best
    // scanned one is then found by a golden-section search, which needs no sign change.
    if best.distance > INVERSE_TOLERANCE_UV
        && let Some(index) = points.iter().position(|point| *point == best.temperature)
    {
        for (mut low, mut high) in [
            (points[index.saturating_sub(1)], points[index]),
            (points[index], points[(index + 1).min(points.len() - 1)]),
        ] {
            const RATIO: f64 = 0.618_033_988_749_894_8;
            for _ in 0..INVERSE_MAX_ITERATIONS {
                if high - low <= f64::EPSILON * high {
                    break;
                }
                let (left, right) = (high - RATIO * (high - low), low + RATIO * (high - low));
                if answer(left)?.distance <= answer(right)?.distance {
                    high = right;
                } else {
                    low = left;
                }
            }
            best = best.closer(answer(0.5 * (low + high))?);
        }
    }
    if !best.tint.is_finite() || best.distance > INVERSE_TOLERANCE_UV {
        return Err(validation(if at_low <= 0.0 {
            "out-of-range: the gains need a temperature below 2000 K".to_owned()
        } else if at_high >= 0.0 {
            "out-of-range: the gains need a temperature above 12000 K".to_owned()
        } else if best.along.abs() <= INVERSE_TOLERANCE_UV {
            format!(
                "out-of-range: the gains need a tint of {:.1}, outside -100..100",
                best.tint
            )
        } else {
            format!(
                "out-of-range: no temperature and tint reach the gains' white within {INVERSE_TOLERANCE_UV} uv (nearest {:.2e} at {:.3} K)",
                best.distance, best.temperature
            )
        }));
    }
    // The end of the tint range answers a white just beyond it on the same terms as a seam: the
    // white the answer selects is within the tolerance of the gains' white. That is what keeps a
    // white stored at ±100 answerable, since its `f32` gains land a rounding beyond.
    let tint = best.tint.clamp(MIN_TINT, MAX_TINT);
    gains_f64(best.temperature, tint, matrix)?;
    Ok([best.temperature, tint])
}

/// One temperature tried by [`temperature_tint_from_gains`].
#[derive(Clone, Copy)]
struct Answer {
    temperature: f64,
    /// The gains' white's offset along the locus tangent: zero on this temperature's tint line.
    along: f64,
    /// The offset along the locus normal, in tint units, before it is held to the range.
    tint: f64,
    /// From the gains' white to the white this temperature and the held tint select, in `uv`.
    distance: f64,
}

impl Answer {
    /// The closer of two answers; the earlier one on a tie, so the result does not depend on
    /// anything but the order of the scan.
    fn closer(self, other: Self) -> Self {
        if other.distance < self.distance {
            other
        } else {
            self
        }
    }
}

/// The exact 3×3 inverse by adjugate and determinant, of a matrix [`camera_matrix`] has already
/// found non-degenerate.
fn mat3_inverse(m: &[[f64; 3]; 3]) -> Result<[[f64; 3]; 3], Error> {
    let cofactor =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let adjugate = [
        [
            cofactor(1, 2, 1, 2),
            -cofactor(0, 2, 1, 2),
            cofactor(0, 1, 1, 2),
        ],
        [
            -cofactor(1, 2, 0, 2),
            cofactor(0, 2, 0, 2),
            -cofactor(0, 1, 0, 2),
        ],
        [
            cofactor(1, 2, 0, 1),
            -cofactor(0, 2, 0, 1),
            cofactor(0, 1, 0, 1),
        ],
    ];
    let determinant =
        m[0][0] * adjugate[0][0] + m[0][1] * adjugate[1][0] + m[0][2] * adjugate[2][0];
    let inverse = adjugate.map(|row| row.map(|value| value / determinant));
    if !determinant.is_finite() || !inverse.iter().flatten().all(|value| value.is_finite()) {
        return Err(validation("camera XYZ matrix has no finite inverse"));
    }
    Ok(inverse)
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

                // Positive Luxforge tint is magenta: its compensating sensor gains increase
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

    /// LibRaw `cam_xyz` of the three supplied RAW fixtures: Nikon Z6 and Fujifilm X100VI from their
    /// metadata, DJI FC3411 the DNG's ColorMatrix2 (D65). The same values the tests above use.
    const Z6_CAM_XYZ: [[f32; 3]; 4] = [
        [0.9943, -0.3269, -0.0839],
        [-0.5323, 1.3269, 0.2259],
        [-0.1198, 0.2083, 0.7557],
        [0.0; 3],
    ];
    const X100VI_CAM_XYZ: [[f32; 3]; 4] = [
        [1.1809, -0.5358, -0.1141],
        [-0.4248, 1.2164, 0.2343],
        [-0.0514, 0.1097, 0.5848],
        [0.0; 3],
    ];
    const FC3411_CAM_XYZ: [[f32; 3]; 4] = [
        [0.8531, -0.3148, -0.0888],
        [-0.4071, 1.2492, 0.1265],
        [-0.0209, 0.0486, 0.5114],
        [0.0; 3],
    ];

    /// The three camera matrices, and synthetic ones that no camera supplies: the identity, a
    /// diagonal that scales red and blue apart, and a dense mixing matrix.
    fn inverse_matrices() -> Vec<(&'static str, [[f32; 3]; 4])> {
        vec![
            ("Nikon Z6", Z6_CAM_XYZ),
            ("Fujifilm X100VI", X100VI_CAM_XYZ),
            ("DJI FC3411", FC3411_CAM_XYZ),
            ("identity", IDENTITY),
            (
                "diagonal",
                [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5], [0.0; 3]],
            ),
            (
                "mixing",
                [
                    [0.7, 0.25, 0.05],
                    [-0.3, 1.2, 0.1],
                    [0.02, -0.05, 0.9],
                    [0.0; 3],
                ],
            ),
        ]
    }

    /// Within a kelvin and a half of a seam the forward map is not one-to-one: the tangent's ±1 K
    /// difference straddles the jump, so a temperature there can come back as another whose gains
    /// are the same.
    fn near_a_seam(temperature: f64) -> bool {
        [2_222.0, 4_000.0, 7_000.0]
            .iter()
            .any(|seam: &f64| (temperature - seam).abs() <= 1.5)
    }

    /// Where the Planckian/daylight blend bends the locus more tightly than a strong green tint
    /// reaches (a radius of about 0.008 `uv` near 4450..4500 K), the tint lines cross and the
    /// forward map folds: one set of gains has several temperatures and tints. The grid below finds
    /// every non-unique case inside this band, on the green side, and none outside it.
    fn in_the_fold(temperature: f64, tint: f64) -> bool {
        (4_400.0..=4_550.0).contains(&temperature) && tint <= -80.0
    }

    fn relative(actual: [f64; 3], expected: [f64; 3]) -> f64 {
        actual
            .iter()
            .zip(expected)
            .map(|(actual, expected)| ((actual - expected) / expected).abs())
            .fold(0.0, f64::max)
    }

    /// The inverse against the forward map over the three camera matrices and three synthetic
    /// ones, on a grid of the whole declared range with both ends, every seam and the fold
    /// included. Gains always come back: to `1e-11` in `f64`, and from the `f32` gains a payload
    /// stores to `1e-6`, the rounding those carry at an end of the range, where the answer is held
    /// to it. Where the map is one-to-one the controls come back too: to `1e-6` in `f64`, and from
    /// stored gains to 0.01 K and 0.001 tint, far inside the whole kelvin and tint unit a field
    /// shows.
    #[test]
    fn the_inverse_round_trips_the_forward_map_over_the_whole_range() {
        let mut temperatures: Vec<f64> = (2_000..=12_000).step_by(100).map(f64::from).collect();
        temperatures.extend([
            2_000.5, 2_221.5, 2_222.5, 3_799.9, 3_800.1, 3_999.5, 4_000.5, 4_450.0, 4_499.9,
            4_500.1, 5_003.7, 6_504.0, 6_999.5, 7_000.5, 11_999.5,
        ]);
        let tints: Vec<f64> = (-100..=100)
            .step_by(10)
            .map(f64::from)
            .chain([-99.5, -85.0, 0.25, 99.5])
            .collect();
        // Gains in f64 and f32; controls in f64 and f32 where the map is one-to-one.
        let mut worst = [0.0_f64; 6];
        let (mut checked, mut folded, mut skipped) = (0, 0, 0);
        for (name, cam_xyz) in inverse_matrices() {
            let matrix = camera_matrix(cam_xyz).unwrap();
            for &temperature in &temperatures {
                for &tint in &tints {
                    // A synthetic matrix can need a gain over 32 at an end of the range, which the
                    // forward map refuses; no camera matrix here does.
                    let gains = match gains_f64(temperature, tint, &matrix) {
                        Ok(gains) => gains,
                        Err(error) => {
                            assert!(
                                !name.contains(' '),
                                "{name} {temperature} K {tint}: {error}"
                            );
                            skipped += 1;
                            continue;
                        }
                    };
                    let unique = !near_a_seam(temperature) && !in_the_fold(temperature, tint);
                    let [kelvin, solved] = temperature_tint_from_gains_f64(gains, &matrix)
                        .unwrap_or_else(|error| panic!("{name} {temperature} K {tint}: {error}"));
                    worst[0] =
                        worst[0].max(relative(gains_f64(kelvin, solved, &matrix).unwrap(), gains));
                    if unique {
                        worst[2] = worst[2].max((kelvin - temperature).abs());
                        worst[3] = worst[3].max((solved - tint).abs());
                    } else if (kelvin - temperature).abs() > 0.01 {
                        folded += 1;
                    }

                    let stored = gains_from_temperature_tint(temperature, tint, cam_xyz).unwrap();
                    let [kelvin, solved] = temperature_tint_from_gains(stored, cam_xyz)
                        .unwrap_or_else(|error| panic!("{name} {temperature} K {tint}: {error}"));
                    let again = gains_f64(kelvin, solved, &matrix).unwrap();
                    worst[1] = worst[1].max(relative(again, stored.map(f64::from)));
                    if unique {
                        worst[4] = worst[4].max((kelvin - temperature).abs());
                        worst[5] = worst[5].max((solved - tint).abs());
                    }
                    assert!((MIN_TEMPERATURE_K..=MAX_TEMPERATURE_K).contains(&kelvin));
                    assert!((MIN_TINT..=MAX_TINT).contains(&solved));
                    checked += 1;
                }
            }
        }
        println!(
            "{checked} cases ({skipped} beyond the forward map's 32× gain, {folded} answered by \
             another preimage at a seam or in the fold); gains: f64 {:.2e}, f32 {:.2e} relative; \
             one-to-one controls: f64 {:.2e} K and {:.2e} tint, f32 {:.2e} K and {:.2e} tint",
            worst[0], worst[1], worst[2], worst[3], worst[4], worst[5]
        );
        assert!(worst[0] <= 1.0e-11, "f64 gains {:.2e}", worst[0]);
        assert!(worst[1] <= 1.0e-6, "f32 gains {:.2e}", worst[1]);
        assert!(worst[2] <= 1.0e-6, "f64 temperature {:.2e} K", worst[2]);
        assert!(worst[3] <= 1.0e-6, "f64 tint {:.2e}", worst[3]);
        assert!(worst[4] <= 0.01, "f32 temperature {:.2e} K", worst[4]);
        assert!(worst[5] <= 1.0e-3, "f32 tint {:.2e}", worst[5]);
    }

    /// Gains outside what the controls can say are refused with the reason, never clamped into
    /// range: a white warmer than 2000 K, cooler than 12000 K, beyond ±100 tint, a white no
    /// visible colour has, and inputs no white can come from.
    #[test]
    fn the_inverse_refuses_whites_outside_the_declared_ranges() {
        // Gains for a white the controls cannot select, through the same matrix product the
        // forward map uses: the locus polynomials themselves reach 1667..25000 K.
        let gains_of = |uv: [f64; 2], cam_xyz: [[f32; 3]; 4]| {
            let [x, y] = uv_to_xy(uv).unwrap();
            let xyz = [x / y, 1.0, (1.0 - x - y) / y];
            let matrix = camera_matrix(cam_xyz).unwrap();
            let response = matrix.map(|row| row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]);
            [response[1] / response[0], 1.0, response[1] / response[2]].map(|gain| gain as f32)
        };
        let refused = |gains: [f32; 3], cam_xyz: [[f32; 3]; 4]| {
            let error = temperature_tint_from_gains(gains, cam_xyz).expect_err("refused");
            assert_eq!(error.kind, ErrorKind::Validation);
            error.detail
        };
        for (name, cam_xyz) in inverse_matrices().into_iter().take(3) {
            let warm = gains_of(xy_to_uv(planck_xy(1_900.0)).unwrap(), cam_xyz);
            assert_eq!(
                refused(warm, cam_xyz),
                "out-of-range: the gains need a temperature below 2000 K",
                "{name}"
            );
            let cool = gains_of(xy_to_uv(daylight_xy(15_000.0)).unwrap(), cam_xyz);
            assert_eq!(
                refused(cool, cam_xyz),
                "out-of-range: the gains need a temperature above 12000 K",
                "{name}"
            );
            for (tint, shown) in [(130.0, "130.0"), (-130.0, "-130.0")] {
                let frame = locus_frame(5_000.0).unwrap();
                let duv = tint * TINT_DUV_UNIT;
                let white = [
                    frame.base[0] + frame.green_normal[0] * duv,
                    frame.base[1] + frame.green_normal[1] * duv,
                ];
                let detail = refused(gains_of(white, cam_xyz), cam_xyz);
                assert!(
                    detail.starts_with(&format!("out-of-range: the gains need a tint of {shown}")),
                    "{name} {tint}: {detail}"
                );
            }
            // Just inside the ends, the same construction is answered.
            let frame = locus_frame(5_000.0).unwrap();
            let white = [
                frame.base[0] + frame.green_normal[0] * 99.0 * TINT_DUV_UNIT,
                frame.base[1] + frame.green_normal[1] * 99.0 * TINT_DUV_UNIT,
            ];
            let [kelvin, tint] = temperature_tint_from_gains(gains_of(white, cam_xyz), cam_xyz)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            close(kelvin, 5_000.0, 0.01);
            close(tint, 99.0, 1.0e-3);
        }
        // A camera whose response to any white has a negative XYZ has no visible white.
        let flipped = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert_eq!(
            refused([1.0, 1.0, 1.0], flipped),
            "out-of-range: the gains describe no visible white"
        );
        for gains in [
            [f32::NAN, 1.0, 1.0],
            [1.0, 1.0, f32::INFINITY],
            [0.0, 1.0, 1.0],
            [2.0, 1.0, -1.5],
        ] {
            assert_eq!(
                refused(gains, Z6_CAM_XYZ),
                "RAW gains must be finite and positive"
            );
        }
        let singular = [[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]];
        assert!(refused([2.0, 1.0, 1.5], singular).contains("degenerate"));
    }

    /// What one inversion costs: run with `--release -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a gate"]
    fn measure_the_inverse() {
        let gains = gains_from_temperature_tint(5_000.0, 10.0, Z6_CAM_XYZ).unwrap();
        let started = std::time::Instant::now();
        let runs = 2_000;
        for _ in 0..runs {
            std::hint::black_box(
                temperature_tint_from_gains(std::hint::black_box(gains), Z6_CAM_XYZ).unwrap(),
            );
        }
        println!(
            "{:.1} µs per inversion",
            started.elapsed().as_secs_f64() * 1e6 / f64::from(runs)
        );
    }

    /// The real fixtures: each supplied RAW's own as-shot gains and camera matrix, read from the
    /// original the private manifest names, inverted and checked back through the forward map, and
    /// a grid round trip over that camera's own matrix. Run with
    /// `LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json cargo test --release -p luxforge-core
    /// --lib raw::white_balance -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires the private RAW fixtures and their manifest"]
    fn the_supplied_raw_fixtures_have_an_as_shot_equivalent() {
        let manifest = std::env::var("LUXFORGE_RAW_MANIFEST").expect("LUXFORGE_RAW_MANIFEST");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
        let sources = manifest["sources"].as_array().expect("manifest sources");
        assert!(!sources.is_empty());
        for source in sources {
            let path = source["path"].as_str().expect("a source path");
            let bytes: std::sync::Arc<[u8]> = std::fs::read(path).unwrap().into();
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let raw = luxforge_raw::RawSource::decode(bytes, &cancel).unwrap();
            let metadata = raw.metadata();
            let (gains, cam_xyz) = (metadata.as_shot_gains, metadata.cam_xyz);
            let matrix = camera_matrix(cam_xyz).unwrap();
            let [kelvin, tint] = temperature_tint_from_gains(gains, cam_xyz)
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            let again = gains_f64(kelvin, tint, &matrix).unwrap();
            let error = relative(again, gains.map(f64::from));
            println!(
                "{}: as-shot gains {gains:?} = {kelvin:.3} K, tint {tint:+.4}; gains back to {error:.2e} relative; cam_xyz {:?}",
                source["id"],
                &cam_xyz[..3]
            );
            assert!(error <= 1.0e-6, "{path}: {error:.2e}");
            let mut worst = [0.0_f64; 3];
            for temperature in (2_000..=12_000).step_by(250).map(f64::from) {
                for tint in (-100..=100).step_by(20).map(f64::from) {
                    let stored = gains_from_temperature_tint(temperature, tint, cam_xyz).unwrap();
                    let [kelvin, solved] = temperature_tint_from_gains(stored, cam_xyz).unwrap();
                    let again = gains_f64(kelvin, solved, &matrix).unwrap();
                    worst[0] = worst[0].max(relative(again, stored.map(f64::from)));
                    if !near_a_seam(temperature) && !in_the_fold(temperature, tint) {
                        worst[1] = worst[1].max((kelvin - temperature).abs());
                        worst[2] = worst[2].max((solved - tint).abs());
                    }
                }
            }
            println!(
                "{}: grid gains {:.2e} relative, controls {:.2e} K and {:.2e} tint",
                source["id"], worst[0], worst[1], worst[2]
            );
            assert!(worst[0] <= 1.0e-6 && worst[1] <= 0.01 && worst[2] <= 1.0e-3);
        }
    }
}
