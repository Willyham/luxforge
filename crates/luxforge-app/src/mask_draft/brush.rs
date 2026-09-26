//! The brush's shape editor: the stroke being painted and the brush it is painted with.
//!
//! A brush has no handles and no shape to reopen: its geometry is a path the pointer draws, and
//! every press on the photograph paints. So its editor holds the stroke in flight and the settings
//! of the brush in the hand, and commits all three of its edits through the one command that carries
//! a path — which of the three it is, is what the identities the draft names say.
use super::editor::{DrawnShape, Pen, ShapeEditor, finite};
use luxforge_core::mask::commands::GeometryOp;
use serde_json::{Map, Value, json};

/// The kind token this editor draws: the one kind whose geometry is painted, from the host's own
/// kind table.
pub(crate) const KIND: &str = luxforge_core::mask::BRUSH;

/// The host command every stroke commits through, whichever of its three edits it turns out to be.
const ADD_STROKE: &str = luxforge_core::mask::commands::ADD_STROKE;

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
    colour_refine: luxforge_core::mask::REFINE_DEFAULT,
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
        let Some(declared) = luxforge_core::mask::commands::find(ADD_STROKE)
            .and_then(|command| command.action.parameter(name))
        else {
            return false;
        };
        let luxforge_core::ParameterKind::Number { min, max } = declared.kind else {
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
        let Some(step) = luxforge_core::mask::commands::find(ADD_STROKE)
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
    pub(super) fn press(&mut self, point: (f64, f64)) {
        if !finite(point) {
            return;
        }
        self.path = vec![[point.0, point.1]];
        self.painting = true;
    }

    /// The pointer moved with the button down. A position identical to the last one is dropped here
    /// rather than posted: the stored grid would drop it anyway, and a still pointer must not grow
    /// the path without bound.
    pub(super) fn paint(&mut self, point: (f64, f64)) -> bool {
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
    pub(super) fn release(&mut self) {
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
        luxforge_core::path::decimate(&self.path).unwrap_or_default()
    }

    /// Something was drawn. An empty stroke commits nothing: a click that painted no position is not
    /// an edit, and a commit of one would be a history entry nobody made.
    pub(crate) fn drawn(&self) -> bool {
        !self.path.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushEditor {
    stroke: BrushStroke,
}

/// The kind table's row: a painted kind has no shape to open, only the next stroke, at the brush the
/// person is holding. A stored component's strokes are already drawn and are objects in their own
/// right, so reopening one is the next stroke on it and not a patch.
pub(super) fn open(_stored: Option<&Value>, brush: Brush) -> Option<DrawnShape> {
    Some(DrawnShape::Brush(BrushEditor {
        stroke: BrushStroke::new(brush),
    }))
}

impl ShapeEditor for BrushEditor {
    fn kind(&self) -> &'static str {
        KIND
    }

    /// A painted kind has no generated geometry method — there is no number a `mask.set-brush` could
    /// patch — so all three of its edits go through the one command that carries a path.
    fn method(&self, _op: GeometryOp) -> Option<&'static str> {
        luxforge_core::mask::commands::find(ADD_STROKE).map(|command| command.method)
    }

    /// A painted gesture's numbers are the brush's.
    fn values(&self) -> Vec<(&'static str, f64)> {
        self.stroke.brush.values()
    }

    /// A stroke's path is decimated here, where it is posted, rather than as it is captured: the
    /// contract is idempotent, so the desktop's decimated path and an agent's raw one reach the same
    /// stored stroke, and the canvas keeps drawing what the pointer actually did.
    fn extra_fields(&self, fields: &mut Map<String, Value>) {
        fields.insert("points".to_owned(), json!(self.stroke.points()));
        fields.insert("erase".to_owned(), json!(self.stroke.brush.erase));
        // The flag, and never a colour: the host reads the pixel the masked operation receives at
        // the stroke's first position and stores that with the stroke.
        fields.insert(
            "limit_to_colour".to_owned(),
            json!(self.stroke.brush.limit_to_colour),
        );
    }

    /// Checked against the stroke command's own declared ranges, and refused once the stroke is down
    /// for the same reason the modifier is: a stroke is drawn with one brush for its whole life.
    fn set_field(&mut self, name: &str, value: f64) -> bool {
        !self.stroke.painting() && self.stroke.brush.set(name, value)
    }

    fn release(&mut self) {
        self.stroke.release();
    }

    fn dragging(&self) -> bool {
        self.stroke.painting()
    }

    fn stroke(&self) -> Option<&BrushStroke> {
        Some(&self.stroke)
    }

    fn stroke_mut(&mut self) -> Option<&mut BrushStroke> {
        Some(&mut self.stroke)
    }

    /// The path this stroke has drawn so far, painted at the brush's own width, and the cursor's two
    /// circles under the pointer.
    ///
    /// The path is drawn from what the pointer captured, not from what will be posted, and it is
    /// drawn by the canvas rather than waiting for a render — so the line follows the hand at the
    /// display's rate while the drafted picture follows one frame behind it. The cursor is the size
    /// circle where coverage ends and the feather ring where the ramp starts, both at the brush's own
    /// scale through the geometry tail, and only while the pointer is over the canvas: a capture with
    /// no pointer at all shows no cursor rather than a stale one.
    fn draw(&self, _aspect: f64, pointer: Option<(f64, f64)>, pen: &mut dyn Pen) {
        let brush = self.stroke.brush;
        let path = self.stroke.captured();
        if !path.is_empty() {
            pen.path(path, brush.size, if brush.erase { 0.35 } else { 0.5 });
        }
        let Some(point) = pointer else {
            return;
        };
        for (scale, alpha, dashed) in [(1.0, 0.9, false), (1.0 - brush.feather / 100.0, 0.5, true)]
        {
            if scale <= 0.0 {
                continue;
            }
            let radius = brush.size * scale;
            pen.ellipse(
                point,
                (radius, radius),
                0.0,
                if brush.erase { alpha * 0.6 } else { alpha },
                dashed,
            );
        }
    }
}
