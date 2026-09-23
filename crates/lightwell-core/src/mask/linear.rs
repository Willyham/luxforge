//! The `linear` component kind: the frozen linear gradient of
//! `docs/design/mask-study.md#the-linear-gradient`, compiled against the content stage its mask's
//! layer receives.
//!
//! This file is the production transcription of that block and of the `linear_coverage` function of
//! the independent `f64` reference at `crates/lightwell-core/tests/reference/mask.rs`. The
//! per-pixel expression is written in the reference's form and the reference's order, so it is
//! bit-identical to it rather than merely within tolerance: `u0, v0, du, dv, l2` are hoisted to
//! compile time because they do not depend on the pixel, and `l2` stays a **divisor** rather than
//! becoming a precomputed reciprocal, because `x / l2` and `x * (1 / l2)` are not the same `f64`.
//!
//! Compiling uses the **stored position** spelling of mask space (`u = x · W/H`, `v = y`); the
//! per-pixel path receives the pixel-centre spelling from [`super::CompiledMask`]. The two agree to
//! `2.220e-16` and not bit for bit, so each is used only where the study froze it.
use super::{DISTANCE_MAX, DISTANCE_MIN, smooth};
use crate::{
    Component, Error, ErrorKind,
    modules::{Region, Stage},
};
use serde::{Deserialize, Serialize};

/// The token a stored component of this kind carries.
pub(super) const KIND: &str = "linear";

/// The legal range of a stored normalized position: the frame is `[0, 1]` and one stage extent of
/// overshoot is legal on each side, because a gradient dragged from off the canvas is an ordinary
/// edit and a gradient whose coverage never reaches zero inside the frame cannot be expressed with
/// endpoints inside it. Widened from `[0, 1]` on 2026-09-23 on the mask study's recommendation: every
/// frozen equation is total on finite inputs, so this is a validation rule and nothing more.
pub const POSITION_MIN: f64 = -1.0;
pub const POSITION_MAX: f64 = 2.0;

/// A linear gradient's stored payload: the two ends of the gradient axis as normalized positions in
/// content-stage coordinates, `p0` at coverage 0 and `p1` at coverage 1.
///
/// Rotation is inherent in the two endpoints, so there is no separate angle to keep consistent with
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinearGradient {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Parse and range-check one stored `linear` payload, without a stage.
///
/// Everything a stage is not needed for is refused here, naming the component and the field: the
/// payload's shape, and each coordinate being a finite number inside the legal range. The axis's own
/// length is a mask-space distance and therefore depends on the stage's aspect ratio, so it is
/// checked when the component is compiled.
pub(super) fn parse(component: &Component) -> Result<LinearGradient, Error> {
    let gradient: LinearGradient =
        serde_json::from_value(component.payload.clone()).map_err(|error| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} has an invalid {KIND} payload: {error}",
                    component.name
                ),
            )
        })?;
    for (field, value) in [
        ("x0", gradient.x0),
        ("y0", gradient.y0),
        ("x1", gradient.x1),
        ("y1", gradient.y1),
    ] {
        if !value.is_finite() || !(POSITION_MIN..=POSITION_MAX).contains(&value) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "component {} {KIND} {field} must be a number within {POSITION_MIN:.0}..={POSITION_MAX:.0}",
                    component.name
                ),
            ));
        }
    }
    Ok(gradient)
}

/// One linear gradient bound to a stage, with every term that does not depend on the pixel already
/// computed.
#[derive(Clone, Copy, Debug)]
pub(super) struct Compiled {
    u0: f64,
    v0: f64,
    du: f64,
    dv: f64,
    /// The squared mask-space length of the axis, bounded away from zero by the legality rule below,
    /// so the per-pixel division needs no guard.
    l2: f64,
    /// The stored payload, kept so [`Compiled::feature_px`] can answer for a stage other than the
    /// one this was compiled against — which is exactly what the proxy path asks, before it compiles
    /// a mask at proxy size.
    stored: LinearGradient,
}

impl Compiled {
    /// Bind a parsed payload to `stage`.
    ///
    /// The axis's mask-space length is itself a stored distance, so the design's "a zero-length axis
    /// is a validation error" is spelled as the same `[DISTANCE_MIN, DISTANCE_MAX]` bound every other
    /// distance takes, compared on the **squared** length so no square root is taken to reject it.
    /// A non-finite `l2` fails the same comparison.
    pub(super) fn new(stored: LinearGradient, stage: Stage, name: &str) -> Result<Self, Error> {
        let aspect = f64::from(stage.width) / f64::from(stage.height);
        let (u0, v0) = (stored.x0 * aspect, stored.y0);
        let (u1, v1) = (stored.x1 * aspect, stored.y1);
        let du = u1 - u0;
        let dv = v1 - v0;
        let l2 = du * du + dv * dv;
        if !(DISTANCE_MIN * DISTANCE_MIN..=DISTANCE_MAX * DISTANCE_MAX).contains(&l2) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "component {name} {KIND} axis length must be within {DISTANCE_MIN:e}..={DISTANCE_MAX:.0} \
                     mask-space units on a {}x{} stage",
                    stage.width, stage.height
                ),
            ));
        }
        Ok(Self {
            u0,
            v0,
            du,
            dv,
            l2,
            stored,
        })
    }

    /// Coverage at a mask-space point, as the study froze it:
    ///
    /// ```text
    /// t = clamp(((u - u0)*du + (v - v0)*dv) / l2, 0, 1)
    /// c = smooth(t)
    /// ```
    ///
    /// Exactly `0.0` at and behind `p0` and exactly `1.0` at and beyond `p1`, because at `p1` the
    /// numerator is `du*du + dv*dv` in the same order as `l2` and `smooth` is exact at both clamp
    /// ends. Constant perpendicular to the axis, because the projection is the only thing coverage
    /// depends on: a gradient has no width.
    pub(super) fn coverage(&self, u: f64, v: f64) -> f64 {
        let t = (((u - self.u0) * self.du + (v - self.v0) * self.dv) / self.l2).clamp(0.0, 1.0);
        smooth(t)
    }

    /// A conservative pixel rectangle of this component's support.
    ///
    /// Coverage is a clamped ramp, so it is exactly zero on the whole half-plane behind `p0` —
    /// `t <= 0`, where `smooth` is exactly `0.0` — and exactly one on the half-plane in front of
    /// `p1`. An inverted component is therefore exactly zero in front of `p1` instead. Either way the
    /// support is one half-plane, and the rectangle is that half-plane clipped to the stage:
    /// `dot > 0` for the component as drawn, `dot < l2` for the inverted one, both affine in the
    /// pixel indices.
    pub(super) fn support(&self, stage: Stage, inverted: bool) -> Region {
        let height = f64::from(stage.height);
        let dot = |x: f64, y: f64| {
            let u = (x + 0.5) / height;
            let v = (y + 0.5) / height;
            (u - self.u0) * self.du + (v - self.v0) * self.dv
        };
        if inverted {
            super::half_plane_bounds(stage, |x, y| self.l2 - dot(x, y))
        } else {
            super::half_plane_bounds(stage, dot)
        }
    }

    /// The only feature a gradient draws is its ramp, so the smallest one is the ramp's width in
    /// pixels: the axis's mask-space length times the stage's height, one mask-space unit being that
    /// height on both axes.
    ///
    /// Computed from the stored payload against the stage asked about, not from the compiled terms,
    /// so the answer is about that stage. The square root is taken here and nowhere near the
    /// rejection rule, which compares squared lengths.
    pub(super) fn feature_px(&self, stage: Stage) -> f64 {
        let aspect = f64::from(stage.width) / f64::from(stage.height);
        let du = self.stored.x1 * aspect - self.stored.x0 * aspect;
        let dv = self.stored.y1 - self.stored.y0;
        (du * du + dv * dv).sqrt() * f64::from(stage.height)
    }
}
