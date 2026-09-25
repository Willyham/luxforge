//! The Basic module's white-balance unit and the neutral picker's solver.
//!
//! Both are frozen by `docs/design/basic-white-balance.md`: a von Kries chromatic adaptation in
//! Bradford LMS anchored at the sRGB D65 white, Temperature as a mired shift along the Planckian
//! locus and Tint as a perpendicular CIE 1960 `(u, v)` offset. Temperature and Tint are **relative
//! corrections of an already-rendered SDR JPEG**, not a reconstruction of a scene illuminant and
//! not a camera's Kelvin metadata.
//!
//! Everything here is stated once, in the units the design names: the two forward matrices are the
//! only colour constants written down, their inverses are computed with the exact 3x3
//! adjugate/determinant formula at `f64` precision (two independently rounded 7-digit matrices
//! compose to identity only to about `1e-7`, which fails the design's `1e-12` identity requirement
//! outright), and the whole matrix sandwich is folded into one linear-sRGB 3x3 in the constructor
//! so a pixel costs one 3x3 multiply in `f32`.
use crate::{modules::PointwiseColor, render::srgb_to_linear_f64};

type Mat3 = [[f64; 3]; 3];

// ---------------------------------------------------------------------------------------------
// Constants, verbatim from the design document.
// ---------------------------------------------------------------------------------------------

/// Linear sRGB (D65) to CIE XYZ, the IEC 61966-2-1 primaries and white point.
const RGB_TO_XYZ: Mat3 = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.0721750],
    [0.0193339, 0.1191920, 0.9503041],
];

/// CIE XYZ to Bradford cone-response space, the 1985 Bradford matrix.
const XYZ_TO_LMS: Mat3 = [
    [0.8951000, 0.2664000, -0.1614000],
    [-0.7502000, 1.7135000, 0.0367000],
    [0.0389000, -0.0685000, 1.0296000],
];

/// The sRGB reference white: the chromaticity the transform reproduces *exactly* at Temperature 0,
/// Tint 0. Deliberately not the Planckian point below, which is a different, nearby point used only
/// to read off a direction.
const D65_X: f64 = 0.3127;
const D65_Y: f64 = 0.3290;

/// The nominal correlated colour temperature of the sRGB/D65 white point: the zero of the mired
/// shift and the point at which the locus tangent is read. The transform never assumes this point's
/// own `(x, y)` equals [`D65_X`]/[`D65_Y`] — it does not, and that separation is what makes the
/// `(0, 0)` identity exact by cancellation rather than by coincidence.
const D65_NOMINAL_CCT: f64 = 6504.0;

/// Mired shift per unit of Temperature, subtracted: positive Temperature lowers the mired value and
/// so raises the nominal colour temperature.
const K_T: f64 = 1.0375;

/// Perpendicular offset in CIE 1960 `(u, v)` per unit of Tint.
const K_TINT: f64 = 0.00020;

/// Mean linear relative luminance below which a picker patch is rejected as near-black.
const NEAR_BLACK_LUMINANCE: f64 = 0.02;

/// Neutral-picker solver: convergence tolerance, as a CIE 1960 `(u, v)` distance.
const SOLVER_TOLERANCE: f64 = 1e-6;

/// Neutral-picker solver: iteration bound.
const SOLVER_MAX_ITERATIONS: u32 = 64;

/// Neutral-picker solver: the finite-difference step the Jacobian uses, in parameter units.
const SOLVER_JACOBIAN_STEP: f64 = 1e-3;

/// The inclusive range Temperature and Tint accept, which is also the range their descriptors
/// declare. A solved correction outside it is reported, never clamped.
pub(super) const PARAMETER_RANGE: f64 = 100.0;

// ---------------------------------------------------------------------------------------------
// Linear algebra.
// ---------------------------------------------------------------------------------------------

fn mat3_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat3_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, slot) in row.iter_mut().enumerate() {
            *slot = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

/// The exact algebraic inverse of a 3x3 matrix via the adjugate and determinant, in `f64`.
///
/// The design requires this to be computed rather than replaced with separately published "inverse"
/// constants: inverting the very matrix the code already uses makes the round trip self-cancelling
/// to double precision (about `1e-15`), whichever way the forward matrix was rounded when it was
/// published.
fn mat3_inverse(m: &Mat3) -> Mat3 {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let cof_a = e * i - f * h;
    let cof_b = -(d * i - f * g);
    let cof_c = d * h - e * g;
    let cof_d = -(b * i - c * h);
    let cof_e = a * i - c * g;
    let cof_f = -(a * h - b * g);
    let cof_g = b * f - c * e;
    let cof_h = -(a * f - c * d);
    let cof_i = a * e - b * d;
    let det = a * cof_a + b * cof_b + c * cof_c;
    [
        [cof_a / det, cof_d / det, cof_g / det],
        [cof_b / det, cof_e / det, cof_h / det],
        [cof_c / det, cof_f / det, cof_i / det],
    ]
}

// ---------------------------------------------------------------------------------------------
// The Planckian locus, CIE 1960 (u, v), and the target chromaticity.
// ---------------------------------------------------------------------------------------------

/// The Planckian-locus chromaticity approximation (Kim et al. 2002), valid 1667 K to 25000 K.
///
/// This is a *model of a blackbody radiator*, used only to obtain a smooth, well-known warm/cool
/// direction in chromaticity space. It is not a claim that a JPEG's illuminant is a blackbody, and
/// it reproduces no camera's or editor's own Kelvin value.
fn planckian_locus_xy(kelvin: f64) -> (f64, f64) {
    let x = if kelvin <= 4000.0 {
        -0.2661239e9 / kelvin.powi(3) - 0.2343589e6 / kelvin.powi(2)
            + 0.8776956e3 / kelvin
            + 0.179910
    } else {
        -3.0258469e9 / kelvin.powi(3)
            + 2.1070379e6 / kelvin.powi(2)
            + 0.2226347e3 / kelvin
            + 0.240390
    };
    let y = if kelvin <= 2222.0 {
        -1.1063814 * x.powi(3) - 1.34811020 * x.powi(2) + 2.18555832 * x - 0.20219683
    } else if kelvin <= 4000.0 {
        -0.9549476 * x.powi(3) - 1.37418593 * x.powi(2) + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x.powi(3) - 5.87338670 * x.powi(2) + 3.75112997 * x - 0.37001483
    };
    (x, y)
}

/// CIE 1931 `(x, y)` to CIE 1960 `(u, v)`.
fn xy_to_uv(x: f64, y: f64) -> (f64, f64) {
    let d = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / d, 6.0 * y / d)
}

/// CIE 1960 `(u, v)` back to CIE 1931 `(x, y)`, the exact inverse of [`xy_to_uv`].
fn uv_to_xy(u: f64, v: f64) -> (f64, f64) {
    let d = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / d, 2.0 * v / d)
}

fn locus_uv(kelvin: f64) -> (f64, f64) {
    let (x, y) = planckian_locus_xy(kelvin);
    xy_to_uv(x, y)
}

/// The locus's unit tangent at `kelvin`, by a 1 K forward difference.
fn tangent_unit(kelvin: f64) -> (f64, f64) {
    let (u0, v0) = locus_uv(kelvin);
    let (u1, v1) = locus_uv(kelvin + 1.0);
    let (du, dv) = (u1 - u0, v1 - v0);
    let n = du.hypot(dv);
    (du / n, dv / n)
}

/// The chromaticity Temperature and Tint select, in CIE 1960 `(u, v)`.
///
/// At `(0, 0)` both offsets are algebraically zero — `locus_uv(T) - locus_uv(T)` and
/// `tint * K_TINT` — so the result is the D65 anchor exactly, independent of any rounding in the
/// locus polynomial.
fn target_uv(temperature: f64, tint: f64) -> (f64, f64) {
    let mired = 1.0e6 / D65_NOMINAL_CCT - temperature * K_T;
    let kelvin = 1.0e6 / mired;
    let (u_p0, v_p0) = locus_uv(D65_NOMINAL_CCT);
    let (u_p, v_p) = locus_uv(kelvin);
    let (tu, tv) = tangent_unit(kelvin);
    // The locked sign: this -90 degree rotation of the locus tangent is the one that makes positive
    // Tint raise R and B while lowering G, which is the magenta convention the design requires.
    let (pu, pv) = (tv, -tu);
    let (u_d65, v_d65) = xy_to_uv(D65_X, D65_Y);
    (
        u_d65 + (u_p - u_p0) + tint * K_TINT * pu,
        v_d65 + (v_p - v_p0) + tint * K_TINT * pv,
    )
}

/// The Bradford cone responses of a chromaticity at unit luminance. A gain ratio is
/// luminance-independent, so the `Y = 1` normalization is the whole of it.
fn lms_of_uv(u: f64, v: f64) -> [f64; 3] {
    let (x, y) = uv_to_xy(u, v);
    mat3_vec(&XYZ_TO_LMS, [x / y, 1.0, (1.0 - x - y) / y])
}

fn d65_lms() -> [f64; 3] {
    let (u, v) = xy_to_uv(D65_X, D65_Y);
    lms_of_uv(u, v)
}

/// The three Bradford gains Temperature and Tint select: `LMS(D65) / LMS(target)`. Applying them to
/// a colour whose own chromaticity *is* the target maps it onto D65 — the property the neutral
/// picker inverts, which is why the solver and the transform share one function instead of being
/// two independently tuned halves.
fn gains(temperature: f64, tint: f64) -> [f64; 3] {
    if temperature == 0.0 && tint == 0.0 {
        return [1.0, 1.0, 1.0];
    }
    let (u, v) = target_uv(temperature, tint);
    let target = lms_of_uv(u, v);
    let d65 = d65_lms();
    [d65[0] / target[0], d65[1] / target[1], d65[2] / target[2]]
}

// ---------------------------------------------------------------------------------------------
// The pointwise unit.
// ---------------------------------------------------------------------------------------------

/// The relative white-balance correction of one linear-sRGB (D65) pixel.
///
/// The whole `sRGB -> XYZ -> Bradford LMS -> gain -> back` sandwich is one linear map, so the
/// constructor folds it into a single 3x3 in `f64` and a pixel costs one 3x3 multiply in `f32`.
/// Nothing is clamped: negative and above-one channels are preserved for the units after this one,
/// exactly as the pointwise contract requires, and the host clamps once at the output boundary.
#[derive(Debug)]
pub(super) struct WhiteBalance {
    /// The stored parameters, kept for [`PointwiseColor::describe`] and the finiteness check.
    temperature: f64,
    tint: f64,
    matrix: [[f32; 3]; 3],
}

impl WhiteBalance {
    pub(super) fn new(temperature: f64, tint: f64) -> Self {
        Self {
            temperature,
            tint,
            matrix: Self::linear_matrix(temperature, tint),
        }
    }

    /// The composite linear-sRGB matrix, computed once in `f64` and stored in `f32`.
    ///
    /// `(0, 0)` is the exact identity, not merely the identity the general formula proves to about
    /// `1e-15`: a neutral Basic layer's white-balance unit must be bit-identical to no unit at all.
    fn linear_matrix(temperature: f64, tint: f64) -> [[f32; 3]; 3] {
        if temperature == 0.0 && tint == 0.0 {
            return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        }
        let g = gains(temperature, tint);
        let diagonal: Mat3 = [[g[0], 0.0, 0.0], [0.0, g[1], 0.0], [0.0, 0.0, g[2]]];
        let to_lms = mat3_mul(&XYZ_TO_LMS, &RGB_TO_XYZ);
        let from_lms = mat3_mul(&mat3_inverse(&RGB_TO_XYZ), &mat3_inverse(&XYZ_TO_LMS));
        let composite = mat3_mul(&from_lms, &mat3_mul(&diagonal, &to_lms));
        let mut matrix = [[0.0f32; 3]; 3];
        for (row, source) in matrix.iter_mut().zip(composite) {
            for (slot, value) in row.iter_mut().zip(source) {
                *slot = value as f32;
            }
        }
        matrix
    }
}

impl PointwiseColor for WhiteBalance {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        let m = &self.matrix;
        for pixel in rgb {
            let [r, g, b] = *pixel;
            *pixel = [
                m[0][0] * r + m[0][1] * g + m[0][2] * b,
                m[1][0] * r + m[1][1] * g + m[1][2] * b,
                m[2][0] * r + m[2][1] * g + m[2][2] * b,
            ];
        }
    }

    /// The gains are finite and strictly positive across the whole representable range, so a
    /// non-finite coefficient can only come from a non-finite stored value. It is refused at
    /// compilation, before any frame is touched.
    fn is_finite(&self) -> bool {
        self.temperature.is_finite()
            && self.tint.is_finite()
            && self
                .matrix
                .iter()
                .all(|row| row.iter().all(|value| value.is_finite()))
    }

    /// The stored values, exactly. The host compares compiled operations by this string, so two
    /// units that describe themselves identically must process identically: the matrix is a pure
    /// function of the two values.
    fn describe(&self) -> String {
        format!("white-balance({:+}, {:+})", self.temperature, self.tint)
    }
}

// ---------------------------------------------------------------------------------------------
// The neutral picker.
// ---------------------------------------------------------------------------------------------

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
    /// The solver did not reach [`SOLVER_TOLERANCE`] within [`SOLVER_MAX_ITERATIONS`]: the patch is
    /// far enough from any representable correction that the iteration left the locus's
    /// well-behaved region.
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
/// then non-finiteness; then near-black on the mean's relative luminance, which also protects the
/// solver's `XYZ -> xy` division from a near-zero denominator.
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
            *slot += srgb_to_linear_f64(*code);
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

/// Solve for the `(temperature, tint)` pair whose [`target_uv`] is this patch's own chromaticity:
/// the pair that, applied by [`WhiteBalance`], maps the patch to neutral.
///
/// Bounded 2-D Newton's method seeded at `(0, 0)` with a finite-difference Jacobian. The converged
/// pair is rounded to the nearest whole step, and a rounded value outside the representable range is
/// reported as [`Reason::OutOfRange`] rather than clamped.
pub(super) fn neutral_settings(mean_linear_rgb: [f64; 3]) -> Result<(i64, i64), Reason> {
    if mean_linear_rgb.iter().any(|value| !value.is_finite()) {
        return Err(Reason::NonFinite);
    }
    let xyz = mat3_vec(&RGB_TO_XYZ, mean_linear_rgb);
    let sum = xyz[0] + xyz[1] + xyz[2];
    if !sum.is_finite() || sum <= 0.0 {
        return Err(Reason::NonFinite);
    }
    let (patch_u, patch_v) = xy_to_uv(xyz[0] / sum, xyz[1] / sum);
    if !patch_u.is_finite() || !patch_v.is_finite() {
        return Err(Reason::NonFinite);
    }

    let (mut temperature, mut tint) = (0.0_f64, 0.0_f64);
    let h = SOLVER_JACOBIAN_STEP;
    let mut converged = false;
    for _ in 0..SOLVER_MAX_ITERATIONS {
        let (u, v) = target_uv(temperature, tint);
        let (ru, rv) = (u - patch_u, v - patch_v);
        // A non-finite residual ends the search rather than spending the remaining budget on it.
        if !ru.is_finite() || !rv.is_finite() {
            return Err(Reason::DidNotConverge);
        }
        if ru.hypot(rv) < SOLVER_TOLERANCE {
            converged = true;
            break;
        }
        let (ut, vt) = target_uv(temperature + h, tint);
        let (utint, vtint) = target_uv(temperature, tint + h);
        let j = [
            [(ut - u) / h, (utint - u) / h],
            [(vt - v) / h, (vtint - v) / h],
        ];
        let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
        if !det.is_finite() || det.abs() < 1e-15 {
            return Err(Reason::DidNotConverge);
        }
        let step_t = (j[1][1] * ru - j[0][1] * rv) / det;
        let step_tint = (-j[1][0] * ru + j[0][0] * rv) / det;
        temperature -= step_t;
        tint -= step_tint;
        if !temperature.is_finite() || !tint.is_finite() {
            return Err(Reason::DidNotConverge);
        }
    }
    if !converged {
        return Err(Reason::DidNotConverge);
    }
    let temperature = temperature.round();
    let tint = tint.round();
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

/// Reject or average a sampled patch, then solve it. The module's query keeps the two steps apart
/// so it can report the mean it averaged; this is the same composition, for the fixture tests.
#[cfg(test)]
fn solve_from_patch(pixels: &[[u8; 3]]) -> Result<(i64, i64), Reason> {
    neutral_settings(average_patch(pixels)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::{fs, path::PathBuf};

    /// The design freezes no per-algorithm tolerance for white balance, so the pointwise contract's
    /// default applies: `1e-6 + 1e-6 * |reference|`. Production folds the sandwich into one `f32`
    /// matrix; the reference keeps every step in `f64`.
    fn tolerance(reference: f64) -> f64 {
        1e-6 + 1e-6 * reference.abs()
    }

    fn fixture() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/basic/white-balance-cases.json"
        ))
    }

    #[derive(Deserialize)]
    struct Cases {
        transform_cases: Vec<TransformCase>,
        solver_cases: Vec<SolverCase>,
    }

    #[derive(Deserialize)]
    struct TransformCase {
        input_rgb_u8: [u8; 3],
        temperature: f64,
        tint: f64,
        expected_linear_f64: [f64; 3],
    }

    #[derive(Deserialize)]
    struct SolverCase {
        description: String,
        patch_u8: Vec<[u8; 3]>,
        expected_solution: Option<[i64; 2]>,
        expected_rejection: Option<String>,
    }

    fn cases() -> Cases {
        serde_json::from_str(&fs::read_to_string(fixture()).expect("the committed fixture"))
            .expect("white-balance cases")
    }

    /// One pixel through the production unit, as a colour run of one unit would apply it.
    fn applied(temperature: f64, tint: f64, rgb: [f64; 3]) -> [f64; 3] {
        let mut row = [[rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]];
        WhiteBalance::new(temperature, tint).apply_row(0, 0, &mut row);
        [
            f64::from(row[0][0]),
            f64::from(row[0][1]),
            f64::from(row[0][2]),
        ]
    }

    /// Every one of the 45 committed transform cases: 5 input colours at 9 parameter pairs,
    /// including out-of-gamut results the unit must preserve rather than clamp.
    #[test]
    fn every_transform_case_matches_the_independent_f64_reference() {
        let cases = cases();
        assert_eq!(cases.transform_cases.len(), 45, "the committed corpus");
        for case in &cases.transform_cases {
            let input = case.input_rgb_u8.map(srgb_to_linear_f64);
            let actual = applied(case.temperature, case.tint, input);
            for (channel, (actual, expected)) in
                actual.iter().zip(case.expected_linear_f64).enumerate()
            {
                assert!(
                    (actual - expected).abs() <= tolerance(expected),
                    "{:?} at ({}, {}) channel {channel}: {actual} against {expected}",
                    case.input_rgb_u8,
                    case.temperature,
                    case.tint,
                );
            }
        }
    }

    /// `(0, 0)` is the exact identity in `f32`, not merely the identity to within a tolerance: the
    /// matrix is the literal identity, so a neutral white-balance unit changes no bit at all.
    #[test]
    fn zero_zero_is_the_exact_identity_matrix() {
        let unit = WhiteBalance::new(0.0, 0.0);
        assert_eq!(
            unit.matrix,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
        let mut row = [
            [0.0, 0.25, 1.0],
            [-0.125, 2.5, 0.051_269_46],
            [0.215_860_5, 1.0, 0.0],
        ];
        let input = row;
        unit.apply_row(0, 0, &mut row);
        assert_eq!(row, input, "a neutral unit is bit-identical to no unit");
        assert_eq!(unit.describe(), "white-balance(+0, +0)");
    }

    /// The general formula, not the special case, already reproduces the anchor to well inside the
    /// design's `1e-12` requirement — which is what makes the special case an optimization and a
    /// bit-identity guarantee rather than a patch over an inaccurate transform. Checked here on the
    /// `f64` composite, since `f32` storage cannot carry `1e-12`.
    #[test]
    fn the_general_formula_reaches_the_anchor_within_1e12() {
        let g = gains(0.0, 0.0);
        assert_eq!(g, [1.0, 1.0, 1.0]);
        // Recompute the gains through the general path by nudging the special case aside: the
        // target at an infinitesimal temperature is the anchor to within the step taken.
        let (u, v) = target_uv(0.0, 0.0);
        let (anchor_u, anchor_v) = xy_to_uv(D65_X, D65_Y);
        assert_eq!((u, v), (anchor_u, anchor_v), "zero offsets cancel exactly");
        let target = lms_of_uv(u, v);
        let d65 = d65_lms();
        for (channel, (d65, target)) in d65.iter().zip(target).enumerate() {
            assert!((d65 / target - 1.0).abs() < 1e-12, "channel {channel}");
        }
        // The matrix sandwich composes to identity to double precision using the computed inverses.
        let round_trip = mat3_mul(&mat3_inverse(&RGB_TO_XYZ), &RGB_TO_XYZ);
        for (r, row) in round_trip.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!((value - expected).abs() < 1e-12, "({r}, {c}) = {value}");
            }
        }
    }

    /// Positive Temperature warms and positive Tint is magenta, at the ends of both axes.
    #[test]
    fn the_sign_conventions_are_warm_cool_and_green_magenta() {
        let mid = [0.5, 0.5, 0.5];
        let neutral = applied(0.0, 0.0, mid);
        for temperature in [25.0, 50.0, 100.0] {
            let warm = applied(temperature, 0.0, mid);
            let cool = applied(-temperature, 0.0, mid);
            assert!(warm[0] > neutral[0] && warm[2] < neutral[2], "{warm:?}");
            assert!(cool[0] < neutral[0] && cool[2] > neutral[2], "{cool:?}");
        }
        for tint in [25.0, 50.0, 100.0] {
            let magenta = applied(0.0, tint, mid);
            let green = applied(0.0, -tint, mid);
            assert!(
                magenta[0] > neutral[0] && magenta[2] > neutral[2] && magenta[1] < neutral[1],
                "{magenta:?}"
            );
            assert!(
                green[0] < neutral[0] && green[2] < neutral[2] && green[1] > neutral[1],
                "{green:?}"
            );
        }
    }

    /// Every gain stays finite and strictly positive over the whole domain, so a non-finite output
    /// can only come from a non-finite input, and out-of-gamut input is preserved rather than
    /// clamped inside the unit.
    #[test]
    fn coefficients_are_finite_over_the_range_and_nothing_is_clamped() {
        let mut temperature = -PARAMETER_RANGE;
        while temperature <= PARAMETER_RANGE {
            let mut tint = -PARAMETER_RANGE;
            while tint <= PARAMETER_RANGE {
                let unit = WhiteBalance::new(temperature, tint);
                assert!(unit.is_finite(), "({temperature}, {tint})");
                for gain in gains(temperature, tint) {
                    assert!(gain.is_finite() && gain > 0.0, "({temperature}, {tint})");
                }
                tint += 5.0;
            }
            temperature += 5.0;
        }
        // A saturated near-blue patch cooled hard pushes red negative and keeps it finite.
        let cooled = applied(-100.0, 0.0, [0.05, 0.1, 0.8]);
        assert!(cooled[0] < 0.0, "{cooled:?}");
        assert!(cooled.iter().all(|value| value.is_finite()));
        let above_one = applied(100.0, 100.0, [1.3, 1.1, 1.2]);
        assert!(above_one.iter().any(|value| *value > 1.0), "{above_one:?}");
        assert!(!WhiteBalance::new(f64::NAN, 0.0).is_finite());
        assert!(!WhiteBalance::new(0.0, f64::INFINITY).is_finite());
    }

    /// Every one of the 125 committed solver cases: the 121-pair round-trip grid, the near-black and
    /// clipped refusals, the out-of-range correction that is reported rather than clamped, and the
    /// edge-clipped 9-of-25 corner patch that still solves.
    #[test]
    fn every_solver_case_recovers_its_parameters_or_its_rejection_exactly() {
        let cases = cases();
        assert_eq!(cases.solver_cases.len(), 125, "the committed corpus");
        let mut rejections = 0;
        let mut clipped_patch = 0;
        for case in &cases.solver_cases {
            let solved = solve_from_patch(&case.patch_u8);
            match (&case.expected_solution, &case.expected_rejection) {
                (Some([temperature, tint]), None) => {
                    assert_eq!(
                        solved.expect(&case.description),
                        (*temperature, *tint),
                        "{}",
                        case.description
                    );
                    if case.patch_u8.len() < 25 {
                        clipped_patch += 1;
                    }
                }
                (None, Some(expected)) => {
                    rejections += 1;
                    let reason = solved.expect_err(&case.description);
                    // The reference names its reasons in Rust debug form; the production reason
                    // must be the same one, matched by name rather than by message prose.
                    let matched = match reason {
                        Reason::Clipped => expected == "Clipped",
                        Reason::NearBlack => expected == "NearBlack",
                        Reason::NonFinite => expected == "NonFinite",
                        Reason::DidNotConverge => expected == "DidNotConverge",
                        Reason::OutOfRange { temperature, value } => {
                            expected
                                == &format!(
                                    "OutOfRange {{ temperature: {temperature}, value: {value:?} }}"
                                )
                        }
                    };
                    assert!(
                        matched,
                        "{}: {reason:?} against {expected}",
                        case.description
                    );
                }
                other => panic!("{}: malformed case {other:?}", case.description),
            }
        }
        assert_eq!(rejections, 3, "near-black, clipped and out-of-range");
        assert_eq!(clipped_patch, 1, "the edge-clipped corner patch");
    }

    /// The rounded settings a patch solves to, applied back to that patch, leave a residual channel
    /// spread within the `0.005` the design accepts.
    #[test]
    fn solved_settings_reapplied_neutralize_their_own_patch() {
        for case in cases().solver_cases {
            let Some([temperature, tint]) = case.expected_solution else {
                continue;
            };
            let mean = average_patch(&case.patch_u8).expect(&case.description);
            let corrected = applied(temperature as f64, tint as f64, mean);
            let spread = corrected.iter().cloned().fold(f64::MIN, f64::max)
                - corrected.iter().cloned().fold(f64::MAX, f64::min);
            assert!(
                spread <= 0.005,
                "{}: residual spread {spread} of {corrected:?}",
                case.description
            );
        }
    }

    /// The rejection order and the messages a client branches on.
    #[test]
    fn rejections_name_themselves_in_the_frozen_order() {
        // Clipping is checked on the raw codes first, so a clipped *and* near-black patch is
        // reported as clipped, not as near-black.
        assert_eq!(average_patch(&[[0, 0, 0]]).unwrap_err(), Reason::Clipped);
        assert_eq!(
            average_patch(&[[255, 200, 200]]).unwrap_err(),
            Reason::Clipped
        );
        assert_eq!(average_patch(&[[8, 8, 8]]).unwrap_err(), Reason::NearBlack);
        assert_eq!(average_patch(&[]).unwrap_err(), Reason::NonFinite);
        assert_eq!(
            neutral_settings([f64::NAN, 0.5, 0.5]).unwrap_err(),
            Reason::NonFinite
        );
        assert_eq!(
            neutral_settings([0.0, 0.0, 0.0]).unwrap_err(),
            Reason::NonFinite,
            "a black patch has no chromaticity to solve"
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

    /// A patch a well-behaved photograph cannot produce: strongly saturated, far from any
    /// representable correction. It is refused, never silently accepted at a clamped value.
    #[test]
    fn a_strongly_saturated_patch_is_refused_rather_than_clamped() {
        let refused = neutral_settings([0.8, 0.05, 0.05]).expect_err("no neutral correction");
        assert!(
            matches!(refused, Reason::OutOfRange { .. } | Reason::DidNotConverge),
            "{refused:?}"
        );
    }

    /// Two different stored values never describe themselves the same way at the declared step.
    #[test]
    fn the_description_carries_both_stored_values_and_their_signs() {
        assert_eq!(
            WhiteBalance::new(20.0, -5.0).describe(),
            "white-balance(+20, -5)"
        );
        assert_ne!(
            WhiteBalance::new(20.0, 0.0).describe(),
            WhiteBalance::new(21.0, 0.0).describe()
        );
        assert_ne!(
            WhiteBalance::new(0.0, -5.0).describe(),
            WhiteBalance::new(0.0, -6.0).describe()
        );
    }
}
