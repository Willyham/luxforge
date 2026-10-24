//! The object picker's canvas ([`crate::object_pick`]): while a tool picks objects, a crosshair
//! over the photograph, the object under the pointer washed white inside its outline, a loose
//! drag's dashed stroke, a stroke that found nothing still tinted, and the numbered pins of one
//! that found several.
//!
//! The canvas owns pointer capture only. It publishes the hover, the press, its moves and its
//! release in its own logical points and normalized content coordinates, and a press on a pin as
//! that pin; whether a press was a click or a loose drag, and what either picks, are decided in the
//! app's update ([`crate::app::Editor::object_pick_message`]), so the same picks are reachable from
//! a script without simulating a pointer.
use crate::{
    app::message::{
        Message,
        object_pick::{ObjectPickMessage, ObjectPointer},
    },
    object_pick::{HoverOverlay, Pin},
    view::mask_canvas::Placement,
};
use iced::{
    Color, Point, Rectangle, Renderer, Size, Vector, alignment,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, LineCap, LineDash, Path, Stroke, Text},
};
use luxforge_ui::{Theme, theme};

/// The dashes of a loose stroke's centre line, in logical points.
const DASH: [f32; 2] = [4.0, 3.0];
/// A pin's radius, and how far from its centre a press still catches it, in points.
const PIN_RADIUS: f32 = 11.0;
const PIN_CATCH: f32 = 14.0;
const WHITE: Color = Color::WHITE;
const UNDER_INK: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
const PIN_FILL: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.75);
/// A chosen pin's number, dark on the accent.
const PIN_CHOSEN_INK: Color = Color::from_rgb(0.1, 0.08, 0.05);
/// A loose stroke's footprint while it is drawn, and once it found nothing.
const STROKE_TINT: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.18);
const NOTHING_TINT: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.32);

/// What the picking canvas is drawn from, for one frame.
#[derive(Clone, Copy)]
pub(crate) struct ObjectPickData<'a> {
    pub(crate) map: &'a crate::brush::ContentMap,
    pub(crate) overlay: Option<&'a HoverOverlay>,
    /// A loose drag in progress, the stroke a pick is finding, the stroke that found nothing or the
    /// stroke the pins came from, in normalized content coordinates.
    pub(crate) stroke: Option<&'a [[f64; 2]]>,
    /// The stroke's radius, in content heights.
    pub(crate) radius: f64,
    pub(crate) nothing: bool,
    pub(crate) pins: Option<&'a [Pin]>,
}

/// The canvas's own pointer bookkeeping: whether the button went down on it, and where it last was.
#[derive(Debug, Default)]
pub(crate) struct Interaction {
    pressed: bool,
    last: Option<((f32, f32), [f64; 2])>,
}

/// The picking canvas over the current render. Built for one `view`.
pub(crate) struct ObjectPickCanvas<'a> {
    placement: Placement,
    data: ObjectPickData<'a>,
    /// Something is open over the canvas, so a press there is that thing's, not a pick.
    covered: bool,
}

impl<'a> ObjectPickCanvas<'a> {
    pub(crate) fn new(placement: Placement, data: ObjectPickData<'a>) -> Self {
        Self {
            placement,
            data,
            covered: false,
        }
    }

    pub(crate) fn covered(mut self, covered: bool) -> Self {
        self.covered = covered;
        self
    }

    fn content(&self, point: Point) -> Option<[f64; 2]> {
        self.placement.content_point(point).map(|(x, y)| [x, y])
    }

    fn publish(message: ObjectPickMessage) -> Action<Message> {
        Action::publish(Message::ObjectPick(message)).and_capture()
    }

    /// The pin a press at `point` catches, by its position among the pins.
    fn pin_at(&self, point: Point) -> Option<usize> {
        self.data.pins?.iter().position(|pin| {
            let [x, y] = pin.centre();
            self.placement
                .canvas_point_unbounded(x, y)
                .is_some_and(|centre| centre.distance(point) <= PIN_CATCH)
        })
    }

    /// One content height in canvas points at `at`, for drawing a stroke at its radius.
    fn content_height(&self, at: [f64; 2]) -> f32 {
        let (Some(from), Some(to)) = (
            self.placement.canvas_point_unbounded(at[0], at[1]),
            self.placement.canvas_point_unbounded(at[0], at[1] + 1.0),
        ) else {
            return 0.0;
        };
        from.distance(to)
    }

    /// An image over content `bounds`, placed by three of its corners: rotated with the crop and
    /// mirrored with a flip.
    fn image_over(
        &self,
        frame: &mut Frame,
        bounds: [f64; 4],
        handle: &iced::widget::image::Handle,
    ) {
        let [x0, y0, x1, y1] = bounds;
        let corner = |x, y| self.placement.canvas_point_unbounded(x, y);
        let (Some(origin), Some(across), Some(down)) =
            (corner(x0, y0), corner(x1, y0), corner(x0, y1))
        else {
            return;
        };
        let (u, v) = (across - origin, down - origin);
        let (width, height) = (u.x.hypot(u.y), v.x.hypot(v.y));
        if width < 0.5 || height < 0.5 {
            return;
        }
        frame.with_save(|frame| {
            frame.translate(Vector::new(origin.x, origin.y));
            frame.rotate(u.y.atan2(u.x));
            if u.x * v.y - u.y * v.x < 0.0 {
                frame.scale_nonuniform(Vector::new(1.0, -1.0));
            }
            frame.draw_image(
                Rectangle::new(Point::ORIGIN, Size::new(width, height)),
                handle,
            );
        });
    }

    /// A loose stroke: its footprint at the radius, tinted, and a dashed centre line over a dark
    /// under-stroke.
    fn stroke(&self, frame: &mut Frame, points: &[[f64; 2]]) {
        let canvas: Vec<Point> = points
            .iter()
            .filter_map(|[x, y]| self.placement.canvas_point_unbounded(*x, *y))
            .collect();
        let Some(first) = canvas.first() else {
            return;
        };
        let path = Path::new(|builder| {
            builder.move_to(*first);
            for point in &canvas[1..] {
                builder.line_to(*point);
            }
        });
        let width = (2.0 * self.data.radius as f32 * self.content_height(points[0])).max(2.0);
        let tint = if self.data.nothing {
            NOTHING_TINT
        } else {
            STROKE_TINT
        };
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(tint)
                .with_width(width)
                .with_line_cap(LineCap::Round),
        );
        frame.stroke(
            &path,
            Stroke::default().with_color(UNDER_INK).with_width(2.5),
        );
        frame.stroke(
            &path,
            Stroke {
                line_dash: LineDash {
                    segments: &DASH,
                    offset: 0,
                },
                ..Stroke::default().with_color(WHITE).with_width(1.25)
            },
        );
    }

    /// The numbered pins, each at its candidate's centre: a chosen pin is filled with the accent
    /// and its object washed as the hover washes it.
    fn pins(&self, frame: &mut Frame, pins: &[Pin]) {
        for pin in pins.iter().filter(|pin| pin.chosen) {
            if let Some(handle) = &pin.handle {
                self.image_over(frame, pin.bounds, handle);
            }
        }
        for (index, pin) in pins.iter().enumerate() {
            let [x, y] = pin.centre();
            let Some(centre) = self.placement.canvas_point_unbounded(x, y) else {
                continue;
            };
            let circle = Path::circle(centre, PIN_RADIUS);
            frame.fill(
                &circle,
                if pin.chosen {
                    theme::PHOTO_ANCHOR
                } else {
                    PIN_FILL
                },
            );
            frame.stroke(
                &circle,
                Stroke::default().with_color(WHITE).with_width(1.25),
            );
            frame.fill_text(Text {
                content: (index + 1).to_string(),
                position: centre,
                color: if pin.chosen { PIN_CHOSEN_INK } else { WHITE },
                size: iced::Pixels(13.0),
                align_x: alignment::Horizontal::Center.into(),
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
        }
    }
}

/// The pointer position in canvas-local logical pixels, even once a drag has left the bounds.
fn local(cursor: Cursor, bounds: Rectangle) -> Option<Point> {
    cursor
        .position()
        .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y))
}

impl canvas::Program<Message, Theme> for ObjectPickCanvas<'_> {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if !self.covered => {
                let point = cursor.position_in(bounds)?;
                if let Some(index) = self.pin_at(point) {
                    return Some(Self::publish(ObjectPickMessage::Pin(index)));
                }
                let at = self.content(point)?;
                let view = (point.x, point.y);
                state.pressed = true;
                state.last = Some((view, at));
                Some(Self::publish(ObjectPickMessage::Pointer(
                    ObjectPointer::Press { view, at },
                )))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.pressed {
                    let point = local(cursor, bounds)?;
                    let at = self.content(point)?;
                    let view = (point.x, point.y);
                    state.last = Some((view, at));
                    return Some(Self::publish(ObjectPickMessage::Pointer(
                        ObjectPointer::Move { view, at },
                    )));
                }
                if self.covered {
                    return None;
                }
                // Hover: each move over the photograph is a pick, the latest asked next.
                let at = cursor
                    .position_in(bounds)
                    .and_then(|point| self.content(point));
                Some(Action::publish(Message::ObjectPick(
                    ObjectPickMessage::Hover(at),
                )))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.pressed => {
                state.pressed = false;
                let (view, at) = local(cursor, bounds)
                    .and_then(|point| Some(((point.x, point.y), self.content(point)?)))
                    .or(state.last.take())?;
                state.last = None;
                Some(Self::publish(ObjectPickMessage::Pointer(
                    ObjectPointer::Release { view, at },
                )))
            }
            Event::Mouse(mouse::Event::CursorLeft) if !state.pressed => Some(Action::publish(
                Message::ObjectPick(ObjectPickMessage::Hover(None)),
            )),
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
        if let Some(overlay) = self.data.overlay
            && self.data.pins.is_none()
        {
            self.image_over(&mut frame, overlay.bounds, &overlay.handle);
        }
        if let Some(stroke) = self.data.stroke {
            self.stroke(&mut frame, stroke);
        }
        if let Some(pins) = self.data.pins {
            self.pins(&mut frame, pins);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        // Off the canvas, or under a menu, sheet or the palette: the system's own pointer.
        match super::brush_cursor::brush_cursor(cursor, bounds, self.covered) {
            Some(point) if self.pin_at(point).is_some() => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Crosshair,
            None => mouse::Interaction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mask_draft::ContentMap, view::canvas_view::CanvasView};

    fn placement() -> Placement {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        Placement {
            map: ContentMap::from_affine((480, 320), (480, 320), identity, identity)
                .expect("a drawable stage"),
            view: CanvasView::fit((480.0, 320.0), Size::new(960.0, 640.0)).expect("a fitted view"),
            scale_factor: 1.0,
        }
    }

    fn published(action: Option<Action<Message>>) -> Option<ObjectPickMessage> {
        let (message, _, _) = action?.into_inner();
        match message? {
            Message::ObjectPick(message) => Some(message),
            _ => None,
        }
    }

    /// The canvas publishes a hover with no button down, the press, its moves and its release in
    /// its own points and in normalized content coordinates, and a press on a pin as that pin.
    #[test]
    fn object_pick_canvas_publishes_hover_press_and_pins_in_content_coordinates() {
        use canvas::Program;
        let map = placement().map.clone();
        let data = ObjectPickData {
            map: &map,
            overlay: None,
            stroke: None,
            radius: 0.03,
            nothing: false,
            pins: None,
        };
        let program = ObjectPickCanvas::new(placement(), data);
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(960.0, 640.0));
        let mut state = Interaction::default();
        let at = Point::new(480.0, 160.0);
        let hover = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::CursorMoved { position: at }),
            bounds,
            Cursor::Available(at),
        );
        let Some(ObjectPickMessage::Hover(Some([x, y]))) = published(hover) else {
            panic!("a move with no button down is a hover")
        };
        assert!((x - 0.5).abs() < 1e-9 && (y - 0.25).abs() < 1e-9, "{x} {y}");
        let press = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(at),
        );
        assert!(matches!(
            published(press),
            Some(ObjectPickMessage::Pointer(ObjectPointer::Press {
                view: (480.0, 160.0),
                ..
            }))
        ));
        let released = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds,
            Cursor::Available(at),
        );
        assert!(matches!(
            published(released),
            Some(ObjectPickMessage::Pointer(ObjectPointer::Release { .. }))
        ));

        let pins = [Pin {
            position: 3,
            bounds: [0.4, 0.2, 0.6, 0.3],
            chosen: false,
            handle: None,
        }];
        let data = ObjectPickData {
            pins: Some(&pins),
            ..data
        };
        let program = ObjectPickCanvas::new(placement(), data);
        let mut state = Interaction::default();
        let pressed = program.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(Point::new(482.0, 161.0)),
        );
        assert!(matches!(
            published(pressed),
            Some(ObjectPickMessage::Pin(0))
        ));
    }
}
