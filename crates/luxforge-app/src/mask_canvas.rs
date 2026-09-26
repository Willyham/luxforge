//! The mask shape canvas: it draws the open gesture's handles over the photograph and turns pointer
//! events into gesture messages.
//!
//! The canvas owns no editing state. It borrows the draft and its [`ContentMap`] for one `view`
//! call, maps pointer positions into normalized content coordinates and publishes messages; every
//! change to the gesture happens in [`crate::app`]'s update, so the same gestures are reachable from
//! the API without simulating a pointer.
//!
//! Unlike the crop canvas it draws over the **current** render rather than a truncated prefix: a
//! mask does not change the stage, so the picture under the handles is the one the release will
//! commit against. Positions are mapped locally, through the affine `render.transform` answered once
//! when the gesture opened and the [`CanvasView`] the output stage is drawn with — never through a
//! `render.locate` per move, which would put a runtime hop on the input path.
use crate::{
    app::message::{MaskMessage, MaskPointer, Message},
    canvas_view::{self, CanvasView},
    mask_draft::{ContentMap, MaskDraft, MaskHandle, Pen},
};
use iced::{
    Point, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke},
};
use luxforge_ui::theme;

/// The hit radius and the drawn size of one handle, in logical pixels.
const HIT_RADIUS: f32 = 9.0;
const HANDLE_RADIUS: f32 = 5.0;
/// Content-normalized to canvas-local, and back: the map and the view composed, which is the whole
/// of what a pointer move costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) map: ContentMap,
    /// The output stage as it is drawn in this canvas.
    pub(crate) view: CanvasView,
}

impl Placement {
    pub(crate) fn canvas_point(self, x: f64, y: f64) -> Point {
        let (ox, oy) = self.map.to_output(x, y);
        self.view.canvas_point(ox, oy)
    }

    pub(crate) fn content_point(self, point: Point) -> (f64, f64) {
        let (ox, oy) = self.view.stage_point(point);
        self.map.to_content(ox, oy)
    }

    /// The hit radius in normalized content units: the pixel radius through the view, then through
    /// the affine.
    pub(crate) fn tolerance(self) -> f64 {
        self.map.tolerance(self.view.tolerance(HIT_RADIUS))
    }
}

/// The canvas's own ephemeral pointer bookkeeping: where a press that grabbed no handle started, so
/// the move after it can be published as one sweep.
#[derive(Debug, Default)]
pub(crate) struct Interaction {
    sweep_from: Option<(f64, f64)>,
}

/// The open gesture's handles over the current render. Borrowed from the app for one `view`.
pub(crate) struct MaskCanvas<'a> {
    draft: &'a MaskDraft,
    placement: Placement,
}

impl<'a> MaskCanvas<'a> {
    pub(crate) fn new(draft: &'a MaskDraft, placement: Placement) -> Self {
        Self { draft, placement }
    }

    fn handle_at(&self, point: Point) -> Option<MaskHandle> {
        self.draft.hit(
            self.placement.content_point(point),
            self.placement.tolerance(),
        )
    }

    fn pointer(&self, pointer: MaskPointer) -> Action<Message> {
        Action::publish(Message::Mask(MaskMessage::Handle(pointer))).and_capture()
    }

    /// An ellipse of mask space — `radii` in mask-space units about a normalized content `centre`,
    /// turned by `angle` degrees — mapped through the affine and the view, so it follows a crop or a
    /// quarter turn with the picture and stays a circle in pixels at any aspect ratio.
    fn mask_ellipse(
        &self,
        centre: (f64, f64),
        radii: (f64, f64),
        angle: f64,
        dashed: bool,
    ) -> Path {
        let aspect = self.draft.aspect();
        canvas_view::ellipse(
            (centre.0 * aspect, centre.1),
            radii,
            angle * std::f64::consts::PI / 180.0,
            dashed,
            |u, v| self.placement.canvas_point(u / aspect, v),
        )
    }

    /// A mask-space radius in canvas-local logical pixels, measured through the same map the figure
    /// is drawn with, so the painted line is as wide as the circles say it is at any zoom.
    fn brush_radius(&self, radius: f64, at: (f64, f64)) -> f32 {
        let aspect = self.draft.aspect();
        let centre = self.placement.canvas_point(at.0, at.1);
        let edge = self
            .placement
            .canvas_point((at.0 * aspect + radius) / aspect, at.1);
        (edge.x - centre.x).hypot(edge.y - centre.y).max(1.0)
    }
}

/// The pen a shape editor describes its figure through, for one frame: every point mapped through
/// the affine and the view, in the mask overlay's own tint.
struct FramePen<'c, 'f> {
    canvas: &'c MaskCanvas<'c>,
    frame: &'f mut Frame,
}

impl Pen for FramePen<'_, '_> {
    fn line(&mut self, from: (f64, f64), to: (f64, f64), alpha: f32) {
        let placement = self.canvas.placement;
        self.frame.stroke(
            &Path::line(
                placement.canvas_point(from.0, from.1),
                placement.canvas_point(to.0, to.1),
            ),
            Stroke::default().with_color(tint(alpha)).with_width(1.0),
        );
    }

    fn ellipse(
        &mut self,
        centre: (f64, f64),
        radii: (f64, f64),
        angle: f64,
        alpha: f32,
        dashed: bool,
    ) {
        self.frame.stroke(
            &self.canvas.mask_ellipse(centre, radii, angle, dashed),
            Stroke::default().with_color(tint(alpha)).with_width(1.0),
        );
    }

    fn path(&mut self, points: &[[f64; 2]], radius: f64, alpha: f32) {
        let Some(first) = points.first() else {
            return;
        };
        let placement = self.canvas.placement;
        let line = Path::new(|builder| {
            builder.move_to(placement.canvas_point(first[0], first[1]));
            for point in &points[1..] {
                builder.line_to(placement.canvas_point(point[0], point[1]));
            }
            // A one-position stroke is a single dab, and a zero-length line draws nothing, so its
            // own end is repeated: the round cap is then the dab the host will evaluate.
            if points.len() == 1 {
                builder.line_to(placement.canvas_point(first[0], first[1]));
            }
        });
        self.frame.stroke(
            &line,
            Stroke::default()
                .with_color(tint(alpha))
                .with_width(2.0 * self.canvas.brush_radius(radius, (first[0], first[1])))
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
    }
}

/// The pointer position in canvas-local logical pixels, even once a drag has left the bounds.
fn local(cursor: Cursor, bounds: Rectangle) -> Option<Point> {
    cursor
        .position()
        .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y))
}

impl canvas::Program<Message> for MaskCanvas<'_> {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        match event {
            // A painted gesture has no handles: every press on the photograph paints, and the path
            // is published position by position so the canvas can draw it as the pointer moves.
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if self.draft.paints() =>
            {
                let point = cursor.position_in(bounds)?;
                let (x, y) = self.placement.content_point(point);
                Some(self.pointer(MaskPointer::PaintBegin { x, y }))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.draft.paints() => {
                let point = local(cursor, bounds)?;
                let (x, y) = self.placement.content_point(point);
                if !self.draft.dragging() {
                    // The cursor's circles follow the pointer whether or not it is down, so the
                    // brush's size is visible before the stroke starts. Redrawing is the canvas's
                    // own, and costs no message and no round trip.
                    return Some(Action::request_redraw());
                }
                Some(self.pointer(MaskPointer::PaintTo { x, y }))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if self.draft.paints() =>
            {
                self.draft
                    .dragging()
                    .then(|| self.pointer(MaskPointer::PaintEnd))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                let (x, y) = self.placement.content_point(point);
                match self.handle_at(point) {
                    Some(handle) => {
                        state.sweep_from = None;
                        Some(self.pointer(MaskPointer::Begin { handle, x, y }))
                    }
                    // A press away from every handle draws a whole gradient in one stroke, from the
                    // untouched side towards the affected one. Nothing is published until it moves,
                    // so a click that grabs nothing changes nothing.
                    None => {
                        state.sweep_from = Some((x, y));
                        Some(Action::capture())
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = local(cursor, bounds)?;
                let to = self.placement.content_point(point);
                if let Some(from) = state.sweep_from.take() {
                    return Some(self.pointer(MaskPointer::Sweep { from, to }));
                }
                if !self.draft.dragging() {
                    return None;
                }
                Some(self.pointer(MaskPointer::Drag { x: to.0, y: to.1 }))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let swept = state.sweep_from.take().is_some();
                if !self.draft.dragging() {
                    return swept.then(Action::capture);
                }
                Some(self.pointer(MaskPointer::End))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // The figure is the shape editor's own, described through the pen. The pointer is handed
        // over only while it is actually over the canvas, and in the canvas's own coordinates: a
        // window position drawn as if it were a canvas one puts a cursor somewhere nobody is
        // pointing, and a capture with no pointer at all must show no cursor rather than a stale one.
        let pointer = cursor
            .position_in(bounds)
            .map(|point| self.placement.content_point(point));
        self.draft.draw(
            pointer,
            &mut FramePen {
                canvas: self,
                frame: &mut frame,
            },
        );
        // One grip per drawn handle, whatever the figure under them is.
        for (handle, (x, y)) in self.draft.handles() {
            let centre = self.placement.canvas_point(x, y);
            let held = self.draft.held() == Some(handle);
            frame.fill(
                &Path::circle(
                    centre,
                    if held {
                        HANDLE_RADIUS + 1.0
                    } else {
                        HANDLE_RADIUS
                    },
                ),
                tint(if held { 1.0 } else { 0.9 }),
            );
            frame.stroke(
                &Path::circle(centre, HANDLE_RADIUS + 1.5),
                Stroke::default()
                    .with_color(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5))
                    .with_width(1.0),
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        let Some(point) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        // A painted gesture draws its own cursor, so the pointer gets out of its way.
        if self.draft.paints() {
            return mouse::Interaction::None;
        }
        match self.handle_at(point) {
            // The handles that move the whole figure say so; the rest are grips.
            Some(handle) if handle.moves_figure() => mouse::Interaction::Move,
            Some(_) => mouse::Interaction::Grab,
            None => mouse::Interaction::Crosshair,
        }
    }
}

/// The handle colour: the mask overlay's own green, never a clipping colour. The delivered clipping
/// indicators own red, blue and the magenta between them on this canvas.
fn tint(alpha: f32) -> iced::Color {
    iced::Color {
        a: alpha,
        ..theme::MASK_OVERLAY_GREEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask_draft::{BRUSH, LINEAR, MaskDraft, NEUTRAL_BRUSH, RADIAL};
    use iced::Size;
    use luxforge_core::{StageSize, StageTransform};

    fn placement(output: (u32, u32), available: Size) -> Placement {
        let transform = StageTransform {
            content: StageSize {
                width: output.0,
                height: output.1,
            },
            output: StageSize {
                width: output.0,
                height: output.1,
            },
            forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        };
        Placement {
            map: ContentMap::new(&transform).expect("a drawable stage"),
            view: CanvasView::fit((f64::from(output.0), f64::from(output.1)), available)
                .expect("a fitted view"),
        }
    }

    /// A handle is drawn where a press on it lands. That is the whole correctness condition for
    /// mapping locally instead of asking the host per move.
    #[test]
    fn a_drawn_handle_is_where_a_press_on_it_is_answered() {
        let placement = placement((480, 320), Size::new(960.0, 640.0));
        for kind in [LINEAR, RADIAL] {
            let mut draft = MaskDraft::creating(kind, NEUTRAL_BRUSH).expect("a drawn kind");
            draft.set_aspect(placement.map.aspect());
            let canvas = MaskCanvas::new(&draft, placement);
            for (handle, (x, y)) in draft.handles() {
                let drawn = placement.canvas_point(x, y);
                let (back_x, back_y) = placement.content_point(drawn);
                // Canvas coordinates are `f32`, so the round trip is exact to the drawn pixel and
                // not to the `f64` the geometry is kept in: a hundredth of a pixel on this stage.
                assert!(
                    (back_x - x).abs() < 1e-5 && (back_y - y).abs() < 1e-5,
                    "{kind} {handle:?} drew at {drawn:?}, which maps back to ({back_x}, {back_y})"
                );
                assert_eq!(canvas.handle_at(drawn), Some(handle), "{kind} {handle:?}");
            }
            // A point well away from every handle grabs none, and is the start of a sweep instead.
            let away = placement.canvas_point(0.02, 0.02);
            assert_eq!(canvas.handle_at(away), None, "{kind}");
        }
    }

    #[test]
    fn a_press_on_a_handle_drags_it_and_a_press_on_the_photograph_sweeps() {
        use canvas::Program;
        let placement = placement((480, 320), Size::new(480.0, 320.0));
        let draft = MaskDraft::creating(LINEAR, NEUTRAL_BRUSH).expect("a drawn kind");
        let program = MaskCanvas::new(&draft, placement);
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 320.0));
        let mut state = Interaction::default();

        // A press on the start handle begins a drag of that handle.
        let (x0, y0) = (
            draft.value("x0").expect("a gradient"),
            draft.value("y0").expect("a gradient"),
        );
        let grip = placement.canvas_point(x0, y0);
        let action = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(grip),
        );
        assert!(action.is_some(), "a press on a handle is answered");
        assert!(state.sweep_from.is_none());

        // A press away from every handle records a sweep origin and publishes nothing yet.
        let mut state = Interaction::default();
        let empty = placement.canvas_point(0.02, 0.02);
        program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(empty),
        );
        assert!(state.sweep_from.is_some(), "the sweep's origin is held");
        // The first move publishes the sweep and clears the origin.
        let action = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(300.0, 300.0),
            }),
            bounds,
            Cursor::Available(Point::new(300.0, 300.0)),
        );
        assert!(action.is_some());
        assert!(state.sweep_from.is_none());
        // A press that never moved changes nothing at all on release.
        let mut state = Interaction::default();
        program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(empty),
        );
        program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds,
            Cursor::Available(empty),
        );
        assert!(state.sweep_from.is_none());
    }

    /// A quarter turn and a crop as one affine, so a brush's circles and its painted path can be
    /// checked under a tail that is not the identity.
    fn rotated(content: (u32, u32), output: (u32, u32), available: Size) -> Placement {
        let transform = StageTransform {
            content: StageSize {
                width: content.0,
                height: content.1,
            },
            output: StageSize {
                width: output.0,
                height: output.1,
            },
            // A quarter turn clockwise: x' = H - y, y' = x, with the crop's origin folded in.
            forward: [0.0, -1.0, f64::from(content.1), 1.0, 0.0, 0.0],
            inverse: [0.0, 1.0, 0.0, -1.0, 0.0, f64::from(content.1)],
        };
        Placement {
            map: ContentMap::new(&transform).expect("a drawable stage"),
            view: CanvasView::fit((f64::from(output.0), f64::from(output.1)), available)
                .expect("a fitted view"),
        }
    }

    /// A painted gesture has no handles: every press on the photograph paints, the path is published
    /// position by position so the canvas can draw it as the pointer moves, and the release commits.
    #[test]
    fn a_painted_gesture_paints_wherever_it_is_pressed_and_never_grabs_a_handle() {
        use canvas::Program;
        let placement = placement((480, 320), Size::new(480.0, 320.0));
        let mut draft = MaskDraft::creating(BRUSH, NEUTRAL_BRUSH).expect("a drawn kind");
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 320.0));
        let at = placement.canvas_point(0.5, 0.5);

        {
            let program = MaskCanvas::new(&draft, placement);
            let mut state = Interaction::default();
            // Nothing is a handle, so nothing is grabbed and nothing is a sweep.
            assert_eq!(program.handle_at(at), None);
            assert!(draft.handles().is_empty(), "a brush draws no grips");
            let action = program.update(
                &mut state,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                bounds,
                Cursor::Available(at),
            );
            assert!(action.is_some(), "a press on the photograph paints");
            assert!(
                state.sweep_from.is_none(),
                "a painted gesture never begins a sweep"
            );
            // A move with the button up redraws the cursor and publishes no message at all.
            let idle = program.update(
                &mut state,
                &Event::Mouse(mouse::Event::CursorMoved { position: at }),
                bounds,
                Cursor::Available(at),
            );
            assert!(idle.is_some(), "the cursor follows the pointer");
        }

        // With the stroke down, a move extends the path and a release ends it. The path is the
        // pointer's, position by position, and nothing here calls the host.
        draft.paint_begin((0.5, 0.5));
        assert!(draft.dragging(), "the stroke is down");
        assert!(draft.paint_to((0.6, 0.55)), "a move extends the path");
        assert!(
            !draft.paint_to((0.6, 0.55)),
            "a position identical to the last one is dropped rather than posted"
        );
        let stroke = draft.brush().expect("a painted gesture");
        assert_eq!(stroke.captured(), [[0.5, 0.5], [0.6, 0.55]]);
        // What it posts is the host's own decimation of what it captured, which is what makes the
        // same drawn path always the same stored stroke.
        assert_eq!(
            stroke.points(),
            luxforge_core::path::decimate(stroke.captured()).expect("a decimated path")
        );
        draft.paint_end();
        assert!(!draft.dragging());
    }

    /// The cursor's circles are the brush's own size through the geometry tail, so they are right at
    /// Fit, at 100% and under a rotated crop — and the painted line is drawn as wide as they say.
    #[test]
    fn the_brush_cursor_is_its_own_size_at_every_zoom_and_under_a_rotated_crop() {
        let mut draft = MaskDraft::creating(BRUSH, NEUTRAL_BRUSH).expect("a drawn kind");
        draft.set_aspect(480.0 / 320.0);
        let radius = NEUTRAL_BRUSH.size;
        // One mask-space unit is the content stage's **height**, so a radius of `r` is `r · H`
        // content pixels — on both axes, which is what makes a round brush round at any aspect.
        let expected = |scale: f64| radius * 320.0 * scale;
        for (available, scale) in [
            // Fit into a surface twice the stage: one content pixel is two logical ones.
            (Size::new(960.0, 640.0), 2.0),
            // Fit into a narrower surface: the width binds.
            (Size::new(480.0, 640.0), 1.0),
        ] {
            let placement = placement((480, 320), available);
            let canvas = MaskCanvas::new(&draft, placement);
            let drawn = f64::from(canvas.brush_radius(radius, (0.5, 0.5)));
            assert!(
                (drawn - expected(scale)).abs() < 1e-6,
                "at scale {scale} the brush drew {drawn} where {} was its size",
                expected(scale)
            );
        }
        // A quarter turn is a rotation and not a stretch, so the circle keeps its size: the stage is
        // 480x320 of content shown as 320x480 of output.
        let placement = rotated((480, 320), (320, 480), Size::new(320.0, 480.0));
        let canvas = MaskCanvas::new(&draft, placement);
        let drawn = f64::from(canvas.brush_radius(radius, (0.5, 0.5)));
        assert!(
            (drawn - expected(1.0)).abs() < 1e-6,
            "a rotated tail changed the brush's drawn size: {drawn}"
        );
    }

    /// The overlay and the handles are green or white, never a clipping colour: a person must be
    /// able to tell a selection from a blown highlight.
    #[test]
    fn the_handle_tint_is_the_mask_overlay_colour_and_not_a_clipping_one() {
        let colour = tint(1.0);
        assert_eq!(
            (colour.r, colour.g, colour.b),
            (
                theme::MASK_OVERLAY_GREEN.r,
                theme::MASK_OVERLAY_GREEN.g,
                theme::MASK_OVERLAY_GREEN.b
            )
        );
        for clipping in [
            theme::CLIPPING_SHADOW,
            theme::CLIPPING_HIGHLIGHT,
            theme::CLIPPING_BOTH,
        ] {
            assert!(
                (colour.r - clipping.r).abs() + (colour.g - clipping.g).abs() > 0.2,
                "the handle tint is too close to a clipping colour"
            );
        }
    }
}
