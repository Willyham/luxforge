//! The transient mask shape draft: the single owner of everything a gradient gesture changes before
//! it commits.
//!
//! This module holds no framework types and, unlike [`crate::crop_draft`], no stage geometry either.
//! A mask component's geometry is stored in **content-stage normalized** coordinates — `x` and `y`
//! as fractions of the content stage, legal over
//! [`POSITION_MIN`][lightwell_core::mask::POSITION_MIN]`..=`[`POSITION_MAX`][lightwell_core::mask::POSITION_MAX]
//! — and that is the space this draft works in throughout. Mapping a pointer position into it is the
//! canvas's job, through `render.transform`'s affine and the canvas view, and it is done locally per
//! move rather than by asking the host ([performance rule 12](../../docs/engineering/performance-rules.md)).
//!
//! What lives here is the state machine: which handle a gesture grabbed, what the gradient looked
//! like when it started, and which command the release will commit. Every drag is evaluated against
//! the gradient the gesture *started* with, never the previous position, so a drag away and back
//! returns the starting gradient exactly.
use lightwell_core::{
    ComponentId, ComponentMode, MaskId, StageTransform,
    mask::{LinearGradient, POSITION_MAX, POSITION_MIN, commands::GeometryOp},
};
use serde_json::{Map, Value, json};

/// The content-to-output map a gesture uses, taken from one `render.transform` answer and then
/// applied locally for every pointer position and every drawn handle.
///
/// The host answers this once per gesture, because the geometry tail is exact transforms plus at
/// most one crop and is therefore affine: asking per pointer move would put a runtime hop on the
/// input path, which [performance rule 12](../../docs/engineering/performance-rules.md) forbids and
/// which `render.locate` exists for instead, for picks.
///
/// A mask stores **normalized** content positions — fractions of the content stage — and the affine
/// is in the host's continuous, pixel-centre coordinates, so this type owns exactly the two
/// multiplications between them and nothing else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContentMap {
    content: (f64, f64),
    output: (f64, f64),
    forward: [f64; 6],
    inverse: [f64; 6],
}

impl ContentMap {
    /// The map one `render.transform` answer describes, or `None` for a degenerate stage, which is
    /// a stack that has no output to draw handles over.
    pub(crate) fn new(transform: &StageTransform) -> Option<Self> {
        let content = (
            f64::from(transform.content.width),
            f64::from(transform.content.height),
        );
        let output = (
            f64::from(transform.output.width),
            f64::from(transform.output.height),
        );
        (content.0 > 0.0 && content.1 > 0.0 && output.0 > 0.0 && output.1 > 0.0).then_some(Self {
            content,
            output,
            forward: transform.forward,
            inverse: transform.inverse,
        })
    }

    /// The output stage's pixel size, which is the raster the canvas draws the handles over.
    pub(crate) fn output(self) -> (f64, f64) {
        self.output
    }

    /// A stored normalized position as a coordinate of the output stage.
    pub(crate) fn to_output(self, x: f64, y: f64) -> (f64, f64) {
        apply(self.forward, x * self.content.0, y * self.content.1)
    }

    /// An output-stage coordinate back to a stored normalized position. Exact inverse of
    /// [`Self::to_output`], because the host answers both matrices rather than one and an inverse.
    pub(crate) fn to_content(self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = apply(self.inverse, x, y);
        (cx / self.content.0, cy / self.content.1)
    }

    /// A length in output pixels as one in normalized content units, for a handle's hit radius. The
    /// affine may scale the two axes differently only through a reflection or a quarter turn, which
    /// swaps them rather than stretching either, so the larger of the two keeps a handle reachable
    /// whatever the tail does.
    pub(crate) fn tolerance(self, output_pixels: f64) -> f64 {
        let across = (self.to_content(output_pixels, 0.0).0 - self.to_content(0.0, 0.0).0).abs();
        let down = (self.to_content(0.0, output_pixels).1 - self.to_content(0.0, 0.0).1).abs();
        let other = (self.to_content(output_pixels, 0.0).1 - self.to_content(0.0, 0.0).1)
            .abs()
            .max((self.to_content(0.0, output_pixels).0 - self.to_content(0.0, 0.0).0).abs());
        across.max(down).max(other)
    }
}

/// `x' = m0·x + m1·y + m2`, `y' = m3·x + m4·y + m5`: the coefficient order the host fixes.
fn apply(matrix: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (
        matrix[0] * x + matrix[1] * y + matrix[2],
        matrix[3] * x + matrix[4] * y + matrix[5],
    )
}

/// The component kind this draft edits. Only the linear gradient has a handle editor today; a
/// component of any other registered kind is edited through its generated number fields, which come
/// from the same declarations.
pub(crate) const LINEAR: &str = "linear";

/// Which part of the drawn gradient a press grabbed.
///
/// The drawn figure is the design's three lines — `p0`, the midpoint and `p1` — with an end handle
/// on each endpoint and the midpoint grabbable to move the whole axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskHandle {
    /// The end of the axis at coverage 0.
    Start,
    /// The midpoint: moves both ends together, keeping the axis's length and direction.
    Middle,
    /// The end of the axis at coverage 1.
    End,
}

impl MaskHandle {
    pub(crate) const ALL: [Self; 3] = [Self::Start, Self::Middle, Self::End];

    /// Where this handle sits on a gradient, in normalized content coordinates.
    pub(crate) fn point(self, gradient: &LinearGradient) -> (f64, f64) {
        match self {
            Self::Start => (gradient.x0, gradient.y0),
            Self::Middle => (
                (gradient.x0 + gradient.x1) / 2.0,
                (gradient.y0 + gradient.y1) / 2.0,
            ),
            Self::End => (gradient.x1, gradient.y1),
        }
    }
}

/// What releasing this draft commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskDraftOp {
    /// A new mask whose first component is this gradient: `mask.create-<kind>`.
    Create,
    /// A further component of an existing mask, with its mode: `mask.add-<kind>`.
    Add(ComponentMode),
    /// A field patch over an existing component's geometry: `mask.set-<kind>`.
    Set,
}

impl MaskDraftOp {
    /// The generated method's operation. A mode belongs to the request, not to the method name.
    fn geometry_op(self) -> GeometryOp {
        match self {
            Self::Create => GeometryOp::Create,
            Self::Add(_) => GeometryOp::Add,
            Self::Set => GeometryOp::Set,
        }
    }

    /// The word the draft bar uses for what this gesture will do.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Create => "New mask",
            Self::Add(ComponentMode::Add) => "Add",
            Self::Add(ComponentMode::Subtract) => "Subtract",
            Self::Add(ComponentMode::Intersect) => "Intersect",
            Self::Set => "Update",
        }
    }
}

/// One gesture in flight. Every `drag` is evaluated against `start`, never against the previous
/// pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Gesture {
    handle: MaskHandle,
    start: LinearGradient,
    start_point: (f64, f64),
}

/// The whole mask shape editor's state between opening a gesture and its commit or cancel.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaskDraft {
    /// The mask this gesture edits, or `None` when the release will create one.
    pub(crate) mask: Option<MaskId>,
    /// The component this gesture patches, or `None` when the release will add or create one.
    pub(crate) component: Option<ComponentId>,
    /// The registered component kind, which is half of the generated method's name.
    pub(crate) kind: String,
    pub(crate) op: MaskDraftOp,
    pub(crate) gradient: LinearGradient,
    /// The revision this draft was opened against; the commit expects it.
    pub(crate) base_revision: u64,
    /// Something else changed the asset; the commit is refused until Discard or Reapply.
    pub(crate) conflicted: bool,
    gesture: Option<Gesture>,
}

/// The smallest axis this draft will produce. The host refuses an axis shorter than one legal
/// mask-space distance; keeping the drawn axis above a visible fraction of the frame means a gesture
/// never produces a gradient the commit would refuse, and never one nobody can see either.
const MIN_AXIS: f64 = 1e-3;

/// A gradient across the middle of the frame, top to bottom: the neutral shape a new gesture starts
/// from when nothing was dragged, drawn from the untouched side towards the affected one.
pub(crate) const NEUTRAL: LinearGradient = LinearGradient {
    x0: 0.5,
    y0: 0.25,
    x1: 0.5,
    y1: 0.75,
};

impl MaskDraft {
    /// Start a gesture that will create a new mask from a gradient.
    pub(crate) fn creating(kind: impl Into<String>, base_revision: u64) -> Self {
        Self::seeded(
            None,
            None,
            kind,
            MaskDraftOp::Create,
            NEUTRAL,
            base_revision,
        )
    }

    /// Start a gesture that will add a further component to an existing mask, in that mode.
    pub(crate) fn adding(
        mask: MaskId,
        kind: impl Into<String>,
        mode: ComponentMode,
        base_revision: u64,
    ) -> Self {
        Self::seeded(
            Some(mask),
            None,
            kind,
            MaskDraftOp::Add(mode),
            NEUTRAL,
            base_revision,
        )
    }

    /// Edit an existing component: the gradient starts at exactly the stored payload, so reopening
    /// a draft shows what was committed.
    pub(crate) fn editing(
        mask: MaskId,
        component: ComponentId,
        kind: impl Into<String>,
        gradient: LinearGradient,
        base_revision: u64,
    ) -> Self {
        Self::seeded(
            Some(mask),
            Some(component),
            kind,
            MaskDraftOp::Set,
            gradient,
            base_revision,
        )
    }

    fn seeded(
        mask: Option<MaskId>,
        component: Option<ComponentId>,
        kind: impl Into<String>,
        op: MaskDraftOp,
        gradient: LinearGradient,
        base_revision: u64,
    ) -> Self {
        Self {
            mask,
            component,
            kind: kind.into(),
            op,
            gradient: clamped(gradient),
            base_revision,
            conflicted: false,
            gesture: None,
        }
    }

    /// The generated method this draft commits through, read from the host's own kind table so the
    /// desktop spells no method name of its own. `None` for a kind this build does not know.
    pub(crate) fn method(&self) -> Option<&'static str> {
        lightwell_core::mask::commands::geometry(self.op.geometry_op(), &self.kind)
            .map(|command| command.method)
    }

    /// The declared parameters the commit carries: the gradient's four fields, and the mode when the
    /// method takes one. The identities travel in the envelope and are not parameters.
    pub(crate) fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        if let MaskDraftOp::Add(mode) = self.op {
            fields.insert("mode".to_owned(), json!(mode.as_str()));
        }
        for (name, value) in self.values() {
            fields.insert(name.to_owned(), json!(value));
        }
        fields
    }

    /// The gradient's four declared fields in the order the kind declares them.
    pub(crate) fn values(&self) -> [(&'static str, f64); 4] {
        [
            ("x0", self.gradient.x0),
            ("y0", self.gradient.y0),
            ("x1", self.gradient.x1),
            ("y1", self.gradient.y1),
        ]
    }

    pub(crate) fn dragging(&self) -> bool {
        self.gesture.is_some()
    }

    /// The handle a gesture currently holds, for the status line and the canvas cursor.
    pub(crate) fn held(&self) -> Option<MaskHandle> {
        self.gesture.map(|gesture| gesture.handle)
    }

    pub(crate) fn mark_conflicted(&mut self) {
        self.conflicted = true;
        self.gesture = None;
    }

    /// Point the draft at a new revision after something else committed. The gradient this client
    /// drew is kept: it is what a Reapply re-sends.
    pub(crate) fn rebase(&mut self, base_revision: u64) {
        self.base_revision = base_revision;
        self.conflicted = false;
        self.gesture = None;
    }

    /// Which handle a press at this normalized point grabbed, or none when it grabbed nothing.
    /// `tolerance` is the hit radius in normalized units, so the canvas keeps a handle the same size
    /// on screen at every zoom by dividing its pixel radius by the drawn scale. Endpoints win over
    /// the midpoint, which is only reachable when the axis is long enough to draw all three.
    pub(crate) fn hit(&self, point: (f64, f64), tolerance: f64) -> Option<MaskHandle> {
        let tolerance = tolerance.max(0.0);
        [MaskHandle::Start, MaskHandle::End, MaskHandle::Middle]
            .into_iter()
            .find(|handle| {
                let (x, y) = handle.point(&self.gradient);
                (point.0 - x).hypot(point.1 - y) <= tolerance
            })
    }

    /// Start a gesture, snapshotting the gradient every later `drag` is measured against.
    pub(crate) fn begin(&mut self, handle: MaskHandle, point: (f64, f64)) {
        if !finite(point) {
            return;
        }
        self.gesture = Some(Gesture {
            handle,
            start: self.gradient,
            start_point: point,
        });
    }

    /// Re-evaluate the gesture at a new pointer position. Nothing is committed and no host method is
    /// called: this is the whole of what a pointer move costs.
    pub(crate) fn drag(&mut self, point: (f64, f64)) {
        let Some(gesture) = self.gesture else {
            return;
        };
        if !finite(point) {
            return;
        }
        let delta = (
            point.0 - gesture.start_point.0,
            point.1 - gesture.start_point.1,
        );
        let start = gesture.start;
        let moved = match gesture.handle {
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
        };
        self.gradient = lengthened(clamped(moved), start);
    }

    /// Draw a whole gradient in one stroke: the press sets `p0` and the drag sets `p1`, which is
    /// Lightroom's gesture — drag from the untouched side towards the affected one.
    pub(crate) fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        if !finite(from) || !finite(to) {
            return;
        }
        self.gradient = lengthened(
            clamped(LinearGradient {
                x0: from.0,
                y0: from.1,
                x1: to.0,
                y1: to.1,
            }),
            self.gradient,
        );
        self.gesture = Some(Gesture {
            handle: MaskHandle::End,
            start: self.gradient,
            start_point: to,
        });
    }

    /// Finish the gesture. The gradient it produced stays; the commit is a separate decision.
    pub(crate) fn end(&mut self) {
        self.gesture = None;
    }

    /// Set one declared field by name, as its generated number field does. An unknown name and a
    /// value the declared range refuses both leave the gradient exactly as it was.
    pub(crate) fn set_field(&mut self, name: &str, value: f64) -> bool {
        if !value.is_finite() || !(POSITION_MIN..=POSITION_MAX).contains(&value) {
            return false;
        }
        let mut next = self.gradient;
        match name {
            "x0" => next.x0 = value,
            "y0" => next.y0 = value,
            "x1" => next.x1 = value,
            "y1" => next.y1 = value,
            _ => return false,
        }
        // A number field may legally produce a degenerate axis; the length rule keeps the draft
        // committable, exactly as it does for a drag.
        self.gradient = lengthened(next, self.gradient);
        true
    }

    /// Correlated evidence: what the draft holds when a frame is captured or an event is logged.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "mask": self.mask.as_ref().map(MaskId::as_str),
            "component": self.component.as_ref().map(ComponentId::as_str),
            "kind": self.kind,
            "op": self.op.label(),
            "method": self.method(),
            "base_revision": self.base_revision,
            "conflicted": self.conflicted,
            "dragging": self.dragging(),
            "gradient": {"x0": self.gradient.x0, "y0": self.gradient.y0, "x1": self.gradient.x1, "y1": self.gradient.y1},
        })
    }
}

fn finite(point: (f64, f64)) -> bool {
    point.0.is_finite() && point.1.is_finite()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> MaskDraft {
        MaskDraft::creating(LINEAR, 7)
    }

    fn axis(gradient: &LinearGradient) -> f64 {
        (gradient.x1 - gradient.x0).hypot(gradient.y1 - gradient.y0)
    }

    /// Every gradient this draft can produce is a payload the host's own range accepts.
    fn check(draft: &MaskDraft, what: &str) {
        for (name, value) in draft.values() {
            assert!(
                value.is_finite() && (POSITION_MIN..=POSITION_MAX).contains(&value),
                "{what}: {name} is {value}, outside {POSITION_MIN}..={POSITION_MAX}"
            );
        }
        assert!(
            axis(&draft.gradient) >= MIN_AXIS,
            "{what}: the axis collapsed to {}",
            axis(&draft.gradient)
        );
    }

    #[test]
    fn the_method_and_fields_come_from_the_hosts_own_kind_table() {
        assert_eq!(
            MaskDraft::creating(LINEAR, 1).method(),
            Some("mask.create-linear")
        );
        assert_eq!(
            MaskDraft::adding(MaskId::new(), LINEAR, ComponentMode::Subtract, 1).method(),
            Some("mask.add-linear")
        );
        assert_eq!(
            MaskDraft::editing(MaskId::new(), ComponentId::new(), LINEAR, NEUTRAL, 1).method(),
            Some("mask.set-linear")
        );
        // A kind this build cannot evaluate has no generated method, so nothing is spelled out here.
        assert_eq!(MaskDraft::creating("brush", 1).method(), None);

        // A create carries the four geometry fields and no mode; an add carries its mode too.
        let create = MaskDraft::creating(LINEAR, 1).fields();
        assert_eq!(create.len(), 4);
        assert_eq!(create["x0"], json!(NEUTRAL.x0));
        assert_eq!(create["y1"], json!(NEUTRAL.y1));
        assert!(create.get("mode").is_none(), "a create is always an add");
        let add = MaskDraft::adding(MaskId::new(), LINEAR, ComponentMode::Intersect, 1).fields();
        assert_eq!(add["mode"], json!("intersect"));
        assert_eq!(add.len(), 5);
        // The identities are never parameters: they travel in the envelope.
        for fields in [create, add] {
            assert!(fields.get("mask").is_none() && fields.get("component").is_none());
        }
    }

    #[test]
    fn each_handle_moves_what_it_names_and_the_midpoint_moves_the_whole_axis() {
        let mut draft = draft();
        let before = draft.gradient;
        // The start handle moves p0 alone.
        draft.begin(MaskHandle::Start, MaskHandle::Start.point(&before));
        draft.drag((before.x0 + 0.1, before.y0 - 0.05));
        draft.end();
        assert_eq!(
            (draft.gradient.x1, draft.gradient.y1),
            (before.x1, before.y1)
        );
        assert!((draft.gradient.x0 - (before.x0 + 0.1)).abs() < 1e-12);
        check(&draft, "start");

        // The end handle moves p1 alone.
        let before = draft.gradient;
        draft.begin(MaskHandle::End, MaskHandle::End.point(&before));
        draft.drag((before.x1 - 0.2, before.y1 + 0.1));
        draft.end();
        assert_eq!(
            (draft.gradient.x0, draft.gradient.y0),
            (before.x0, before.y0)
        );
        check(&draft, "end");

        // The midpoint translates both ends, so the axis keeps its length and direction exactly.
        let before = draft.gradient;
        let length = axis(&before);
        draft.begin(MaskHandle::Middle, MaskHandle::Middle.point(&before));
        draft.drag((
            MaskHandle::Middle.point(&before).0 + 0.05,
            MaskHandle::Middle.point(&before).1 + 0.05,
        ));
        draft.end();
        assert!(
            (axis(&draft.gradient) - length).abs() < 1e-12,
            "a move resized the axis"
        );
        assert!((draft.gradient.x0 - (before.x0 + 0.05)).abs() < 1e-12);
        assert!((draft.gradient.y1 - (before.y1 + 0.05)).abs() < 1e-12);
        check(&draft, "middle");
    }

    #[test]
    fn a_drag_away_and_back_returns_the_starting_gradient_exactly() {
        for handle in MaskHandle::ALL {
            let mut draft = draft();
            let start = draft.gradient;
            let from = handle.point(&start);
            draft.begin(handle, from);
            for step in [(-0.3, 0.2), (0.6, -0.4), (0.0, 0.0)] {
                draft.drag((from.0 + step.0, from.1 + step.1));
            }
            draft.end();
            assert_eq!(
                (
                    draft.gradient.x0,
                    draft.gradient.y0,
                    draft.gradient.x1,
                    draft.gradient.y1
                ),
                (start.x0, start.y0, start.x1, start.y1),
                "{handle:?}"
            );
        }
    }

    #[test]
    fn every_gesture_stays_inside_the_declared_range_and_never_collapses_the_axis() {
        for handle in MaskHandle::ALL {
            let mut draft = draft();
            let from = handle.point(&draft.gradient);
            draft.begin(handle, from);
            for step in [
                (-900.0, -900.0),
                (900.0, 900.0),
                (0.0, 0.0),
                (f64::NAN, 0.0),
                (0.37, -0.91),
            ] {
                draft.drag((from.0 + step.0, from.1 + step.1));
                check(&draft, &format!("{handle:?} {step:?}"));
            }
            draft.end();
        }
        // Dragging one endpoint exactly onto the other still leaves a committable axis.
        let mut draft = draft();
        let target = MaskHandle::Start.point(&draft.gradient);
        draft.begin(MaskHandle::End, MaskHandle::End.point(&draft.gradient));
        draft.drag(target);
        draft.end();
        check(&draft, "collapsed onto the other end");
    }

    #[test]
    fn a_sweep_draws_the_whole_gradient_from_the_press_to_the_pointer() {
        let mut draft = draft();
        draft.sweep((0.2, 0.1), (0.8, 0.9));
        assert_eq!((draft.gradient.x0, draft.gradient.y0), (0.2, 0.1));
        assert_eq!((draft.gradient.x1, draft.gradient.y1), (0.8, 0.9));
        assert!(
            draft.dragging(),
            "the sweep continues as an end-handle drag"
        );
        assert_eq!(draft.held(), Some(MaskHandle::End));
        draft.drag((0.5, 0.5));
        assert!((draft.gradient.x1 - 0.5).abs() < 1e-12);
        draft.end();
        check(&draft, "swept");
        // A sweep that never moved still leaves an axis the host will accept.
        let mut still = MaskDraft::creating(LINEAR, 7);
        still.sweep((0.4, 0.4), (0.4, 0.4));
        check(&still, "a sweep that did not move");
    }

    #[test]
    fn hit_testing_prefers_the_endpoints_and_misses_cleanly() {
        let draft = draft();
        assert_eq!(
            draft.hit(MaskHandle::Start.point(&draft.gradient), 0.02),
            Some(MaskHandle::Start)
        );
        assert_eq!(
            draft.hit(MaskHandle::End.point(&draft.gradient), 0.02),
            Some(MaskHandle::End)
        );
        assert_eq!(
            draft.hit(MaskHandle::Middle.point(&draft.gradient), 0.02),
            Some(MaskHandle::Middle)
        );
        assert_eq!(
            draft.hit((0.0, 0.0), 0.02),
            None,
            "a press on nothing grabs nothing"
        );
        // A tolerance large enough to cover every handle answers with an endpoint, not the midpoint.
        assert_eq!(
            draft.hit(MaskHandle::Middle.point(&draft.gradient), 9.0),
            Some(MaskHandle::Start)
        );
    }

    #[test]
    fn a_number_field_sets_exactly_its_own_declared_value_and_refuses_the_rest() {
        let mut draft = draft();
        assert!(draft.set_field("x0", 0.125));
        assert_eq!(draft.gradient.x0, 0.125);
        assert!(draft.set_field("y1", POSITION_MAX));
        assert_eq!(draft.gradient.y1, POSITION_MAX);
        check(&draft, "typed");
        // Out of range, not a number, and a field this kind does not declare: each refused, and
        // each leaves the gradient untouched.
        let before = draft.gradient;
        for (name, value) in [
            ("x0", POSITION_MAX + 1.0),
            ("y0", POSITION_MIN - 1.0),
            ("x1", f64::NAN),
            ("radius_x", 0.5),
            ("mode", 1.0),
        ] {
            assert!(
                !draft.set_field(name, value),
                "{name} = {value} was accepted"
            );
            assert_eq!(
                draft.gradient, before,
                "{name} = {value} changed the gradient"
            );
        }
    }

    #[test]
    fn a_gesture_without_a_press_changes_nothing_and_a_conflict_ends_the_drag() {
        let mut draft = draft();
        let before = draft.gradient;
        draft.drag((0.9, 0.9));
        draft.end();
        assert_eq!(draft.gradient, before);
        assert!(!draft.dragging());

        draft.begin(MaskHandle::End, MaskHandle::End.point(&before));
        assert!(draft.dragging());
        draft.mark_conflicted();
        assert!(
            draft.conflicted && !draft.dragging(),
            "a conflict drops the gesture"
        );
        draft.drag((0.9, 0.9));
        assert_eq!(
            draft.gradient, before,
            "a conflicted draft ignores the pointer"
        );
        draft.rebase(11);
        assert_eq!(draft.base_revision, 11);
        assert!(!draft.conflicted);
        assert_eq!(
            draft.gradient, before,
            "a reapply keeps what this client drew"
        );
    }

    /// The map is applied locally, so it must agree with the host both ways and for every tail the
    /// delivered modules produce: the identity, a crop, and a quarter turn that swaps the axes.
    #[test]
    fn the_content_map_round_trips_every_tail_the_geometry_produces() {
        use lightwell_core::{StageSize, StageTransform};
        let stage = |w, h| StageSize {
            width: w,
            height: h,
        };
        let cases = [
            (
                "identity",
                StageTransform {
                    content: stage(480, 320),
                    output: stage(480, 320),
                    forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                },
            ),
            (
                "a crop of 40 by 30",
                StageTransform {
                    content: stage(480, 320),
                    output: stage(400, 260),
                    forward: [1.0, 0.0, -40.0, 0.0, 1.0, -30.0],
                    inverse: [1.0, 0.0, 40.0, 0.0, 1.0, 30.0],
                },
            ),
            (
                "a quarter turn",
                StageTransform {
                    content: stage(480, 320),
                    output: stage(320, 480),
                    forward: [0.0, -1.0, 320.0, 1.0, 0.0, 0.0],
                    inverse: [0.0, 1.0, 0.0, -1.0, 0.0, 320.0],
                },
            ),
        ];
        for (what, transform) in cases {
            let map = ContentMap::new(&transform).expect("a drawable stage");
            assert_eq!(
                map.output(),
                (
                    f64::from(transform.output.width),
                    f64::from(transform.output.height)
                )
            );
            for (x, y) in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (-0.25, 1.75)] {
                let (ox, oy) = map.to_output(x, y);
                let (bx, by) = map.to_content(ox, oy);
                assert!(
                    (bx - x).abs() < 1e-9 && (by - y).abs() < 1e-9,
                    "{what}: ({x}, {y}) came back as ({bx}, {by})"
                );
            }
            // The origin of the content stage is the origin of the output stage under the identity
            // and is moved by exactly the crop's offset under a crop.
            let tolerance = map.tolerance(8.0);
            assert!(
                tolerance > 0.0 && tolerance < 1.0,
                "{what}: a hit radius of {tolerance} is not a usable fraction of the frame"
            );
        }
        // A stage with no extent has no map rather than an invented one.
        assert!(
            ContentMap::new(&StageTransform {
                content: stage(0, 320),
                output: stage(480, 320),
                forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            })
            .is_none()
        );
    }

    #[test]
    fn the_summary_reports_the_state_a_capture_is_correlated_with() {
        let mask = MaskId::new();
        let component = ComponentId::new();
        let draft = MaskDraft::editing(mask.clone(), component.clone(), LINEAR, NEUTRAL, 4);
        let summary = draft.summary();
        assert_eq!(summary["mask"], json!(mask.as_str()));
        assert_eq!(summary["component"], json!(component.as_str()));
        assert_eq!(summary["kind"], json!(LINEAR));
        assert_eq!(summary["op"], json!("Update"));
        assert_eq!(summary["method"], json!("mask.set-linear"));
        assert_eq!(summary["base_revision"], json!(4));
        assert_eq!(summary["conflicted"], json!(false));
        assert_eq!(summary["gradient"]["y1"], json!(NEUTRAL.y1));
        // A create names no mask and no component, because it has none yet.
        let creating = MaskDraft::creating(LINEAR, 4).summary();
        assert_eq!(creating["mask"], Value::Null);
        assert_eq!(creating["op"], json!("New mask"));
    }
}
