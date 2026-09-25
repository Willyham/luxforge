//! Independent f64 reference for the frozen mask coverage mathematics (TASK-001).
//!
//! This module shares no code with production — there is no production mask
//! code yet, and when there is, this file is the oracle it cannot influence,
//! matching the convention `crates/lightwell-reference/src/vignette.rs` set. The frozen
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
//! tolerance, but not bit for bit (measured in the core's `tests/mask/study.rs`), so each
//! caller uses the one for its own input.
//!
//! Domain: every function takes and returns plain `f64` and validates nothing.
//! Legality of a stored payload is [`distance_is_legal`] and [`axis_is_legal`],
//! which state the frozen rules but are not applied automatically; production
//! rejects an illegal payload before any of this is reached, exactly as
//! `crates/lightwell-reference/src/vignette.rs` states for its own inputs. Given finite inputs
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
/// and in `crates/lightwell-reference/src/vignette.rs`).
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
/// at the clamp's ends; the core's `tests/mask/study.rs` asserts that equality rather than
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
/// the core's `tests/mask/study.rs` can prove it equals [`radial_coverage`] bit for bit. No
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

/// A component's geometry. Only the two gradient kinds are represented, because
/// the composition algebra below is what this enum exists for and the brush's
/// own field is frozen standalone in [`brush_coverage`]: a single-component
/// `Add` mask at amount 100 composes to `max(0, c)` and `1.0 * c`, both exact,
/// so the brush is compared against its own reference without a third variant
/// here. The range selections are a separate study.
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

// ---------------------------------------------------------------------------
// The brush (TASK-018), frozen in `docs/design/mask-study.md#the-brush`.
//
// A brush component holds an ordered list of strokes. A stroke is a polyline in
// stored normalized coordinates plus the brush settings it was drawn with; its
// coverage is the capsule profile at the distance to the nearest of its
// segments; and the strokes accumulate in stored order by screen union, or by
// multiply-complement for an erase stroke.
//
// Everything below shares this study's `smooth` and its `DISTANCE_MIN` floor
// rather than introducing a second convention: a stroke's radius *is* a stored
// mask-space distance, so the divisor `band = R · f` is bounded exactly as a
// radius and an axis length are, and the `f = 0` hard edge is an explicit case
// for the same reason the radial's is.
// ---------------------------------------------------------------------------

/// One stroke of a brush component: the path in stored normalized coordinates
/// and the brush at the moment the stroke was made.
///
/// `size` is the radius in mask-space units (one unit is the stage's height on
/// both axes, so a round brush is round at any aspect ratio), `feather` and
/// `flow` are the editor's usual `0..=100` percentages, and `erase` marks a
/// stroke that takes coverage away rather than adding it.
#[derive(Clone, Debug, PartialEq)]
pub struct BrushStroke {
    pub points: Vec<[f64; 2]>,
    pub size: f64,
    pub feather: f64,
    pub flow: f64,
    pub erase: bool,
    /// The colour this stroke is constrained to, when it carries one. It is part
    /// of the stroke, so a stored stroke reproduces its own constraint and
    /// nothing is sampled again when the picture is drawn.
    pub colour: Option<ColourLimit>,
}

/// The colour a constrained stroke was seeded on, and how tight the similarity
/// around it is
/// (`docs/design/mask-study.md#the-colour-constraint`).
///
/// `seed` is a **linear sRGB** triple in the domain of the operation the mask
/// modulates — the pixel that operation receives where the stroke began —
/// sampled once, when the stroke is made. `refine` is the colour range's own
/// slider in `0..100` and means exactly what it means there: higher is tighter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColourLimit {
    pub seed: [f64; 3],
    pub refine: f64,
}

/// The similarity a constrained stroke multiplies its coverage by, in the exact
/// order a production unit transcribes:
///
/// ```text
/// (a0, b0) = the Oklab a and b of seed        -- once per stroke
/// radius   = refine_radius(refine)            -- once per stroke
///
/// lab = to_oklab(rgb)
/// da  = lab.a - a0
/// db  = lab.b - b0
/// d2  = da*da + db*db
/// d   = sqrt(d2)
/// r   = d / radius
/// k   = smooth(clamp((1 - r) / SPAN, 0, 1))
/// ```
///
/// **This is the [`super::range`] study's colour range at one sample**, not a
/// second metric that resembles it: the same Oklab chromaticity distance, the
/// same plateau and span, and the same geometric refine mapping, reached through
/// that module's own constants and conversions.
/// `the_colour_similarity_is_the_frozen_colour_range_at_one_sample` proves the
/// two equal bit for bit against [`colour_similarity_as_range`], which they are
/// because folding one sample by `min` against `+infinity` returns that sample's
/// squared distance exactly.
///
/// Exactly `1.0` at the seed and out to `PLATEAU * radius`, and exactly `0.0` at
/// and beyond `radius`, because `smooth` is exact at both ends of its clamp.
pub fn colour_similarity(limit: &ColourLimit, rgb_linear: [f64; 3]) -> f64 {
    let seed = super::colour::to_oklab(limit.seed);
    let (a0, b0) = (seed.a, seed.b);
    let radius = super::range::refine_radius(limit.refine);
    let lab = super::colour::to_oklab(rgb_linear);
    let da = lab.a - a0;
    let db = lab.b - b0;
    let d2 = da * da + db * db;
    let d = d2.sqrt();
    let r = d / radius;
    smooth(((1.0 - r) / super::range::SPAN).clamp(0.0, 1.0))
}

/// The same similarity reached through the colour range's own compiled form,
/// kept only so the proofs can show the two are the same number rather than
/// asserting it in prose. No production unit transcribes this one — but a
/// production unit is free to *evaluate* the range's own compiled component,
/// which is what this shows costs nothing.
pub fn colour_similarity_as_range(limit: &ColourLimit, rgb_linear: [f64; 3]) -> f64 {
    let compiled = super::range::compile_colour_range(&super::range::ColourRange {
        samples: vec![limit.seed],
        refine: limit.refine,
    });
    super::range::colour_coverage(&compiled, rgb_linear)
}

/// A brush component: its strokes in stored order. Order is load-bearing only
/// where an erase stroke appears, but it is stored for every stroke because
/// that is where the erase lands.
#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    pub strokes: Vec<BrushStroke>,
}

/// One segment of a stroke, in mask space, with the terms that do not depend on
/// the pixel already computed:
///
/// ```text
/// ex   = bx - ax
/// ey   = by - ay
/// len2 = ex*ex + ey*ey
/// if len2 == 0:  ex = 0, ey = 0, len2 = 1
/// ```
///
/// The normalization of a degenerate segment is the frozen spelling of "the
/// distance to the point itself for a one-point stroke": with `ex = ey = 0` and
/// `len2 = 1` the projection below evaluates `t = 0/1 = 0`, `q = A` and the
/// distance to `A`, exactly, with **no branch on the per-pixel path**. It also
/// absorbs a repeated position, which a stored path cannot hold but a reference
/// must still be total on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub ax: f64,
    pub ay: f64,
    pub ex: f64,
    pub ey: f64,
    pub len2: f64,
}

/// The segments of one stroke, in drawn order, through the **stored position**
/// spelling of mask space (compiling a payload, not reading a pixel).
///
/// A stroke of `n >= 2` positions has `n - 1` segments; a one-point stroke has
/// the single degenerate segment above, so every stroke has at least one and
/// the evaluation below needs no empty case.
pub fn brush_segments(stroke: &BrushStroke, stage: &Stage) -> Vec<Segment> {
    let uv: Vec<(f64, f64)> = stroke
        .points
        .iter()
        .map(|[x, y]| stage.position_uv(*x, *y))
        .collect();
    let mut segments = Vec::new();
    if uv.len() == 1 {
        segments.push(segment(uv[0], uv[0]));
        return segments;
    }
    for pair in uv.windows(2) {
        segments.push(segment(pair[0], pair[1]));
    }
    segments
}

fn segment(a: (f64, f64), b: (f64, f64)) -> Segment {
    let ex = b.0 - a.0;
    let ey = b.1 - a.1;
    let len2 = ex * ex + ey * ey;
    if len2 == 0.0 {
        Segment {
            ax: a.0,
            ay: a.1,
            ex: 0.0,
            ey: 0.0,
            len2: 1.0,
        }
    } else {
        Segment {
            ax: a.0,
            ay: a.1,
            ex,
            ey,
            len2,
        }
    }
}

/// The **squared** distance from mask-space point `(u, v)` to one segment, in
/// the exact order a production unit transcribes:
///
/// ```text
/// wx = u - ax
/// wy = v - ay
/// t  = clamp((wx*ex + wy*ey) / len2, 0, 1)
/// qx = ax + t*ex
/// qy = ay + t*ey
/// dx = u - qx
/// dy = v - qy
/// d2 = dx*dx + dy*dy
/// ```
///
/// Squared, because the capsule profile needs **one** square root per stroke and
/// not one per segment: `sqrt` is monotone and correctly rounded, so the square
/// root of the smallest squared distance is the smallest distance, bit for bit.
/// `len2` is a divisor and never a precomputed reciprocal, and the dot product
/// is not reassociated.
pub fn segment_distance2(segment: &Segment, u: f64, v: f64) -> f64 {
    let wx = u - segment.ax;
    let wy = v - segment.ay;
    let t = ((wx * segment.ex + wy * segment.ey) / segment.len2).clamp(0.0, 1.0);
    let qx = segment.ax + t * segment.ex;
    let qy = segment.ay + t * segment.ey;
    let dx = u - qx;
    let dy = v - qy;
    dx * dx + dy * dy
}

/// The capsule profile of one stroke at distance `d`, before `flow`:
///
/// ```text
/// R    = size                       -- once per stroke
/// f    = feather / 100              -- once per stroke
/// band = R * f                      -- once per stroke: the ramp's width
/// hard = (band == 0)                -- once per stroke
///
/// s = if hard { if d <= R { 1 } else { 0 } }
///     else    { smooth(clamp((R - d) / band, 0, 1)) }
/// ```
///
/// `hard` is the `feather = 0` hard edge taken as an explicit case, exactly as
/// the radial's is, so no division by a vanishing band is ever evaluated. It
/// absorbs the second route to the same edge as well: a feather so small that
/// `R · f/100` underflows to exactly zero is the same hard edge, reached by
/// rounding rather than by an equality against zero.
///
/// Exactly `1.0` on the core (`d <= R - band`, where the clamp's upper end and
/// `smooth(1)` are both exact) and exactly `0.0` at and beyond `d = R` on the
/// feathered branch, which is what makes the support rectangle and the grid
/// index below exactly right rather than nearly right.
pub fn capsule_profile(stroke: &BrushStroke, d: f64) -> f64 {
    let r = stroke.size;
    let f = stroke.feather / 100.0;
    let band = r * f;
    if band == 0.0 {
        if d <= r { 1.0 } else { 0.0 }
    } else {
        smooth(((r - d) / band).clamp(0.0, 1.0))
    }
}

/// One stroke's coverage at mask-space point `(u, v)`: the profile at the
/// distance to the nearest of its segments, scaled by its flow.
///
/// ```text
/// d      = sqrt(min over the stroke's segments of segment_distance2)
/// stroke = capsule_profile(d) * (flow / 100)                  -- unconstrained
/// stroke = capsule_profile(d) * (flow / 100) * k              -- constrained
/// ```
///
/// **The minimum distance, not the maximum profile.** The two are the same
/// number — the profile is nonincreasing in `d`, so the largest profile over the
/// segments is the profile of the smallest distance — and
/// the core's `tests/mask/brush_study.rs` proves that equality bit for bit against
/// [`stroke_coverage_max_form`] rather than assuming it. This spelling is the
/// frozen one because it evaluates one `sqrt` and one `smooth` per stroke
/// instead of one per segment.
///
/// **The colour constraint's `k` is the last multiply, and only a constrained
/// stroke performs one.** An unconstrained stroke evaluates the same expression
/// it always did, with no `* 1.0` appended, so the geometric brush's
/// bit-identity is untouched by the constraint; `rgb` is the pixel the operation
/// the mask modulates receives, and a stroke with no constraint ignores it
/// exactly as a geometric component ignores it.
pub fn stroke_coverage(
    stroke: &BrushStroke,
    segments: &[Segment],
    u: f64,
    v: f64,
    rgb_linear: [f64; 3],
) -> f64 {
    let mut nearest = f64::INFINITY;
    for segment in segments {
        let d2 = segment_distance2(segment, u, v);
        if d2 < nearest {
            nearest = d2;
        }
    }
    let d = nearest.sqrt();
    let s = capsule_profile(stroke, d) * (stroke.flow / 100.0);
    match &stroke.colour {
        None => s,
        Some(limit) => s * colour_similarity(limit, rgb_linear),
    }
}

/// The design's "the maximum over its segments of a capsule profile" spelling,
/// kept only so the core's `tests/mask/brush_study.rs` can prove it equals
/// [`stroke_coverage`] bit for bit. No production unit transcribes this one.
pub fn stroke_coverage_max_form(
    stroke: &BrushStroke,
    segments: &[Segment],
    u: f64,
    v: f64,
    rgb_linear: [f64; 3],
) -> f64 {
    let mut best = 0.0f64;
    for segment in segments {
        let d = segment_distance2(segment, u, v).sqrt();
        let s = capsule_profile(stroke, d);
        if s > best {
            best = s;
        }
    }
    let s = best * (stroke.flow / 100.0);
    match &stroke.colour {
        None => s,
        Some(limit) => s * colour_similarity(limit, rgb_linear),
    }
}

/// How one stroke folds into the coverage the strokes before it accumulated:
///
/// ```text
/// c = c * (1.0 - s)          if the stroke erases
/// c = c + (1.0 - c) * s      otherwise
/// ```
///
/// The add is the **screen union** `1 − (1 − c)(1 − s)`, written in the one
/// spelling that is an exact identity when the stroke contributes nothing:
/// `c + (1 − c)·0` is `c` bit for bit, while `1 − (1 − c)·1` is not `c` for
/// every `c`. The erase is the **multiply-complement** `c · (1 − s)`, exact at
/// `s = 0` for the same reason. That exactness is not decoration: it is what
/// lets a production unit's grid index skip a stroke the pixel is nowhere near
/// and still produce, bit for bit, the field the whole list would have produced.
///
/// Both are exact at the other end too: `c + (1 − c)·1` is exactly `1.0` and
/// `c · (1 − 1)` is exactly `0.0`.
pub fn accumulate(c: f64, s: f64, erase: bool) -> f64 {
    if erase {
        c * (1.0 - s)
    } else {
        c + (1.0 - c) * s
    }
}

/// A brush component's coverage at mask-space point `(u, v)`, in the exact order
/// a production unit transcribes:
///
/// ```text
/// c = 0
/// for stroke in strokes:                   -- stored order
///     s = the stroke's coverage at (u, v)
///     c = c * (1 - s)                      if stroke.erase
///     c = c + (1 - c) * s                  otherwise
/// ```
///
/// A component with no strokes covers nothing: `c = 0`, which is the same answer
/// an empty component list gives the composition above.
///
/// `rgb` is the pixel the operation the mask modulates receives, which only a
/// stroke carrying a [`ColourLimit`] reads.
pub fn brush_coverage(brush: &Brush, stage: &Stage, u: f64, v: f64, rgb_linear: [f64; 3]) -> f64 {
    let mut c = 0.0;
    for stroke in &brush.strokes {
        let segments = brush_segments(stroke, stage);
        let s = stroke_coverage(stroke, &segments, u, v, rgb_linear);
        c = accumulate(c, s, stroke.erase);
    }
    c
}

/// The frozen legality rule for a stroke's radius. A radius is a stored
/// mask-space distance like any other, so it takes [`distance_is_legal`] and not
/// a rule of its own: the floor is what bounds the divisor `band = R · f` by
/// `1e4 / f` instead of guarding it, and a radius below it is refused when the
/// component compiles rather than clamped.
pub fn brush_size_is_legal(stroke: &BrushStroke) -> bool {
    distance_is_legal(stroke.size)
}

/// A conservative mask-space rectangle of one brush component's support, as
/// `[u0, u1] x [v0, v1]`, or `None` when the component covers nothing anywhere.
///
/// Coverage is exactly zero wherever every stroke's is, and a stroke's is
/// exactly zero wherever `d > R` on both branches of the profile, so the
/// component's support is the union of its **add** strokes' segment boxes each
/// grown by that stroke's own radius. An erase stroke never creates coverage —
/// `c · (1 − s)` cannot raise `c` — so it contributes nothing to the box, and a
/// stroke whose flow is exactly zero contributes nothing either, because
/// `s = profile · 0.0` is exactly `0.0` and the fold is an exact identity there.
pub fn brush_bounds(brush: &Brush, stage: &Stage) -> Option<[f64; 4]> {
    let mut box_ = None::<[f64; 4]>;
    for stroke in &brush.strokes {
        if stroke.erase || stroke.flow == 0.0 {
            continue;
        }
        for [x, y] in &stroke.points {
            let (u, v) = stage.position_uv(*x, *y);
            let grown = [
                u - stroke.size,
                v - stroke.size,
                u + stroke.size,
                v + stroke.size,
            ];
            box_ = Some(match box_ {
                None => grown,
                Some(held) => [
                    held[0].min(grown[0]),
                    held[1].min(grown[1]),
                    held[2].max(grown[2]),
                    held[3].max(grown[3]),
                ],
            });
        }
    }
    box_
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
