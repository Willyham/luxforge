//! The brush's shape editor: the stroke being painted and the brush it is painted with.
//!
//! A brush has no handles and no shape to reopen: its geometry is a path the pointer draws, and
//! every press on the photograph paints. So its editor holds the stroke in flight and the settings
//! of the brush in the hand, and commits all three of its edits through the one command that carries
//! a path — which of the three it is, is what the identities the draft names say.
use super::editor::{DISTANCE_DECIMALS, DrawnShape, Pen, ShapeEditor, WHOLE, compact};
use luxforge_core::mask::commands::GeometryOp;
use serde_json::{Map, Value, json};
use std::cell::RefCell;

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
/// The path is held on the host's own grid through [`luxforge_core::path::PathCapture`], which
/// checks and snaps each position once, when it is painted, and refuses the whole gesture at its
/// bound. It is **decimated only when it is posted**, at the host's own tolerance, which is
/// idempotent — so what the desktop sends and what an agent would send arrive at the same stored
/// stroke. Coverage is drawn by the production evaluator; the canvas draws only the cursor. One
/// decimation is cached per capture/brush size, so fields and evidence summaries never repeat that
/// work for the same input.
#[derive(Clone, Debug)]
pub(crate) struct BrushStroke {
    /// The brush this stroke was begun with. `erase` is frozen for the stroke's whole life, which is
    /// what "holding the modifier erases while the stroke lasts" means.
    pub(crate) brush: Brush,
    /// The path on the stored grid, snapped one position at a time as it is painted.
    grid: luxforge_core::path::PathCapture,
    /// The last position painted and how many were, for evidence of what the pointer did. Positions
    /// in one grid cell cost only this count.
    last: Option<[f64; 2]>,
    captured: usize,
    /// One whole-path reduction per capture/size, shared by fields and summaries. At most the
    /// capture's own bounded number of positions; no pixels or source are retained.
    points: RefCell<Option<CachedPoints>>,
    /// The pointer is down: moves extend the path, and a move with it up is not painting.
    painting: bool,
}

impl PartialEq for BrushStroke {
    fn eq(&self, other: &Self) -> bool {
        self.brush == other.brush
            && self.grid == other.grid
            && self.last == other.last
            && self.captured == other.captured
            && self.painting == other.painting
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CachedPoints {
    size: u64,
    points: Vec<[f64; 2]>,
}

impl BrushStroke {
    pub(crate) fn new(brush: Brush) -> Self {
        Self {
            brush,
            grid: luxforge_core::path::PathCapture::default(),
            last: None,
            captured: 0,
            points: RefCell::new(None),
            painting: false,
        }
    }

    /// The pointer went down: this stroke starts here, at the brush it is holding now.
    pub(super) fn press(&mut self, point: (f64, f64)) {
        self.grid = luxforge_core::path::PathCapture::default();
        self.last = None;
        self.captured = 0;
        self.painting = true;
        self.push([point.0, point.1]);
    }

    /// The pointer moved with the button down. A position identical to the last one is dropped here
    /// rather than posted, and a failed capture takes nothing more: it stays failed until the next
    /// press.
    pub(super) fn paint(&mut self, point: (f64, f64)) -> bool {
        let point = [point.0, point.1];
        if !self.painting || self.grid.capture_error().is_some() || self.last == Some(point) {
            return false;
        }
        self.push(point);
        true
    }

    fn push(&mut self, point: [f64; 2]) {
        self.grid.push(point);
        self.last = Some(point);
        self.captured = self.captured.saturating_add(1);
        *self.points.get_mut() = None;
    }

    /// The pointer came up. The path it drew stays; committing it is a separate decision.
    pub(super) fn release(&mut self) {
        self.painting = false;
    }

    /// How many positions the pointer painted, for bounded evidence and diagnostics of the stroke.
    pub(crate) fn captured(&self) -> usize {
        self.captured
    }

    pub(crate) fn painting(&self) -> bool {
        self.painting
    }

    /// The path this stroke posts: the host's own stored stroke of the captured path at this
    /// stroke's size ([`luxforge_core::path::PathCapture::stroke`]). Deterministic, so the same
    /// captured path at the same size is always the same stored stroke and therefore the same
    /// content address.
    pub(crate) fn points(&self) -> Result<Vec<[f64; 2]>, luxforge_core::Error> {
        self.prepare_points()?;
        Ok(self
            .points
            .borrow()
            .as_ref()
            .expect("the reduction was cached")
            .points
            .clone())
    }

    /// Reduce only after a capture/size change. A request clones its at-most-1024 posted
    /// positions once.
    fn prepare_points(&self) -> Result<(), luxforge_core::Error> {
        let mut cached = self.points.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|points| points.size != self.brush.size.to_bits())
        {
            *cached = Some(CachedPoints {
                size: self.brush.size.to_bits(),
                points: self.grid.stroke(self.brush.size)?,
            });
        }
        Ok(())
    }

    /// A drawn stroke's actual capture/decimation failure, before any draft request or commit.
    pub(crate) fn capture_error(&self) -> Option<luxforge_core::Error> {
        self.drawn().then(|| self.prepare_points().err()).flatten()
    }

    /// The count a readout reports without processing the path again. Unknown until first posted.
    pub(crate) fn posted_count(&self) -> Option<usize> {
        self.points
            .borrow()
            .as_ref()
            .map(|points| points.points.len())
    }

    /// Something was drawn. An empty stroke commits nothing: a click that painted no position is not
    /// an edit, and a commit of one would be a history entry nobody made.
    pub(crate) fn drawn(&self) -> bool {
        self.captured > 0
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

    /// `painting` while the stroke is down, which is all there is to say until it commits on
    /// release; between strokes, the size and feather the next one will be drawn with, which are
    /// what the bracket keys change.
    fn readout(&self) -> String {
        if self.stroke.painting() {
            return "painting".to_owned();
        }
        let brush = self.stroke.brush;
        format!(
            "size {} \u{b7} feather {}",
            compact(brush.size, DISTANCE_DECIMALS),
            compact(brush.feather, WHOLE),
        )
    }

    /// A stroke's path is decimated here, where it is posted, rather than as it is captured: the
    /// contract is idempotent, so the desktop's decimated path and an agent's raw one reach the same
    /// stored stroke, and the production evaluator draws precisely that coverage.
    fn extra_fields(&self, fields: &mut Map<String, Value>) {
        // Controller checks preserve and show capture failures before a request is built. An
        // errored capture has no successful posted path and must never become an empty stroke.
        if let Ok(points) = self.stroke.points() {
            fields.insert("points".to_owned(), json!(points));
        }
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

    /// The cursor's two circles under the pointer. Actual stroke coverage is drawn once by the
    /// production evaluator, never by a constant-alpha canvas path. The cursor is the size
    /// circle where coverage ends and the feather ring where the ramp starts, both at the brush's own
    /// scale through the geometry tail, and only while the pointer is over the canvas: a capture with
    /// no pointer at all shows no cursor rather than a stale one.
    fn draw(&self, _aspect: f64, pointer: Option<(f64, f64)>, pen: &mut dyn Pen) {
        let brush = self.stroke.brush;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_capture_is_bounded_by_the_grid_and_failure_is_sticky_until_a_new_press() {
        use luxforge_core::path::{CAPTURED_POINTS_PER_STROKE, COORDINATE_STEPS_PER_UNIT};
        let mut stroke = BrushStroke::new(NEUTRAL_BRUSH);
        stroke.press((0.0, 0.3));
        // Positions inside one grid cell store nothing and consume no bound.
        for index in 1..1000 {
            assert!(stroke.paint((index as f64 * 1e-9, 0.3)));
        }
        assert!(stroke.capture_error().is_none());
        for index in 1..CAPTURED_POINTS_PER_STROKE {
            assert!(stroke.paint((index as f64 / COORDINATE_STEPS_PER_UNIT, 0.3)));
        }
        assert_eq!(stroke.points().unwrap().len(), 2);
        assert!(stroke.paint((0.5, 0.5)));
        let error = stroke.points().unwrap_err();
        assert_eq!(error.kind, luxforge_core::ErrorKind::ResourceLimit);
        assert!(error.detail.contains("16384 captured positions"));
        let captured = stroke.captured();
        for _ in 0..100 {
            assert!(!stroke.paint((0.6, 0.6)));
        }
        assert_eq!(
            stroke.captured(),
            captured,
            "a failed capture takes nothing more"
        );
        assert_eq!(stroke.capture_error().unwrap().detail, error.detail);
        stroke.press((0.4, 0.5));
        assert!(stroke.capture_error().is_none());
        assert_eq!(
            stroke.points().unwrap(),
            luxforge_core::path::decimate(&[[0.4, 0.5]], NEUTRAL_BRUSH.size).unwrap()
        );
    }

    #[test]
    fn invalid_capture_never_becomes_an_empty_or_partial_posted_stroke() {
        for point in [
            (2.1, 0.5),
            (0.5, -1.1),
            (f64::NAN, 0.5),
            (0.5, f64::INFINITY),
        ] {
            let mut stroke = BrushStroke::new(NEUTRAL_BRUSH);
            stroke.press((0.2, 0.3));
            assert!(stroke.paint(point));
            let error = stroke.capture_error().unwrap();
            assert_eq!(error.kind, luxforge_core::ErrorKind::Validation);
            assert!(error.detail.contains("path position 1"));
            assert_eq!(stroke.points().unwrap_err().detail, error.detail);
            assert!(!stroke.paint((0.4, 0.4)));
        }
    }

    #[test]
    fn posted_path_is_the_core_path_and_reduces_only_once_until_input_or_size_changes() {
        let mut stroke = BrushStroke::new(NEUTRAL_BRUSH);
        let mut path = vec![[0.2, 0.3]];
        stroke.press((0.2, 0.3));
        for index in 1..200 {
            let point = (
                0.2 + index as f64 * 0.002,
                0.3 + (index as f64 * 0.1).sin() * 0.04,
            );
            stroke.paint(point);
            path.push([point.0, point.1]);
        }
        assert_eq!(stroke.posted_count(), None);
        let untouched = stroke.clone();
        let expected = luxforge_core::path::decimate(&path, stroke.brush.size).unwrap();
        assert_eq!(stroke.points().unwrap(), expected);
        let allocation = stroke.points.borrow().as_ref().unwrap().points.as_ptr();
        assert_eq!(stroke.points().unwrap(), expected);
        assert!(stroke.capture_error().is_none());
        assert_eq!(stroke.posted_count(), Some(expected.len()));
        assert_eq!(
            allocation,
            stroke.points.borrow().as_ref().unwrap().points.as_ptr()
        );
        assert_eq!(stroke, untouched, "memoizing does not change editing state");
        stroke.paint((0.8, 0.4));
        path.push([0.8, 0.4]);
        assert_eq!(stroke.posted_count(), None);
        assert_eq!(
            stroke.points().unwrap(),
            luxforge_core::path::decimate(&path, stroke.brush.size).unwrap()
        );
        stroke.brush.size = 0.01;
        assert_eq!(
            stroke.points().unwrap(),
            luxforge_core::path::decimate(&path, stroke.brush.size).unwrap()
        );
    }

    #[derive(Default)]
    struct CursorPen {
        lines: usize,
        ellipses: Vec<(f64, f64)>,
    }
    impl Pen for CursorPen {
        fn line(&mut self, _: (f64, f64), _: (f64, f64), _: f32) {
            self.lines += 1;
        }
        fn ellipse(&mut self, _: (f64, f64), radii: (f64, f64), _: f64, _: f32, _: bool) {
            self.ellipses.push(radii);
        }
    }

    #[test]
    fn brush_draw_only_builds_its_two_cursor_rings_regardless_of_path_length() {
        let mut editor = BrushEditor {
            stroke: BrushStroke::new(NEUTRAL_BRUSH),
        };
        editor.stroke.press((0.2, 0.3));
        for index in 1..1000 {
            editor.stroke.paint((0.2 + index as f64 * 0.0005, 0.4));
        }
        let mut pen = CursorPen::default();
        editor.draw(1.5, Some((0.5, 0.5)), &mut pen);
        assert_eq!(pen.lines, 0);
        assert_eq!(pen.ellipses, vec![(0.1, 0.1), (0.05, 0.05)]);
        let mut no_pointer = CursorPen::default();
        editor.draw(1.5, None, &mut no_pointer);
        assert!(no_pointer.ellipses.is_empty());
    }
}
