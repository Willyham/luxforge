//! The linear gradient's shape editor: two endpoints and the axis between them.
//!
//! The figure is the design's three lines — one through `p0`, one through the midpoint and one
//! through `p1`, each perpendicular to the axis — and the axis itself, so the direction of the
//! gradient is visible rather than inferred. The gesture is Lightroom's: a press away from every
//! handle sets `p0` and the drag sets `p1`, from the untouched side towards the affected one.
use super::editor::{DrawnShape, Grab, MaskHandle, Pen, ShapeEditor, finite};
use lightwell_core::mask::{LinearGradient, POSITION_MAX, POSITION_MIN};
use serde_json::Value;

/// The kind token this editor draws, from the host's own vocabulary.
pub(crate) const KIND: &str = "linear";

/// The smallest axis this editor will produce. The host refuses an axis shorter than one legal
/// mask-space distance; keeping the drawn axis above a visible fraction of the frame means a gesture
/// never produces a gradient the commit would refuse, and never one nobody can see either.
pub(super) const MIN_AXIS: f64 = 1e-3;

/// A gradient across the middle of the frame, top to bottom: the neutral shape a new gesture starts
/// from when nothing was dragged, drawn from the untouched side towards the affected one.
pub(crate) const NEUTRAL: LinearGradient = LinearGradient {
    x0: 0.5,
    y0: 0.25,
    x1: 0.5,
    y1: 0.75,
};

/// How far past the drawn frame the three perpendicular lines are extended, in normalized content
/// units. A gradient's lines are infinite; this is enough to leave the frame from any position and
/// angle the legal range allows.
const LINE_REACH: f64 = 2.0;

/// The handles, in hit order: the endpoints win over the midpoint that moves them both.
const HANDLES: [MaskHandle; 3] = [MaskHandle::Start, MaskHandle::End, MaskHandle::Middle];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LinearEditor {
    shape: LinearGradient,
    grab: Option<Grab<LinearGradient>>,
}

/// The kind table's row: a stored payload, or the neutral gradient, as a committable shape. A
/// payload this editor cannot read opens nothing.
pub(super) fn open(stored: Option<&Value>, _brush: super::Brush) -> Option<DrawnShape> {
    let shape = match stored {
        Some(payload) => serde_json::from_value(payload.clone()).ok()?,
        None => NEUTRAL,
    };
    Some(DrawnShape::Linear(LinearEditor {
        shape: legal(shape, shape),
        grab: None,
    }))
}

impl LinearEditor {
    fn point(&self, handle: MaskHandle) -> Option<(f64, f64)> {
        let linear = self.shape;
        match handle {
            MaskHandle::Start => Some((linear.x0, linear.y0)),
            MaskHandle::Middle => {
                Some(((linear.x0 + linear.x1) / 2.0, (linear.y0 + linear.y1) / 2.0))
            }
            MaskHandle::End => Some((linear.x1, linear.y1)),
            _ => None,
        }
    }
}

impl ShapeEditor for LinearEditor {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn values(&self) -> Vec<(&'static str, f64)> {
        let linear = self.shape;
        vec![
            ("x0", linear.x0),
            ("y0", linear.y0),
            ("x1", linear.x1),
            ("y1", linear.y1),
        ]
    }

    /// The range checked here is the kind's own, which is the range the host's parser enforces: a
    /// position. A number field may legally produce a degenerate axis; the legality rule keeps the
    /// shape committable, exactly as it does for a drag.
    fn set_field(&mut self, name: &str, value: f64) -> bool {
        if !value.is_finite() || !(POSITION_MIN..=POSITION_MAX).contains(&value) {
            return false;
        }
        let held = self.shape;
        let mut next = held;
        match name {
            "x0" => next.x0 = value,
            "y0" => next.y0 = value,
            "x1" => next.x1 = value,
            "y1" => next.y1 = value,
            _ => return false,
        }
        self.shape = legal(next, held);
        true
    }

    fn handles(&self, _aspect: f64) -> Vec<(MaskHandle, (f64, f64))> {
        HANDLES
            .iter()
            .filter_map(|handle| self.point(*handle).map(|point| (*handle, point)))
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

    fn drag(&mut self, point: (f64, f64), _aspect: f64) {
        let Some(grab) = self.grab else {
            return;
        };
        if !finite(point) {
            return;
        }
        let start = grab.start;
        let delta = grab.travel(point);
        let moved = match grab.handle {
            MaskHandle::Start => LinearGradient {
                x0: start.x0 + delta.0,
                y0: start.y0 + delta.1,
                ..start
            },
            MaskHandle::End => LinearGradient {
                x1: start.x1 + delta.0,
                y1: start.y1 + delta.1,
                ..start
            },
            // A move is a move: the travel is clamped rather than the endpoints, so a move into the
            // edge of the legal range slides along it and the axis keeps its length and direction
            // exactly. Clamping the endpoints instead would shorten the gradient at the boundary.
            MaskHandle::Middle => translated(start, delta),
            // Another kind's handle on a gradient moves nothing rather than moving the wrong thing.
            _ => start,
        };
        self.shape = legal(moved, start);
    }

    /// The press sets `p0` and the drag sets `p1`, and the stroke continues as a drag of the end
    /// handle.
    fn sweep(&mut self, from: (f64, f64), to: (f64, f64), _aspect: f64) {
        if !finite(from) || !finite(to) {
            return;
        }
        let held = self.shape;
        let shape = legal(
            LinearGradient {
                x0: from.0,
                y0: from.1,
                x1: to.0,
                y1: to.1,
            },
            held,
        );
        self.shape = shape;
        self.grab = Some(Grab {
            handle: MaskHandle::End,
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

    /// The three perpendicular lines and the axis. They are drawn in content space and mapped, so a
    /// crop, a straighten or a quarter turn moves them with the picture.
    fn draw(&self, _aspect: f64, _pointer: Option<(f64, f64)>, pen: &mut dyn Pen) {
        let gradient = self.shape;
        let (dx, dy) = (gradient.x1 - gradient.x0, gradient.y1 - gradient.y0);
        let length = dx.hypot(dy);
        if length <= 0.0 {
            return;
        }
        let (px, py) = (-dy / length * LINE_REACH, dx / length * LINE_REACH);
        for (handle, alpha) in [
            (MaskHandle::Start, 0.85),
            (MaskHandle::Middle, 0.45),
            (MaskHandle::End, 0.85),
        ] {
            if let Some((x, y)) = self.point(handle) {
                pen.line((x - px, y - py), (x + px, y + py), alpha);
            }
        }
        pen.line((gradient.x0, gradient.y0), (gradient.x1, gradient.y1), 0.55);
    }
}

/// A gradient the host's own ranges accept, so a gesture never produces a payload the commit
/// refuses. `reference` is the gradient to borrow a direction from when this one has collapsed.
fn legal(shape: LinearGradient, reference: LinearGradient) -> LinearGradient {
    lengthened(clamped(shape), reference)
}

/// Every endpoint inside the legal stored range. The range is the host's, checked here so a gesture
/// never produces a payload the commit would refuse.
fn clamped(gradient: LinearGradient) -> LinearGradient {
    let clamp = |value: f64| {
        if value.is_finite() {
            value.clamp(POSITION_MIN, POSITION_MAX)
        } else {
            0.0
        }
    };
    LinearGradient {
        x0: clamp(gradient.x0),
        y0: clamp(gradient.y0),
        x1: clamp(gradient.x1),
        y1: clamp(gradient.y1),
    }
}

/// The whole gradient moved by `delta`, with the travel clamped so both endpoints stay inside the
/// legal range. The horizontal travel is clamped against both endpoints and the vertical one
/// likewise, so a move into a boundary slides along it rather than stopping dead.
fn translated(start: LinearGradient, delta: (f64, f64)) -> LinearGradient {
    let axis = |a: f64, b: f64, step: f64| {
        let low = POSITION_MIN - a.min(b);
        let high = POSITION_MAX - a.max(b);
        // A gradient already outside the range — which only a stored payload from another build
        // could be — keeps whatever room it has rather than being dragged further out.
        step.clamp(low.min(0.0), high.max(0.0))
    };
    let dx = axis(start.x0, start.x1, delta.0);
    let dy = axis(start.y0, start.y1, delta.1);
    LinearGradient {
        x0: start.x0 + dx,
        y0: start.y0 + dy,
        x1: start.x1 + dx,
        y1: start.y1 + dy,
    }
}

/// A gradient whose axis is long enough to be a legal payload. A collapsed axis has no direction of
/// its own, so it borrows the one `reference` had, and falls back to straight down when that is
/// collapsed too — never a direction invented from nothing.
///
/// The axis is restored by moving whichever endpoint has room: extending `p1` forward, or, when the
/// legal range has run out there, pulling `p0` back instead. The range spans three stage extents and
/// the minimum axis is a thousandth of one, so one of the two always has room.
fn lengthened(gradient: LinearGradient, reference: LinearGradient) -> LinearGradient {
    let (dx, dy) = (gradient.x1 - gradient.x0, gradient.y1 - gradient.y0);
    if dx.hypot(dy) >= MIN_AXIS {
        return gradient;
    }
    let (rx, ry) = (reference.x1 - reference.x0, reference.y1 - reference.y0);
    let length = rx.hypot(ry);
    let (ux, uy) = if length >= MIN_AXIS {
        (rx / length, ry / length)
    } else {
        (0.0, 1.0)
    };
    let (ex, ey) = (gradient.x0 + ux * MIN_AXIS, gradient.y0 + uy * MIN_AXIS);
    if in_range(ex) && in_range(ey) {
        return LinearGradient {
            x1: ex,
            y1: ey,
            ..gradient
        };
    }
    clamped(LinearGradient {
        x0: gradient.x1 - ux * MIN_AXIS,
        y0: gradient.y1 - uy * MIN_AXIS,
        ..gradient
    })
}

fn in_range(value: f64) -> bool {
    value.is_finite() && (POSITION_MIN..=POSITION_MAX).contains(&value)
}
