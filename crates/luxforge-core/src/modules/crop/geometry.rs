//! Crop geometry: the rotated box of one input stage, payload validation, output rounding and the
//! coverage and fitting math that the crop module, the host renderer and the desktop share.
//!
//! Everything here is pure, allocation-free f64 arithmetic over one immutable input stage. Box
//! space has its origin at the top-left corner of the rotated bounding box, x to the right and y
//! down, and a positive angle turns the image clockwise on screen. The formulas are the geometry
//! contract in `docs/specs/single-image.md`.
#[cfg(test)]
use crate::ErrorKind;
use crate::{Error, Orientation};
use serde::{Deserialize, Serialize};

/// The most counter-clockwise straightening angle a crop payload may carry, in degrees.
pub const MIN_ANGLE: f64 = -45.0;
/// The most clockwise straightening angle a crop payload may carry, in degrees.
pub const MAX_ANGLE: f64 = 45.0;
/// How far outside the input stage a rounded output corner may map, in source pixels.
pub const COVERAGE_TOLERANCE: f64 = 0.001;

/// Slack for normalized payload bounds and for whole-pixel snapping, in normalized or box units.
const EPSILON: f64 = 1e-9;

/// Slack the growth and movement limits allow themselves, in source pixels. A limit that lands
/// exactly on a boundary must survive floating-point rounding, and a rectangle already at the
/// boundary must still scale by exactly 1 so fitting is idempotent. This is a thousandth of
/// [`COVERAGE_TOLERANCE`], so whatever those limits return is covered with room to spare.
const LIMIT_SLACK: f64 = COVERAGE_TOLERANCE / 1000.0;

/// One ray in box space: where it starts and the direction it grows in.
type Ray = ((f64, f64), (f64, f64));

/// The persisted crop payload: a straightening angle and the rectangle normalized to the rotated
/// box, so the numbers do not depend on how the box was computed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropPayload {
    /// Degrees, clockwise on screen, within [`MIN_ANGLE`]..=[`MAX_ANGLE`].
    pub angle: f64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CropPayload {
    /// No straightening and the whole rotated box: the payload `crop-reset` writes.
    pub const NEUTRAL: Self = Self {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    /// Exactly the neutral payload, which renders as the untouched input stage.
    pub fn is_neutral(&self) -> bool {
        *self == Self::NEUTRAL
    }

    /// Structural validation independent of any stage: finite numbers, an in-range angle, positive
    /// extents and a rectangle inside the normalized box. Bounds carry a 1e-9 tolerance so a
    /// rectangle normalized from whole box pixels is never rejected by rounding alone.
    pub fn validate(&self) -> Result<(), Error> {
        for (name, value) in [
            ("angle", self.angle),
            ("x", self.x),
            ("y", self.y),
            ("width", self.width),
            ("height", self.height),
        ] {
            if !value.is_finite() {
                return Err(Error::validation(format!(
                    "crop {name} must be a finite number"
                )));
            }
        }
        if self.angle < MIN_ANGLE || self.angle > MAX_ANGLE {
            return Err(Error::validation(format!(
                "crop angle {} is outside {MIN_ANGLE} to {MAX_ANGLE} degrees",
                self.angle
            )));
        }
        if self.x < -EPSILON || self.y < -EPSILON {
            return Err(Error::validation(format!(
                "crop origin ({}, {}) is outside the rotated box",
                self.x, self.y
            )));
        }
        if self.width <= 0.0 || self.height <= 0.0 {
            return Err(Error::validation(format!(
                "crop extents ({} x {}) must be positive",
                self.width, self.height
            )));
        }
        if self.x + self.width > 1.0 + EPSILON || self.y + self.height > 1.0 + EPSILON {
            return Err(Error::validation(format!(
                "crop rectangle ({}, {}, {} x {}) leaves the rotated box",
                self.x, self.y, self.width, self.height
            )));
        }
        Ok(())
    }

    /// The whole-pixel output stage this payload produces on `stage`, rounding half away from zero
    /// with extents of at least one pixel.
    ///
    /// The payload is valid only when all four corners of the rounded rectangle map back into the
    /// input stage within [`COVERAGE_TOLERANCE`]; because the rotated source is convex that proves
    /// every output pixel samples real content, so an empty corner can never reach a render. The
    /// error names the offending corner, where it maps and how far outside it lands.
    pub fn output_rect(&self, stage: &CropStage) -> Result<OutputRect, Error> {
        self.validate()?;
        let (box_width, box_height) = stage.bounding_box();
        let x = (self.x * box_width).round();
        let y = (self.y * box_height).round();
        let width = (self.width * box_width).round().max(1.0);
        let height = (self.height * box_height).round().max(1.0);
        let input_width = f64::from(stage.width);
        let input_height = f64::from(stage.height);
        for (name, corner) in [
            ("top-left", (x, y)),
            ("top-right", (x + width, y)),
            ("bottom-left", (x, y + height)),
            ("bottom-right", (x + width, y + height)),
        ] {
            let (u, v) = stage.to_input(corner.0, corner.1);
            let outside = [-u, u - input_width, -v, v - input_height]
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max);
            if outside > COVERAGE_TOLERANCE {
                return Err(Error::validation(format!(
                    "crop {name} corner maps to ({u:.3}, {v:.3}), {outside:.3} px outside the {}x{} input stage",
                    stage.width, stage.height
                )));
            }
        }
        Ok(OutputRect {
            x: x as i64,
            y: y as i64,
            width: width as u32,
            height: height as u32,
        })
    }

    /// [`Self::output_rect`] for rendering at a stage other than the one the payload was fitted
    /// on, such as a display proxy: a re-rounding of the whole-pixel rectangle at another scale can
    /// put a corner less than one source pixel outside the input stage, and that corner is shrunk
    /// back in by whole pixels rather than refused. At the fitted stage the rectangle passes
    /// [`Self::output_rect`] unchanged, so a payload the desktop or the fitting functions produced
    /// never changes here; a payload that is genuinely outside by a pixel or more still fails with
    /// the same error, so an empty corner can never reach a render.
    pub fn output_rect_covered(&self, stage: &CropStage) -> Result<OutputRect, Error> {
        self.validate()?;
        let (box_width, box_height) = stage.bounding_box();
        let mut x = (self.x * box_width).round();
        let mut y = (self.y * box_height).round();
        let mut width = (self.width * box_width).round().max(1.0);
        let mut height = (self.height * box_height).round().max(1.0);
        let input_width = f64::from(stage.width);
        let input_height = f64::from(stage.height);
        // Rounding the origin and the extent each move a corner by at most half a pixel, so one
        // pixel per side covers every re-rounding; a second pass settles the corner the first one
        // shares with another side.
        for _ in 0..=2 {
            let mut shrink_left = false;
            let mut shrink_right = false;
            let mut shrink_top = false;
            let mut shrink_bottom = false;
            let mut worst = f64::NEG_INFINITY;
            for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
                let corner = (
                    if left { x } else { x + width },
                    if top { y } else { y + height },
                );
                let (u, v) = stage.to_input(corner.0, corner.1);
                let outside = [-u, u - input_width, -v, v - input_height]
                    .into_iter()
                    .fold(f64::NEG_INFINITY, f64::max);
                if outside > COVERAGE_TOLERANCE {
                    worst = worst.max(outside);
                    shrink_left |= left;
                    shrink_right |= !left;
                    shrink_top |= top;
                    shrink_bottom |= !top;
                }
            }
            if worst <= COVERAGE_TOLERANCE {
                return Ok(OutputRect {
                    x: x as i64,
                    y: y as i64,
                    width: width as u32,
                    height: height as u32,
                });
            }
            if worst >= 1.0 + COVERAGE_TOLERANCE || width <= 2.0 || height <= 2.0 {
                break;
            }
            // Only a side whose corners are all outside is moved; a corner outside on one side
            // and inside on the other is the other side's rounding.
            if shrink_left && !shrink_right {
                x += 1.0;
                width -= 1.0;
            } else if shrink_right && !shrink_left {
                width -= 1.0;
            } else if shrink_left && shrink_right {
                x += 1.0;
                width -= 2.0;
            }
            if shrink_top && !shrink_bottom {
                y += 1.0;
                height -= 1.0;
            } else if shrink_bottom && !shrink_top {
                height -= 1.0;
            } else if shrink_top && shrink_bottom {
                y += 1.0;
                height -= 2.0;
            }
        }
        self.output_rect(stage)
    }

    /// This payload re-expressed for its input stage turned or reflected by `orientation`: it
    /// selects the same content, so the crop's new output is its old output under the same
    /// orientation. `input` is the crop's input stage before the orientation. This is how a
    /// transform goes ahead of a crop without moving what the crop shows; the rules for a
    /// straightened rectangle are [`CropStage::carried`]'s.
    pub fn carried(&self, input: (u32, u32), orientation: Orientation) -> Result<Self, Error> {
        if self.is_neutral() {
            return Ok(Self::NEUTRAL);
        }
        let stage = CropStage {
            width: input.0,
            height: input.1,
            angle: self.angle,
        };
        let output = self.output_rect(&stage)?;
        let (stage, rect) = stage.carried(
            BoxRect {
                x: output.x as f64,
                y: output.y as f64,
                width: f64::from(output.width),
                height: f64::from(output.height),
            },
            orientation,
        );
        Ok(rect.normalized(&stage))
    }
}

/// The whole-pixel rectangle a crop payload addresses in box space, and therefore the stage the
/// crop layer produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputRect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

/// One rectangle in box pixel units with a top-left origin. Extents are lengths, not coordinates,
/// so they survive an angle change unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl BoxRect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn from_center(center: (f64, f64), width: f64, height: f64) -> Self {
        Self {
            x: center.0 - width / 2.0,
            y: center.1 - height / 2.0,
            width,
            height,
        }
    }

    /// Top-left, top-right, bottom-left, bottom-right; the far edges are the outer boundary of the
    /// last pixel, which is what coverage validates.
    pub fn corners(&self) -> [(f64, f64); 4] {
        [
            (self.x, self.y),
            (self.x + self.width, self.y),
            (self.x, self.y + self.height),
            (self.x + self.width, self.y + self.height),
        ]
    }

    pub fn scaled_about_center(&self, scale: f64) -> Self {
        Self::from_center(self.center(), self.width * scale, self.height * scale)
    }

    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }

    /// The largest whole-pixel rectangle inside this one: the origin rounds up, the far edge rounds
    /// down and each extent is at least one pixel. Snapping only ever shrinks a rectangle, except
    /// for that one-pixel floor.
    pub fn snapped_inward(&self) -> Self {
        let x = (self.x - EPSILON).ceil();
        let y = (self.y - EPSILON).ceil();
        let far_x = (self.x + self.width + EPSILON).floor();
        let far_y = (self.y + self.height + EPSILON).floor();
        Self {
            x,
            y,
            width: (far_x - x).max(1.0),
            height: (far_y - y).max(1.0),
        }
    }

    /// Divide out the rotated box of `stage` to get the persisted payload.
    pub fn normalized(&self, stage: &CropStage) -> CropPayload {
        let (box_width, box_height) = stage.bounding_box();
        CropPayload {
            angle: stage.angle,
            x: self.x / box_width,
            y: self.y / box_height,
            width: self.width / box_width,
            height: self.height / box_height,
        }
    }
}

/// Which edge of a rectangle a free drag moves; the opposite edge stays fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// One input stage seen through a straightening angle: the rotated bounding box, the forward and
/// inverse mappings, and the coverage and fitting math over the rotated source polygon.
///
/// `width` and `height` are the layer's input stage in pixels and must be at least one; `angle` is
/// a validated payload angle in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropStage {
    pub width: u32,
    pub height: u32,
    pub angle: f64,
}

impl CropStage {
    fn trigonometry(&self) -> (f64, f64) {
        let radians = self.angle.to_radians();
        (radians.cos(), radians.sin())
    }

    /// `(BW, BH)`: the axis-aligned bounding box of the input rotated about its center.
    pub fn bounding_box(&self) -> (f64, f64) {
        let (cos, sin) = self.trigonometry();
        let width = f64::from(self.width);
        let height = f64::from(self.height);
        (
            width * cos.abs() + height * sin.abs(),
            width * sin.abs() + height * cos.abs(),
        )
    }

    /// Input continuous coordinates to box space.
    pub fn to_box(&self, u: f64, v: f64) -> (f64, f64) {
        let (cos, sin) = self.trigonometry();
        let (box_width, box_height) = self.bounding_box();
        let du = u - f64::from(self.width) / 2.0;
        let dv = v - f64::from(self.height) / 2.0;
        (
            cos * du - sin * dv + box_width / 2.0,
            sin * du + cos * dv + box_height / 2.0,
        )
    }

    /// Box space to input continuous coordinates: the inverse of [`CropStage::to_box`].
    pub fn to_input(&self, x: f64, y: f64) -> (f64, f64) {
        let (cos, sin) = self.trigonometry();
        let (box_width, box_height) = self.bounding_box();
        let dx = x - box_width / 2.0;
        let dy = y - box_height / 2.0;
        (
            cos * dx + sin * dy + f64::from(self.width) / 2.0,
            -sin * dx + cos * dy + f64::from(self.height) / 2.0,
        )
    }

    /// [`CropStage::to_input`] composed with an output offset, as the six coefficients
    /// `[m0, m1, m2, m3, m4, m5]` of `u = m0·x' + m1·y' + m2`, `v = m3·x' + m4·y' + m5`, where
    /// `(x', y')` is a continuous coordinate relative to `origin` in box space. This is what the
    /// crop module hands the host's resample primitive.
    pub fn inverse_map(&self, origin: (f64, f64)) -> [f64; 6] {
        let (cos, sin) = self.trigonometry();
        let (u, v) = self.to_input(origin.0, origin.1);
        [cos, sin, u, -sin, cos, v]
    }

    /// Whether a box-space point lies on the rotated source, within [`COVERAGE_TOLERANCE`]. The
    /// rotation is rigid, so the tolerance means the same distance in both spaces.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (u, v) = self.to_input(x, y);
        u >= -COVERAGE_TOLERANCE
            && u <= f64::from(self.width) + COVERAGE_TOLERANCE
            && v >= -COVERAGE_TOLERANCE
            && v <= f64::from(self.height) + COVERAGE_TOLERANCE
    }

    /// The nearest box-space point on the rotated source.
    pub fn project_inside(&self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = self.to_input(x, y);
        self.to_box(
            u.clamp(0.0, f64::from(self.width)),
            v.clamp(0.0, f64::from(self.height)),
        )
    }

    /// Whether all four corners of a rectangle lie on the rotated source. Because the source is
    /// convex this means the whole rectangle does, so every output pixel samples real content.
    pub fn covers(&self, rect: &BoxRect) -> bool {
        rect.corners().into_iter().all(|(x, y)| self.contains(x, y))
    }

    /// The largest `t >= 0` for which every `base + t·direction` still lies on the rotated source,
    /// or infinity when no edge binds. Each ray contributes four linear bounds, one per polygon
    /// edge, so the answer is exact and needs no iteration.
    fn max_parameter(&self, rays: &[Ray]) -> f64 {
        let (cos, sin) = self.trigonometry();
        let width = f64::from(self.width);
        let height = f64::from(self.height);
        let mut limit = f64::INFINITY;
        for &((base_x, base_y), (dx, dy)) in rays {
            let (u, v) = self.to_input(base_x, base_y);
            // The rotation is rigid, so a box-space offset maps through its linear part alone.
            let du = cos * dx + sin * dy;
            let dv = -sin * dx + cos * dy;
            for (slack, rate) in [
                (u + LIMIT_SLACK, du),
                (width - u + LIMIT_SLACK, -du),
                (v + LIMIT_SLACK, dv),
                (height - v + LIMIT_SLACK, -dv),
            ] {
                limit = limit.min(edge_bound(slack, rate));
            }
        }
        limit
    }

    /// The largest scale about `center` that keeps all four corners of the rectangle with those
    /// half extents on the rotated source. Infinity when the rectangle is degenerate and the center
    /// is inside; zero when the center itself is outside. Callers cap it at 1 to avoid growing.
    pub fn max_scale_about(&self, center: (f64, f64), half_width: f64, half_height: f64) -> f64 {
        let half_width = half_width.abs();
        let half_height = half_height.abs();
        self.max_parameter(&[
            (center, (-half_width, -half_height)),
            (center, (half_width, -half_height)),
            (center, (-half_width, half_height)),
            (center, (half_width, half_height)),
        ])
    }

    /// The largest scale of a rectangle whose `anchor` corner is fixed and whose opposite corner
    /// sits at `anchor + offset` at scale 1. This is a locked-ratio corner or side drag: both
    /// components of `offset` scale together, so the ratio never changes.
    pub fn max_scale_about_anchor(&self, anchor: (f64, f64), offset: (f64, f64)) -> f64 {
        self.max_parameter(&[
            (anchor, (0.0, 0.0)),
            (anchor, (offset.0, 0.0)),
            (anchor, (0.0, offset.1)),
            (anchor, offset),
        ])
    }

    /// The largest extent of one moving edge with the opposite edge fixed: a width for
    /// [`Edge::Left`] and [`Edge::Right`], a height for [`Edge::Top`] and [`Edge::Bottom`]. Only
    /// the two corners on the moving edge can leave the source, and the perpendicular extent is
    /// unchanged. Callers clamp the result to their own minimum extent.
    pub fn max_extent(&self, rect: BoxRect, edge: Edge) -> f64 {
        let far_x = rect.x + rect.width;
        let far_y = rect.y + rect.height;
        let (first, second, direction) = match edge {
            Edge::Left => ((far_x, rect.y), (far_x, far_y), (-1.0, 0.0)),
            Edge::Right => ((rect.x, rect.y), (rect.x, far_y), (1.0, 0.0)),
            Edge::Top => ((rect.x, far_y), (far_x, far_y), (0.0, -1.0)),
            Edge::Bottom => ((rect.x, rect.y), (far_x, rect.y), (0.0, 1.0)),
        };
        self.max_parameter(&[(first, direction), (second, direction)])
    }

    /// Fit a rectangle onto the rotated source, keeping the composition and trimming only what an
    /// empty corner would require: keep the center when it is inside and otherwise move it to the
    /// nearest inside point, scale about it by the largest factor up to 1 that keeps every corner
    /// covered, then snap inward to whole box pixels.
    ///
    /// The result is a whole-pixel rectangle, never larger than the request, that passes
    /// [`CropPayload::output_rect`] validation whenever any rectangle of its size fits on the
    /// source at all.
    pub fn fit_about_center(&self, rect: BoxRect) -> BoxRect {
        let center = rect.center();
        let center = if self.contains(center.0, center.1) {
            center
        } else {
            self.project_inside(center.0, center.1)
        };
        let scale = self
            .max_scale_about(center, rect.width / 2.0, rect.height / 2.0)
            .min(1.0);
        let snapped =
            BoxRect::from_center(center, rect.width * scale, rect.height * scale).snapped_inward();
        if self.covers(&snapped) {
            return snapped;
        }
        // Snapping a covered rectangle inward can only shrink it, except for the one-pixel floor on
        // its extents, which can leave a very small rectangle reaching past a steeply rotated
        // source. Keep those whole-pixel extents and move the rectangle the shortest distance onto
        // the exact set of centers that cover its size.
        let (x, y) = self.covered_center(&snapped);
        BoxRect {
            x: (x - snapped.width / 2.0).round(),
            y: (y - snapped.height / 2.0).round(),
            ..snapped
        }
    }

    /// The center nearest `rect`'s own for which a rectangle of its extents is fully covered, with
    /// room for rounding its origin to whole box pixels. In input space that set is the stage eroded
    /// by the rectangle's rotated half extents, so the clamp is exact and needs no search. When the
    /// erosion is empty no rectangle of that size fits and the stage center is the closest possible
    /// answer, which [`CropPayload::output_rect`] then rejects by naming a corner.
    fn covered_center(&self, rect: &BoxRect) -> (f64, f64) {
        let (cos, sin) = self.trigonometry();
        // Rounding a box-space origin moves the center by at most half a pixel on each axis.
        let rounding = (cos.abs() + sin.abs()) / 2.0;
        let margin_u = cos.abs() * rect.width / 2.0 + sin.abs() * rect.height / 2.0 + rounding;
        let margin_v = sin.abs() * rect.width / 2.0 + cos.abs() * rect.height / 2.0 + rounding;
        let (x, y) = rect.center();
        let (u, v) = self.to_input(x, y);
        self.to_box(
            clamp_inside(u, margin_u, f64::from(self.width)),
            clamp_inside(v, margin_v, f64::from(self.height)),
        )
    }

    /// Refit the rectangle of the last frame gesture, expressed in `reference`'s box space, into
    /// this stage's box space: the center keeps addressing the same input point and the pixel
    /// extents are unchanged. Every angle value refits the same reference, so sweeping the angle
    /// away and back returns the reference rectangle with no cumulative trim.
    pub fn refit(&self, reference: &CropStage, rect: BoxRect) -> BoxRect {
        let center = rect.center();
        let (u, v) = reference.to_input(center.0, center.1);
        self.fit_about_center(BoxRect::from_center(
            self.to_box(u, v),
            rect.width,
            rect.height,
        ))
    }

    /// This stage and a whole-pixel rectangle in its box, turned or reflected with the input by
    /// `orientation`: the stage the input has afterwards, at the angle that keeps the same content
    /// straight, and the rectangle that selects the same content in its box.
    ///
    /// The rotated box turns and reflects with its input, and a reflection reverses the
    /// straightening angle, so the mapping is exact in continuous box space. At angle zero the box
    /// is the input, every edge stays on whole pixels and the rectangle is carried exactly. At any
    /// other angle the box extents are not whole pixels, so an edge measured from the far side of
    /// the box lands between pixels: the extents are kept and the origin moves to the nearest whole
    /// box pixel at which they are still covered. Only a rectangle with no such position, one that
    /// touches the rotated source on opposite sides, is fitted instead, which trims it by at most a
    /// pixel on an axis.
    pub fn carried(&self, rect: BoxRect, orientation: Orientation) -> (Self, BoxRect) {
        let (mut box_width, mut box_height) = self.bounding_box();
        let mut stage = *self;
        let mut exact = rect;
        if orientation.mirror {
            exact.x = box_width - (exact.x + exact.width);
            // Written as a difference so an unstraightened crop keeps a positive zero angle.
            stage.angle = 0.0 - stage.angle;
        }
        for _ in 0..orientation.turns {
            // A clockwise quarter turn: box point (x, y) goes to (BH − y, x).
            exact = BoxRect {
                x: box_height - (exact.y + exact.height),
                y: exact.x,
                width: exact.height,
                height: exact.width,
            };
            (stage.width, stage.height) = (stage.height, stage.width);
            (box_width, box_height) = (box_height, box_width);
        }
        // The nearest whole pixel to an edge, and the one on its other side unless it is already
        // whole.
        let near = |value: f64| {
            let nearest = value.round();
            let other = nearest + if nearest > value { -1.0 } else { 1.0 };
            let whole = (value - nearest).abs() <= EPSILON;
            [nearest, if whole { nearest } else { other }]
        };
        let [x_near, x_far] = near(exact.x);
        let [y_near, y_far] = near(exact.y);
        let placed = [
            (x_near, y_near),
            (x_far, y_near),
            (x_near, y_far),
            (x_far, y_far),
        ]
        .into_iter()
        .map(|(x, y)| BoxRect { x, y, ..exact })
        .find(|candidate| stage.covers(candidate))
        .unwrap_or_else(|| stage.fit_about_center(exact));
        (stage, placed)
    }

    /// Clamp a move of a covered rectangle: the horizontal delta is clamped first and the vertical
    /// delta against the already shifted rectangle, so a move into a boundary slides along it
    /// instead of stopping. The moved rectangle is still covered.
    pub fn clamp_translation(&self, rect: BoxRect, dx: f64, dy: f64) -> (f64, f64) {
        let horizontal = dx * self.max_travel(&rect, (dx, 0.0));
        let shifted = rect.translated(horizontal, 0.0);
        let vertical = dy * self.max_travel(&shifted, (0.0, dy));
        (horizontal, vertical)
    }

    /// The fraction of one translation, at most all of it, that keeps every corner covered.
    fn max_travel(&self, rect: &BoxRect, delta: (f64, f64)) -> f64 {
        let corners = rect.corners();
        self.max_parameter(&[
            (corners[0], delta),
            (corners[1], delta),
            (corners[2], delta),
            (corners[3], delta),
        ])
        .min(1.0)
    }
}

/// The largest `t >= 0` with `slack + t·rate >= 0`. A point moving away from this edge, or parallel
/// to it, is never bound by it; one moving towards it stops at the edge, and a start already past it
/// cannot move towards it at all.
fn edge_bound(slack: f64, rate: f64) -> f64 {
    if rate >= 0.0 {
        f64::INFINITY
    } else {
        slack.max(0.0) / -rate
    }
}

/// `value` clamped to `margin..=limit - margin`, or the midpoint when that range is empty.
fn clamp_inside(value: f64, margin: f64, limit: f64) -> f64 {
    if 2.0 * margin >= limit {
        limit / 2.0
    } else {
        value.clamp(margin, limit - margin)
    }
}

/// The largest rectangle with `width / height == ratio` that shares `rect`'s center and fits inside
/// it: what choosing a ratio preset or swapping its orientation does to the current rectangle. A
/// ratio that is not finite and positive returns `rect` unchanged.
pub fn largest_with_ratio_inside(rect: BoxRect, ratio: f64) -> BoxRect {
    if !ratio.is_finite() || ratio <= 0.0 {
        return rect;
    }
    let width = rect.width.min(rect.height * ratio);
    BoxRect::from_center(rect.center(), width, width / ratio)
}

/// The angle a straightening guide commits: the rotation that makes the dragged line horizontal or
/// vertical, whichever is nearer, added to the current angle and clamped to ±45. The line is in the
/// box space of `current_angle`. A zero-length or non-finite line keeps the current angle, and a
/// line at exactly 45° to both axes turns clockwise.
pub fn guide_angle(current_angle: f64, from: (f64, f64), to: (f64, f64)) -> f64 {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    if !dx.is_finite() || !dy.is_finite() || (dx == 0.0 && dy == 0.0) {
        return current_angle;
    }
    // Turning the image by δ turns a line drawn on it by δ, so the nearest axis is at
    // δ ≡ −line (mod 90); reducing that into ±45 picks horizontal or vertical automatically.
    const QUARTER_TURN: f64 = 90.0;
    let turn = -dy.atan2(dx).to_degrees();
    let delta = turn - QUARTER_TURN * (turn / QUARTER_TURN).round();
    (current_angle + delta).clamp(MIN_ANGLE, MAX_ANGLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANGLES: [f64; 9] = [-45.0, -37.5, -22.5, -7.0, 0.0, 3.5, 12.0, 22.5, 45.0];

    fn stage(width: u32, height: u32, angle: f64) -> CropStage {
        CropStage {
            width,
            height,
            angle,
        }
    }

    fn close(actual: f64, expected: f64, tolerance: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{what}: {actual} is not {expected} within {tolerance}"
        );
    }

    /// The rectangle with one moving edge at `extent` and the opposite edge fixed: what the desktop
    /// builds from [`CropStage::max_extent`].
    fn with_extent(rect: BoxRect, edge: Edge, extent: f64) -> BoxRect {
        match edge {
            Edge::Left => BoxRect {
                x: rect.x + rect.width - extent,
                width: extent,
                ..rect
            },
            Edge::Right => BoxRect {
                width: extent,
                ..rect
            },
            Edge::Top => BoxRect {
                y: rect.y + rect.height - extent,
                height: extent,
                ..rect
            },
            Edge::Bottom => BoxRect {
                height: extent,
                ..rect
            },
        }
    }

    /// The eight exact orientations.
    fn orientations() -> Vec<Orientation> {
        [false, true]
            .into_iter()
            .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }))
            .collect()
    }

    /// Where a continuous point of a `size` stage goes under `orientation`, with the stage it goes
    /// to: the reflection, then clockwise quarter turns, as the exact layer maps pixels.
    fn oriented(
        point: (f64, f64),
        size: (f64, f64),
        orientation: Orientation,
    ) -> ((f64, f64), (f64, f64)) {
        let (mut point, mut size) = (point, size);
        if orientation.mirror {
            point = (size.0 - point.0, point.1);
        }
        for _ in 0..orientation.turns {
            point = (size.1 - point.1, point.0);
            size = (size.1, size.0);
        }
        (point, size)
    }

    /// Carrying a crop through each of the eight orientations turns its stage with the input,
    /// reverses the angle for a reflection and frames the same content: exactly at angle zero,
    /// where the inverse orientation carries it back, and otherwise within a box pixel of the
    /// exactly carried rectangle, on whole box pixels, covered, and with its extents kept unless
    /// it touches the rotated source.
    #[test]
    fn carrying_a_crop_through_an_orientation_frames_the_same_content() {
        let mut shifted = 0;
        let mut kept = 0;
        for (width, height) in [(480, 320), (4000, 6000), (7, 5)] {
            for angle in ANGLES {
                let from = stage(width, height, angle);
                let (box_width, box_height) = from.bounding_box();
                for (center, extent) in [
                    ((0.5, 0.5), (0.4, 0.3)),
                    ((0.42, 0.56), (0.5, 0.45)),
                    ((0.3, 0.7), (0.2, 0.25)),
                    // Larger than anything that fits: the largest rectangle, touching the source.
                    ((0.5, 0.5), (4.0, 4.0)),
                ] {
                    let rect = from.fit_about_center(BoxRect::from_center(
                        (box_width * center.0, box_height * center.1),
                        box_width * extent.0,
                        box_height * extent.1,
                    ));
                    // Clear of the source edges by a box pixel on every side, so any move onto
                    // the nearest whole pixel stays covered.
                    let clear = from.covers(&BoxRect {
                        x: rect.x - 1.0,
                        y: rect.y - 1.0,
                        width: rect.width + 2.0,
                        height: rect.height + 2.0,
                    });
                    for orientation in orientations() {
                        let case = format!("{width}x{height} at {angle}° {rect:?} {orientation:?}");
                        let (to, carried) = from.carried(rect, orientation);
                        let odd = orientation.turns % 2 == 1;
                        assert_eq!(
                            (to.width, to.height),
                            if odd {
                                (height, width)
                            } else {
                                (width, height)
                            },
                            "{case}"
                        );
                        assert_eq!(
                            to.angle,
                            if orientation.mirror {
                                0.0 - angle
                            } else {
                                angle
                            },
                            "{case}"
                        );
                        assert!(to.angle != 0.0 || to.angle.is_sign_positive(), "{case}");
                        // Whole box pixels, covered, and exactly what its payload renders.
                        assert_eq!(carried, carried.snapped_inward(), "{case}");
                        assert!(to.covers(&carried), "{case}");
                        assert_eq!(
                            carried.normalized(&to).output_rect(&to).unwrap(),
                            OutputRect {
                                x: carried.x as i64,
                                y: carried.y as i64,
                                width: carried.width as u32,
                                height: carried.height as u32,
                            },
                            "{case}"
                        );
                        // The exactly carried rectangle, from two opposite corners in box space.
                        let size = (box_width, box_height);
                        let ((ax, ay), _) = oriented((rect.x, rect.y), size, orientation);
                        let far = (rect.x + rect.width, rect.y + rect.height);
                        let ((bx, by), _) = oriented(far, size, orientation);
                        let exact = BoxRect {
                            x: ax.min(bx),
                            y: ay.min(by),
                            width: (ax - bx).abs(),
                            height: (ay - by).abs(),
                        };
                        if angle == 0.0 {
                            assert_eq!(carried, exact, "{case}: exact at angle zero");
                            assert_eq!(
                                to.carried(carried, orientation.inverse()),
                                (from, rect),
                                "{case}: the inverse carries it back"
                            );
                        }
                        let (dx, dy) = (
                            carried.center().0 - exact.center().0,
                            carried.center().1 - exact.center().1,
                        );
                        assert!(
                            dx.abs() <= 1.0 && dy.abs() <= 1.0,
                            "{case}: moved {dx}, {dy}"
                        );
                        assert!(
                            carried.width <= exact.width + EPSILON
                                && carried.width >= exact.width - 1.0 - EPSILON
                                && carried.height <= exact.height + EPSILON
                                && carried.height >= exact.height - 1.0 - EPSILON,
                            "{case}: {carried:?} trims {exact:?} by at most a pixel"
                        );
                        // Clear of the source edges, the extents are kept and the rectangle moves
                        // only onto the nearest whole box pixel.
                        if clear {
                            assert_eq!(
                                (carried.width, carried.height),
                                (exact.width, exact.height),
                                "{case}"
                            );
                            assert!(
                                dx.abs() <= 0.5 && dy.abs() <= 0.5,
                                "{case}: moved {dx}, {dy}"
                            );
                        }
                        // The content it frames: the center's input point, carried with the input,
                        // is the carried center's within the same distance, the rotation being rigid.
                        let (u, v) = from.to_input(rect.center().0, rect.center().1);
                        let ((u, v), _) =
                            oriented((u, v), (f64::from(width), f64::from(height)), orientation);
                        let (cu, cv) = to.to_input(carried.center().0, carried.center().1);
                        close(
                            (cu - u).hypot(cv - v),
                            dx.hypot(dy),
                            1e-6,
                            &format!("{case}: content moves as the rectangle does"),
                        );
                        shifted += usize::from(dx != 0.0 || dy != 0.0);
                        kept += usize::from(
                            (carried.width, carried.height) == (exact.width, exact.height),
                        );
                    }
                }
            }
        }
        // Both branches ran: straightened crops do move onto the turned box's pixels, and most
        // keep their extents.
        assert!(
            shifted > 0 && kept > shifted / 2,
            "{shifted} moved, {kept} kept"
        );
    }

    /// The payload form: a neutral crop stays neutral, and any other is the stage form normalized.
    #[test]
    fn a_carried_payload_is_the_carried_rectangle_normalized() {
        for orientation in orientations() {
            assert_eq!(
                CropPayload::NEUTRAL
                    .carried((480, 320), orientation)
                    .unwrap(),
                CropPayload::NEUTRAL
            );
            let from = stage(480, 320, 12.0);
            let (box_width, box_height) = from.bounding_box();
            let rect = from.fit_about_center(BoxRect::from_center(
                (box_width * 0.4, box_height * 0.6),
                box_width * 0.3,
                box_height * 0.3,
            ));
            let (to, carried) = from.carried(rect, orientation);
            assert_eq!(
                rect.normalized(&from)
                    .carried((480, 320), orientation)
                    .unwrap(),
                carried.normalized(&to),
                "{orientation:?}"
            );
        }
        // A payload that is not covered is refused rather than carried.
        let uncovered = CropPayload {
            angle: 20.0,
            ..CropPayload::NEUTRAL
        };
        assert!(uncovered.carried((480, 320), orientations()[1]).is_err());
    }

    /// A whole-box rectangle fitted at full resolution is exact there, and re-rounded at a display
    /// proxy's scale it may put a corner a fraction of a pixel outside the smaller stage. The
    /// covered rectangle shrinks that side by whole pixels and stays within a few pixels of the
    /// scaled rectangle; a payload a whole pixel or more outside still fails as before.
    #[test]
    fn a_fitted_rectangle_stays_covered_when_re_rounded_at_a_proxy_scale() {
        let mut covered_by_shrinking = 0;
        for &angle in &ANGLES {
            for (width, height) in [(6000_u32, 4000_u32), (4000, 6000), (4032, 3024)] {
                let full = stage(width, height, angle);
                let (box_width, box_height) = full.bounding_box();
                let fitted = full.fit_about_center(BoxRect {
                    x: 0.0,
                    y: 0.0,
                    width: box_width,
                    height: box_height,
                });
                let payload = fitted.normalized(&full);
                let exact = payload.output_rect(&full).expect("fitted at its own stage");
                assert_eq!(
                    payload.output_rect_covered(&full).unwrap(),
                    exact,
                    "{angle}"
                );
                for scale in [0.45_f64, 0.3, 0.2727, 0.1] {
                    let proxy = stage(
                        ((f64::from(width) * scale).round() as u32).max(1),
                        ((f64::from(height) * scale).round() as u32).max(1),
                        angle,
                    );
                    let strict = payload.output_rect(&proxy);
                    let covered = payload
                        .output_rect_covered(&proxy)
                        .unwrap_or_else(|error| panic!("{angle} at {scale}: {error}"));
                    if strict.is_err() {
                        covered_by_shrinking += 1;
                    }
                    let (proxy_box_width, proxy_box_height) = proxy.bounding_box();
                    close(
                        covered.width as f64,
                        payload.width * proxy_box_width,
                        4.0,
                        "covered width",
                    );
                    close(
                        covered.height as f64,
                        payload.height * proxy_box_height,
                        4.0,
                        "covered height",
                    );
                    // Every corner of the covered rectangle samples real content.
                    for corner in [
                        (covered.x, covered.y),
                        (covered.x + i64::from(covered.width), covered.y),
                        (covered.x, covered.y + i64::from(covered.height)),
                        (
                            covered.x + i64::from(covered.width),
                            covered.y + i64::from(covered.height),
                        ),
                    ] {
                        let (u, v) = proxy.to_input(corner.0 as f64, corner.1 as f64);
                        assert!(
                            u >= -COVERAGE_TOLERANCE
                                && v >= -COVERAGE_TOLERANCE
                                && u <= f64::from(proxy.width) + COVERAGE_TOLERANCE
                                && v <= f64::from(proxy.height) + COVERAGE_TOLERANCE,
                            "{angle} at {scale}: corner maps to ({u}, {v})"
                        );
                    }
                }
            }
        }
        assert!(
            covered_by_shrinking > 0,
            "no re-rounding ever needed shrinking, so the test proves nothing"
        );
        // A rectangle a whole pixel outside is refused by both.
        let full = stage(480, 320, 0.0);
        let outside = CropPayload {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            ..CropPayload::NEUTRAL
        };
        let wide = stage(482, 320, 0.0);
        let _ = full;
        // At an angle of zero the box is the stage, so a full rectangle re-rounded onto a wider
        // stage maps exactly onto it; to land a pixel outside, shift the origin below zero.
        let shifted = CropPayload {
            x: -2.0 / 482.0,
            ..outside
        };
        assert!(shifted.validate().is_err() || shifted.output_rect_covered(&wide).is_err());
    }

    #[test]
    fn bounding_boxes_and_mappings_match_the_geometry_contract() {
        assert_eq!(stage(480, 320, 0.0).bounding_box(), (480.0, 320.0));
        let diagonal = 800.0 * (2.0_f64).sqrt() / 2.0;
        for angle in [45.0, -45.0] {
            let rotated = stage(480, 320, angle);
            let (width, height) = rotated.bounding_box();
            close(width, diagonal, 1e-9, "45 degree box width");
            close(height, diagonal, 1e-9, "45 degree box height");
        }
        // At +45 the top-left corner of a clockwise-turned image becomes the topmost box point.
        let rotated = stage(480, 320, 45.0);
        let (x, y) = rotated.to_box(0.0, 0.0);
        close(x, 226.274_169_979_695_2, 1e-9, "top-left box x");
        close(y, 0.0, 1e-9, "top-left box y");
        // 90 degrees is out of range: it is a quarter turn, not straightening.
        for angle in [90.0, -90.0, 45.000_001, f64::NAN] {
            assert!(
                CropPayload {
                    angle,
                    ..CropPayload::NEUTRAL
                }
                .validate()
                .is_err(),
                "{angle}"
            );
        }
        for angle in ANGLES {
            let stage = stage(481, 321, angle);
            let (box_width, box_height) = stage.bounding_box();
            for u in [0.0, 1.5, 240.25, 481.0] {
                for v in [0.0, 7.0, 160.75, 321.0] {
                    let (x, y) = stage.to_box(u, v);
                    let (back_u, back_v) = stage.to_input(x, y);
                    close(back_u, u, 1e-9, "inverse u");
                    close(back_v, v, 1e-9, "inverse v");
                    // Every input point lies on the source, and the box holds the whole source.
                    assert!(stage.contains(x, y), "{angle}: ({u}, {v})");
                    assert!(x >= -1e-9 && x <= box_width + 1e-9, "{angle}: box x {x}");
                    assert!(y >= -1e-9 && y <= box_height + 1e-9, "{angle}: box y {y}");
                }
            }
            // The offset form the host resamples through agrees with the mapping it comes from.
            let [m0, m1, m2, m3, m4, m5] = stage.inverse_map((11.0, 23.0));
            for (x, y) in [(0.5, 0.5), (17.5, 4.5)] {
                let (u, v) = stage.to_input(11.0 + x, 23.0 + y);
                close(m0 * x + m1 * y + m2, u, 1e-9, "inverse map u");
                close(m3 * x + m4 * y + m5, v, 1e-9, "inverse map v");
            }
        }
    }

    #[test]
    fn payload_validation_names_the_broken_field() {
        assert!(CropPayload::NEUTRAL.is_neutral());
        assert!(CropPayload::NEUTRAL.validate().is_ok());
        assert!(
            !CropPayload {
                width: 0.999,
                ..CropPayload::NEUTRAL
            }
            .is_neutral()
        );
        for (fragment, payload) in [
            (
                "finite",
                CropPayload {
                    x: f64::INFINITY,
                    ..CropPayload::NEUTRAL
                },
            ),
            (
                "angle",
                CropPayload {
                    angle: 45.5,
                    ..CropPayload::NEUTRAL
                },
            ),
            (
                "origin",
                CropPayload {
                    x: -0.1,
                    width: 0.5,
                    ..CropPayload::NEUTRAL
                },
            ),
            (
                "extents",
                CropPayload {
                    width: 0.0,
                    ..CropPayload::NEUTRAL
                },
            ),
            (
                "leaves the rotated box",
                CropPayload {
                    y: 0.5,
                    height: 0.75,
                    ..CropPayload::NEUTRAL
                },
            ),
        ] {
            let error = payload.validate().expect_err(fragment);
            assert_eq!(error.kind, ErrorKind::Validation, "{fragment}");
            assert!(error.detail.contains(fragment), "{fragment}: {error}");
        }
        // Whole box pixels can normalize to sums a hair over one; the tolerance keeps them valid.
        let stage = stage(481, 321, 0.0);
        let full = BoxRect {
            x: 0.0,
            y: 0.0,
            width: 481.0,
            height: 321.0,
        };
        assert!(full.normalized(&stage).validate().is_ok());
    }

    #[test]
    fn output_rounding_is_exact_at_angle_zero() {
        let stage = stage(480, 320, 0.0);
        assert_eq!(
            CropPayload::NEUTRAL.output_rect(&stage).unwrap(),
            OutputRect {
                x: 0,
                y: 0,
                width: 480,
                height: 320
            }
        );
        for rect in [
            BoxRect {
                x: 37.0,
                y: 11.0,
                width: 200.0,
                height: 150.0,
            },
            BoxRect {
                x: 1.0,
                y: 319.0,
                width: 479.0,
                height: 1.0,
            },
            BoxRect {
                x: 0.0,
                y: 0.0,
                width: 480.0,
                height: 320.0,
            },
        ] {
            let payload = rect.normalized(&stage);
            assert_eq!(payload.angle, 0.0);
            assert_eq!(
                payload.output_rect(&stage).unwrap(),
                OutputRect {
                    x: rect.x as i64,
                    y: rect.y as i64,
                    width: rect.width as u32,
                    height: rect.height as u32
                },
                "{rect:?} must round-trip exactly"
            );
        }
        // An odd stage rounds the same way.
        let odd = self::stage(481, 321, 0.0);
        assert_eq!(
            CropPayload::NEUTRAL.output_rect(&odd).unwrap(),
            OutputRect {
                x: 0,
                y: 0,
                width: 481,
                height: 321
            }
        );
    }

    #[test]
    fn coverage_rejects_a_rectangle_with_an_empty_corner_and_names_it() {
        let stage = stage(480, 320, 5.0);
        let error = CropPayload {
            angle: 5.0,
            ..CropPayload::NEUTRAL
        }
        .output_rect(&stage)
        .expect_err("the whole rotated box has empty corners");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("top-left corner"), "{error}");
        assert!(error.detail.contains("px outside the 480x320"), "{error}");
        // A rectangle that leaves only through its bottom-right corner names that corner instead.
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: 40.0,
            y: 40.0,
            width: box_width - 80.0,
            height: box_height - 80.0,
        });
        assert!(stage.covers(&fitted), "the fitted rectangle is covered");
        assert!(fitted.normalized(&stage).output_rect(&stage).is_ok());
        let stretched = BoxRect {
            width: box_width - fitted.x,
            height: box_height - fitted.y,
            ..fitted
        };
        let error = stretched
            .normalized(&stage)
            .output_rect(&stage)
            .expect_err("the stretched rectangle reaches the box corner");
        // Turning clockwise lifts the source's top-right corner, so that corner leaves first.
        assert!(error.detail.contains("top-right corner"), "{error}");
        assert!(error.detail.contains("corner maps to"), "{error}");
    }

    #[test]
    fn fitting_keeps_the_center_never_grows_and_stays_covered() {
        let stage = stage(480, 320, 10.0);
        let (box_width, box_height) = stage.bounding_box();
        let center = (box_width / 2.0, box_height / 2.0);
        let fitted = stage.fit_about_center(BoxRect::from_center(center, 200.0, 100.0));
        assert!(stage.covers(&fitted));
        // The request is covered, so only snapping to whole box pixels trims it, by under a pixel.
        assert!(
            fitted.width > 199.0 - EPSILON && fitted.width <= 200.0,
            "{fitted:?}"
        );
        assert!(
            fitted.height > 99.0 - EPSILON && fitted.height <= 100.0,
            "{fitted:?}"
        );
        close(fitted.center().0, center.0, 1.0, "center held");
        close(fitted.center().1, center.1, 1.0, "center held");
        // A rectangle that already sits on whole box pixels is returned untouched.
        let whole = BoxRect {
            x: fitted.x,
            y: fitted.y,
            width: 160.0,
            height: 80.0,
        };
        assert_eq!(stage.fit_about_center(whole), whole);
        assert_eq!(
            stage.fit_about_center(fitted),
            fitted,
            "fitting a fitted rectangle changes nothing"
        );
        // Off-center, near-boundary, portrait and landscape requests at every golden angle.
        for angle in ANGLES {
            for &(width, height) in &[(480_u32, 320_u32), (320, 480), (97, 640)] {
                let stage = self::stage(width, height, angle);
                let (box_width, box_height) = stage.bounding_box();
                for request in [
                    BoxRect {
                        x: 0.0,
                        y: 0.0,
                        width: box_width,
                        height: box_height,
                    },
                    BoxRect {
                        x: -40.0,
                        y: box_height - 30.0,
                        width: 120.0,
                        height: 90.0,
                    },
                    BoxRect::from_center(
                        (box_width * 0.75, box_height * 0.25),
                        box_width * 0.5,
                        box_height * 0.5,
                    ),
                    BoxRect::from_center((box_width / 2.0, box_height / 2.0), 30.0, 240.0),
                ] {
                    let fitted = stage.fit_about_center(request);
                    let case = format!("{width}x{height} at {angle} from {request:?}");
                    assert!(stage.covers(&fitted), "{case}: {fitted:?} is not covered");
                    assert!(
                        fitted.width <= request.width.max(1.0) + EPSILON
                            && fitted.height <= request.height.max(1.0) + EPSILON,
                        "{case}: {fitted:?} grew"
                    );
                    assert_eq!(fitted, fitted.snapped_inward(), "{case}: not whole pixels");
                    assert_eq!(
                        stage.fit_about_center(fitted),
                        fitted,
                        "{case}: not idempotent"
                    );
                    fitted
                        .normalized(&stage)
                        .output_rect(&stage)
                        .unwrap_or_else(|error| panic!("{case}: {error}"));
                }
            }
        }
    }

    #[test]
    fn sweeping_the_angle_away_and_back_returns_the_reference_rectangle() {
        let reference_stage = stage(480, 320, 12.0);
        let reference = reference_stage.fit_about_center(BoxRect::from_center(
            reference_stage.to_box(300.0, 120.0),
            260.0,
            180.0,
        ));
        assert!(reference_stage.covers(&reference));
        let mut trimmed = 0;
        for angle in ANGLES {
            let swept = CropStage {
                angle,
                ..reference_stage
            };
            let rect = swept.refit(&reference_stage, reference);
            assert!(swept.covers(&rect), "{angle}: {rect:?}");
            assert!(
                rect.normalized(&swept).output_rect(&swept).is_ok(),
                "{angle}"
            );
            assert!(rect.width <= reference.width && rect.height <= reference.height);
            if rect.width < reference.width || rect.height < reference.height {
                trimmed += 1;
            }
            // Returning to the reference angle from anywhere restores the reference exactly.
            assert_eq!(
                reference_stage.refit(&reference_stage, reference),
                reference,
                "{angle}: the reference must survive the sweep"
            );
        }
        assert!(trimmed > 0, "steep angles must trim the reference");
    }

    #[test]
    fn clamping_a_move_slides_along_the_boundary() {
        let stage = stage(480, 320, 20.0);
        let rect = stage.fit_about_center(BoxRect::from_center(
            stage.to_box(240.0, 160.0),
            300.0,
            200.0,
        ));
        assert!(stage.covers(&rect));
        let (dx, dy) = stage.clamp_translation(rect, 1000.0, 1000.0);
        assert!(dx > 0.0, "the rectangle moves as far right as it can");
        assert!(stage.covers(&rect.translated(dx, dy)));
        assert!(
            !stage.covers(&rect.translated(dx + 0.05, 0.0)),
            "the horizontal clamp is tight"
        );
        assert!(
            !stage.covers(&rect.translated(dx, dy + 0.05)),
            "the vertical clamp is tight"
        );
        // Clamping the axes in turn is exactly what makes the rectangle slide along a boundary.
        let (horizontal, _) = stage.clamp_translation(rect, 1000.0, 0.0);
        assert_eq!(horizontal, dx);
        let against_edge = rect.translated(dx, 0.0);
        let (_, up) = stage.clamp_translation(against_edge, 0.0, -1000.0);
        let (_, down) = stage.clamp_translation(against_edge, 0.0, 1000.0);
        assert!(
            up < 0.0 || down > 0.0,
            "a rectangle against one boundary still slides along it"
        );
        assert_eq!(dy, down);
        // A move that needs no clamping is returned unchanged, and a blocked axis returns zero.
        assert_eq!(stage.clamp_translation(rect, 1.0, -2.0), (1.0, -2.0));
        let (blocked, _) = stage.clamp_translation(against_edge, 1000.0, 0.0);
        close(blocked, 0.0, 0.01, "already against the boundary");
    }

    #[test]
    fn edge_and_anchor_limits_stop_at_the_boundary() {
        let stage = stage(480, 320, 15.0);
        let rect = stage.fit_about_center(BoxRect::from_center(
            stage.to_box(200.0, 140.0),
            150.0,
            120.0,
        ));
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let extent = stage.max_extent(rect, edge);
            assert!(extent.is_finite() && extent > 0.0, "{edge:?}: {extent}");
            let grown = with_extent(rect, edge, extent);
            assert!(stage.covers(&grown), "{edge:?}: {grown:?}");
            assert!(
                !stage.covers(&with_extent(rect, edge, extent + 0.05)),
                "{edge:?}: the edge limit is tight"
            );
            match edge {
                Edge::Left | Edge::Right => {
                    assert_eq!((grown.y, grown.height), (rect.y, rect.height));
                }
                Edge::Top | Edge::Bottom => {
                    assert_eq!((grown.x, grown.width), (rect.x, rect.width));
                }
            }
        }
        // A locked-ratio corner drag scales both extents about the fixed opposite corner.
        let anchor = (rect.x, rect.y);
        let offset = (rect.width, rect.height);
        let scale = stage.max_scale_about_anchor(anchor, offset);
        assert!(scale > 1.0, "this rectangle can still grow: {scale}");
        for (factor, covered) in [(scale, true), (scale + 0.01, false)] {
            let scaled = BoxRect {
                x: anchor.0,
                y: anchor.1,
                width: offset.0 * factor,
                height: offset.1 * factor,
            };
            assert_eq!(stage.covers(&scaled), covered, "anchor scale {factor}");
            close(
                scaled.width / scaled.height,
                rect.width / rect.height,
                1e-9,
                "locked ratio",
            );
        }
        // Option scaling about the fixed center uses the same bounds.
        let center = rect.center();
        let scale = stage.max_scale_about(center, rect.width / 2.0, rect.height / 2.0);
        assert!(stage.covers(&rect.scaled_about_center(scale)));
        assert!(!stage.covers(&rect.scaled_about_center(scale + 0.01)));
        assert_eq!(rect.scaled_about_center(scale).center(), center);
        // A center outside the source admits no scale at all.
        assert_eq!(stage.max_scale_about((-50.0, -50.0), 10.0, 10.0), 0.0);
    }

    #[test]
    fn largest_ratio_rectangle_matches_hand_computed_cases() {
        let landscape = BoxRect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        let portrait = BoxRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 200.0,
        };
        assert_eq!(
            largest_with_ratio_inside(landscape, 1.0),
            BoxRect {
                x: 50.0,
                y: 0.0,
                width: 100.0,
                height: 100.0
            }
        );
        assert_eq!(
            largest_with_ratio_inside(landscape, 1.5),
            BoxRect {
                x: 25.0,
                y: 0.0,
                width: 150.0,
                height: 100.0
            }
        );
        assert_eq!(
            largest_with_ratio_inside(portrait, 1.0),
            BoxRect {
                x: 10.0,
                y: 70.0,
                width: 100.0,
                height: 100.0
            }
        );
        assert_eq!(
            largest_with_ratio_inside(portrait, 1.5),
            BoxRect {
                x: 10.0,
                y: 120.0 - 100.0 / 3.0,
                width: 100.0,
                height: 100.0 / 1.5
            }
        );
        // 2:3 inside a landscape rectangle is limited by its height, and the center never moves.
        let swapped = largest_with_ratio_inside(landscape, 2.0 / 3.0);
        close(swapped.width, 200.0 / 3.0, 1e-9, "2:3 width");
        close(swapped.height, 100.0, 1e-9, "2:3 height");
        for (rect, ratio) in [
            (landscape, 1.0),
            (landscape, 16.0 / 9.0),
            (portrait, 2.0 / 3.0),
            (portrait, 4.0 / 3.0),
        ] {
            let inside = largest_with_ratio_inside(rect, ratio);
            assert_eq!(inside.center(), rect.center(), "the center is kept");
            assert!(inside.width <= rect.width + 1e-9 && inside.height <= rect.height + 1e-9);
            close(inside.width / inside.height, ratio, 1e-9, "ratio");
        }
        // A ratio that is not a positive number changes nothing.
        for ratio in [0.0, -1.5, f64::NAN, f64::INFINITY] {
            assert_eq!(largest_with_ratio_inside(landscape, ratio), landscape);
        }
    }

    #[test]
    fn the_guide_turns_to_the_nearer_axis_and_clamps() {
        let tilt = (0.05_f64).atan().to_degrees();
        // A line falling to the right is levelled by turning counter-clockwise.
        close(
            guide_angle(0.0, (0.0, 0.0), (100.0, 5.0)),
            -tilt,
            1e-9,
            "near horizontal",
        );
        close(
            guide_angle(0.0, (0.0, 0.0), (100.0, -5.0)),
            tilt,
            1e-9,
            "near horizontal, rising",
        );
        // A near-vertical line snaps to the vertical axis instead.
        close(
            guide_angle(0.0, (0.0, 0.0), (5.0, 100.0)),
            tilt,
            1e-9,
            "near vertical",
        );
        close(
            guide_angle(0.0, (0.0, 0.0), (-5.0, 100.0)),
            -tilt,
            1e-9,
            "near vertical, leaning left",
        );
        // The correction is added to the current angle, and dragging either way is the same line.
        close(
            guide_angle(10.0, (0.0, 0.0), (100.0, 5.0)),
            10.0 - tilt,
            1e-9,
            "added to the current angle",
        );
        close(
            guide_angle(0.0, (100.0, 5.0), (0.0, 0.0)),
            guide_angle(0.0, (0.0, 0.0), (100.0, 5.0)),
            1e-12,
            "dragging the same line backwards",
        );
        // Exactly diagonal lines turn clockwise, and the result never leaves the legal range.
        assert_eq!(guide_angle(0.0, (0.0, 0.0), (100.0, 100.0)), MAX_ANGLE);
        assert_eq!(guide_angle(40.0, (0.0, 0.0), (100.0, 100.0)), MAX_ANGLE);
        assert_eq!(guide_angle(-40.0, (0.0, 0.0), (100.0, -100.0)), MIN_ANGLE);
        for to in [(0.0, 0.0), (f64::NAN, 1.0)] {
            assert_eq!(guide_angle(7.5, (0.0, 0.0), to), 7.5, "{to:?}");
        }
        // Every drag leaves a payload-legal angle.
        for from in [(0.0, 0.0), (12.5, -3.0)] {
            for to in [(1.0, 0.0), (0.0, 1.0), (-7.0, 7.1), (3.0, -80.0)] {
                let angle = guide_angle(30.0, from, to);
                assert!(
                    CropPayload {
                        angle,
                        ..CropPayload::NEUTRAL
                    }
                    .validate()
                    .is_ok(),
                    "{from:?} to {to:?} gave {angle}"
                );
            }
        }
    }

    #[test]
    fn every_fitted_rectangle_has_a_valid_output_stage() {
        let mut checked = 0;
        for angle in ANGLES {
            for &(width, height) in &[(480_u32, 320_u32), (321, 481), (64, 64)] {
                let stage = self::stage(width, height, angle);
                let (box_width, box_height) = stage.bounding_box();
                for fraction_x in [0.0, 0.17, 0.5, 0.83, 1.0] {
                    for fraction_y in [0.0, 0.31, 0.5, 1.0] {
                        let center = (box_width * fraction_x, box_height * fraction_y);
                        for &(rect_width, rect_height) in &[
                            (40.0, 40.0),
                            (box_width, box_height),
                            (box_width * 0.75, box_height * 0.2),
                            (12.0, box_height),
                        ] {
                            let case = format!(
                                "{width}x{height} at {angle}, center {center:?}, size {rect_width}x{rect_height}"
                            );
                            let valid = |rect: BoxRect, what: &str| {
                                assert!(stage.covers(&rect), "{case}: {what} {rect:?} uncovered");
                                rect.normalized(&stage)
                                    .output_rect(&stage)
                                    .unwrap_or_else(|error| panic!("{case}: {what}: {error}"));
                            };
                            let fitted = stage.fit_about_center(BoxRect::from_center(
                                center,
                                rect_width,
                                rect_height,
                            ));
                            valid(fitted, "fitted");
                            valid(stage.fit_about_center(fitted), "refitted");
                            for ratio in [1.0, 1.5, 16.0 / 9.0, 2.0 / 3.0] {
                                valid(
                                    stage
                                        .fit_about_center(largest_with_ratio_inside(fitted, ratio)),
                                    "ratio",
                                );
                            }
                            for swept in ANGLES {
                                let target = CropStage {
                                    angle: swept,
                                    ..stage
                                };
                                let rect = target.refit(&stage, fitted);
                                assert!(target.covers(&rect), "{case}: swept to {swept}");
                                rect.normalized(&target)
                                    .output_rect(&target)
                                    .unwrap_or_else(|error| {
                                        panic!("{case}: swept to {swept}: {error}")
                                    });
                            }
                            for (dx, dy) in [(37.0, -19.0), (-1000.0, 500.0), (0.0, 1e4)] {
                                let (mx, my) = stage.clamp_translation(fitted, dx, dy);
                                let moved = fitted.translated(mx, my);
                                assert!(stage.covers(&moved), "{case}: moved {mx}, {my}");
                                // A dragged rectangle is committed through the same fit, which is
                                // what turns its continuous position back into whole box pixels.
                                valid(stage.fit_about_center(moved), "moved");
                                if moved.width >= 2.0 && moved.height >= 2.0 {
                                    valid(moved.snapped_inward(), "snapped");
                                }
                            }
                            for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                                let extent = stage.max_extent(fitted, edge);
                                assert!(extent.is_finite(), "{case}: {edge:?} is unbounded");
                                valid(
                                    with_extent(fitted, edge, extent.floor().max(1.0)),
                                    "edge drag",
                                );
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(checked, ANGLES.len() * 3 * 5 * 4 * 4);
    }
}
