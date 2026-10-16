//! The range picker's canvas: while the eyedropper is armed, a crosshair over the photograph and,
//! while a drag lasts, the rectangle it covers as a dashed accent outline.
//!
//! The canvas owns pointer capture only. It publishes the press, its moves and its release, in its
//! own logical points and in output-stage pixels, with the Shift held at the release; whether that
//! was a click or a drag, what it picks and where that lands in the content stage are decided in
//! [`crate::app`]'s update ([`crate::app::Editor::pick_pointer`]), so the same picks are reachable
//! from a script without simulating a pointer. The rectangle it draws is the one the app holds,
//! which Escape can take away mid-drag.
use crate::{
    app::message::{
        Message,
        mask::{MaskMessage, PickPointer},
    },
    view::mask_canvas::Placement,
};
use iced::{
    Point, Rectangle, Renderer, keyboard,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, LineDash, Path, Stroke},
};
use luxforge_ui::Theme;

/// The dashes of the drag rectangle's outline, in logical points.
const DASH: [f32; 2] = [4.0, 3.0];

/// The canvas's own pointer bookkeeping: whether the button went down on it, where it last was, and
/// whether Shift is held.
#[derive(Debug, Default)]
pub(crate) struct Interaction {
    pressed: bool,
    last: Option<((f32, f32), (f64, f64))>,
    shift: bool,
}

/// The armed eyedropper over the current render. Built for one `view`.
pub(crate) struct RangePickCanvas {
    placement: Placement,
    /// The drag in progress, as output-stage pixels `[x0, y0, x1, y1]`.
    rectangle: Option<[f64; 4]>,
    /// Something is open over the canvas, so a press there is that thing's, not a pick.
    covered: bool,
}

impl RangePickCanvas {
    pub(crate) fn new(placement: Placement, rectangle: Option<[f64; 4]>) -> Self {
        Self {
            placement,
            rectangle,
            covered: false,
        }
    }

    pub(crate) fn covered(mut self, covered: bool) -> Self {
        self.covered = covered;
        self
    }

    /// A canvas-local point as the step the app receives: the point itself and its output-stage
    /// position.
    fn at(&self, point: Point) -> ((f32, f32), (f64, f64)) {
        ((point.x, point.y), self.placement.view.stage_point(point))
    }

    fn publish(pointer: PickPointer) -> Action<Message> {
        Action::publish(Message::Mask(MaskMessage::PickPointer(pointer))).and_capture()
    }
}

/// The pointer position in canvas-local logical pixels, even once a drag has left the bounds.
fn local(cursor: Cursor, bounds: Rectangle) -> Option<Point> {
    cursor
        .position()
        .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y))
}

impl canvas::Program<Message, Theme> for RangePickCanvas {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.shift = modifiers.shift();
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if !self.covered => {
                let point = cursor.position_in(bounds)?;
                let (view, at) = self.at(point);
                state.pressed = true;
                state.last = Some((view, at));
                Some(Self::publish(PickPointer::Press { view, at }))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.pressed => {
                let (view, at) = self.at(local(cursor, bounds)?);
                state.last = Some((view, at));
                Some(Self::publish(PickPointer::Move { view, at }))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.pressed => {
                state.pressed = false;
                let (view, at) = local(cursor, bounds)
                    .map(|point| self.at(point))
                    .or(state.last.take())?;
                state.last = None;
                Some(Self::publish(PickPointer::Release {
                    view,
                    at,
                    shift: state.shift,
                }))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        // A drag may run in any direction; the rectangle is drawn from its top-left corner.
        let Some(rectangle) = self.rectangle.map(|[x0, y0, x1, y1]| {
            let view = self.placement.view;
            let (from, to) = (view.canvas_point(x0, y0), view.canvas_point(x1, y1));
            Rectangle::new(
                Point::new(from.x.min(to.x), from.y.min(to.y)),
                iced::Size::new((to.x - from.x).abs(), (to.y - from.y).abs()),
            )
        }) else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let path = Path::rectangle(rectangle.position(), rectangle.size());
        let accent = theme.palette().accent;
        frame.fill(&path, iced::Color { a: 0.08, ..accent });
        frame.stroke(
            &path,
            Stroke {
                line_dash: LineDash {
                    segments: &DASH,
                    offset: 0,
                },
                ..Stroke::default().with_color(accent).with_width(1.0)
            },
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        // Off the canvas, or under a menu, sheet or the palette: the system's own pointer.
        let over = super::brush_cursor::brush_cursor(cursor, bounds, self.covered).is_some();
        if over {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mask_draft::ContentMap, view::canvas_view::CanvasView};
    use iced::Size;

    fn placement() -> Placement {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        Placement {
            map: ContentMap::from_affine((480, 320), (480, 320), identity, identity)
                .expect("a drawable stage"),
            view: CanvasView::fit((480.0, 320.0), Size::new(960.0, 640.0)).expect("a fitted view"),
            scale_factor: 1.0,
        }
    }

    fn published(action: Option<Action<Message>>) -> Option<PickPointer> {
        let (message, _, _) = action?.into_inner();
        match message? {
            Message::Mask(MaskMessage::PickPointer(pointer)) => Some(pointer),
            _ => None,
        }
    }

    /// The canvas reports the press, the moves and the release in its own points and in output-stage
    /// pixels, with the Shift held when the button came up, and nothing once it is covered.
    #[test]
    fn a_press_its_moves_and_its_release_are_published_with_shift_at_the_release() {
        use canvas::Program;
        let program = RangePickCanvas::new(placement(), None);
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(960.0, 640.0));
        let mut state = Interaction::default();
        let press = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(Point::new(200.0, 100.0)),
        );
        assert_eq!(
            published(press),
            Some(PickPointer::Press {
                view: (200.0, 100.0),
                at: (100.0, 50.0)
            }),
            "a fitted view at twice the stage halves the canvas point"
        );
        let moved = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(260.0, 140.0),
            }),
            bounds,
            Cursor::Available(Point::new(260.0, 140.0)),
        );
        assert_eq!(
            published(moved),
            Some(PickPointer::Move {
                view: (260.0, 140.0),
                at: (130.0, 70.0)
            })
        );
        let _ = program.update(
            &mut state,
            &Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::SHIFT,
            )),
            bounds,
            Cursor::Available(Point::new(260.0, 140.0)),
        );
        let released = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds,
            Cursor::Available(Point::new(260.0, 140.0)),
        );
        assert_eq!(
            published(released),
            Some(PickPointer::Release {
                view: (260.0, 140.0),
                at: (130.0, 70.0),
                shift: true
            })
        );

        let covered = RangePickCanvas::new(placement(), None).covered(true);
        let mut state = Interaction::default();
        let press = covered.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(Point::new(200.0, 100.0)),
        );
        assert!(press.is_none(), "a press under a menu is the menu's");
        assert_eq!(
            covered.mouse_interaction(&state, bounds, Cursor::Available(Point::new(1.0, 1.0))),
            mouse::Interaction::None
        );
        assert_eq!(
            program.mouse_interaction(&state, bounds, Cursor::Available(Point::new(1.0, 1.0))),
            mouse::Interaction::Crosshair
        );
    }
}
