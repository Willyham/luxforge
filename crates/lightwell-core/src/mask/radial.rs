//! The `radial` component kind: the frozen radial gradient of
//! `docs/design/mask-study.md#the-radial-gradient`, compiled against the content stage its mask's
//! layer receives.
//!
//! This file is the production transcription of that block and of the `radial_coverage` function of
//! the independent `f64` reference at `crates/lightwell-core/tests/reference/mask.rs`. The
//! per-pixel expression is written in the reference's form and the reference's order, so it is
//! bit-identical to it rather than merely within tolerance: `cu, cv, ca, sa, r0, span, hard` are
//! hoisted to compile time because they do not depend on the pixel — including the one `cos`/`sin`
//! pair, so the per-pixel path has no transcendental at all — and the two radii stay **divisors**
//! rather than becoming precomputed reciprocals, because `a / radius_x` and `a * (1 / radius_x)` are
//! not the same `f64`.
//!
//! **`feather = 0` is an explicit hard edge, not a limit.** `hard` is set from the computed span
//! being exactly zero, following the delivered vignette unit's `hard_step` discipline
//! (`crates/lightwell-core/src/modules/vignette/unit.rs`), so no division by a vanishing span is
//! ever evaluated: the divisor `span` is only reached on the branch the constructor has already
//! proved is non-zero. It absorbs the second route to the same edge as well — a feather so small
//! that `1 - feather / 100` rounds to exactly `1.0` is the same hard edge, reached by rounding
//! rather than by an equality against zero.
//!
//! **Inside is selected** (`docs/design/masking.md`, proposal P3, confirmed by the study on
//! figures). Lightroom's radial affects the outside until Invert is ticked; ours affects the inside,
//! and the component's own `invert` gives the other reading as an exact complement. That default is
//! what makes [`Compiled::support`] worth having: an inside-selected radial's coverage is exactly
//! zero outside its ellipse, so a masked run skips most of an ordinary frame, where an
//! outside-selected one would be non-zero everywhere and no span or tile could be skipped.
//!
//! The ellipse is defined in mask space, where one unit is the stage's height on **both** axes, so a
//! circle stays a circle in pixels at any aspect ratio and under any crop.
//!
//! Compiling uses the **stored position** spelling of mask space (`u = x · W/H`, `v = y`); the
//! per-pixel path receives the pixel-centre spelling from [`super::CompiledMask`]. The two agree to
//! `2.220e-16` and not bit for bit, so each is used only where the study froze it.
use super::{
    DISTANCE_MAX, DISTANCE_MIN, POSITION_MAX, POSITION_MIN,
    parameters::{angle, distance, percentage, position},
    smooth,
};
use crate::{
    Component, Error, ErrorKind, ParameterDescriptor,
    modules::{Region, Stage},
};
use serde::{Deserialize, Serialize};

/// The token a stored component of this kind carries.
pub(super) const KIND: &str = "radial";

/// The legal range of a stored `angle`, in degrees. The falloff is total on any finite angle — the
/// rotation is a `cos`/`sin` pair — so this is a payload rule and nothing more: it keeps one
/// orientation to one stored number rather than to a winding of them, so two payloads that draw the
/// same ellipse compare equal and a slider has ends.
pub const ANGLE_MIN: f64 = -180.0;
pub const ANGLE_MAX: f64 = 180.0;

/// The legal range of a stored `feather`, as a percentage of the radius. `0` is the hard edge and
/// `100` puts the start of the ramp at the centre.
pub const FEATHER_MIN: f64 = 0.0;
pub const FEATHER_MAX: f64 = 100.0;

/// A radial gradient's stored payload: the centre as a normalized position in content-stage
/// coordinates, the two radii in mask-space units, `angle` in degrees and `feather` as a percentage
/// of the radius.
///
/// `angle` rotates the ellipse clockwise as drawn, because `v` increases down the frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadialGradient {
    pub x: f64,
    pub y: f64,
    pub radius_x: f64,
    pub radius_y: f64,
    pub angle: f64,
    pub feather: f64,
}

/// What this kind declares to the API: one parameter per stored field, each carrying the range
/// [`parse`] enforces below — a centre is a position, the two radii are mask-space distances, the
/// angle is one turn and the feather is a percentage of the radius.
///
/// This is the *only* declaration of a radial gradient's geometry, and it is the whole of what makes
/// the kind creatable: the host's kind table reads it, the generated `mask.create-radial`,
/// `mask.add-radial` and `mask.set-radial` methods declare exactly these parameters and no other
/// kind's, and the stored payload's field names are their names. Nothing else has to be told the
/// radial exists.
///
/// `required` is false for the patch method, where every field is optional and the ones a request
/// names are merged over the stored payload.
pub(super) fn parameters(required: bool) -> Vec<ParameterDescriptor> {
    vec![
        position("x", required, "the centre of the ellipse, across the frame"),
        position("y", required, "the centre of the ellipse, down the frame"),
        distance(
            "radius_x",
            required,
            "the ellipse's radius along its own x axis, in mask-space units of the stage's height",
        ),
        distance(
            "radius_y",
            required,
            "the ellipse's radius along its own y axis, in mask-space units of the stage's height",
        ),
        angle(
            "angle",
            required,
            ANGLE_MIN,
            ANGLE_MAX,
            "the ellipse's rotation, clockwise as drawn because v increases down the frame",
        ),
        percentage(
            "feather",
            required,
            FEATHER_MIN,
            FEATHER_MAX,
            FEATHER_MIN,
            "where the ramp starts, as a percentage of the radius; 0 is a hard edge and 100 ramps \
             from the centre. Inside the ellipse is selected",
        ),
    ]
}

/// Parse and range-check one stored `radial` payload, without a stage.
///
/// Every field of a radial is checkable without a stage, unlike the linear gradient's axis length:
/// a radius is a mask-space distance as stored, so the study's `[DISTANCE_MIN, DISTANCE_MAX]` rule
/// applies to the number itself and does not wait on an aspect ratio. Each refusal names the
/// component and the field.
pub(super) fn parse(component: &Component) -> Result<RadialGradient, Error> {
    let radial: RadialGradient =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid {KIND} payload: {error}",
                    component.name
                ),
            )
        })?;
    let refuse = |field: &str, what: String| {
        Err(Error::new(
            ErrorKind::Validation,
            format!("component {} {KIND} {field} {what}", component.name),
        ))
    };
    for (field, value) in [("x", radial.x), ("y", radial.y)] {
        if !value.is_finite() || !(POSITION_MIN..=POSITION_MAX).contains(&value) {
            return refuse(
                field,
                format!("must be a number within {POSITION_MIN:.0}..={POSITION_MAX:.0}"),
            );
        }
    }
    for (field, value) in [("radius_x", radial.radius_x), ("radius_y", radial.radius_y)] {
        if !value.is_finite() || !(DISTANCE_MIN..=DISTANCE_MAX).contains(&value) {
            return refuse(
                field,
                format!(
                    "must be a number within {DISTANCE_MIN:e}..={DISTANCE_MAX:.0} mask-space units"
                ),
            );
        }
    }
    if !radial.angle.is_finite() || !(ANGLE_MIN..=ANGLE_MAX).contains(&radial.angle) {
        return refuse(
            "angle",
            format!("must be a number within {ANGLE_MIN:.0}..={ANGLE_MAX:.0} degrees"),
        );
    }
    if !radial.feather.is_finite() || !(FEATHER_MIN..=FEATHER_MAX).contains(&radial.feather) {
        return refuse(
            "feather",
            format!("must be a number within {FEATHER_MIN:.0}..={FEATHER_MAX:.0}"),
        );
    }
    Ok(radial)
}

/// One radial gradient bound to a stage, with every term that does not depend on the pixel already
/// computed — the one `cos`/`sin` pair included.
#[derive(Clone, Copy, Debug)]
pub(super) struct Compiled {
    /// The centre in mask space, through the **stored position** spelling.
    cu: f64,
    cv: f64,
    /// `cos(theta)` and `sin(theta)` for `theta = angle · pi / 180`, computed once so the per-pixel
    /// path has no transcendental at all.
    ca: f64,
    sa: f64,
    /// The two radii as stored, kept as **divisors** rather than reciprocals.
    radius_x: f64,
    radius_y: f64,
    /// Where the ramp starts, `1 - feather / 100`, and the span it covers, `1 - r0`.
    r0: f64,
    span: f64,
    /// The study's explicit `feather = 0` case, exactly as the vignette unit's `hard_step` is: the
    /// division by `span` below lives on the other branch, so a vanishing span is never divided by.
    hard: bool,
}

impl Compiled {
    /// Bind a parsed payload to `stage`.
    ///
    /// Infallible: [`parse`] already refused every illegal field, and unlike a linear gradient's
    /// axis length nothing about a radial's legality depends on the stage — a stored radius is a
    /// mask-space distance as written. The stage is needed only for the aspect ratio the centre's
    /// `u` is scaled by.
    pub(super) fn new(stored: RadialGradient, stage: Stage) -> Self {
        let aspect = f64::from(stage.width) / f64::from(stage.height);
        let (cu, cv) = (stored.x * aspect, stored.y);
        let theta = stored.angle * std::f64::consts::PI / 180.0;
        let ca = theta.cos();
        let sa = theta.sin();
        let r0 = 1.0 - stored.feather / 100.0;
        let span = 1.0 - r0;
        Self {
            cu,
            cv,
            ca,
            sa,
            radius_x: stored.radius_x,
            radius_y: stored.radius_y,
            r0,
            span,
            hard: span == 0.0,
        }
    }

    /// Coverage at a mask-space point, as the study froze it:
    ///
    /// ```text
    /// du = u - cu
    /// dv = v - cv
    /// a  =  ca*du + sa*dv
    /// b  = -sa*du + ca*dv
    /// r  = sqrt((a / radius_x)^2 + (b / radius_y)^2)
    /// c  = if hard { if r <= r0 { 1 } else { 0 } }
    ///      else    { smooth(clamp((1 - r) / span, 0, 1)) }
    /// ```
    ///
    /// Exactly `1.0` at the centre for every feather and exactly `0.0` at and past the boundary,
    /// because `smooth` is exact at both clamp ends. The clamped one-branch spelling is the frozen
    /// one and is bit-identical to the design's three-branch form for that reason.
    ///
    /// The `hard` branch is the `feather = 0` hard edge taken as an explicit case, so the divisor
    /// `span` is only ever reached where the constructor has already established it is not zero.
    pub(super) fn coverage(&self, u: f64, v: f64) -> f64 {
        let du = u - self.cu;
        let dv = v - self.cv;
        let a = self.ca * du + self.sa * dv;
        let b = -self.sa * du + self.ca * dv;
        let ax = a / self.radius_x;
        let by = b / self.radius_y;
        let r = (ax * ax + by * by).sqrt();
        if self.hard {
            if r <= self.r0 { 1.0 } else { 0.0 }
        } else {
            smooth(((1.0 - r) / self.span).clamp(0.0, 1.0))
        }
    }

    /// A conservative pixel rectangle of this component's support.
    ///
    /// **As drawn**, coverage is exactly zero wherever `r >= 1`: the clamp's lower end, where
    /// `smooth` is exactly `0.0`. On the hard branch it is exactly zero wherever `r > r0`, and
    /// `hard` means `r0 == 1.0` exactly, so the same ellipse bounds both branches. The support is
    /// therefore the closed ellipse `r <= 1`, and its axis-aligned box is closed form: a point of
    /// that ellipse is `du = rx·ca·cos t - ry·sa·sin t`, `dv = rx·sa·cos t + ry·ca·sin t`, and a
    /// sinusoid `p·cos t + q·sin t` has amplitude `sqrt(p² + q²)`, so the half-extents are
    ///
    /// ```text
    /// ex = sqrt((radius_x·ca)² + (radius_y·sa)²)
    /// ey = sqrt((radius_x·sa)² + (radius_y·ca)²)
    /// ```
    ///
    /// **Inverted**, coverage is `1 - c`, which is exactly zero only where `c` is exactly `1` — the
    /// inner ellipse `r <= r0` — and non-zero over the whole of the rest of the stage. The
    /// complement of an ellipse is not a rectangle, so the whole stage is the honest conservative
    /// answer, exactly as a whole-mask inversion answers it.
    pub(super) fn support(&self, stage: Stage, inverted: bool) -> Region {
        if inverted {
            return super::whole_stage(stage);
        }
        let rxca = self.radius_x * self.ca;
        let rysa = self.radius_y * self.sa;
        let rxsa = self.radius_x * self.sa;
        let ryca = self.radius_y * self.ca;
        let ex = (rxca * rxca + rysa * rysa).sqrt();
        let ey = (rxsa * rxsa + ryca * ryca).sqrt();
        ellipse_bounds(stage, self.cu, self.cv, ex, ey)
    }

    /// The only feature a radial draws is its ramp, so the smallest one is the ramp's width in
    /// pixels: the ramp runs from `r = r0` to `r = 1`, which is `span · radius_x` mask-space units
    /// across the ellipse's own x axis and `span · radius_y` across its y axis, and one mask-space
    /// unit is the stage's height on both axes. The narrower of the two is the feature a pixel grid
    /// has to resolve.
    ///
    /// Nothing here depends on the aspect ratio — a radius is a mask-space distance as stored — so
    /// unlike the linear gradient's ramp this answers for any stage from the compiled terms.
    ///
    /// A hard edge has no ramp at all and answers `0.0`: no pixel grid resolves it, at proxy size or
    /// at full size, and the proxy path is told that rather than given a width the edge does not
    /// have.
    pub(super) fn feature_px(&self, stage: Stage) -> f64 {
        self.span * self.radius_x.min(self.radius_y) * f64::from(stage.height)
    }
}

/// The conservative pixel rectangle of the axis-aligned box `[cu ± ex] x [cv ± ey]` of mask space,
/// clipped to the stage.
///
/// Closed form, so a component's rectangle costs `O(1)` rather than a scan of the stage's side. Two
/// deliberate slacks make the result conservative against its own arithmetic, which is what "exactly
/// zero outside" demands of a rectangle computed in floating point, and they are the same two
/// [`super::half_plane_bounds`] takes: the half-extent is widened by a few ulps of the magnitudes
/// involved, so a pixel whose exact extent is a hair larger than the computed one is kept rather
/// than excluded; and the rectangle is then grown by one pixel on every side. A non-finite value
/// anywhere answers the whole stage, which is always a correct rectangle.
///
/// Mask space to pixel indices inverts the pixel-centre spelling: `u = (px + 0.5) / H`, so
/// `px = u · H - 0.5`.
fn ellipse_bounds(stage: Stage, cu: f64, cv: f64, ex: f64, ey: f64) -> Region {
    if !cu.is_finite() || !cv.is_finite() || !ex.is_finite() || !ey.is_finite() {
        return super::whole_stage(stage);
    }
    let height = f64::from(stage.height);
    // A few ulps of the magnitudes the extent was built from, so the slack is in the arithmetic
    // rather than a fixed coverage threshold.
    let slack = |centre: f64, extent: f64| 16.0 * f64::EPSILON * (centre.abs() + extent);
    let su = ex + slack(cu, ex);
    let sv = ey + slack(cv, ey);
    let min_x = (cu - su) * height - 0.5;
    let max_x = (cu + su) * height - 0.5;
    let min_y = (cv - sv) * height - 0.5;
    let max_y = (cv + sv) * height - 0.5;
    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return super::whole_stage(stage);
    }
    let grow_low = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 - 1;
        index.clamp(0, i64::from(limit)) as u32
    };
    let grow_high = |value: f64, limit: u32| -> u32 {
        let index = value.floor() as i64 + 2;
        index.clamp(0, i64::from(limit)) as u32
    };
    let x0 = grow_low(min_x, stage.width);
    let y0 = grow_low(min_y, stage.height);
    let x1 = grow_high(max_x, stage.width);
    let y1 = grow_high(max_y, stage.height);
    if x1 <= x0 || y1 <= y0 {
        return super::empty_region();
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}
