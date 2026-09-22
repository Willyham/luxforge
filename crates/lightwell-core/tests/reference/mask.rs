//! Independent f64 reference for the frozen mask coverage mathematics (TASK-001).
//!
//! This module shares no code with production — there is no production mask
//! code yet, and when there is, this file is the oracle it cannot influence,
//! matching the convention `tests/reference/vignette.rs` set. The frozen
//! equations are written out in full in `docs/design/mask-study.md`; this file
//! is their literal transcription, in the same order and the same spelling, and
//! the two must be read together. A later production unit that writes the same
//! expressions in the same order is bit-identical to this reference rather than
//! merely within tolerance of it.
//!
//! Two spellings are deliberately distinct and must not be interchanged:
//! `Stage::pixel_uv` maps a content-stage pixel index to mask space and is what
//! a rasterizing pass and `render.sample` both call; `Stage::position_uv` maps a
//! stored normalized payload coordinate to mask space and is what compiling a
//! component calls. They agree mathematically and to within the frozen
//! tolerance, but not bit for bit (measured in `mask_reference.rs`), so each
//! caller uses the one for its own input.
//!
//! Domain: every function takes and returns plain `f64` and validates nothing.
//! Legality of a stored payload is [`distance_is_legal`] and [`axis_is_legal`],
//! which state the frozen rules but are not applied automatically; production
//! rejects an illegal payload before any of this is reached, exactly as
//! `tests/reference/vignette.rs` states for its own inputs. Given finite inputs
//! that satisfy those rules, every step below is finite: the only divisions are
//! by an axis length or a radius the rules bound away from zero, and by a
//! feather span guarded by an explicit hard-edge branch.

/// The content stage a mask is evaluated against: the stage every pixel-,
/// colour- and spatial-stage layer already addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage {
    pub width: u32,
    pub height: u32,
}

impl Stage {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// `W / H`, computed once per compiled mask and reused, so `position_uv`
    /// multiplies by a single stored value rather than recomputing the ratio.
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height)
    }

    /// Mask space at the centre of content-stage pixel `(px, py)`:
    ///
    /// ```text
    /// u = (px + 0.5) / H
    /// v = (py + 0.5) / H
    /// ```
    ///
    /// One mask-space unit is `H` pixels **on both axes**, which is the whole
    /// point of the convention: the mapping from the pixel grid to mask space is
    /// a single isotropic scale, so a circle in mask space is a circle in
    /// pixels at any aspect ratio. `u` spans `(0, W/H)` and `v` spans `(0, 1)`
    /// over a frame's pixel centres.
    pub fn pixel_uv(&self, px: u32, py: u32) -> (f64, f64) {
        let h = f64::from(self.height);
        ((f64::from(px) + 0.5) / h, (f64::from(py) + 0.5) / h)
    }

    /// Mask space at a stored normalized position `x, y` (fractions of the
    /// content stage's width and height):
    ///
    /// ```text
    /// u = x * (W / H)
    /// v = y
    /// ```
    ///
    /// `v` is the stored fraction unchanged, which is what makes the vertical
    /// axis the unit axis and a stored distance a fraction of the height.
    pub fn position_uv(&self, x: f64, y: f64) -> (f64, f64) {
        (x * self.aspect(), y)
    }
}

/// The smallest legal stored distance, in mask-space units. Every falloff
/// divides by a stored distance (the linear axis length, a radial radius, a
/// brush radius times its feather), so a floor is what replaces a runtime guard
/// against a vanishing divisor. `1e-4` is below one pixel on every supported
/// stage — 0.4 px at 4000 px of height, 1.6 px at the 16384 px maximum — so no
/// distance a gesture can draw is excluded by it.
pub const DISTANCE_MIN: f64 = 1e-4;

/// The largest legal stored distance, in mask-space units. A distance of
/// `sqrt((W/H)² + 1)` from any point already covers the whole stage, so `64`
/// covers every aspect ratio up to 63.99:1 — far beyond anything the 16384 px
/// per-side admission limit can produce as a photograph — while keeping the
/// largest representable squared ratio `(DISTANCE_MAX / DISTANCE_MIN)²` at
/// `4.096e11`, which an `f32` accumulation carries with room to spare.
pub const DISTANCE_MAX: f64 = 64.0;

/// The frozen legality rule for a stored distance: finite and within
/// `[DISTANCE_MIN, DISTANCE_MAX]`.
pub fn distance_is_legal(distance: f64) -> bool {
    distance.is_finite() && (DISTANCE_MIN..=DISTANCE_MAX).contains(&distance)
}

/// The frozen legality rule for a linear gradient's axis: its **mask-space**
/// length is itself a distance, so the design's "a zero-length axis is a
/// validation error" is spelled as the same bound every other distance takes.
/// The squared length is compared against `DISTANCE_MIN²` so the rule needs no
/// square root.
pub fn axis_is_legal(linear: &Linear, stage: &Stage) -> bool {
    let (u0, v0) = stage.position_uv(linear.x0, linear.y0);
    let (u1, v1) = stage.position_uv(linear.x1, linear.y1);
    let du = u1 - u0;
    let dv = v1 - v0;
    let l2 = du * du + dv * dv;
    l2.is_finite() && (DISTANCE_MIN * DISTANCE_MIN..=DISTANCE_MAX * DISTANCE_MAX).contains(&l2)
}

/// The frozen easing, `smooth(s) = s²(3 − 2s)`, on `s` already clamped to
/// `[0, 1]` by the caller. `smooth(0) == 0.0` and `smooth(1) == 1.0` exactly in
/// `f64` (`1·1·(3 − 2) == 1`), which is what lets the clamped one-branch
/// spelling of both falloffs below reproduce their hard limits exactly instead
/// of approaching them.
///
/// This is the same falloff the delivered vignette froze (written there as
/// `3t² − 2t³`, the same polynomial in the cheaper Horner-free form used here
/// and in `tests/reference/vignette.rs`).
pub fn smooth(s: f64) -> f64 {
    s * s * (3.0 - 2.0 * s)
}

/// The two easings this study compares `smooth` against and rejects. They exist
/// so the comparison is reproducible, not because production may use them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    /// The frozen `s²(3 − 2s)`.
    Smoothstep,
    /// `s³(6s² − 15s + 10)`, the C² refinement.
    Smootherstep,
    /// `(1 − cos(pi s)) / 2`, the raised cosine.
    RaisedCosine,
}

/// Evaluate one candidate easing on `s` already clamped to `[0, 1]`.
pub fn ease(easing: Easing, s: f64) -> f64 {
    match easing {
        Easing::Smoothstep => smooth(s),
        Easing::Smootherstep => s * s * s * (6.0 * s * s - 15.0 * s + 10.0),
        Easing::RaisedCosine => (1.0 - (std::f64::consts::PI * s).cos()) / 2.0,
    }
}

/// A linear gradient component's payload: the two ends of the gradient axis as
/// stored normalized positions, `p0` at coverage 0 and `p1` at coverage 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Linear {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// A radial gradient component's payload: centre as a stored normalized
/// position, the two radii in mask-space units, `angle` in degrees
/// (`-180..180`) and `feather` `0..100`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radial {
    pub x: f64,
    pub y: f64,
    pub radius_x: f64,
    pub radius_y: f64,
    pub angle: f64,
    pub feather: f64,
}

/// The linear gradient's coverage at mask-space point `(u, v)`, in the exact
/// order a production unit transcribes:
///
/// ```text
/// (u0, v0) = position_uv(x0, y0)
/// (u1, v1) = position_uv(x1, y1)
/// du = u1 - u0
/// dv = v1 - v0
/// l2 = du*du + dv*dv                       -- axis_is_legal bounds this below
/// t  = clamp(((u - u0)*du + (v - v0)*dv) / l2, 0, 1)
/// c  = smooth(t)
/// ```
///
/// `u0, v0, du, dv, l2` do not depend on the pixel and are computed once when
/// the component is compiled; `l2` is a **divisor, not a precomputed
/// reciprocal**, because `x / l2` and `x * (1 / l2)` are not the same `f64`.
pub fn linear_coverage(linear: &Linear, stage: &Stage, u: f64, v: f64) -> f64 {
    let (u0, v0) = stage.position_uv(linear.x0, linear.y0);
    let (u1, v1) = stage.position_uv(linear.x1, linear.y1);
    let du = u1 - u0;
    let dv = v1 - v0;
    let l2 = du * du + dv * dv;
    let t = (((u - u0) * du + (v - v0) * dv) / l2).clamp(0.0, 1.0);
    smooth(t)
}

/// The radial gradient's coverage at mask-space point `(u, v)`, **inside
/// selected**, in the exact order a production unit transcribes:
///
/// ```text
/// theta  = angle * pi / 180                -- once per component
/// ca     = cos(theta),  sa = sin(theta)    -- once per component
/// r0     = 1 - feather / 100               -- once per component
/// span   = 1 - r0                          -- once per component
/// hard   = (span == 0)                     -- once per component
///
/// du = u - centre_u
/// dv = v - centre_v
/// a  =  ca*du + sa*dv
/// b  = -sa*du + ca*dv
/// r  = sqrt((a / radius_x)^2 + (b / radius_y)^2)
/// c  = if hard { if r <= r0 { 1 } else { 0 } }
///      else    { smooth(clamp((1 - r) / span, 0, 1)) }
/// ```
///
/// The `hard` branch is the `feather = 0` hard edge, taken as an explicit case
/// exactly as the vignette unit's `hard_step` is, so no division by a vanishing
/// span is ever evaluated. It also absorbs a feather so small that `1 - f/100`
/// rounds to exactly `1.0`, which is the same edge by a different route.
///
/// The clamped one-branch spelling of the smooth case is bit-identical to the
/// design's three-branch form (`1` inside `r0`, `0` beyond `1`, `smooth((1 - r)
/// / span)` between), because `smooth` returns exactly `0.0` and exactly `1.0`
/// at the clamp's ends; `mask_reference.rs` asserts that equality rather than
/// assuming it.
///
/// `angle` rotates the ellipse clockwise as drawn, because `v` increases down
/// the frame.
pub fn radial_coverage(radial: &Radial, stage: &Stage, u: f64, v: f64) -> f64 {
    let (cu, cv) = stage.position_uv(radial.x, radial.y);
    let theta = radial.angle * std::f64::consts::PI / 180.0;
    let ca = theta.cos();
    let sa = theta.sin();
    let r0 = 1.0 - radial.feather / 100.0;
    let span = 1.0 - r0;
    let du = u - cu;
    let dv = v - cv;
    let a = ca * du + sa * dv;
    let b = -sa * du + ca * dv;
    let ax = a / radial.radius_x;
    let by = b / radial.radius_y;
    let r = (ax * ax + by * by).sqrt();
    if span == 0.0 {
        if r <= r0 { 1.0 } else { 0.0 }
    } else {
        smooth(((1.0 - r) / span).clamp(0.0, 1.0))
    }
}

/// The design's literal three-branch radial spelling, kept only so
/// `mask_reference.rs` can prove it equals [`radial_coverage`] bit for bit. No
/// production unit transcribes this one.
pub fn radial_coverage_branch_form(radial: &Radial, stage: &Stage, u: f64, v: f64) -> f64 {
    let (cu, cv) = stage.position_uv(radial.x, radial.y);
    let theta = radial.angle * std::f64::consts::PI / 180.0;
    let ca = theta.cos();
    let sa = theta.sin();
    let r0 = 1.0 - radial.feather / 100.0;
    let span = 1.0 - r0;
    let du = u - cu;
    let dv = v - cv;
    let a = ca * du + sa * dv;
    let b = -sa * du + ca * dv;
    let ax = a / radial.radius_x;
    let by = b / radial.radius_y;
    let r = (ax * ax + by * by).sqrt();
    if span == 0.0 {
        if r <= r0 { 1.0 } else { 0.0 }
    } else if r <= r0 {
        1.0
    } else if r >= 1.0 {
        0.0
    } else {
        smooth((1.0 - r) / span)
    }
}

/// How one component combines with the coverage composed so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Add,
    Subtract,
    Intersect,
}

/// The two candidate composition algebras. [`Algebra::Zadeh`] is the frozen
/// one; [`Algebra::Product`] exists so the comparison in the study is
/// reproducible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algebra {
    /// The frozen fuzzy-set algebra: `max`, `min(m, 1 - c)`, `min`.
    Zadeh,
    /// The coherent product algebra: the probabilistic sum `1 - (1 - m)(1 - c)`
    /// for add, `m(1 - c)` for subtract and `m·c` for intersect. Stated as the
    /// whole algebra rather than only the design's `m(1 - c)`, because mixing a
    /// `max` union with a product difference is neither of the two families.
    Product,
}

/// One step of the composition: fold `c` into the coverage `m` composed so far
/// under `mode`.
///
/// The frozen [`Algebra::Zadeh`] step is exactly one of `max(m, c)`,
/// `min(m, 1 - c)` or `min(m, c)`. `max` and `min` introduce no rounding at
/// all; the single `1 - c` introduces at most half an ulp, so a whole
/// component list contributes at most one half-ulp per subtract step.
pub fn combine(algebra: Algebra, m: f64, mode: Mode, c: f64) -> f64 {
    match (algebra, mode) {
        (Algebra::Zadeh, Mode::Add) => m.max(c),
        (Algebra::Zadeh, Mode::Subtract) => m.min(1.0 - c),
        (Algebra::Zadeh, Mode::Intersect) => m.min(c),
        (Algebra::Product, Mode::Add) => 1.0 - (1.0 - m) * (1.0 - c),
        (Algebra::Product, Mode::Subtract) => m * (1.0 - c),
        (Algebra::Product, Mode::Intersect) => m * c,
    }
}

/// A component's geometry. Only the two kinds this study freezes are
/// represented; the brush and the range selections are named in the study as
/// not frozen here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Linear(Linear),
    Radial(Radial),
}

/// One mask component: its mode, its own inversion flag and its geometry. The
/// first component of a mask is always `Add`, which the composition relies on
/// only in that it starts from `m = 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Component {
    pub mode: Mode,
    pub invert: bool,
    pub kind: Kind,
}

/// A mask: its amount in `0..100`, its own inversion flag and its ordered
/// component list.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub amount: f64,
    pub invert: bool,
    pub components: Vec<Component>,
}

/// One component's own coverage at `(u, v)`, with its inversion applied:
/// `c = 1 - c` when the component says so, which is the one rounding step a
/// component contributes beyond its own falloff.
pub fn component_coverage(component: &Component, stage: &Stage, u: f64, v: f64) -> f64 {
    let c = match &component.kind {
        Kind::Linear(linear) => linear_coverage(linear, stage, u, v),
        Kind::Radial(radial) => radial_coverage(radial, stage, u, v),
    };
    if component.invert { 1.0 - c } else { c }
}

/// The composed coverage `M` of a whole mask at `(u, v)`, in the exact order a
/// production unit transcribes:
///
/// ```text
/// m = 0
/// for component in components:
///     c = component's falloff at (u, v)
///     c = 1 - c                       if component.invert
///     m = max(m, c)                   if mode == add
///     m = min(m, 1 - c)               if mode == subtract
///     m = min(m, c)                   if mode == intersect
/// M = (amount / 100) * (invert ? 1 - m : m)
/// ```
///
/// A mask with no components composes to `m = 0`, so `M = 0`: an empty mask
/// selects nothing rather than everything.
pub fn coverage(mask: &Mask, algebra: Algebra, stage: &Stage, u: f64, v: f64) -> f64 {
    let mut m = 0.0;
    for component in &mask.components {
        let c = component_coverage(component, stage, u, v);
        m = combine(algebra, m, component.mode, c);
    }
    let m = if mask.invert { 1.0 - m } else { m };
    (mask.amount / 100.0) * m
}

/// The blend a masked colour operation performs, per channel, in linear light:
/// `out = (1 - M)·in + M·effect(in)`. It is written here because the study's
/// visibility figures are quoted in output codes, which means passing the
/// coverage difference through this blend and the sRGB quantizer; the blend
/// itself is frozen by the masked-primitive task, not by this study.
pub fn blend(input: f64, effect: f64, m: f64) -> f64 {
    (1.0 - m) * input + m * effect
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn smooth_endpoints_are_exact() {
        assert_eq!(smooth(0.0), 0.0);
        assert_eq!(smooth(1.0), 1.0);
        assert_eq!(smooth(0.5), 0.5);
    }

    #[test]
    fn mask_space_is_one_isotropic_scale_of_the_pixel_grid() {
        // Stepping one pixel moves the same distance on both axes, whatever the
        // aspect ratio: that is the property "a circle is a circle" rests on.
        for stage in [Stage::new(6000, 4000), Stage::new(4000, 6000)] {
            let (u0, v0) = stage.pixel_uv(10, 10);
            let (u1, _) = stage.pixel_uv(11, 10);
            let (_, v1) = stage.pixel_uv(10, 11);
            // Exactly equal: the two axes are divided by the same `H`.
            assert_eq!(u1 - u0, v1 - v0);
            // One pixel is `1 / H` mask-space units, to within the rounding of
            // the two quotients themselves: their difference is exact
            // (Sterbenz), so the whole error is at most half an ulp of each
            // coordinate, not of the much smaller step.
            let step = 1.0 / f64::from(stage.height);
            assert!((u1 - u0 - step).abs() <= f64::EPSILON * u1);
        }
    }

    #[test]
    fn feather_zero_is_the_hard_edge_branch() {
        let stage = Stage::new(1000, 1000);
        let radial = Radial {
            x: 0.5,
            y: 0.5,
            radius_x: 0.25,
            radius_y: 0.25,
            angle: 0.0,
            feather: 0.0,
        };
        // r0 = 1 exactly, so coverage is 1 inside the ellipse and 0 outside it.
        assert_eq!(radial_coverage(&radial, &stage, 0.5, 0.5), 1.0);
        assert_eq!(radial_coverage(&radial, &stage, 0.5, 0.74), 1.0);
        assert_eq!(radial_coverage(&radial, &stage, 0.5, 0.76), 0.0);
    }

    #[test]
    fn zadeh_subtract_repeated_is_exactly_one_subtract() {
        let once = combine(Algebra::Zadeh, 0.8, Mode::Subtract, 0.3);
        let twice = combine(Algebra::Zadeh, once, Mode::Subtract, 0.3);
        assert_eq!(once, twice);
    }

    #[test]
    fn distance_rules_bound_both_ends() {
        assert!(!distance_is_legal(0.0));
        assert!(!distance_is_legal(DISTANCE_MIN / 2.0));
        assert!(distance_is_legal(DISTANCE_MIN));
        assert!(distance_is_legal(DISTANCE_MAX));
        assert!(!distance_is_legal(DISTANCE_MAX * 2.0));
        assert!(!distance_is_legal(f64::NAN));
        assert!(!distance_is_legal(f64::INFINITY));
    }
}
