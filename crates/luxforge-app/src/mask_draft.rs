//! The transient mask shape draft: the single owner of everything a shape gesture changes before it
//! commits.
//!
//! This module holds no framework types and, unlike [`crate::crop_draft`], no stage raster either.
//! A mask component's geometry is stored in **content-stage normalized** coordinates — `x` and `y`
//! as fractions of the content stage, legal over
//! [`POSITION_MIN`][luxforge_core::mask::POSITION_MIN]`..=`[`POSITION_MAX`][luxforge_core::mask::POSITION_MAX]
//! — and that is the space this draft works in throughout. Mapping a pointer position into it is the
//! canvas's job, through `render.transform`'s affine and the canvas view, and it is done locally per
//! move rather than by asking the host ([performance rule 12](../../docs/engineering/performance-rules.md)).
//!
//! The one number about the stage this draft does hold is its **aspect**, `W/H`, because mask space
//! is defined in terms of it: a stored distance is in units of the content stage's height on both
//! axes, so a circle stays a circle at any aspect ratio. It is read from the same one
//! `render.transform` answer the handles are mapped through, once per gesture, and never per move.
//!
//! What lives here is the draft itself — what the release targets and which command it commits — and
//! the map a pointer is placed through. Everything about one drawn kind's geometry — its handles, what
//! a press, a drag and a sweep do to it, what a typed number may be and what its figure looks like —
//! is that kind's [`editor::ShapeEditor`], in its own module beside this one, and the draft never asks which
//! kind it holds.
use luxforge_core::{
    ComponentId, ComponentMode, MaskId, StageTransform, mask::commands::GeometryOp,
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

    /// The **content** stage's aspect, `W/H`. Mask space is defined in terms of it — one unit is the
    /// content stage's height on both axes — so a radial's stored radii cannot be placed without it.
    /// It is the content stage's own ratio and not the output's: a crop changes what is shown, and
    /// the geometry a mask stores is in the stage the mask is compiled against.
    pub(crate) fn aspect(self) -> f64 {
        self.content.0 / self.content.1
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

/// This build's drawn kinds and their editors: one [`editor::ShapeEditor`] per kind, reached through the kind
/// table in [`editor`].
mod brush;
mod editor;
mod linear;
mod radial;

pub(crate) use brush::{Brush, BrushStroke, NEUTRAL_BRUSH};
pub(crate) use editor::{DrawnShape, MaskHandle, Pen, drawable, paintable};
#[cfg(test)]
pub(crate) use {linear::NEUTRAL, radial::NEUTRAL_RADIAL};

/// The drawn kinds' tokens, from the host's own vocabulary, for the callers that name one.
#[cfg(test)]
pub(crate) const LINEAR: &str = linear::KIND;
#[cfg(test)]
pub(crate) const RADIAL: &str = radial::KIND;
pub(crate) const BRUSH: &str = brush::KIND;

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

/// The whole mask shape editor's state between opening a gesture and its commit or cancel: what the
/// release targets, and the one drawn kind's editor that owns the geometry.
///
/// There is no third state and no "either": a draft is opened for a kind this build draws, with the
/// geometry that kind has, or it is not opened at all.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaskDraft {
    /// The mask this gesture edits, or `None` when the release will create one.
    pub(crate) mask: Option<MaskId>,
    /// The component this gesture patches, or `None` when the release will add or create one.
    pub(crate) component: Option<ComponentId>,
    pub(crate) op: MaskDraftOp,
    /// The drawn kind's editor: a gradient's numbers and handles, or a painted path.
    shape: DrawnShape,
    /// The content stage's `W/H`, which mask space is defined in terms of. It is `1.0` until
    /// `render.transform` answers, which is also when the handles first become drawable, so no
    /// gesture is ever evaluated against an aspect that was guessed.
    aspect: f64,
}

impl MaskDraft {
    /// Start a gesture that will create a new mask from a shape of this kind, or from a stroke.
    pub(crate) fn creating(kind: &str, brush: Brush) -> Option<Self> {
        Self::opened(None, None, kind, MaskDraftOp::Create, None, brush)
    }

    /// Start a gesture that will add a further component to an existing mask, in that mode.
    pub(crate) fn adding(
        mask: MaskId,
        kind: &str,
        mode: ComponentMode,
        brush: Brush,
    ) -> Option<Self> {
        Self::opened(Some(mask), None, kind, MaskDraftOp::Add(mode), None, brush)
    }

    /// Edit an existing component: the shape starts at exactly the stored payload, so reopening a
    /// draft shows what was committed. A brush component has no shape to start from — its strokes
    /// are already drawn — so this opens the next stroke on it instead.
    pub(crate) fn editing(
        mask: MaskId,
        component: ComponentId,
        kind: &str,
        stored: &Value,
        brush: Brush,
    ) -> Option<Self> {
        Self::opened(
            Some(mask),
            Some(component),
            kind,
            MaskDraftOp::Set,
            Some(stored),
            brush,
        )
    }

    fn opened(
        mask: Option<MaskId>,
        component: Option<ComponentId>,
        kind: &str,
        op: MaskDraftOp,
        stored: Option<&Value>,
        brush: Brush,
    ) -> Option<Self> {
        Some(Self {
            mask,
            component,
            op,
            shape: DrawnShape::open(kind, stored, brush)?,
            aspect: 1.0,
        })
    }

    /// The registered component kind this gesture draws.
    pub(crate) fn kind(&self) -> &'static str {
        self.shape.kind()
    }

    /// The stroke this gesture paints, when it paints one.
    pub(crate) fn brush(&self) -> Option<&BrushStroke> {
        self.shape.stroke()
    }

    /// Every press on the photograph paints, because this gesture's geometry is a drawn path.
    pub(crate) fn paints(&self) -> bool {
        self.brush().is_some()
    }

    /// The pointer went down on the photograph: this stroke starts here.
    pub(crate) fn paint_begin(&mut self, point: (f64, f64)) {
        if let Some(stroke) = self.shape.stroke_mut() {
            stroke.press(point);
        }
    }

    /// One pointer move with the button down. It returns whether the path grew, because a move that
    /// added nothing must not cost a round trip.
    pub(crate) fn paint_to(&mut self, point: (f64, f64)) -> bool {
        self.shape
            .stroke_mut()
            .is_some_and(|stroke| stroke.paint(point))
    }

    /// The pointer came up. What it drew stays; the commit is a separate decision.
    pub(crate) fn paint_end(&mut self) {
        if let Some(stroke) = self.shape.stroke_mut() {
            stroke.release();
        }
    }

    /// Change the brush this stroke is being drawn with. Refused once the stroke is down: the brush
    /// a stroke was begun with is the brush it was drawn with, for its whole life, which is what
    /// makes a stored stroke the record of one pass and not of a setting that moved under it.
    pub(crate) fn set_brush(&mut self, brush: Brush) -> bool {
        match self.shape.stroke_mut() {
            Some(stroke) if !stroke.painting() => {
                stroke.brush = brush;
                true
            }
            _ => false,
        }
    }

    /// Tell the gesture the content stage's aspect, from the one `render.transform` answer its
    /// handles are mapped through. Mask space is defined in terms of it, so a radial's handles are
    /// not drawable until it arrives.
    pub(crate) fn set_aspect(&mut self, aspect: f64) {
        if aspect.is_finite() && aspect > 0.0 {
            self.aspect = aspect;
        }
    }

    pub(crate) fn aspect(&self) -> f64 {
        self.aspect
    }

    /// The host method this draft commits through, read from the host's own tables so the desktop
    /// spells no method name of its own. `None` when the host generates no method for this kind and
    /// operation.
    pub(crate) fn method(&self) -> Option<&'static str> {
        self.shape.method(self.op.geometry_op())
    }

    /// The drafted fields the commit carries: the geometry's own fields, and the mode when the
    /// method takes one. The identities it addresses are the draft's target, which the commit sends
    /// beside them as the command's identity parameters.
    pub(crate) fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        // A mode belongs to a component, so only the edit that makes one carries it. A stroke
        // appended to a component that exists is refused for carrying one, which is why this is the
        // same rule for every kind rather than a brush-shaped exception.
        if let MaskDraftOp::Add(mode) = self.op {
            fields.insert("mode".to_owned(), json!(mode.as_str()));
        }
        self.shape.extra_fields(&mut fields);
        for (name, value) in self.values() {
            fields.insert(name.to_owned(), json!(value));
        }
        fields
    }

    /// The gesture's declared number fields, in the order its command declares them.
    pub(crate) fn values(&self) -> Vec<(&'static str, f64)> {
        self.shape.values()
    }

    /// A pointer is down: a handle is being dragged, or a stroke is being painted.
    pub(crate) fn dragging(&self) -> bool {
        self.shape.dragging()
    }

    /// The handle a gesture currently holds, for the status line and the canvas cursor.
    pub(crate) fn held(&self) -> Option<MaskHandle> {
        self.shape.held()
    }

    /// Let go of whatever the pointer holds: the core draft behind this gesture was conflicted or
    /// rebased, so a drag must not carry on into it. What was drawn is kept — it is what a Reapply
    /// re-sends.
    pub(crate) fn interrupt(&mut self) {
        self.shape.release();
    }

    /// Which handle a press at this normalized point grabbed, or none when it grabbed nothing.
    /// `tolerance` is the hit radius in normalized units, so the canvas keeps a handle the same size
    /// on screen at every zoom by dividing its pixel radius by the drawn scale. The order is the
    /// editor's own: the handles that sit on a specific point win over the ones that move everything.
    ///
    /// A painted gesture has no handles at all: every press on the photograph paints, which is why a
    /// brush draws its cursor rather than grips.
    pub(crate) fn hit(&self, point: (f64, f64), tolerance: f64) -> Option<MaskHandle> {
        let tolerance = tolerance.max(0.0);
        self.handles()
            .into_iter()
            .find(|(_, (x, y))| (point.0 - x).hypot(point.1 - y) <= tolerance)
            .map(|(handle, _)| handle)
    }

    /// Where every drawn handle of this gesture sits, in normalized content coordinates. One list,
    /// read by the canvas that draws them and by the hit test above.
    pub(crate) fn handles(&self) -> Vec<(MaskHandle, (f64, f64))> {
        self.shape.handles(self.aspect)
    }

    /// Start a gesture, snapshotting the shape every later `drag` is measured against.
    pub(crate) fn begin(&mut self, handle: MaskHandle, point: (f64, f64)) {
        self.shape.begin(handle, point);
    }

    /// Re-evaluate the gesture at a new pointer position. Nothing is committed and no host method is
    /// called: this is the whole of what a pointer move costs.
    pub(crate) fn drag(&mut self, point: (f64, f64)) {
        self.shape.drag(point, self.aspect);
    }

    /// Draw a whole shape in one stroke, from a press that grabbed no handle to the pointer.
    pub(crate) fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.shape.sweep(from, to, self.aspect);
    }

    /// Finish the gesture. The shape it produced stays; the commit is a separate decision.
    pub(crate) fn end(&mut self) {
        self.shape.release();
    }

    /// Set one declared field by name, as its generated number field does. An unknown name and a
    /// value the declared range refuses both leave the shape exactly as it was.
    pub(crate) fn set_field(&mut self, name: &str, value: f64) -> bool {
        self.shape.set_field(name, value)
    }

    /// Describe the figure through `pen`, with the pointer where the canvas has it.
    pub(crate) fn draw(&self, pointer: Option<(f64, f64)>, pen: &mut dyn Pen) {
        self.shape.draw(self.aspect, pointer, pen);
    }

    /// One declared number by name, for a caller that reads a single field.
    #[cfg(test)]
    pub(crate) fn value(&self, name: &str) -> Option<f64> {
        self.values()
            .into_iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value)
    }

    /// Correlated evidence: what the draft holds when a frame is captured or an event is logged.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "mask": self.mask.as_ref().map(MaskId::as_str),
            "component": self.component.as_ref().map(ComponentId::as_str),
            "kind": self.kind(),
            "op": self.op.label(),
            "method": self.method(),
            "dragging": self.dragging(),
            "aspect": self.aspect,
            "shape": Value::Object(
                self.values()
                    .into_iter()
                    .map(|(name, value)| (name.to_owned(), json!(value)))
                    .collect(),
            ),
            // The stroke a painted gesture is drawing: what the pointer captured, what will be
            // posted after decimation, and the one setting that is not a number. A frame captured
            // mid-stroke is evidence of this, so the two counts are both here.
            "stroke": self.brush().map(|stroke| json!({
                "captured": stroke.captured().len(),
                "posted": stroke.points().len(),
                "erase": stroke.brush.erase,
                "painting": stroke.painting(),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        ParameterKind,
        mask::{
            ANGLE_MAX, DISTANCE_MAX, DISTANCE_MIN, FEATHER_MAX, FEATHER_MIN, LinearGradient,
            POSITION_MAX, POSITION_MIN, RadialGradient, commands,
        },
    };

    fn draft() -> MaskDraft {
        MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).expect("a drawn kind")
    }

    fn radial_draft() -> MaskDraft {
        let mut draft = MaskDraft::creating(RADIAL, NEUTRAL_BRUSH).expect("a drawn kind");
        // A landscape frame, so a bug that confuses mask space with normalized content coordinates
        // cannot hide behind a square one.
        draft.set_aspect(1.5);
        draft
    }

    /// The draft's declared numbers read back as one kind's payload, which is how a test names a
    /// field of the gradient it is checking without the draft exposing its kind.
    fn geometry<T: serde::de::DeserializeOwned>(draft: &MaskDraft) -> Option<T> {
        serde_json::from_value(Value::Object(
            draft
                .values()
                .into_iter()
                .map(|(name, value)| (name.to_owned(), json!(value)))
                .collect(),
        ))
        .ok()
    }

    fn linear(draft: &MaskDraft) -> Option<LinearGradient> {
        geometry(draft)
    }

    fn radial(draft: &MaskDraft) -> Option<RadialGradient> {
        geometry(draft)
    }

    fn axis(gradient: &LinearGradient) -> f64 {
        (gradient.x1 - gradient.x0).hypot(gradient.y1 - gradient.y0)
    }

    /// Every shape this draft can produce is a payload the host's own declared ranges accept. The
    /// ranges are read from the declarations the commit is validated against, so this asserts the
    /// draft against the host rather than against a second copy of its rules.
    fn check(draft: &MaskDraft, what: &str) {
        let patch = commands::geometry(commands::GeometryOp::Set, draft.kind())
            .expect("the kind declares a patch method");
        for (name, value) in draft.values() {
            let declared = patch
                .action
                .parameter(name)
                .unwrap_or_else(|| panic!("{name} is declared by {}", patch.method));
            let ParameterKind::Number { min, max } = declared.kind else {
                panic!("{name} is a number");
            };
            assert!(
                value.is_finite() && (min..=max).contains(&value),
                "{what}: {name} is {value}, outside {min}..={max}"
            );
        }
        if let Some(linear) = linear(draft) {
            assert!(
                axis(&linear) >= linear::MIN_AXIS,
                "{what}: the axis collapsed to {}",
                axis(&linear)
            );
        }
    }

    #[test]
    fn the_method_and_fields_come_from_the_hosts_own_kind_table() {
        assert_eq!(
            MaskDraft::creating(LINEAR, NEUTRAL_BRUSH)
                .expect("a drawn kind")
                .method(),
            Some("mask.create-linear")
        );
        assert_eq!(
            MaskDraft::adding(
                MaskId::new(),
                LINEAR,
                ComponentMode::Subtract,
                NEUTRAL_BRUSH,
            )
            .expect("a drawn kind")
            .method(),
            Some("mask.add-linear")
        );
        assert_eq!(
            MaskDraft::editing(
                MaskId::new(),
                ComponentId::new(),
                LINEAR,
                &json!(NEUTRAL),
                NEUTRAL_BRUSH,
            )
            .expect("a drawn kind")
            .method(),
            Some("mask.set-linear")
        );
        // The radial's methods are generated by the same table, from the same three operations.
        assert_eq!(
            MaskDraft::creating(RADIAL, NEUTRAL_BRUSH)
                .expect("a drawn kind")
                .method(),
            Some("mask.create-radial")
        );
        assert_eq!(
            MaskDraft::adding(
                MaskId::new(),
                RADIAL,
                ComponentMode::Intersect,
                NEUTRAL_BRUSH,
            )
            .expect("a drawn kind")
            .method(),
            Some("mask.add-radial")
        );
        // A kind this build draws nothing for opens no draft at all, so nothing is spelled out here.
        assert!(MaskDraft::creating("cloud", NEUTRAL_BRUSH).is_none());
        // A painted kind has no *generated* method — there is no number a `mask.set-brush` could
        // patch — so all three of its edits go through the one command that carries a path, and the
        // identities it names say which of the three the stroke was.
        for op in [
            MaskDraft::creating(BRUSH, NEUTRAL_BRUSH),
            MaskDraft::adding(MaskId::new(), BRUSH, ComponentMode::Subtract, NEUTRAL_BRUSH),
            MaskDraft::editing(
                MaskId::new(),
                ComponentId::new(),
                BRUSH,
                &Value::Null,
                NEUTRAL_BRUSH,
            ),
        ] {
            assert_eq!(op.expect("a drawn kind").method(), Some("mask.add-stroke"));
        }

        // A create carries the four geometry fields and no mode; an add carries its mode too.
        let create = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH)
            .expect("a drawn kind")
            .fields();
        assert_eq!(create.len(), 4);
        assert_eq!(create["x0"], json!(NEUTRAL.x0));
        assert_eq!(create["y1"], json!(NEUTRAL.y1));
        assert!(create.get("mode").is_none(), "a create is always an add");
        let add = MaskDraft::adding(
            MaskId::new(),
            LINEAR,
            ComponentMode::Intersect,
            NEUTRAL_BRUSH,
        )
        .expect("a drawn kind")
        .fields();
        assert_eq!(add["mode"], json!("intersect"));
        assert_eq!(add.len(), 5);
        // A radial carries its own six, named exactly as its kind declares them.
        let radial = MaskDraft::creating(RADIAL, NEUTRAL_BRUSH)
            .expect("a drawn kind")
            .fields();
        assert_eq!(radial.len(), 6);
        for name in ["x", "y", "radius_x", "radius_y", "angle", "feather"] {
            assert!(radial.contains_key(name), "a radial declares {name}");
        }
        // The identities are never drafted fields: they are the draft's fixed target.
        for fields in [create, add, radial] {
            assert!(fields.get("mask").is_none() && fields.get("component").is_none());
        }
    }

    #[test]
    fn each_handle_moves_what_it_names_and_the_midpoint_moves_the_whole_axis() {
        let mut draft = draft();
        let before = linear(&draft).expect("a gradient");
        // The start handle moves p0 alone.
        draft.begin(MaskHandle::Start, (before.x0, before.y0));
        draft.drag((before.x0 + 0.1, before.y0 - 0.05));
        draft.end();
        let now = linear(&draft).expect("a gradient");
        assert_eq!((now.x1, now.y1), (before.x1, before.y1));
        assert!((now.x0 - (before.x0 + 0.1)).abs() < 1e-12);
        check(&draft, "start");

        // The end handle moves p1 alone.
        let before = linear(&draft).expect("a gradient");
        draft.begin(MaskHandle::End, (before.x1, before.y1));
        draft.drag((before.x1 - 0.2, before.y1 + 0.1));
        draft.end();
        let now = linear(&draft).expect("a gradient");
        assert_eq!((now.x0, now.y0), (before.x0, before.y0));
        check(&draft, "end");

        // The midpoint translates both ends, so the axis keeps its length and direction exactly.
        let before = linear(&draft).expect("a gradient");
        let length = axis(&before);
        let middle = ((before.x0 + before.x1) / 2.0, (before.y0 + before.y1) / 2.0);
        draft.begin(MaskHandle::Middle, middle);
        draft.drag((middle.0 + 0.05, middle.1 + 0.05));
        draft.end();
        let now = linear(&draft).expect("a gradient");
        assert!(
            (axis(&now) - length).abs() < 1e-12,
            "a move resized the axis"
        );
        assert!((now.x0 - (before.x0 + 0.05)).abs() < 1e-12);
        assert!((now.y1 - (before.y1 + 0.05)).abs() < 1e-12);
        check(&draft, "middle");
    }

    #[test]
    fn a_drag_away_and_back_returns_the_starting_shape_exactly() {
        for mut draft in [draft(), radial_draft()] {
            let kind = draft.kind();
            for (handle, from) in draft.handles() {
                let start = draft.values();
                draft.begin(handle, from);
                for step in [(-0.3, 0.2), (0.6, -0.4), (0.0, 0.0)] {
                    draft.drag((from.0 + step.0, from.1 + step.1));
                }
                draft.end();
                assert_eq!(
                    draft.values(),
                    start,
                    "{kind} {handle:?} did not return to where it started"
                );
            }
        }
    }

    #[test]
    fn every_gesture_stays_inside_the_declared_range_and_never_collapses_the_axis() {
        for mut draft in [draft(), radial_draft()] {
            let kind = draft.kind();
            for (handle, from) in draft.handles() {
                draft.begin(handle, from);
                for step in [
                    (-900.0, -900.0),
                    (900.0, 900.0),
                    (0.0, 0.0),
                    (f64::NAN, 0.0),
                    (0.37, -0.91),
                ] {
                    draft.drag((from.0 + step.0, from.1 + step.1));
                    check(&draft, &format!("{kind} {handle:?} {step:?}"));
                }
                draft.end();
            }
        }
        // Dragging one endpoint exactly onto the other still leaves a committable axis.
        let mut draft = draft();
        let gradient = linear(&draft).expect("a gradient");
        draft.begin(MaskHandle::End, (gradient.x1, gradient.y1));
        draft.drag((gradient.x0, gradient.y0));
        draft.end();
        check(&draft, "collapsed onto the other end");
        // And collapsing a radius onto the centre leaves a radius the host's floor accepts.
        let mut draft = radial_draft();
        let ellipse = radial(&draft).expect("an ellipse");
        let centre = (ellipse.x, ellipse.y);
        let grip = draft
            .handles()
            .into_iter()
            .find(|(handle, _)| *handle == MaskHandle::RadiusPlusX)
            .map(|(_, point)| point)
            .expect("the +x radius handle");
        draft.begin(MaskHandle::RadiusPlusX, grip);
        draft.drag(centre);
        draft.end();
        check(&draft, "a radius collapsed onto the centre");
    }

    #[test]
    fn a_sweep_draws_the_whole_gradient_from_the_press_to_the_pointer() {
        let mut draft = draft();
        draft.sweep((0.2, 0.1), (0.8, 0.9));
        let now = linear(&draft).expect("a gradient");
        assert_eq!((now.x0, now.y0), (0.2, 0.1));
        assert_eq!((now.x1, now.y1), (0.8, 0.9));
        assert!(
            draft.dragging(),
            "the sweep continues as an end-handle drag"
        );
        assert_eq!(draft.held(), Some(MaskHandle::End));
        draft.drag((0.5, 0.5));
        assert!((linear(&draft).expect("a gradient").x1 - 0.5).abs() < 1e-12);
        draft.end();
        check(&draft, "swept");
        // A sweep that never moved still leaves an axis the host will accept.
        let mut still = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).expect("a drawn kind");
        still.sweep((0.4, 0.4), (0.4, 0.4));
        check(&still, "a sweep that did not move");
    }

    /// A radial's sweep is the same stroke read as an extent: the press is the centre and the drag
    /// sets both radii, in mask-space units of the stage's height on both axes.
    #[test]
    fn a_radial_sweep_sets_the_centre_and_both_radii_in_mask_space() {
        let mut draft = radial_draft();
        draft.sweep((0.4, 0.5), (0.6, 0.8));
        let now = radial(&draft).expect("an ellipse");
        assert_eq!((now.x, now.y), (0.4, 0.5));
        // 0.2 of the width at an aspect of 1.5 is 0.3 of the height, which is what mask space counts.
        assert!((now.radius_x - 0.3).abs() < 1e-12, "{}", now.radius_x);
        assert!((now.radius_y - 0.3).abs() < 1e-12, "{}", now.radius_y);
        assert_eq!(now.angle, 0.0, "a swept ellipse is upright");
        assert!(draft.dragging());
        draft.drag((0.5, 0.6));
        let now = radial(&draft).expect("an ellipse");
        assert!((now.radius_x - 0.15).abs() < 1e-12, "{}", now.radius_x);
        assert!((now.radius_y - 0.1).abs() < 1e-12, "{}", now.radius_y);
        draft.end();
        check(&draft, "a swept ellipse");
        // A sweep that never moved still leaves radii the host's floor accepts.
        let mut still = radial_draft();
        still.sweep((0.4, 0.4), (0.4, 0.4));
        check(&still, "a radial sweep that did not move");
    }

    /// Each radial handle changes exactly the declared fields it names and leaves the rest alone,
    /// which is what makes the number fields beside them agree with the drag at every point.
    #[test]
    fn each_radial_handle_changes_exactly_the_fields_it_names() {
        let expected: [(MaskHandle, &[&str]); 7] = [
            (MaskHandle::Centre, &["x", "y"]),
            (MaskHandle::RadiusPlusX, &["radius_x"]),
            (MaskHandle::RadiusMinusX, &["radius_x"]),
            (MaskHandle::RadiusPlusY, &["radius_y"]),
            (MaskHandle::RadiusMinusY, &["radius_y"]),
            (MaskHandle::Rotation, &["angle"]),
            (MaskHandle::Feather, &["feather"]),
        ];
        for (handle, changes) in expected {
            let mut draft = radial_draft();
            let before = draft.values();
            let from = draft
                .handles()
                .into_iter()
                .find(|(known, _)| *known == handle)
                .map(|(_, point)| point)
                .unwrap_or_else(|| panic!("{handle:?} is drawn"));
            draft.begin(handle, from);
            draft.drag((from.0 + 0.07, from.1 - 0.05));
            draft.end();
            let after = draft.values();
            for ((name, was), (_, now)) in before.iter().zip(after.iter()) {
                if changes.contains(name) {
                    assert_ne!(was, now, "{handle:?} left {name} where it was");
                } else {
                    assert_eq!(
                        was, now,
                        "{handle:?} changed {name}, which it does not name"
                    );
                }
            }
            check(&draft, &format!("{handle:?}"));
        }
    }

    /// The feather ring sits on the ellipse's own diagonal, so it is reachable even at `feather = 0`
    /// where the ring *is* the boundary and would otherwise sit under a radius handle.
    #[test]
    fn the_feather_ring_is_grabbable_at_every_feather() {
        for feather in [0.0, 1e-9, 50.0, 100.0] {
            let mut draft = radial_draft();
            assert!(draft.set_field("feather", feather), "feather = {feather}");
            let handles = draft.handles();
            let ring = handles
                .iter()
                .find(|(handle, _)| *handle == MaskHandle::Feather)
                .map(|(_, point)| *point)
                .expect("the feather ring is drawn");
            for (handle, point) in &handles {
                if *handle == MaskHandle::Feather {
                    continue;
                }
                assert!(
                    (ring.0 - point.0).hypot(ring.1 - point.1) > 1e-3,
                    "at feather {feather} the ring sits on {handle:?}"
                );
            }
            // And a press on it grabs it rather than one of the others.
            assert_eq!(draft.hit(ring, 0.01), Some(MaskHandle::Feather));
        }
    }

    #[test]
    fn hit_testing_prefers_the_endpoints_and_misses_cleanly() {
        let draft = draft();
        for (handle, point) in draft.handles() {
            assert_eq!(draft.hit(point, 0.02), Some(handle), "{handle:?}");
        }
        assert_eq!(
            draft.hit((0.0, 0.0), 0.02),
            None,
            "a press on nothing grabs nothing"
        );
        // A tolerance large enough to cover every handle answers with an endpoint, not the midpoint.
        let middle = draft
            .handles()
            .into_iter()
            .find(|(handle, _)| *handle == MaskHandle::Middle)
            .map(|(_, point)| point)
            .expect("the midpoint");
        assert_eq!(draft.hit(middle, 9.0), Some(MaskHandle::Start));
        // On a radial the centre is the last resort, so a tolerance that covers everything answers
        // with a radius handle rather than moving the whole ellipse.
        let radial = radial_draft();
        for (handle, point) in radial.handles() {
            assert_eq!(radial.hit(point, 0.005), Some(handle), "{handle:?}");
        }
        assert_ne!(radial.hit((0.5, 0.5), 9.0), Some(MaskHandle::Centre));
    }

    #[test]
    fn a_number_field_sets_exactly_its_own_declared_value_and_refuses_the_rest() {
        let mut draft = draft();
        assert!(draft.set_field("x0", 0.125));
        assert_eq!(draft.value("x0"), Some(0.125));
        assert!(draft.set_field("y1", POSITION_MAX));
        assert_eq!(draft.value("y1"), Some(POSITION_MAX));
        check(&draft, "typed");
        // Out of range, not a number, and a field this kind does not declare: each refused, and
        // each leaves the shape untouched.
        let before = draft.values();
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
            assert_eq!(draft.values(), before, "{name} = {value} changed the shape");
        }

        // A radial's six fields take their own declared ranges, which are not the position range.
        let mut draft = radial_draft();
        for (name, value) in [
            ("x", 0.25),
            ("y", 0.75),
            ("radius_x", 0.4),
            ("radius_y", DISTANCE_MAX),
            ("angle", -173.5),
            ("feather", 0.0),
        ] {
            assert!(draft.set_field(name, value), "{name} = {value} was refused");
        }
        let now = radial(&draft).expect("an ellipse");
        assert_eq!((now.x, now.y), (0.25, 0.75));
        assert_eq!((now.radius_x, now.radius_y), (0.4, DISTANCE_MAX));
        assert_eq!((now.angle, now.feather), (-173.5, 0.0));
        check(&draft, "a typed ellipse");
        let before = draft.values();
        for (name, value) in [
            ("x", POSITION_MAX + 1.0),
            ("radius_x", DISTANCE_MIN / 2.0),
            ("radius_y", DISTANCE_MAX + 1.0),
            ("angle", ANGLE_MAX + 1.0),
            ("feather", FEATHER_MAX + 1.0),
            ("feather", FEATHER_MIN - 1.0),
            ("x0", 0.5),
        ] {
            assert!(
                !draft.set_field(name, value),
                "{name} = {value} was accepted"
            );
            assert_eq!(draft.values(), before, "{name} = {value} changed the shape");
        }
    }

    #[test]
    fn a_gesture_without_a_press_changes_nothing_and_an_interruption_ends_the_drag() {
        let mut draft = draft();
        let before = draft.values();
        draft.drag((0.9, 0.9));
        draft.end();
        assert_eq!(draft.values(), before);
        assert!(!draft.dragging());

        let gradient = linear(&draft).expect("a gradient");
        draft.begin(MaskHandle::End, (gradient.x1, gradient.y1));
        assert!(draft.dragging());
        // A conflict or a reapply of the core draft behind the gesture interrupts it.
        draft.interrupt();
        assert!(!draft.dragging(), "an interruption drops the gesture");
        draft.drag((0.9, 0.9));
        assert_eq!(
            draft.values(),
            before,
            "an interrupted drag ignores the pointer and keeps what this client drew"
        );
    }

    /// The map is applied locally, so it must agree with the host both ways and for every tail the
    /// delivered modules produce: the identity, a crop, and a quarter turn that swaps the axes.
    #[test]
    fn the_content_map_round_trips_every_tail_the_geometry_produces() {
        use luxforge_core::{StageSize, StageTransform};
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
            // The aspect mask space is defined in is the content stage's, whatever the crop did.
            assert!(
                (map.aspect() - 1.5).abs() < 1e-12,
                "{what}: {}",
                map.aspect()
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
        let draft = MaskDraft::editing(
            mask.clone(),
            component.clone(),
            LINEAR,
            &json!(NEUTRAL),
            NEUTRAL_BRUSH,
        )
        .expect("a drawn kind");
        let summary = draft.summary();
        assert_eq!(summary["mask"], json!(mask.as_str()));
        assert_eq!(summary["component"], json!(component.as_str()));
        assert_eq!(summary["kind"], json!(LINEAR));
        assert_eq!(summary["op"], json!("Update"));
        assert_eq!(summary["method"], json!("mask.set-linear"));
        assert_eq!(summary["shape"]["y1"], json!(NEUTRAL.y1));
        // A create names no mask and no component, because it has none yet.
        let creating = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH)
            .expect("a drawn kind")
            .summary();
        assert_eq!(creating["mask"], Value::Null);
        assert_eq!(creating["op"], json!("New mask"));
        // A radial's summary carries its own six fields under the same key.
        let radial = radial_draft().summary();
        assert_eq!(radial["kind"], json!(RADIAL));
        assert_eq!(radial["aspect"], json!(1.5));
        assert_eq!(radial["shape"]["feather"], json!(NEUTRAL_RADIAL.feather));
    }
}
