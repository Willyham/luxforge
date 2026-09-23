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
//! when the gesture opened and the [`OutputView`] the photograph is drawn with — never through a
//! `render.locate` per move, which would put a runtime hop on the input path.
use crate::{
    app::message::{MaskMessage, MaskPointer, Message},
    mask_draft::{ContentMap, MaskDraft, MaskHandle, MaskShape},
};
use iced::{
    Point, Rectangle, Renderer, Size, Theme, Vector,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke},
};
use lightwell_ui::theme;

/// The hit radius and the drawn size of one handle, in logical pixels.
const HIT_RADIUS: f32 = 9.0;
const HANDLE_RADIUS: f32 = 5.0;
/// How far past the drawn frame the three perpendicular lines are extended, as a multiple of the
/// output stage's diagonal. A gradient's lines are infinite; this is enough to leave the frame from
/// any position and angle the legal range allows.
const LINE_REACH: f64 = 2.0;
/// How many segments one drawn ellipse is built from. Fixed, so a figure costs the same at every
/// zoom, and fine enough that the boundary reads as a curve on a full-screen radial.
const ELLIPSE_STEPS: usize = 96;

/// Where the output stage is drawn inside this canvas, in logical pixels. Fit centres it, a
/// percentage zoom draws it at its own size with the origin at the corner and the surrounding
/// scrollable handles the offset — exactly the two cases the photo surface itself draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OutputView {
    /// Output pixels to logical pixels.
    pub(crate) scale: f32,
    /// Where output (0, 0) sits inside the canvas, in logical pixels.
    pub(crate) origin: Vector,
}

impl OutputView {
    /// Fit: the largest scale that shows the whole output stage, centred in `available`.
    pub(crate) fn fit(output: (f64, f64), available: Size) -> Option<Self> {
        let (width, height) = (output.0 as f32, output.1 as f32);
        if !(width > 0.0 && height > 0.0 && available.width > 0.0 && available.height > 0.0) {
            return None;
        }
        let scale = (available.width / width).min(available.height / height);
        (scale.is_finite() && scale > 0.0).then_some(Self {
            scale,
            origin: Vector::new(
                (available.width - width * scale) / 2.0,
                (available.height - height * scale) / 2.0,
            ),
        })
    }

    /// A percentage zoom: `value / 100 / display scale`, the same arithmetic the plain image path
    /// uses, so 100% keeps one output pixel per physical pixel.
    pub(crate) fn percent(value: f32, scale_factor: f32) -> Option<Self> {
        let scale = value / 100.0 / scale_factor;
        (scale.is_finite() && scale > 0.0).then_some(Self {
            scale,
            origin: Vector::new(0.0, 0.0),
        })
    }

    /// Canvas-local logical pixels to output-stage pixels.
    pub(crate) fn output_point(self, point: Point) -> (f64, f64) {
        (
            f64::from((point.x - self.origin.x) / self.scale),
            f64::from((point.y - self.origin.y) / self.scale),
        )
    }

    /// Output-stage pixels to canvas-local logical pixels.
    pub(crate) fn canvas_point(self, x: f64, y: f64) -> Point {
        Point::new(
            self.origin.x + x as f32 * self.scale,
            self.origin.y + y as f32 * self.scale,
        )
    }

    /// The hit radius in output pixels, so a handle is the same size on screen at every zoom.
    fn tolerance(self) -> f64 {
        f64::from(HIT_RADIUS / self.scale)
    }
}

/// Content-normalized to canvas-local, and back: the map and the view composed, which is the whole
/// of what a pointer move costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) map: ContentMap,
    pub(crate) view: OutputView,
}

impl Placement {
    pub(crate) fn canvas_point(self, x: f64, y: f64) -> Point {
        let (ox, oy) = self.map.to_output(x, y);
        self.view.canvas_point(ox, oy)
    }

    pub(crate) fn content_point(self, point: Point) -> (f64, f64) {
        let (ox, oy) = self.view.output_point(point);
        self.map.to_content(ox, oy)
    }

    /// The hit radius in normalized content units: the pixel radius through the view, then through
    /// the affine.
    pub(crate) fn tolerance(self) -> f64 {
        self.map.tolerance(self.view.tolerance())
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

    /// The linear gradient's figure: the design's three lines — one through `p0`, one through the
    /// midpoint and one through `p1`, each perpendicular to the axis — and the axis itself, so the
    /// direction of the gradient is visible rather than inferred. They are drawn in content space
    /// and mapped, so a crop, a straighten or a quarter turn moves them with the picture.
    fn draw_linear(&self, frame: &mut Frame, gradient: lightwell_core::mask::LinearGradient) {
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
            let Some((x, y)) = handle.point(&MaskShape::Linear(gradient), self.draft.aspect())
            else {
                continue;
            };
            frame.stroke(
                &Path::line(
                    self.placement.canvas_point(x - px, y - py),
                    self.placement.canvas_point(x + px, y + py),
                ),
                Stroke::default().with_color(tint(alpha)).with_width(1.0),
            );
        }
        frame.stroke(
            &Path::line(
                self.placement.canvas_point(gradient.x0, gradient.y0),
                self.placement.canvas_point(gradient.x1, gradient.y1),
            ),
            Stroke::default().with_color(tint(0.55)).with_width(1.0),
        );
    }

    /// The radial's figure: the boundary ellipse, where coverage reaches zero, and the feather ring
    /// at `1 - feather/100` of it, where the ramp starts. Both are drawn by mapping the ellipse's
    /// own parametrization through mask space and the affine, so they follow a crop or a quarter
    /// turn with the picture and stay a circle in pixels at any aspect ratio.
    ///
    /// At `feather = 0` the two coincide, which is the truth: the edge is hard.
    fn draw_radial(&self, frame: &mut Frame, radial: lightwell_core::mask::RadialGradient) {
        let ring = 1.0 - radial.feather / 100.0;
        for (scale, alpha, dashes) in [(1.0, 0.85, false), (ring, 0.5, true)] {
            if scale <= 0.0 {
                continue;
            }
            let outline = self.ellipse_path(radial, scale, dashes);
            frame.stroke(
                &outline,
                Stroke::default().with_color(tint(alpha)).with_width(1.0),
            );
        }
        // The grip's tether, so the rotation handle reads as belonging to the ellipse rather than
        // floating beside it.
        if let (Some(axis), Some(grip)) = (
            MaskHandle::RadiusPlusX.point(&MaskShape::Radial(radial), self.draft.aspect()),
            MaskHandle::Rotation.point(&MaskShape::Radial(radial), self.draft.aspect()),
        ) {
            frame.stroke(
                &Path::line(
                    self.placement.canvas_point(axis.0, axis.1),
                    self.placement.canvas_point(grip.0, grip.1),
                ),
                Stroke::default().with_color(tint(0.55)).with_width(1.0),
            );
        }
    }

    /// One ellipse of the radial's family, at `scale` of its radii, as a closed canvas path.
    ///
    /// `ELLIPSE_STEPS` segments is what a bounded figure costs: the path is rebuilt per frame like
    /// every other canvas figure, and a fixed step count keeps that cost independent of zoom.
    fn ellipse_path(
        &self,
        radial: lightwell_core::mask::RadialGradient,
        scale: f64,
        dashed: bool,
    ) -> Path {
        let aspect = self.draft.aspect();
        let theta = radial.angle * std::f64::consts::PI / 180.0;
        let (ca, sa) = (theta.cos(), theta.sin());
        let point = |step: usize| {
            let t = step as f64 / ELLIPSE_STEPS as f64 * std::f64::consts::TAU;
            let (a, b) = (
                scale * radial.radius_x * t.cos(),
                scale * radial.radius_y * t.sin(),
            );
            let (du, dv) = (ca * a - sa * b, sa * a + ca * b);
            self.placement
                .canvas_point((radial.x * aspect + du) / aspect, radial.y + dv)
        };
        Path::new(|builder| {
            builder.move_to(point(0));
            for step in 1..=ELLIPSE_STEPS {
                // A dashed ring is drawn as alternate segments rather than with a dash pattern, so
                // the feather ring reads as the softer of the two figures at every zoom.
                if dashed && step % 2 == 0 {
                    builder.move_to(point(step));
                } else {
                    builder.line_to(point(step));
                }
            }
        })
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
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        if let Some(gradient) = self.draft.linear() {
            self.draw_linear(&mut frame, gradient);
        }
        if let Some(radial) = self.draft.radial() {
            self.draw_radial(&mut frame, radial);
        }
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
        match self.handle_at(point) {
            // The handles that move the whole figure say so; the rest are grips.
            Some(MaskHandle::Middle | MaskHandle::Centre) => mouse::Interaction::Move,
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
    use crate::mask_draft::{LINEAR, MaskDraft, RADIAL};
    use lightwell_core::{StageSize, StageTransform};

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
            view: OutputView::fit((f64::from(output.0), f64::from(output.1)), available)
                .expect("a fitted view"),
        }
    }

    #[test]
    fn fit_centres_the_output_stage_and_maps_pointers_through_it_both_ways() {
        // A 200x100 stage in a 400x400 surface: scale 2, centred vertically like the image path.
        let view = OutputView::fit((200.0, 100.0), Size::new(400.0, 400.0)).expect("a view");
        assert_eq!(view.scale, 2.0);
        assert_eq!(view.origin, Vector::new(0.0, 100.0));
        assert_eq!(view.output_point(Point::new(0.0, 100.0)), (0.0, 0.0));
        assert_eq!(view.output_point(Point::new(400.0, 300.0)), (200.0, 100.0));
        for (x, y) in [(0.0, 0.0), (37.0, 91.0), (200.0, 100.0)] {
            assert_eq!(view.output_point(view.canvas_point(x, y)), (x, y));
        }
        assert!(OutputView::fit((0.0, 100.0), Size::new(400.0, 400.0)).is_none());
        assert!(OutputView::percent(0.0, 1.0).is_none());
        // The hit radius is constant on screen: it grows in stage units as the view shrinks.
        let zoomed = OutputView::percent(400.0, 1.0).expect("a percent view");
        assert_eq!(zoomed.tolerance(), f64::from(HIT_RADIUS) / 4.0);
    }

    /// A handle is drawn where a press on it lands. That is the whole correctness condition for
    /// mapping locally instead of asking the host per move.
    #[test]
    fn a_drawn_handle_is_where_a_press_on_it_is_answered() {
        let placement = placement((480, 320), Size::new(960.0, 640.0));
        for kind in [LINEAR, RADIAL] {
            let mut draft = MaskDraft::creating(kind, 1);
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
        let draft = MaskDraft::creating(LINEAR, 1);
        let program = MaskCanvas::new(&draft, placement);
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 320.0));
        let mut state = Interaction::default();

        // A press on the start handle begins a drag of that handle.
        let gradient = draft.linear().expect("a gradient");
        let grip = placement.canvas_point(gradient.x0, gradient.y0);
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
