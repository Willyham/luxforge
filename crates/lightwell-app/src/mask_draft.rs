//! The transient mask shape draft: the single owner of everything a shape gesture changes before it
//! commits.
//!
//! This module holds no framework types and, unlike [`crate::crop_draft`], no stage raster either.
//! A mask component's geometry is stored in **content-stage normalized** coordinates — `x` and `y`
//! as fractions of the content stage, legal over
//! [`POSITION_MIN`][lightwell_core::mask::POSITION_MIN]`..=`[`POSITION_MAX`][lightwell_core::mask::POSITION_MAX]
//! — and that is the space this draft works in throughout. Mapping a pointer position into it is the
//! canvas's job, through `render.transform`'s affine and the canvas view, and it is done locally per
//! move rather than by asking the host ([performance rule 12](../../docs/engineering/performance-rules.md)).
//!
//! The one number about the stage this draft does hold is its **aspect**, `W/H`, because mask space
//! is defined in terms of it: a stored distance is in units of the content stage's height on both
//! axes, so a circle stays a circle at any aspect ratio. It is read from the same one
//! `render.transform` answer the handles are mapped through, once per gesture, and never per move.
//!
//! What lives here is the state machine: which handle a gesture grabbed, what the shape looked like
//! when it started, and which command the release will commit. Every drag is evaluated against the
//! shape the gesture *started* with, never the previous position, and every handle moves by a
//! **difference** from where the press landed, so a drag away and back returns the starting shape
//! exactly and a press slightly off a handle never makes the shape jump.
use lightwell_core::{
    ComponentId, ComponentMode, MaskId, StageTransform,
    mask::{
        ANGLE_MAX, ANGLE_MIN, DISTANCE_MAX, DISTANCE_MIN, FEATHER_MAX, FEATHER_MIN, LinearGradient,
        POSITION_MAX, POSITION_MIN, RadialGradient, commands::GeometryOp,
    },
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

/// The component kinds this build draws handles for. A component of any other registered kind is
/// edited through its generated number fields, which come from the same declarations.
pub(crate) const LINEAR: &str = "linear";
pub(crate) const RADIAL: &str = "radial";
/// The one kind whose geometry is painted rather than dragged, from the host's own kind table.
pub(crate) const BRUSH: &str = lightwell_core::mask::BRUSH;

/// This kind is edited on the canvas in this build: a gradient by its handles, a brush by painting
/// it. One list, read by the panel that offers the gesture and by the draft that opens one, so the
/// two cannot disagree.
pub(crate) fn drawable(kind: &str) -> bool {
    kind == LINEAR || kind == RADIAL || kind == BRUSH
}

/// This kind's geometry is a drawn path, so its gesture paints rather than drags handles.
pub(crate) fn paintable(kind: &str) -> bool {
    kind == BRUSH
}

/// The brush one stroke is drawn with: the settings the gesture offers, in the ranges
/// `mask.add-stroke` declares for them.
///
/// **There is no density.** Lightroom's Density needs a build-up model along a single stroke, which
/// would make coverage depend on the stamp spacing and therefore on the resolution the stroke was
/// stamped at; nothing in the frozen mathematics has a stamp in it. Flow is delivered and is exactly
/// what the study states: the coverage one pass reaches.
///
/// **`limit_to_colour` is not Auto Mask**, and the panel and the guide say so. It multiplies the
/// stroke's coverage by a similarity to the colour under the brush where the stroke began — a
/// per-pixel colour test with no notion of an edge or of connectivity — and the colour itself is
/// never the client's: the request carries this flag and the host reads the pixel the masked
/// operation receives at the stroke's first position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Brush {
    /// The radius in mask-space units, one unit being the content stage's height on both axes.
    pub(crate) size: f64,
    pub(crate) feather: f64,
    pub(crate) flow: f64,
    /// This stroke removes coverage rather than adding it, for the whole of its life.
    pub(crate) erase: bool,
    /// This stroke is held to the colour under the brush where it began.
    pub(crate) limit_to_colour: bool,
    /// How tight that hold is, on the colour range's own refine axis.
    pub(crate) colour_refine: f64,
}

/// What the brush starts at: a fifth of the frame's height across, softly feathered, at full flow —
/// the brush a person reaches for to lighten a face. It is unlimited, because a limit is something a
/// person asks for; the refine starts where the colour range's own does, which is the measured
/// setting that holds an ordinary surface across a stop of shading.
pub(crate) const NEUTRAL_BRUSH: Brush = Brush {
    size: 0.1,
    feather: 50.0,
    flow: 100.0,
    erase: false,
    limit_to_colour: false,
    colour_refine: lightwell_core::mask::REFINE_DEFAULT,
};

impl Brush {
    /// The declared fields one `mask.add-stroke` carries besides its path, in the order the command
    /// declares them. The names are the command's; this spells no field of its own.
    pub(crate) fn values(self) -> Vec<(&'static str, f64)> {
        vec![
            ("size", self.size),
            ("feather", self.feather),
            ("flow", self.flow),
            ("colour_refine", self.colour_refine),
        ]
    }

    /// Set one declared number by name, refusing a value the command's own range would refuse, so a
    /// key, a nudge and a typed field all land on the same rule.
    pub(crate) fn set(&mut self, name: &str, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        let Some(declared) = lightwell_core::mask::commands::find(ADD_STROKE)
            .and_then(|command| command.action.parameter(name))
        else {
            return false;
        };
        let lightwell_core::ParameterKind::Number { min, max } = declared.kind else {
            return false;
        };
        let value = value.clamp(min, max);
        match name {
            "size" => self.size = value,
            "feather" => self.feather = value,
            "flow" => self.flow = value,
            "colour_refine" => self.colour_refine = value,
            _ => return false,
        }
        true
    }

    /// Move one declared number by `steps` of its own declared step. The brackets and the panel's
    /// nudges are the same call, so the key and the button can never move by different amounts.
    pub(crate) fn nudge(&mut self, name: &str, steps: f64) -> bool {
        let Some(step) = lightwell_core::mask::commands::find(ADD_STROKE)
            .and_then(|command| command.action.parameter(name))
            .and_then(|declared| declared.step)
            .filter(|step| step.is_finite() && *step > 0.0)
        else {
            return false;
        };
        let current = self
            .values()
            .into_iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value);
        match current {
            Some(value) => self.set(name, value + steps * step),
            None => false,
        }
    }
}

/// The host command every stroke commits through, whichever of its three edits it turns out to be.
const ADD_STROKE: &str = lightwell_core::mask::commands::ADD_STROKE;

/// One stroke as it is being painted: the path the pointer has drawn so far, and the brush it is
/// being drawn with.
///
/// The path is captured raw and **decimated only when it is posted**, on the host's own grid at the
/// host's own tolerance, which is idempotent — so what the desktop sends and what an agent would
/// send arrive at the same stored stroke. Nothing here calls the host: a pointer move appends a
/// position and the canvas redraws, which is why path feedback never waits on a render.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushStroke {
    /// The brush this stroke was begun with. `erase` is frozen for the stroke's whole life, which is
    /// what "holding the modifier erases while the stroke lasts" means.
    pub(crate) brush: Brush,
    /// The captured path in normalized content coordinates, in drawn order.
    path: Vec<[f64; 2]>,
    /// The pointer is down: moves extend the path, and a move with it up is not painting.
    painting: bool,
}

impl BrushStroke {
    pub(crate) fn new(brush: Brush) -> Self {
        Self {
            brush,
            path: Vec::new(),
            painting: false,
        }
    }

    /// The pointer went down: this stroke starts here, at the brush it is holding now.
    fn press(&mut self, point: (f64, f64)) {
        if !finite(point) {
            return;
        }
        self.path = vec![[point.0, point.1]];
        self.painting = true;
    }

    /// The pointer moved with the button down. A position identical to the last one is dropped here
    /// rather than posted: the stored grid would drop it anyway, and a still pointer must not grow
    /// the path without bound.
    fn paint(&mut self, point: (f64, f64)) -> bool {
        if !self.painting || !finite(point) {
            return false;
        }
        let point = [point.0, point.1];
        if self.path.last() == Some(&point) {
            return false;
        }
        self.path.push(point);
        true
    }

    /// The pointer came up. The path it drew stays; committing it is a separate decision.
    fn release(&mut self) {
        self.painting = false;
    }

    /// The path as it was captured, for the canvas to draw while the stroke is in flight.
    pub(crate) fn captured(&self) -> &[[f64; 2]] {
        &self.path
    }

    pub(crate) fn painting(&self) -> bool {
        self.painting
    }

    /// The path this stroke posts: decimated by the host's own contract, on its grid, at its
    /// tolerance. Deterministic, so the same captured path is always the same stored stroke and
    /// therefore the same content address.
    pub(crate) fn points(&self) -> Vec<[f64; 2]> {
        lightwell_core::path::decimate(&self.path).unwrap_or_default()
    }

    /// Something was drawn. An empty stroke commits nothing: a click that painted no position is not
    /// an edit, and a commit of one would be a history entry nobody made.
    pub(crate) fn drawn(&self) -> bool {
        !self.path.is_empty()
    }
}

/// The geometry one gesture edits: a registered kind's declared numbers, or a painted path.
///
/// There is no third state and no "either": a draft is opened for a kind this build draws, with the
/// geometry that kind has, or it is not opened at all.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaskGeometry {
    Shape(MaskShape),
    Brush(BrushStroke),
}

/// The geometry one gesture edits: one registered kind's stored payload, in the shape the host
/// parses and the generated method declares.
///
/// There is no third state. A draft is opened for a kind this build draws, or it is not opened at
/// all, so a shape is never "a linear gradient that might be a radial".
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MaskShape {
    Linear(LinearGradient),
    Radial(RadialGradient),
}

impl MaskShape {
    /// The shape one kind's gesture starts from when nothing has been drawn yet.
    fn neutral(kind: &str) -> Option<Self> {
        match kind {
            LINEAR => Some(Self::Linear(NEUTRAL)),
            RADIAL => Some(Self::Radial(NEUTRAL_RADIAL)),
            _ => None,
        }
    }

    /// The declared fields this kind's methods carry, in the order the kind declares them.
    pub(crate) fn values(self) -> Vec<(&'static str, f64)> {
        match self {
            Self::Linear(linear) => vec![
                ("x0", linear.x0),
                ("y0", linear.y0),
                ("x1", linear.x1),
                ("y1", linear.y1),
            ],
            Self::Radial(radial) => vec![
                ("x", radial.x),
                ("y", radial.y),
                ("radius_x", radial.radius_x),
                ("radius_y", radial.radius_y),
                ("angle", radial.angle),
                ("feather", radial.feather),
            ],
        }
    }

    /// The handles this shape draws, in the order they are hit tested: the ones that sit on a
    /// specific point win over the ones a whole region answers for.
    pub(crate) fn handles(self) -> &'static [MaskHandle] {
        match self {
            Self::Linear(_) => &[MaskHandle::Start, MaskHandle::End, MaskHandle::Middle],
            Self::Radial(_) => &[
                MaskHandle::RadiusPlusX,
                MaskHandle::RadiusMinusX,
                MaskHandle::RadiusPlusY,
                MaskHandle::RadiusMinusY,
                MaskHandle::Rotation,
                MaskHandle::Feather,
                MaskHandle::Centre,
            ],
        }
    }
}

/// Which part of the drawn figure a press grabbed.
///
/// The linear gradient's figure is the design's three lines — `p0`, the midpoint and `p1` — with an
/// end handle on each endpoint and the midpoint grabbable to move the whole axis. The radial's is
/// the design's set: four radius handles, a centre, a rotation grip and a feather ring.
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

impl MaskHandle {
    /// Where this handle sits, in normalized content coordinates, or `None` when it belongs to
    /// another kind's figure.
    pub(crate) fn point(self, shape: &MaskShape, aspect: f64) -> Option<(f64, f64)> {
        match (self, shape) {
            (Self::Start, MaskShape::Linear(linear)) => Some((linear.x0, linear.y0)),
            (Self::Middle, MaskShape::Linear(linear)) => {
                Some(((linear.x0 + linear.x1) / 2.0, (linear.y0 + linear.y1) / 2.0))
            }
            (Self::End, MaskShape::Linear(linear)) => Some((linear.x1, linear.y1)),
            (_, MaskShape::Radial(radial)) => {
                let ellipse = Ellipse::new(*radial, aspect);
                let (du, dv) = match self {
                    Self::Centre => (0.0, 0.0),
                    Self::RadiusPlusX => ellipse.local(radial.radius_x, 0.0),
                    Self::RadiusMinusX => ellipse.local(-radial.radius_x, 0.0),
                    Self::RadiusPlusY => ellipse.local(0.0, radial.radius_y),
                    Self::RadiusMinusY => ellipse.local(0.0, -radial.radius_y),
                    Self::Rotation => ellipse.local(radial.radius_x + ROTATION_REACH, 0.0),
                    Self::Feather => {
                        let ring = (1.0 - radial.feather / 100.0).max(MIN_RING_GRIP);
                        ellipse.local(
                            ring * radial.radius_x * DIAGONAL,
                            ring * radial.radius_y * DIAGONAL,
                        )
                    }
                    Self::Start | Self::Middle | Self::End | Self::Extent => return None,
                };
                Some(ellipse.to_content(ellipse.cu + du, ellipse.cv + dv))
            }
            _ => None,
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
    start: MaskShape,
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
    /// What this gesture is drawing: a gradient's numbers, or a painted path.
    pub(crate) geometry: MaskGeometry,
    /// The content stage's `W/H`, which mask space is defined in terms of. It is `1.0` until
    /// `render.transform` answers, which is also when the handles first become drawable, so no
    /// gesture is ever evaluated against an aspect that was guessed.
    aspect: f64,
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

impl MaskDraft {
    /// Start a gesture that will create a new mask from a shape of this kind, or from a stroke.
    pub(crate) fn creating(kind: impl Into<String>, brush: Brush) -> Self {
        Self::seeded(None, None, kind, MaskDraftOp::Create, None, brush)
    }

    /// Start a gesture that will add a further component to an existing mask, in that mode.
    pub(crate) fn adding(
        mask: MaskId,
        kind: impl Into<String>,
        mode: ComponentMode,
        brush: Brush,
    ) -> Self {
        Self::seeded(Some(mask), None, kind, MaskDraftOp::Add(mode), None, brush)
    }

    /// Edit an existing component: the shape starts at exactly the stored payload, so reopening a
    /// draft shows what was committed. A brush component has no shape to start from — its strokes
    /// are already drawn — so this opens the next stroke on it instead.
    pub(crate) fn editing(
        mask: MaskId,
        component: ComponentId,
        kind: impl Into<String>,
        shape: Option<MaskShape>,
        brush: Brush,
    ) -> Self {
        Self::seeded(
            Some(mask),
            Some(component),
            kind,
            MaskDraftOp::Set,
            shape,
            brush,
        )
    }

    fn seeded(
        mask: Option<MaskId>,
        component: Option<ComponentId>,
        kind: impl Into<String>,
        op: MaskDraftOp,
        shape: Option<MaskShape>,
        brush: Brush,
    ) -> Self {
        let kind = kind.into();
        // A painted kind has no shape at all: what it edits is the stroke about to be drawn, at the
        // brush the person is holding.
        let geometry = if paintable(&kind) {
            MaskGeometry::Brush(BrushStroke::new(brush))
        } else {
            // A kind this build cannot draw has no neutral shape and therefore no handles. The draft
            // still exists — it names the kind and it has no method, which is the refusal the panel
            // shows — rather than being quietly turned into a linear gradient.
            let shape = shape
                .or_else(|| MaskShape::neutral(&kind))
                .unwrap_or(MaskShape::Linear(NEUTRAL));
            MaskGeometry::Shape(legal(shape, shape))
        };
        Self {
            mask,
            component,
            kind,
            op,
            geometry,
            aspect: 1.0,
            gesture: None,
        }
    }

    /// The shape this gesture drags, when it drags one.
    pub(crate) fn shape(&self) -> Option<MaskShape> {
        match &self.geometry {
            MaskGeometry::Shape(shape) => Some(*shape),
            MaskGeometry::Brush(_) => None,
        }
    }

    /// The stroke this gesture paints, when it paints one.
    pub(crate) fn brush(&self) -> Option<&BrushStroke> {
        match &self.geometry {
            MaskGeometry::Brush(stroke) => Some(stroke),
            MaskGeometry::Shape(_) => None,
        }
    }

    fn brush_mut(&mut self) -> Option<&mut BrushStroke> {
        match &mut self.geometry {
            MaskGeometry::Brush(stroke) => Some(stroke),
            MaskGeometry::Shape(_) => None,
        }
    }

    /// The pointer went down on the photograph: this stroke starts here.
    pub(crate) fn paint_begin(&mut self, point: (f64, f64)) {
        if let Some(stroke) = self.brush_mut() {
            stroke.press(point);
        }
    }

    /// One pointer move with the button down. It returns whether the path grew, because a move that
    /// added nothing must not cost a round trip.
    pub(crate) fn paint_to(&mut self, point: (f64, f64)) -> bool {
        self.brush_mut().is_some_and(|stroke| stroke.paint(point))
    }

    /// The pointer came up. What it drew stays; the commit is a separate decision.
    pub(crate) fn paint_end(&mut self) {
        if let Some(stroke) = self.brush_mut() {
            stroke.release();
        }
    }

    /// Change the brush this stroke is being drawn with. Refused once the stroke is down: the brush
    /// a stroke was begun with is the brush it was drawn with, for its whole life, which is what
    /// makes a stored stroke the record of one pass and not of a setting that moved under it.
    pub(crate) fn set_brush(&mut self, brush: Brush) -> bool {
        match self.brush_mut() {
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

    /// The gradient this gesture edits, when it edits one.
    pub(crate) fn linear(&self) -> Option<LinearGradient> {
        match self.shape()? {
            MaskShape::Linear(linear) => Some(linear),
            MaskShape::Radial(_) => None,
        }
    }

    /// The ellipse this gesture edits, when it edits one.
    pub(crate) fn radial(&self) -> Option<RadialGradient> {
        match self.shape()? {
            MaskShape::Radial(radial) => Some(radial),
            MaskShape::Linear(_) => None,
        }
    }

    /// The host method this draft commits through, read from the host's own tables so the desktop
    /// spells no method name of its own.
    ///
    /// A painted kind has no generated geometry method — there is no number a `mask.set-brush` could
    /// patch — so all three of its edits go through the one command that carries a path, and which
    /// of the three it is, is what the identities it names say. `None` for a kind this build
    /// cannot draw.
    pub(crate) fn method(&self) -> Option<&'static str> {
        match self.geometry {
            MaskGeometry::Brush(_) => {
                lightwell_core::mask::commands::find(ADD_STROKE).map(|command| command.method)
            }
            MaskGeometry::Shape(_) => {
                lightwell_core::mask::commands::geometry(self.op.geometry_op(), &self.kind)
                    .map(|command| command.method)
            }
        }
    }

    /// The drafted fields the commit carries: the geometry's own fields, and the mode when the
    /// method takes one. The identities it addresses are the draft's target, which the commit sends
    /// beside them as the command's identity parameters.
    ///
    /// A stroke's path is decimated here, where it is posted, rather than as it is captured: the
    /// contract is idempotent, so the desktop's decimated path and an agent's raw one reach the same
    /// stored stroke, and the canvas keeps drawing what the pointer actually did.
    pub(crate) fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        // A mode belongs to a component, so only the edit that makes one carries it. A stroke
        // appended to a component that exists is refused for carrying one, which is why this is the
        // same rule for both geometries rather than a brush-shaped exception.
        if let MaskDraftOp::Add(mode) = self.op {
            fields.insert("mode".to_owned(), json!(mode.as_str()));
        }
        if let MaskGeometry::Brush(stroke) = &self.geometry {
            fields.insert("points".to_owned(), json!(stroke.points()));
            fields.insert("erase".to_owned(), json!(stroke.brush.erase));
            // The flag, and never a colour: the host reads the pixel the masked operation receives
            // at the stroke's first position and stores that with the stroke.
            fields.insert(
                "limit_to_colour".to_owned(),
                json!(stroke.brush.limit_to_colour),
            );
        }
        for (name, value) in self.values() {
            fields.insert(name.to_owned(), json!(value));
        }
        fields
    }

    /// The gesture's declared number fields, in the order its command declares them.
    pub(crate) fn values(&self) -> Vec<(&'static str, f64)> {
        match &self.geometry {
            MaskGeometry::Shape(shape) => shape.values(),
            MaskGeometry::Brush(stroke) => stroke.brush.values(),
        }
    }

    /// A pointer is down: a handle is being dragged, or a stroke is being painted.
    pub(crate) fn dragging(&self) -> bool {
        self.gesture.is_some() || self.brush().is_some_and(BrushStroke::painting)
    }

    /// The handle a gesture currently holds, for the status line and the canvas cursor.
    pub(crate) fn held(&self) -> Option<MaskHandle> {
        self.gesture.map(|gesture| gesture.handle)
    }

    /// Let go of whatever the pointer holds: the core draft behind this gesture was conflicted or
    /// rebased, so a drag must not carry on into it. What was drawn is kept — it is what a Reapply
    /// re-sends.
    pub(crate) fn interrupt(&mut self) {
        self.gesture = None;
        self.paint_end();
    }

    /// Which handle a press at this normalized point grabbed, or none when it grabbed nothing.
    /// `tolerance` is the hit radius in normalized units, so the canvas keeps a handle the same size
    /// on screen at every zoom by dividing its pixel radius by the drawn scale. The order is the
    /// shape's own: the handles that sit on a specific point win over the ones that move everything.
    ///
    /// A painted gesture has no handles at all: every press on the photograph paints, which is why a
    /// brush draws its cursor rather than grips.
    pub(crate) fn hit(&self, point: (f64, f64), tolerance: f64) -> Option<MaskHandle> {
        let shape = self.shape()?;
        let tolerance = tolerance.max(0.0);
        shape
            .handles()
            .iter()
            .copied()
            .find(|handle| match handle.point(&shape, self.aspect) {
                Some((x, y)) => (point.0 - x).hypot(point.1 - y) <= tolerance,
                None => false,
            })
    }

    /// Where every drawn handle of this gesture sits, in normalized content coordinates. One list,
    /// read by the canvas that draws them and by the hit test above.
    pub(crate) fn handles(&self) -> Vec<(MaskHandle, (f64, f64))> {
        let Some(shape) = self.shape() else {
            return Vec::new();
        };
        shape
            .handles()
            .iter()
            .filter_map(|handle| {
                handle
                    .point(&shape, self.aspect)
                    .map(|point| (*handle, point))
            })
            .collect()
    }

    /// Start a gesture, snapshotting the shape every later `drag` is measured against.
    pub(crate) fn begin(&mut self, handle: MaskHandle, point: (f64, f64)) {
        let Some(shape) = self.shape() else {
            return;
        };
        if !finite(point) {
            return;
        }
        self.gesture = Some(Gesture {
            handle,
            start: shape,
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
        let moved = match gesture.start {
            MaskShape::Linear(start) => MaskShape::Linear(dragged_linear(start, gesture, point)),
            MaskShape::Radial(start) => {
                MaskShape::Radial(dragged_radial(start, gesture, point, self.aspect))
            }
        };
        self.geometry = MaskGeometry::Shape(legal(moved, gesture.start));
    }

    /// Draw a whole shape in one stroke.
    ///
    /// The linear gesture is Lightroom's: the press sets `p0` and the drag sets `p1`, from the
    /// untouched side towards the affected one. The radial's is the same stroke read as an extent:
    /// the press sets the centre and the drag sets both radii, upright, so one drag draws the
    /// ellipse a person meant to draw.
    pub(crate) fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        let Some(held) = self.shape() else {
            return;
        };
        if !finite(from) || !finite(to) {
            return;
        }
        let (shape, handle) = match held {
            MaskShape::Linear(_) => (
                MaskShape::Linear(LinearGradient {
                    x0: from.0,
                    y0: from.1,
                    x1: to.0,
                    y1: to.1,
                }),
                MaskHandle::End,
            ),
            MaskShape::Radial(radial) => (
                MaskShape::Radial(RadialGradient {
                    x: from.0,
                    y: from.1,
                    angle: 0.0,
                    ..extent(radial, from, to, self.aspect)
                }),
                MaskHandle::Extent,
            ),
        };
        let shape = legal(shape, held);
        self.geometry = MaskGeometry::Shape(shape);
        self.gesture = Some(Gesture {
            handle,
            start: shape,
            start_point: to,
        });
    }

    /// Finish the gesture. The shape it produced stays; the commit is a separate decision.
    pub(crate) fn end(&mut self) {
        self.gesture = None;
        self.paint_end();
    }

    /// Set one declared field by name, as its generated number field does. An unknown name and a
    /// value the declared range refuses both leave the shape exactly as it was.
    ///
    /// The range checked here is the declaring kind's own, which is the range the host's parser
    /// enforces: a position, a mask-space distance, one turn of degrees and a percentage.
    pub(crate) fn set_field(&mut self, name: &str, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        // A painted gesture's numbers are the brush's, checked against the same command's own
        // declared ranges, and refused once the stroke is down for the same reason the modifier is.
        let Some(held) = self.shape() else {
            let Some(stroke) = self.brush_mut() else {
                return false;
            };
            if stroke.painting() {
                return false;
            }
            return stroke.brush.set(name, value);
        };
        let within = |low: f64, high: f64| (low..=high).contains(&value);
        let next = match held {
            MaskShape::Linear(mut linear) => {
                if !within(POSITION_MIN, POSITION_MAX) {
                    return false;
                }
                match name {
                    "x0" => linear.x0 = value,
                    "y0" => linear.y0 = value,
                    "x1" => linear.x1 = value,
                    "y1" => linear.y1 = value,
                    _ => return false,
                }
                MaskShape::Linear(linear)
            }
            MaskShape::Radial(mut radial) => {
                match name {
                    "x" | "y" if !within(POSITION_MIN, POSITION_MAX) => return false,
                    "radius_x" | "radius_y" if !within(DISTANCE_MIN, DISTANCE_MAX) => return false,
                    "angle" if !within(ANGLE_MIN, ANGLE_MAX) => return false,
                    "feather" if !within(FEATHER_MIN, FEATHER_MAX) => return false,
                    _ => {}
                }
                match name {
                    "x" => radial.x = value,
                    "y" => radial.y = value,
                    "radius_x" => radial.radius_x = value,
                    "radius_y" => radial.radius_y = value,
                    "angle" => radial.angle = value,
                    "feather" => radial.feather = value,
                    _ => return false,
                }
                MaskShape::Radial(radial)
            }
        };
        // A number field may legally produce a degenerate axis; the legality rule keeps the draft
        // committable, exactly as it does for a drag.
        self.geometry = MaskGeometry::Shape(legal(next, held));
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

fn finite(point: (f64, f64)) -> bool {
    point.0.is_finite() && point.1.is_finite()
}

/// One pointer step of a linear gesture, measured from where the press landed.
fn dragged_linear(start: LinearGradient, gesture: Gesture, point: (f64, f64)) -> LinearGradient {
    let delta = (
        point.0 - gesture.start_point.0,
        point.1 - gesture.start_point.1,
    );
    match gesture.handle {
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
        // A move is a move: the travel is clamped rather than the endpoints, so a move into the edge
        // of the legal range slides along it and the axis keeps its length and direction exactly.
        // Clamping the endpoints instead would shorten the gradient at the boundary.
        MaskHandle::Middle => translated(start, delta),
        // A radial's handle on a gradient moves nothing rather than moving the wrong thing.
        _ => start,
    }
}

/// One pointer step of a radial gesture.
///
/// Every handle moves the payload by the **difference** between where the pointer is now and where
/// the press landed, measured on the ellipse the gesture started with. That is what makes a drag
/// away and back return the starting ellipse exactly, and what stops a press a pixel off a handle
/// from snapping the shape to the pointer.
fn dragged_radial(
    start: RadialGradient,
    gesture: Gesture,
    point: (f64, f64),
    aspect: f64,
) -> RadialGradient {
    let ellipse = Ellipse::new(start, aspect);
    let (a, b) = ellipse.offset(point);
    let (a0, b0) = ellipse.offset(gesture.start_point);
    match gesture.handle {
        MaskHandle::Centre => {
            let delta = (
                point.0 - gesture.start_point.0,
                point.1 - gesture.start_point.1,
            );
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
        // The grip turns the ellipse by however far the pointer has swung around the centre, so the
        // shape follows the hand rather than jumping to it.
        MaskHandle::Rotation => RadialGradient {
            angle: wrapped(start.angle + (degrees(b, a) - degrees(b0, a0))),
            ..start
        },
        // The ring is at relative radius `1 - feather/100`, so moving out by `d` of a radius takes
        // that much off the feather. The arithmetic stays in the stored percentage, which is what
        // makes a drag that returns to its press return the stored feather bit for bit.
        MaskHandle::Feather => RadialGradient {
            feather: start.feather - (relative(a, b, start) - relative(a0, b0, start)) * 100.0,
            ..start
        },
        // The create gesture's own grab: both radii from one drag, about the fixed centre.
        MaskHandle::Extent => extent(start, (start.x, start.y), point, aspect),
        _ => start,
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

/// A shape the host's own ranges accept, so a gesture never produces a payload the commit refuses.
/// `reference` is the shape to borrow a direction from when this one has collapsed.
fn legal(shape: MaskShape, reference: MaskShape) -> MaskShape {
    match (shape, reference) {
        (MaskShape::Linear(linear), MaskShape::Linear(was)) => {
            MaskShape::Linear(lengthened(clamped(linear), was))
        }
        (MaskShape::Linear(linear), _) => MaskShape::Linear(lengthened(clamped(linear), linear)),
        (MaskShape::Radial(radial), _) => MaskShape::Radial(bounded(radial)),
    }
}

/// Every field of a radial inside the range its own parser enforces.
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
    use lightwell_core::{ParameterKind, mask::commands};

    fn draft() -> MaskDraft {
        MaskDraft::creating(LINEAR, NEUTRAL_BRUSH)
    }

    fn radial_draft() -> MaskDraft {
        let mut draft = MaskDraft::creating(RADIAL, NEUTRAL_BRUSH);
        // A landscape frame, so a bug that confuses mask space with normalized content coordinates
        // cannot hide behind a square one.
        draft.set_aspect(1.5);
        draft
    }

    fn axis(gradient: &LinearGradient) -> f64 {
        (gradient.x1 - gradient.x0).hypot(gradient.y1 - gradient.y0)
    }

    /// Every shape this draft can produce is a payload the host's own declared ranges accept. The
    /// ranges are read from the declarations the commit is validated against, so this asserts the
    /// draft against the host rather than against a second copy of its rules.
    fn check(draft: &MaskDraft, what: &str) {
        let patch = commands::geometry(commands::GeometryOp::Set, &draft.kind)
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
        if let Some(linear) = draft.linear() {
            assert!(
                axis(&linear) >= MIN_AXIS,
                "{what}: the axis collapsed to {}",
                axis(&linear)
            );
        }
    }

    #[test]
    fn the_method_and_fields_come_from_the_hosts_own_kind_table() {
        assert_eq!(
            MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).method(),
            Some("mask.create-linear")
        );
        assert_eq!(
            MaskDraft::adding(
                MaskId::new(),
                LINEAR,
                ComponentMode::Subtract,
                NEUTRAL_BRUSH,
            )
            .method(),
            Some("mask.add-linear")
        );
        assert_eq!(
            MaskDraft::editing(
                MaskId::new(),
                ComponentId::new(),
                LINEAR,
                Some(MaskShape::Linear(NEUTRAL)),
                NEUTRAL_BRUSH,
            )
            .method(),
            Some("mask.set-linear")
        );
        // The radial's methods are generated by the same table, from the same three operations.
        assert_eq!(
            MaskDraft::creating(RADIAL, NEUTRAL_BRUSH).method(),
            Some("mask.create-radial")
        );
        assert_eq!(
            MaskDraft::adding(
                MaskId::new(),
                RADIAL,
                ComponentMode::Intersect,
                NEUTRAL_BRUSH,
            )
            .method(),
            Some("mask.add-radial")
        );
        // A kind this build cannot evaluate has no method at all, so nothing is spelled out here.
        assert_eq!(MaskDraft::creating("cloud", NEUTRAL_BRUSH).method(), None);
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
                None,
                NEUTRAL_BRUSH,
            ),
        ] {
            assert_eq!(op.method(), Some("mask.add-stroke"));
        }

        // A create carries the four geometry fields and no mode; an add carries its mode too.
        let create = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).fields();
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
        .fields();
        assert_eq!(add["mode"], json!("intersect"));
        assert_eq!(add.len(), 5);
        // A radial carries its own six, named exactly as its kind declares them.
        let radial = MaskDraft::creating(RADIAL, NEUTRAL_BRUSH).fields();
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
        let before = draft.linear().expect("a gradient");
        // The start handle moves p0 alone.
        draft.begin(MaskHandle::Start, (before.x0, before.y0));
        draft.drag((before.x0 + 0.1, before.y0 - 0.05));
        draft.end();
        let now = draft.linear().expect("a gradient");
        assert_eq!((now.x1, now.y1), (before.x1, before.y1));
        assert!((now.x0 - (before.x0 + 0.1)).abs() < 1e-12);
        check(&draft, "start");

        // The end handle moves p1 alone.
        let before = draft.linear().expect("a gradient");
        draft.begin(MaskHandle::End, (before.x1, before.y1));
        draft.drag((before.x1 - 0.2, before.y1 + 0.1));
        draft.end();
        let now = draft.linear().expect("a gradient");
        assert_eq!((now.x0, now.y0), (before.x0, before.y0));
        check(&draft, "end");

        // The midpoint translates both ends, so the axis keeps its length and direction exactly.
        let before = draft.linear().expect("a gradient");
        let length = axis(&before);
        let middle = ((before.x0 + before.x1) / 2.0, (before.y0 + before.y1) / 2.0);
        draft.begin(MaskHandle::Middle, middle);
        draft.drag((middle.0 + 0.05, middle.1 + 0.05));
        draft.end();
        let now = draft.linear().expect("a gradient");
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
            let kind = draft.kind.clone();
            for (handle, from) in draft.handles() {
                let start = draft.shape().expect("a shape gesture");
                draft.begin(handle, from);
                for step in [(-0.3, 0.2), (0.6, -0.4), (0.0, 0.0)] {
                    draft.drag((from.0 + step.0, from.1 + step.1));
                }
                draft.end();
                assert_eq!(
                    draft.values(),
                    start.values(),
                    "{kind} {handle:?} did not return to where it started"
                );
            }
        }
    }

    #[test]
    fn every_gesture_stays_inside_the_declared_range_and_never_collapses_the_axis() {
        for mut draft in [draft(), radial_draft()] {
            let kind = draft.kind.clone();
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
        let gradient = draft.linear().expect("a gradient");
        draft.begin(MaskHandle::End, (gradient.x1, gradient.y1));
        draft.drag((gradient.x0, gradient.y0));
        draft.end();
        check(&draft, "collapsed onto the other end");
        // And collapsing a radius onto the centre leaves a radius the host's floor accepts.
        let mut draft = radial_draft();
        let ellipse = draft.radial().expect("an ellipse");
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
        let now = draft.linear().expect("a gradient");
        assert_eq!((now.x0, now.y0), (0.2, 0.1));
        assert_eq!((now.x1, now.y1), (0.8, 0.9));
        assert!(
            draft.dragging(),
            "the sweep continues as an end-handle drag"
        );
        assert_eq!(draft.held(), Some(MaskHandle::End));
        draft.drag((0.5, 0.5));
        assert!((draft.linear().expect("a gradient").x1 - 0.5).abs() < 1e-12);
        draft.end();
        check(&draft, "swept");
        // A sweep that never moved still leaves an axis the host will accept.
        let mut still = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH);
        still.sweep((0.4, 0.4), (0.4, 0.4));
        check(&still, "a sweep that did not move");
    }

    /// A radial's sweep is the same stroke read as an extent: the press is the centre and the drag
    /// sets both radii, in mask-space units of the stage's height on both axes.
    #[test]
    fn a_radial_sweep_sets_the_centre_and_both_radii_in_mask_space() {
        let mut draft = radial_draft();
        draft.sweep((0.4, 0.5), (0.6, 0.8));
        let now = draft.radial().expect("an ellipse");
        assert_eq!((now.x, now.y), (0.4, 0.5));
        // 0.2 of the width at an aspect of 1.5 is 0.3 of the height, which is what mask space counts.
        assert!((now.radius_x - 0.3).abs() < 1e-12, "{}", now.radius_x);
        assert!((now.radius_y - 0.3).abs() < 1e-12, "{}", now.radius_y);
        assert_eq!(now.angle, 0.0, "a swept ellipse is upright");
        assert!(draft.dragging());
        draft.drag((0.5, 0.6));
        let now = draft.radial().expect("an ellipse");
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
        assert_eq!(draft.linear().expect("a gradient").x0, 0.125);
        assert!(draft.set_field("y1", POSITION_MAX));
        assert_eq!(draft.linear().expect("a gradient").y1, POSITION_MAX);
        check(&draft, "typed");
        // Out of range, not a number, and a field this kind does not declare: each refused, and
        // each leaves the shape untouched.
        let before = draft.shape().expect("a shape gesture");
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
                draft.shape().expect("a shape gesture"),
                before,
                "{name} = {value} changed the shape"
            );
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
        let now = draft.radial().expect("an ellipse");
        assert_eq!((now.x, now.y), (0.25, 0.75));
        assert_eq!((now.radius_x, now.radius_y), (0.4, DISTANCE_MAX));
        assert_eq!((now.angle, now.feather), (-173.5, 0.0));
        check(&draft, "a typed ellipse");
        let before = draft.shape().expect("a shape gesture");
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
            assert_eq!(
                draft.shape().expect("a shape gesture"),
                before,
                "{name} = {value} changed the shape"
            );
        }
    }

    #[test]
    fn a_gesture_without_a_press_changes_nothing_and_an_interruption_ends_the_drag() {
        let mut draft = draft();
        let before = draft.shape().expect("a shape gesture");
        draft.drag((0.9, 0.9));
        draft.end();
        assert_eq!(draft.shape().expect("a shape gesture"), before);
        assert!(!draft.dragging());

        let gradient = draft.linear().expect("a gradient");
        draft.begin(MaskHandle::End, (gradient.x1, gradient.y1));
        assert!(draft.dragging());
        // A conflict or a reapply of the core draft behind the gesture interrupts it.
        draft.interrupt();
        assert!(!draft.dragging(), "an interruption drops the gesture");
        draft.drag((0.9, 0.9));
        assert_eq!(
            draft.shape().expect("a shape gesture"),
            before,
            "an interrupted drag ignores the pointer and keeps what this client drew"
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
            Some(MaskShape::Linear(NEUTRAL)),
            NEUTRAL_BRUSH,
        );
        let summary = draft.summary();
        assert_eq!(summary["mask"], json!(mask.as_str()));
        assert_eq!(summary["component"], json!(component.as_str()));
        assert_eq!(summary["kind"], json!(LINEAR));
        assert_eq!(summary["op"], json!("Update"));
        assert_eq!(summary["method"], json!("mask.set-linear"));
        assert_eq!(summary["shape"]["y1"], json!(NEUTRAL.y1));
        // A create names no mask and no component, because it has none yet.
        let creating = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).summary();
        assert_eq!(creating["mask"], Value::Null);
        assert_eq!(creating["op"], json!("New mask"));
        // A radial's summary carries its own six fields under the same key.
        let radial = radial_draft().summary();
        assert_eq!(radial["kind"], json!(RADIAL));
        assert_eq!(radial["aspect"], json!(1.5));
        assert_eq!(radial["shape"]["feather"], json!(NEUTRAL_RADIAL.feather));
    }
}
