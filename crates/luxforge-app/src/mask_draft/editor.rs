//! The shape editors: one per component kind this build draws, each owning everything about its
//! own kind that a gesture touches.
//!
//! A drawn kind's editor holds that kind's geometry and the gesture in flight over it, and answers
//! every question the draft, the canvas and the panel ask of a shape: its declared numbers, its
//! handles and where they sit, what a press, a drag and a sweep do, what a typed number is allowed
//! to be, which host method the release commits through and what the figure looks like. The draft
//! ([`super::MaskDraft`]) and the canvas ([`crate::mask_canvas`]) hold a [`DrawnShape`] and never ask
//! which kind it is: they call its [`ShapeEditor`], and the one place a kind is matched is
//! [`DrawnShape`]'s dereference below. Adding a drawn kind is one module beside [`super::linear`],
//! [`super::radial`] and [`super::brush`], one variant and one row of [`DRAWN_KINDS`].
//!
//! Nothing here holds a framework type. A figure is described through a [`Pen`] in normalized
//! content coordinates and mask-space distances, and the canvas implements the pen, so a shape's
//! drawing is testable without a renderer and the canvas draws every kind with the same three
//! primitives.
use super::{
    brush::{Brush, BrushEditor, BrushStroke},
    linear::LinearEditor,
    radial::RadialEditor,
};
use luxforge_core::mask::commands::GeometryOp;
use serde_json::{Map, Value};
use std::ops::{Deref, DerefMut};

/// Which part of the drawn figure a press grabbed.
///
/// The linear gradient's figure is the design's three lines — `p0`, the midpoint and `p1` — with an
/// end handle on each endpoint and the midpoint grabbable to move the whole axis. The radial's is
/// the design's set: four radius handles, a centre, a rotation grip and a feather ring. A painted
/// kind has none: every press on the photograph paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskHandle {
    /// Linear: the end of the axis at coverage 0.
    Start,
    /// Linear: the midpoint, which moves both ends together, keeping length and direction.
    Middle,
    /// Linear: the end of the axis at coverage 1.
    End,
    /// Radial: the centre, which moves the whole ellipse.
    Centre,
    /// Radial: the four radius handles, on the ellipse's own axes.
    RadiusPlusX,
    RadiusMinusX,
    RadiusPlusY,
    RadiusMinusY,
    /// Radial: the rotation grip, beyond the `+x` radius handle on the same axis.
    Rotation,
    /// Radial: the feather ring, on the ellipse's own 45° diagonal so it never coincides with a
    /// radius handle — which it would on any axis at `feather = 0`, where the ring is the boundary.
    Feather,
    /// The create gesture's own grab, which sets both radii from one drag. It is not drawn: the
    /// handles above are what a committed component shows.
    Extent,
}

impl MaskHandle {
    /// This handle moves the whole figure rather than reshaping it, which is what the pointer says
    /// over it.
    pub(crate) fn moves_figure(self) -> bool {
        matches!(self, Self::Middle | Self::Centre)
    }
}

/// What a figure is drawn with. The canvas implements it; an editor describes its figure through it
/// in the coordinates its geometry is stored in, so the figure follows a crop, a straighten or a
/// quarter turn with the picture because the canvas maps every point through the same affine.
pub(crate) trait Pen {
    /// A straight line between two normalized content positions.
    fn line(&mut self, from: (f64, f64), to: (f64, f64), alpha: f32);
    /// An ellipse of mask space: `radii` in mask-space units about a normalized content `centre`,
    /// turned by `angle` degrees. A circle in mask space is a circle in pixels at any aspect ratio.
    fn ellipse(
        &mut self,
        centre: (f64, f64),
        radii: (f64, f64),
        angle: f64,
        alpha: f32,
        dashed: bool,
    );
    /// A painted path of normalized content positions, drawn as wide as a mask-space `radius` with
    /// round ends, so a one-position path is the single dab the host evaluates.
    fn path(&mut self, points: &[[f64; 2]], radius: f64, alpha: f32);
}

/// Everything one drawn kind's editor owns.
///
/// Every drag is evaluated against the shape the gesture *started* with, never the previous
/// position, and every handle moves by a **difference** from where the press landed, so a drag away
/// and back returns the starting shape exactly and a press slightly off a handle never makes the
/// shape jump. Every shape an editor produces is one its kind's own declared ranges accept, so a
/// gesture never produces a payload the commit would refuse.
pub(crate) trait ShapeEditor {
    /// The registered component kind, which is half of the generated method's name.
    fn kind(&self) -> &'static str;

    /// The host method one operation commits through, read from the host's own tables so the
    /// desktop spells no method name of its own. A kind whose geometry is declared commits through
    /// its generated method; a painted kind overrides this.
    fn method(&self, op: GeometryOp) -> Option<&'static str> {
        luxforge_core::mask::commands::geometry(op, self.kind()).map(|command| command.method)
    }

    /// The gesture's declared number fields, in the order its command declares them.
    fn values(&self) -> Vec<(&'static str, f64)>;

    /// The fields the commit carries besides the declared numbers and the mode.
    fn extra_fields(&self, _fields: &mut Map<String, Value>) {}

    /// Set one declared field by name, as its generated number field does. An unknown name and a
    /// value the declared range refuses both leave the shape exactly as it was.
    fn set_field(&mut self, name: &str, value: f64) -> bool;

    /// Where every drawn handle sits, in normalized content coordinates, in the order they are hit
    /// tested: the ones that sit on a specific point win over the ones a whole region answers for.
    fn handles(&self, _aspect: f64) -> Vec<(MaskHandle, (f64, f64))> {
        Vec::new()
    }

    /// Start a drag of one handle, snapshotting the shape every later drag is measured against.
    fn begin(&mut self, _handle: MaskHandle, _point: (f64, f64)) {}

    /// Re-evaluate the drag at a new pointer position. Nothing is committed and no host method is
    /// called: this is the whole of what a pointer move costs.
    fn drag(&mut self, _point: (f64, f64), _aspect: f64) {}

    /// Draw a whole shape in one stroke from a press that grabbed no handle.
    fn sweep(&mut self, _from: (f64, f64), _to: (f64, f64), _aspect: f64) {}

    /// Let go of whatever the pointer holds. What was drawn stays; the commit is a separate decision.
    fn release(&mut self);

    /// The handle a drag currently holds.
    fn held(&self) -> Option<MaskHandle> {
        None
    }

    /// A pointer is down: a handle is being dragged, or a stroke is being painted.
    fn dragging(&self) -> bool;

    /// The stroke a painted kind is drawing, which is also what says this editor paints.
    fn stroke(&self) -> Option<&BrushStroke> {
        None
    }

    fn stroke_mut(&mut self) -> Option<&mut BrushStroke> {
        None
    }

    /// Describe this shape's figure. `pointer` is where the pointer is over the canvas, in
    /// normalized content coordinates, for a kind that draws a cursor of its own.
    fn draw(&self, aspect: f64, pointer: Option<(f64, f64)>, pen: &mut dyn Pen);
}

/// One drawn kind's editor, as the draft holds it.
///
/// An enum rather than a box so the draft stays a plain value — cloned into a gesture's history and
/// compared by value — and so opening one allocates nothing. It dereferences to its
/// [`ShapeEditor`], and that dereference is the one match on a drawn kind in the desktop.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DrawnShape {
    Linear(LinearEditor),
    Radial(RadialEditor),
    Brush(BrushEditor),
}

impl Deref for DrawnShape {
    type Target = dyn ShapeEditor;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Linear(editor) => editor,
            Self::Radial(editor) => editor,
            Self::Brush(editor) => editor,
        }
    }
}

impl DerefMut for DrawnShape {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Linear(editor) => editor,
            Self::Radial(editor) => editor,
            Self::Brush(editor) => editor,
        }
    }
}

impl DrawnShape {
    /// The editor for one kind: from its stored payload when one is given, from the kind's neutral
    /// shape otherwise. `None` for a kind this build draws nothing for, and for a stored payload
    /// that kind's editor cannot read — which is what the panel says rather than opening a gesture
    /// that would edit the wrong geometry.
    pub(crate) fn open(kind: &str, stored: Option<&Value>, brush: Brush) -> Option<Self> {
        drawn_kind(kind).and_then(|row| (row.open)(stored, brush))
    }
}

/// One drawn kind's row: its token, whether its geometry is painted, and how its editor opens.
struct DrawnKind {
    kind: &'static str,
    paints: bool,
    open: fn(Option<&Value>, Brush) -> Option<DrawnShape>,
}

/// The component kinds this build draws on the canvas: a gradient by its handles, a brush by
/// painting it. A registered kind that is not here is edited through its generated number fields,
/// which come from the same declarations.
const DRAWN_KINDS: &[DrawnKind] = &[
    DrawnKind {
        kind: super::linear::KIND,
        paints: false,
        open: super::linear::open,
    },
    DrawnKind {
        kind: super::radial::KIND,
        paints: false,
        open: super::radial::open,
    },
    DrawnKind {
        kind: super::brush::KIND,
        paints: true,
        open: super::brush::open,
    },
];

fn drawn_kind(kind: &str) -> Option<&'static DrawnKind> {
    DRAWN_KINDS.iter().find(|row| row.kind == kind)
}

/// This kind is edited on the canvas in this build. One list, read by the panel that offers the
/// gesture and by the draft that opens one, so the two cannot disagree.
pub(crate) fn drawable(kind: &str) -> bool {
    drawn_kind(kind).is_some()
}

/// This kind's geometry is a drawn path, so its gesture paints rather than drags handles.
pub(crate) fn paintable(kind: &str) -> bool {
    drawn_kind(kind).is_some_and(|row| row.paints)
}

/// A handle's drag state: which handle, what the shape was when the press landed and where it
/// landed. Every drag is measured against these, never against the previous pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Grab<S> {
    pub(super) handle: MaskHandle,
    pub(super) start: S,
    pub(super) start_point: (f64, f64),
}

impl<S> Grab<S> {
    /// How far the pointer has travelled since the press, in normalized content coordinates.
    pub(super) fn travel(&self, point: (f64, f64)) -> (f64, f64) {
        (point.0 - self.start_point.0, point.1 - self.start_point.1)
    }
}

pub(super) fn finite(point: (f64, f64)) -> bool {
    point.0.is_finite() && point.1.is_finite()
}
