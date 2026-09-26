//! The radial gradient's shape editor: an ellipse of mask space, its feather ring and its grips.
//!
//! The figure is the boundary ellipse, where coverage reaches zero, and the feather ring at
//! `1 - feather/100` of it, where the ramp starts, with the design's set of handles: four radius
//! handles, a centre, a rotation grip and the ring. Both ellipses and every handle are placed through
//! mask space, `u = x · W/H`, `v = y` — the spelling the host compiles a radial through — so they
//! stay a circle in pixels at any aspect ratio and follow a crop or a quarter turn with the picture.
use super::editor::{DrawnShape, Grab, MaskHandle, Pen, ShapeEditor, finite};
use luxforge_core::mask::{
    ANGLE_MAX, ANGLE_MIN, DISTANCE_MAX, DISTANCE_MIN, FEATHER_MAX, FEATHER_MIN, POSITION_MAX,
    POSITION_MIN, RadialGradient,
};
use serde_json::Value;

/// The kind token this editor draws, from the host's own vocabulary.
pub(crate) const KIND: &str = "radial";

/// An ellipse in the middle of the frame, a quarter of the stage's height across, upright, with the
/// ramp starting halfway out. Inside is selected, so this is a soft-edged circle over the middle of
/// the picture — what a person drawing around a face starts from.
pub(crate) const NEUTRAL_RADIAL: RadialGradient = RadialGradient {
    x: 0.5,
    y: 0.5,
    radius_x: 0.25,
    radius_y: 0.25,
    angle: 0.0,
    feather: 50.0,
};

/// How far beyond the `+x` radius handle the rotation grip sits, in mask-space units.
const ROTATION_REACH: f64 = 0.06;

/// `cos(45°)`, which is also `sin(45°)`: where the feather ring's handle sits on the ellipse's own
/// parametrization.
const DIAGONAL: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// The smallest relative radius the feather **grip** is drawn at. At `feather = 100` the ring itself
/// has collapsed to the centre — which is the truth, and the drawn ring shows it — but a grip under
/// the centre handle would be a grip nobody could take hold of again. The drag reads the pointer's
/// travel from wherever the press landed, so flooring where the grip sits changes no arithmetic: it
/// only keeps the feather reachable by pointer as well as by its number field.
const MIN_RING_GRIP: f64 = 0.12;

/// The handles, in hit order: the ones on a specific point of the figure win over the centre, which
/// moves the whole ellipse and is the last resort.
const HANDLES: [MaskHandle; 7] = [
    MaskHandle::RadiusPlusX,
    MaskHandle::RadiusMinusX,
    MaskHandle::RadiusPlusY,
    MaskHandle::RadiusMinusY,
    MaskHandle::Rotation,
    MaskHandle::Feather,
    MaskHandle::Centre,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RadialEditor {
    shape: RadialGradient,
    grab: Option<Grab<RadialGradient>>,
}

/// The kind table's row: a stored payload, or the neutral ellipse, as a committable shape. A
/// payload this editor cannot read opens nothing.
pub(super) fn open(stored: Option<&Value>, _brush: super::Brush) -> Option<DrawnShape> {
    let shape = match stored {
        Some(payload) => serde_json::from_value(payload.clone()).ok()?,
        None => NEUTRAL_RADIAL,
    };
    Some(DrawnShape::Radial(RadialEditor {
        shape: bounded(shape),
        grab: None,
    }))
}

impl RadialEditor {
    /// Where one handle sits, in normalized content coordinates, or `None` when it belongs to
    /// another kind's figure.
    fn point(&self, handle: MaskHandle, aspect: f64) -> Option<(f64, f64)> {
        let radial = self.shape;
        let ellipse = Ellipse::new(radial, aspect);
        let (du, dv) = match handle {
            MaskHandle::Centre => (0.0, 0.0),
            MaskHandle::RadiusPlusX => ellipse.local(radial.radius_x, 0.0),
            MaskHandle::RadiusMinusX => ellipse.local(-radial.radius_x, 0.0),
            MaskHandle::RadiusPlusY => ellipse.local(0.0, radial.radius_y),
            MaskHandle::RadiusMinusY => ellipse.local(0.0, -radial.radius_y),
            MaskHandle::Rotation => ellipse.local(radial.radius_x + ROTATION_REACH, 0.0),
            MaskHandle::Feather => {
                let ring = (1.0 - radial.feather / 100.0).max(MIN_RING_GRIP);
                ellipse.local(
                    ring * radial.radius_x * DIAGONAL,
                    ring * radial.radius_y * DIAGONAL,
                )
            }
            MaskHandle::Start | MaskHandle::Middle | MaskHandle::End | MaskHandle::Extent => {
                return None;
            }
        };
        Some(ellipse.to_content(ellipse.cu + du, ellipse.cv + dv))
    }
}

impl ShapeEditor for RadialEditor {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn values(&self) -> Vec<(&'static str, f64)> {
        let radial = self.shape;
        vec![
            ("x", radial.x),
            ("y", radial.y),
            ("radius_x", radial.radius_x),
            ("radius_y", radial.radius_y),
            ("angle", radial.angle),
            ("feather", radial.feather),
        ]
    }

    /// Each field takes its own declared range, which is the range the host's parser enforces: a
    /// position, a mask-space distance, one turn of degrees and a percentage.
    fn set_field(&mut self, name: &str, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        let within = |low: f64, high: f64| (low..=high).contains(&value);
        match name {
            "x" | "y" if !within(POSITION_MIN, POSITION_MAX) => return false,
            "radius_x" | "radius_y" if !within(DISTANCE_MIN, DISTANCE_MAX) => return false,
            "angle" if !within(ANGLE_MIN, ANGLE_MAX) => return false,
            "feather" if !within(FEATHER_MIN, FEATHER_MAX) => return false,
            _ => {}
        }
        let mut radial = self.shape;
        match name {
            "x" => radial.x = value,
            "y" => radial.y = value,
            "radius_x" => radial.radius_x = value,
            "radius_y" => radial.radius_y = value,
            "angle" => radial.angle = value,
            "feather" => radial.feather = value,
            _ => return false,
        }
        self.shape = bounded(radial);
        true
    }

    fn handles(&self, aspect: f64) -> Vec<(MaskHandle, (f64, f64))> {
        HANDLES
            .iter()
            .filter_map(|handle| self.point(*handle, aspect).map(|point| (*handle, point)))
            .collect()
    }

    fn begin(&mut self, handle: MaskHandle, point: (f64, f64)) {
        if finite(point) {
            self.grab = Some(Grab {
                handle,
                start: self.shape,
                start_point: point,
            });
        }
    }

    /// Every handle moves the payload by the **difference** between where the pointer is now and
    /// where the press landed, measured on the ellipse the gesture started with. That is what makes
    /// a drag away and back return the starting ellipse exactly, and what stops a press a pixel off a
    /// handle from snapping the shape to the pointer.
    fn drag(&mut self, point: (f64, f64), aspect: f64) {
        let Some(grab) = self.grab else {
            return;
        };
        if !finite(point) {
            return;
        }
        let start = grab.start;
        let ellipse = Ellipse::new(start, aspect);
        let (a, b) = ellipse.offset(point);
        let (a0, b0) = ellipse.offset(grab.start_point);
        let moved = match grab.handle {
            MaskHandle::Centre => {
                let delta = grab.travel(point);
                RadialGradient {
                    x: start.x + delta.0,
                    y: start.y + delta.1,
                    ..start
                }
            }
            MaskHandle::RadiusPlusX => RadialGradient {
                radius_x: start.radius_x + (a - a0),
                ..start
            },
            MaskHandle::RadiusMinusX => RadialGradient {
                radius_x: start.radius_x - (a - a0),
                ..start
            },
            MaskHandle::RadiusPlusY => RadialGradient {
                radius_y: start.radius_y + (b - b0),
                ..start
            },
            MaskHandle::RadiusMinusY => RadialGradient {
                radius_y: start.radius_y - (b - b0),
                ..start
            },
            // The grip turns the ellipse by however far the pointer has swung around the centre, so
            // the shape follows the hand rather than jumping to it.
            MaskHandle::Rotation => RadialGradient {
                angle: wrapped(start.angle + (degrees(b, a) - degrees(b0, a0))),
                ..start
            },
            // The ring is at relative radius `1 - feather/100`, so moving out by `d` of a radius
            // takes that much off the feather. The arithmetic stays in the stored percentage, which
            // is what makes a drag that returns to its press return the stored feather bit for bit.
            MaskHandle::Feather => RadialGradient {
                feather: start.feather - (relative(a, b, start) - relative(a0, b0, start)) * 100.0,
                ..start
            },
            // The create gesture's own grab: both radii from one drag, about the fixed centre.
            MaskHandle::Extent => extent(start, (start.x, start.y), point, aspect),
            // Another kind's handle on an ellipse moves nothing rather than moving the wrong thing.
            MaskHandle::Start | MaskHandle::Middle | MaskHandle::End => start,
        };
        self.shape = bounded(moved);
    }

    /// The same stroke a gradient is drawn with, read as an extent: the press sets the centre and the
    /// drag sets both radii, upright, so one drag draws the ellipse a person meant to draw.
    fn sweep(&mut self, from: (f64, f64), to: (f64, f64), aspect: f64) {
        if !finite(from) || !finite(to) {
            return;
        }
        let shape = bounded(RadialGradient {
            x: from.0,
            y: from.1,
            angle: 0.0,
            ..extent(self.shape, from, to, aspect)
        });
        self.shape = shape;
        self.grab = Some(Grab {
            handle: MaskHandle::Extent,
            start: shape,
            start_point: to,
        });
    }

    fn release(&mut self) {
        self.grab = None;
    }

    fn held(&self) -> Option<MaskHandle> {
        self.grab.map(|grab| grab.handle)
    }

    fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    /// The boundary ellipse and the feather ring, and the rotation grip's tether. At `feather = 0`
    /// the two ellipses coincide, which is the truth: the edge is hard.
    fn draw(&self, aspect: f64, _pointer: Option<(f64, f64)>, pen: &mut dyn Pen) {
        let radial = self.shape;
        let ring = 1.0 - radial.feather / 100.0;
        for (scale, alpha, dashed) in [(1.0, 0.85, false), (ring, 0.5, true)] {
            if scale <= 0.0 {
                continue;
            }
            pen.ellipse(
                (radial.x, radial.y),
                (scale * radial.radius_x, scale * radial.radius_y),
                radial.angle,
                alpha,
                dashed,
            );
        }
        // The grip's tether, so the rotation handle reads as belonging to the ellipse rather than
        // floating beside it.
        if let (Some(axis), Some(grip)) = (
            self.point(MaskHandle::RadiusPlusX, aspect),
            self.point(MaskHandle::Rotation, aspect),
        ) {
            pen.line(axis, grip, 0.55);
        }
    }
}

/// One radial payload bound to an aspect ratio: mask space, and the ellipse's own axes inside it.
///
/// Mask space is `u = x · W/H`, `v = y`, which is the spelling the host compiles a radial through,
/// so the handles are placed by the same arithmetic the coverage is evaluated by rather than by a
/// second description of the same ellipse.
#[derive(Clone, Copy, Debug)]
struct Ellipse {
    cu: f64,
    cv: f64,
    ca: f64,
    sa: f64,
    aspect: f64,
}

impl Ellipse {
    fn new(radial: RadialGradient, aspect: f64) -> Self {
        let theta = radial.angle * std::f64::consts::PI / 180.0;
        Self {
            cu: radial.x * aspect,
            cv: radial.y,
            ca: theta.cos(),
            sa: theta.sin(),
            aspect,
        }
    }

    /// A point on the ellipse's own axes, as an offset from the centre in mask space.
    fn local(self, a: f64, b: f64) -> (f64, f64) {
        (self.ca * a - self.sa * b, self.sa * a + self.ca * b)
    }

    /// A mask-space offset from the centre, back onto the ellipse's own axes.
    fn axes(self, du: f64, dv: f64) -> (f64, f64) {
        (self.ca * du + self.sa * dv, -self.sa * du + self.ca * dv)
    }

    /// A normalized content position as a mask-space one.
    fn to_mask(self, x: f64, y: f64) -> (f64, f64) {
        (x * self.aspect, y)
    }

    /// A mask-space position back to a normalized content one.
    fn to_content(self, u: f64, v: f64) -> (f64, f64) {
        (u / self.aspect, v)
    }

    /// That point's offset from the centre, on the ellipse's own axes.
    fn offset(self, point: (f64, f64)) -> (f64, f64) {
        let (u, v) = self.to_mask(point.0, point.1);
        self.axes(u - self.cu, v - self.cv)
    }
}

/// The ellipse of the given centre that reaches the pointer on both axes, upright in mask space.
/// The radii are taken from the pointer's own offset rather than from a difference, because the
/// centre this is measured against does not move while the stroke lasts.
fn extent(
    start: RadialGradient,
    centre: (f64, f64),
    point: (f64, f64),
    aspect: f64,
) -> RadialGradient {
    RadialGradient {
        radius_x: ((point.0 - centre.0) * aspect).abs(),
        radius_y: (point.1 - centre.1).abs(),
        ..start
    }
}

/// A point's radius relative to the ellipse's own boundary: `1.0` on it, `0.0` at the centre.
fn relative(a: f64, b: f64, radial: RadialGradient) -> f64 {
    let ax = a / radial.radius_x;
    let by = b / radial.radius_y;
    (ax * ax + by * by).sqrt()
}

fn degrees(dv: f64, du: f64) -> f64 {
    dv.atan2(du) * 180.0 / std::f64::consts::PI
}

/// One turn's worth of degrees folded into the stored range, so two payloads that draw the same
/// ellipse compare equal and the number field has ends.
fn wrapped(angle: f64) -> f64 {
    if !angle.is_finite() {
        return 0.0;
    }
    let mut angle = angle;
    while angle > ANGLE_MAX {
        angle -= 360.0;
    }
    while angle < ANGLE_MIN {
        angle += 360.0;
    }
    angle
}

/// Every field of a radial inside the range its own parser enforces, so a gesture never produces a
/// payload the commit refuses.
fn bounded(radial: RadialGradient) -> RadialGradient {
    let clamp = |value: f64, low: f64, high: f64, fallback: f64| {
        if value.is_finite() {
            value.clamp(low, high)
        } else {
            fallback
        }
    };
    RadialGradient {
        x: clamp(radial.x, POSITION_MIN, POSITION_MAX, 0.5),
        y: clamp(radial.y, POSITION_MIN, POSITION_MAX, 0.5),
        radius_x: clamp(radial.radius_x, DISTANCE_MIN, DISTANCE_MAX, DISTANCE_MIN),
        radius_y: clamp(radial.radius_y, DISTANCE_MIN, DISTANCE_MAX, DISTANCE_MIN),
        angle: clamp(wrapped(radial.angle), ANGLE_MIN, ANGLE_MAX, 0.0),
        feather: clamp(radial.feather, FEATHER_MIN, FEATHER_MAX, FEATHER_MIN),
    }
}
