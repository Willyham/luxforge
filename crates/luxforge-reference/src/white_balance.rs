//! Independent f64 reference for the frozen relative white-balance transform and neutral-picker
//! solver described in `docs/design/basic-white-balance.md`. That document is the source of
//! truth for every constant and formula here; this file exists to prove the document's claims
//! with exact-buffer tests and to produce `fixtures/basic/white-balance-cases.json`.
//!
//! No production code depends on this file. It is deliberately independent: it does not import
//! anything from `luxforge_core::render` or reuse its sRGB table, so a match between the two is
//! evidence, not a tautology.

use super::srgb_decode;

// ---------------------------------------------------------------------------------------------
// Linear algebra: plain 3x3 matrices, no crate dependency.
// ---------------------------------------------------------------------------------------------

pub type Mat3 = [[f64; 3]; 3];

pub fn mat3_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The exact algebraic inverse of a 3x3 matrix via the adjugate/determinant, computed in f64.
///
/// This is called on [`RGB_TO_XYZ`] and [`XYZ_TO_LMS`] at point of use rather than replaced with
/// separately-published "inverse" constants: two independently-rounded 7-digit matrices compose
/// to identity only to about `1e-7`, which fails the design's `1e-12` requirement at temperature
/// 0 / tint 0. Computing the inverse of the exact same forward matrix the code uses composes to
/// identity to double-precision (about `1e-15`) regardless of how many digits the forward matrix
/// was published with. See `identity_at_zero_zero_is_exact_to_1e12` below.
pub fn mat3_inverse(m: &Mat3) -> Mat3 {
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

fn mat3_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Colour constants, frozen by the design document. Digits as stated there.
// ---------------------------------------------------------------------------------------------

/// Linear sRGB (D65) to CIE XYZ, IEC 61966-2-1 primaries and white point.
pub const RGB_TO_XYZ: Mat3 = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.0721750],
    [0.0193339, 0.1191920, 0.9503041],
];

/// CIE XYZ to Bradford cone-response (LMS-like) space, the 1985 Bradford matrix.
pub const XYZ_TO_LMS: Mat3 = [
    [0.8951000, 0.2664000, -0.1614000],
    [-0.7502000, 1.7135000, 0.0367000],
    [0.0389000, -0.0685000, 1.0296000],
];

/// The sRGB reference white, IEC 61966-2-1 (the standard "D65" chromaticity used to build
/// [`RGB_TO_XYZ`]). This is the anchor the transform maps back onto exactly at temperature 0,
/// tint 0 — *not* the nominal-CCT Planckian point below, which is a different, nearby point used
/// only to read off a direction.
pub const D65_X: f64 = 0.3127;
pub const D65_Y: f64 = 0.3290;

/// The nominal correlated colour temperature of the sRGB/D65 white point on the Planckian locus.
/// Used only as the zero of the mired shift and the point at which the locus tangent is read; the
/// transform never assumes this point's `(x, y)` equals [`D65_X`]/[`D65_Y`] (it does not: D65 is
/// a daylight illuminant a small distance off the blackbody locus, not a blackbody itself).
pub const D65_NOMINAL_CCT: f64 = 6504.0;

/// Mired shift per unit of the Temperature parameter. `mired(t) = 1e6/D65_NOMINAL_CCT - t * K_T`.
/// Chosen so `t = +100` reaches a nominal CCT of ~20000 K; see the design doc for the resulting
/// (necessarily asymmetric — mired and Kelvin are reciprocal) value at `t = -100`.
pub const K_T: f64 = 1.0375;

/// Perpendicular offset in CIE 1960 (u, v) per unit of the Tint parameter.
pub const K_TINT: f64 = 0.00020;

/// Mean linear-light relative luminance below which a picker patch is rejected as near-black.
pub const NEAR_BLACK_LUMINANCE: f64 = 0.02;

/// Neutral-picker solver: convergence tolerance, in CIE 1960 (u, v) distance.
pub const SOLVER_TOLERANCE: f64 = 1e-6;

/// Neutral-picker solver: iteration bound.
pub const SOLVER_MAX_ITERATIONS: u32 = 64;

/// Neutral-picker solver: finite-difference step used to build the Jacobian, in parameter units.
const SOLVER_JACOBIAN_STEP: f64 = 1e-3;

/// Parameter range shared by Temperature and Tint.
pub const PARAMETER_RANGE: f64 = 100.0;

fn rgb_to_xyz_inv() -> Mat3 {
    mat3_inverse(&RGB_TO_XYZ)
}
fn xyz_to_lms_inv() -> Mat3 {
    mat3_inverse(&XYZ_TO_LMS)
}

// ---------------------------------------------------------------------------------------------
// Planckian locus (Kim et al. 2002 cubic approximation, valid 1667 K - 25000 K) and CIE 1960 uv.
// ---------------------------------------------------------------------------------------------

/// Planckian-locus chromaticity approximation (Kim, Kim, Tam-koto, Konig, Kim 2002), reproduced
/// from its widely published coefficients. This is a *model of a blackbody radiator*: it is used
/// here only to obtain a smooth, well-known warm/cool direction in chromaticity space, not as a
/// claim that JPEG illuminants are blackbodies or that this reproduces any camera or Lightroom
/// Kelvin value.
pub fn planckian_locus_xy(kelvin: f64) -> (f64, f64) {
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
pub fn xy_to_uv(x: f64, y: f64) -> (f64, f64) {
    let d = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / d, 6.0 * y / d)
}

/// CIE 1960 `(u, v)` to CIE 1931 `(x, y)`, the exact inverse of [`xy_to_uv`].
pub fn uv_to_xy(u: f64, v: f64) -> (f64, f64) {
    let d = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / d, 2.0 * v / d)
}

fn locus_uv(kelvin: f64) -> (f64, f64) {
    let (x, y) = planckian_locus_xy(kelvin);
    xy_to_uv(x, y)
}

/// Unit tangent to the Planckian locus at `kelvin`, in CIE 1960 (u, v), by a 1 K forward
/// difference. Smooth and well away from a degenerate step anywhere in the range this transform
/// uses (the locus has no cusp or inflection collapsing the tangent to zero length there).
fn tangent_unit(kelvin: f64) -> (f64, f64) {
    let (u0, v0) = locus_uv(kelvin);
    let (u1, v1) = locus_uv(kelvin + 1.0);
    let (du, dv) = (u1 - u0, v1 - v0);
    let n = du.hypot(dv);
    (du / n, dv / n)
}

// ---------------------------------------------------------------------------------------------
// Target chromaticity, gains, and the transform itself.
// ---------------------------------------------------------------------------------------------

/// The target chromaticity in CIE 1960 (u, v) that Temperature/Tint select, plus the nominal
/// Planckian CCT used to compute it (informational only — the *anchor* is [`D65_X`]/[`D65_Y`],
/// reached exactly when `temperature == 0.0 && tint == 0.0` because both offsets below are then
/// algebraically zero: `locus_uv(T) - locus_uv(T)` and `tint * K_TINT`).
pub fn target_uv(temperature: f64, tint: f64) -> (f64, f64, f64) {
    let mired = 1.0e6 / D65_NOMINAL_CCT - temperature * K_T;
    let kelvin = 1.0e6 / mired;
    let (u_p0, v_p0) = locus_uv(D65_NOMINAL_CCT);
    let (u_p, v_p) = locus_uv(kelvin);
    let (du, dv) = (u_p - u_p0, v_p - v_p0);
    let (tu, tv) = tangent_unit(kelvin);
    // Locked sign: this perpendicular direction (a -90 degree rotation of the locus tangent)
    // makes positive tint raise R and B while lowering G — magenta — proved by
    // `positive_tint_is_magenta` below.
    let (pu, pv) = (tv, -tu);
    let (u_d65, v_d65) = xy_to_uv(D65_X, D65_Y);
    let u = u_d65 + du + tint * K_TINT * pu;
    let v = v_d65 + dv + tint * K_TINT * pv;
    (u, v, kelvin)
}

fn lms_of_uv(u: f64, v: f64) -> [f64; 3] {
    let (x, y) = uv_to_xy(u, v);
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    mat3_vec(&XYZ_TO_LMS, xyz)
}

fn d65_lms() -> [f64; 3] {
    let (u, v) = xy_to_uv(D65_X, D65_Y);
    lms_of_uv(u, v)
}

/// The three Bradford-space gains Temperature/Tint select: `LMS(D65) / LMS(target)`. Applying
/// this diagonal scale to a colour whose own chromaticity is the target chromaticity maps it back
/// onto D65 (neutral) — which is exactly the property the neutral-picker solver inverts.
pub fn gains(temperature: f64, tint: f64) -> [f64; 3] {
    if temperature == 0.0 && tint == 0.0 {
        return [1.0, 1.0, 1.0];
    }
    let (u, v, _) = target_uv(temperature, tint);
    let lms_target = lms_of_uv(u, v);
    let lms_d65 = d65_lms();
    [
        lms_d65[0] / lms_target[0],
        lms_d65[1] / lms_target[1],
        lms_d65[2] / lms_target[2],
    ]
}

/// Apply the white-balance unit to one linear-sRGB (D65) triple. `temperature` and `tint` are
/// expected in `-100.0..=100.0` (the transform is mathematically defined outside that range too;
/// the neutral-picker solver uses that to detect an out-of-range correction, see
/// [`solve_neutral`]).
///
/// Out-of-gamut input (negative or above 1.0) is preserved: nothing here clamps. A non-finite
/// result is only possible if the input already contained one, since `gains` is finite and
/// strictly positive for every representable temperature/tint (proved by
/// `gains_finite_and_positive_over_full_range` below).
pub fn apply(temperature: f64, tint: f64, rgb: [f64; 3]) -> [f64; 3] {
    // Explicit special case, not just a consequence of the general formula: required by the
    // design so a neutral Basic layer's white-balance unit is bit-identical to no unit at all,
    // and so it costs nothing when the layer is neutral.
    if temperature == 0.0 && tint == 0.0 {
        return rgb;
    }
    let g = gains(temperature, tint);
    let xyz = mat3_vec(&RGB_TO_XYZ, rgb);
    let lms = mat3_vec(&XYZ_TO_LMS, xyz);
    let lms2 = [lms[0] * g[0], lms[1] * g[1], lms[2] * g[2]];
    let xyz2 = mat3_vec(&xyz_to_lms_inv(), lms2);
    mat3_vec(&rgb_to_xyz_inv(), xyz2)
}

// ---------------------------------------------------------------------------------------------
// Neutral picker: patch gathering, averaging, rejection, and the solver.
// ---------------------------------------------------------------------------------------------

/// Why a sampled patch was rejected, with no numeric parameters returned: the design requires an
/// explicit reason rather than a guess or a silent clamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RejectReason {
    /// The patch's mean linear luminance is below [`NEAR_BLACK_LUMINANCE`].
    NearBlack,
    /// At least one sampled pixel has a channel at code 0 or 255.
    Clipped,
    /// A sampled or computed value was not finite.
    NonFinite,
    /// The solver converged, but the required correction on this axis exceeds the representable
    /// `-100.0..=100.0` range. Carries the rounded value that would have been out of range.
    OutOfRange { temperature: bool, value: f64 },
    /// The solver did not reach [`SOLVER_TOLERANCE`] within [`SOLVER_MAX_ITERATIONS`].
    DidNotConverge,
}

/// Gather up to 5x5 = 25 pixels centred on `(center_x, center_y)`, dropping any position that
/// falls outside `[0, width) x [0, height)` — "clipped at the image edges": a corner patch may
/// return as few as one pixel (the centre itself), never zero for an in-bounds centre.
pub fn gather_patch(
    width: u32,
    height: u32,
    center_x: u32,
    center_y: u32,
    mut fetch: impl FnMut(u32, u32) -> [u8; 3],
) -> Vec<[u8; 3]> {
    let mut out = Vec::with_capacity(25);
    let (cx, cy) = (center_x as i64, center_y as i64);
    for dy in -2..=2i64 {
        for dx in -2..=2i64 {
            let (x, y) = (cx + dx, cy + dy);
            if x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height {
                out.push(fetch(x as u32, y as u32));
            }
        }
    }
    out
}

fn relative_luminance(rgb: [f64; 3]) -> f64 {
    RGB_TO_XYZ[1][0] * rgb[0] + RGB_TO_XYZ[1][1] * rgb[1] + RGB_TO_XYZ[1][2] * rgb[2]
}

/// Decode and average up to 25 sampled 8-bit pixels in linear sRGB, or reject the patch. Checks
/// clipping on the raw sampled codes (before decoding/averaging) and near-black on the mean.
pub fn average_patch(pixels: &[[u8; 3]]) -> Result<[f64; 3], RejectReason> {
    if pixels.iter().any(|p| p.iter().any(|&c| c == 0 || c == 255)) {
        return Err(RejectReason::Clipped);
    }
    let n = pixels.len().max(1) as f64;
    let mut sum = [0.0; 3];
    for p in pixels {
        for c in 0..3 {
            sum[c] += srgb_decode(p[c]);
        }
    }
    let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
    if mean.iter().any(|v| !v.is_finite()) {
        return Err(RejectReason::NonFinite);
    }
    let y = relative_luminance(mean);
    if !y.is_finite() || y < NEAR_BLACK_LUMINANCE {
        return Err(RejectReason::NearBlack);
    }
    Ok(mean)
}

/// Solve for the `(temperature, tint)` pair whose [`target_uv`] equals the chromaticity of
/// `patch_linear` — the pair that, applied by [`apply`], maps this patch to neutral.
///
/// Bounded 2-D Newton's method from `(0, 0)`, finite-difference Jacobian, [`SOLVER_TOLERANCE`] on
/// the residual (u, v) distance, at most [`SOLVER_MAX_ITERATIONS`] iterations. A non-finite
/// residual (only reachable while chasing a patch far enough from any representable correction
/// that the iterate has left the locus's well-behaved region) ends the search immediately as
/// non-convergent rather than spending the remaining iteration budget on it.
///
/// The converged continuous pair is rounded to the nearest integer step (the parameters' step is
/// 1); if either rounded value falls outside `-100.0..=100.0` the correction is reported as
/// [`RejectReason::OutOfRange`] rather than clamped.
pub fn solve_neutral(patch_linear: [f64; 3]) -> Result<(i32, i32), RejectReason> {
    let xyz = mat3_vec(&RGB_TO_XYZ, patch_linear);
    let sum = xyz[0] + xyz[1] + xyz[2];
    if !sum.is_finite() || sum <= 0.0 {
        return Err(RejectReason::NonFinite);
    }
    let (x, y) = (xyz[0] / sum, xyz[1] / sum);
    let (patch_u, patch_v) = xy_to_uv(x, y);
    if !patch_u.is_finite() || !patch_v.is_finite() {
        return Err(RejectReason::NonFinite);
    }

    let (mut t, mut tint) = (0.0_f64, 0.0_f64);
    let h = SOLVER_JACOBIAN_STEP;
    let mut converged = false;
    for _ in 0..SOLVER_MAX_ITERATIONS {
        let (u, v, _) = target_uv(t, tint);
        let (ru, rv) = (u - patch_u, v - patch_v);
        if !ru.is_finite() || !rv.is_finite() {
            return Err(RejectReason::DidNotConverge);
        }
        if ru.hypot(rv) < SOLVER_TOLERANCE {
            converged = true;
            break;
        }
        let (ut, vt, _) = target_uv(t + h, tint);
        let (utint, vtint, _) = target_uv(t, tint + h);
        let j = [
            [(ut - u) / h, (utint - u) / h],
            [(vt - v) / h, (vtint - v) / h],
        ];
        let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
        if !det.is_finite() || det.abs() < 1e-15 {
            return Err(RejectReason::DidNotConverge);
        }
        let inv = [
            [j[1][1] / det, -j[0][1] / det],
            [-j[1][0] / det, j[0][0] / det],
        ];
        let dt = inv[0][0] * ru + inv[0][1] * rv;
        let dtint = inv[1][0] * ru + inv[1][1] * rv;
        t -= dt;
        tint -= dtint;
        if !t.is_finite() || !tint.is_finite() {
            return Err(RejectReason::DidNotConverge);
        }
    }
    if !converged {
        return Err(RejectReason::DidNotConverge);
    }

    let t_rounded = t.round();
    let tint_rounded = tint.round();
    if t_rounded.abs() > PARAMETER_RANGE {
        return Err(RejectReason::OutOfRange {
            temperature: true,
            value: t_rounded,
        });
    }
    if tint_rounded.abs() > PARAMETER_RANGE {
        return Err(RejectReason::OutOfRange {
            temperature: false,
            value: tint_rounded,
        });
    }
    Ok((t_rounded as i32, tint_rounded as i32))
}

/// Reject or average a sampled patch, then solve it. The one entry point the neutral picker uses.
pub fn solve_from_patch(pixels: &[[u8; 3]]) -> Result<(i32, i32), RejectReason> {
    let mean = average_patch(pixels)?;
    solve_neutral(mean)
}

#[allow(dead_code)]
fn identity_check_matrix() -> Mat3 {
    // Exposed for the outer test file to print/inspect if needed; composes RGB->XYZ->RGB.
    mat3_mul(&rgb_to_xyz_inv(), &RGB_TO_XYZ)
}
