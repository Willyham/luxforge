//! Divider chrome and pointer mapping. The owner holds the position; this canvas only publishes
//! normalized input and keeps whether its pointer is currently down.
use crate::app::message::{Message, history::HistoryMessage};
use iced::{
    Color, Point, Rectangle, Renderer, Theme,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke, Text},
};
use luxforge_ui::theme;

pub(crate) struct CompareCanvas {
    pub(crate) photo: Rectangle,
    pub(crate) position: f32,
}

#[derive(Default)]
pub(crate) struct Drag {
    held: bool,
}

impl CompareCanvas {
    fn position(&self, point: Point) -> f32 {
        ((point.x - self.photo.x) / self.photo.width).clamp(0.0, 1.0)
    }

    fn publish(&self, point: Point) -> Action<Message> {
        Action::publish(Message::History(HistoryMessage::ComparePosition(
            self.position(point),
        )))
        .and_capture()
    }
}

impl canvas::Program<Message> for CompareCanvas {
    type State = Drag;

    fn update(
        &self,
        state: &mut Drag,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        let point = cursor
            .position()
            .map(|p| Point::new(p.x - bounds.x, p.y - bounds.y));
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = point.filter(|p| self.photo.contains(*p))?;
                state.held = true;
                Some(self.publish(point))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.held => {
                point.map(|p| self.publish(p))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.held => {
                state.held = false;
                Some(point.map_or_else(
                    || Action::request_redraw().and_capture(),
                    |p| self.publish(p),
                ))
            }
            Event::Window(iced::window::Event::Unfocused) if state.held => {
                state.held = false;
                Some(Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Drag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let rect = self.photo;
        let x = rect.x + rect.width * self.position;
        let line = Path::line(Point::new(x, rect.y), Point::new(x, rect.y + rect.height));
        frame.stroke(
            &line,
            Stroke::default()
                .with_color(theme::THUMB_OUTLINE)
                .with_width(4.0),
        );
        frame.stroke(
            &line,
            Stroke::default().with_color(theme::THUMB).with_width(2.0),
        );
        let centre = Point::new(x, rect.center_y());
        let grip = Path::circle(centre, 14.0);
        frame.fill(&grip, theme::BAR);
        frame.stroke(
            &grip,
            Stroke::default().with_color(theme::THUMB).with_width(1.5),
        );
        for direction in [-1.0, 1.0] {
            let arrow = Path::new(|path| {
                path.move_to(Point::new(x + direction * 3.0, centre.y - 4.0));
                path.line_to(Point::new(x + direction * 7.0, centre.y));
                path.line_to(Point::new(x + direction * 3.0, centre.y + 4.0));
            });
            frame.stroke(
                &arrow,
                Stroke::default()
                    .with_color(theme::TEXT_PRIMARY)
                    .with_width(1.5),
            );
        }
        for (label, left, shown) in [
            ("Before", rect.x + 12.0, rect.width * self.position >= 76.0),
            (
                "After",
                rect.x + rect.width - 66.0,
                rect.width * (1.0 - self.position) >= 76.0,
            ),
        ] {
            if shown {
                let origin = Point::new(left, rect.y + 12.0);
                frame.fill_rectangle(
                    origin,
                    iced::Size::new(54.0, 23.0),
                    Color {
                        a: 0.78,
                        ..theme::CANVAS
                    },
                );
                frame.fill_text(Text {
                    content: label.into(),
                    position: Point::new(left + 7.0, origin.y + 4.0),
                    color: theme::TEXT_PRIMARY,
                    size: theme::SIZE_CAPTION.into(),
                    font: theme::FONT,
                    ..Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Drag,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if state.held
            || cursor
                .position_in(bounds)
                .is_some_and(|p| self.photo.contains(p))
        {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compare_drag_keeps_capture_outside_and_releases_once() {
        use canvas::Program;
        let divider = CompareCanvas {
            photo: Rectangle::new(Point::new(20.0, 10.0), iced::Size::new(200.0, 100.0)),
            position: 0.5,
        };
        let bounds = Rectangle::new(Point::new(100.0, 200.0), iced::Size::new(240.0, 120.0));
        let mut drag = Drag::default();
        let pressed = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert!(
            divider
                .update(
                    &mut drag,
                    &pressed,
                    bounds,
                    Cursor::Available(Point::new(105.0, 205.0))
                )
                .is_none()
        );
        let (message, _, status) = divider
            .update(
                &mut drag,
                &pressed,
                bounds,
                Cursor::Available(Point::new(220.0, 250.0)),
            )
            .unwrap()
            .into_inner();
        assert!(matches!(
            message,
            Some(Message::History(HistoryMessage::ComparePosition(0.5)))
        ));
        assert_eq!(status, iced::event::Status::Captured);
        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(900.0, 250.0),
        });
        let (message, _, _) = divider
            .update(
                &mut drag,
                &moved,
                bounds,
                Cursor::Available(Point::new(900.0, 250.0)),
            )
            .unwrap()
            .into_inner();
        assert!(matches!(
            message,
            Some(Message::History(HistoryMessage::ComparePosition(1.0)))
        ));
        let released = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert!(
            divider
                .update(&mut drag, &released, bounds, Cursor::Unavailable)
                .is_some()
        );
        assert!(
            divider
                .update(&mut drag, &released, bounds, Cursor::Unavailable)
                .is_none()
        );
        assert!(!drag.held);
    }

    #[test]
    fn compare_drag_maps_the_photo_and_clamps_both_edges() {
        let divider = CompareCanvas {
            photo: Rectangle::new(Point::new(20.0, 10.0), iced::Size::new(200.0, 100.0)),
            position: 0.5,
        };
        assert_eq!(divider.position(Point::new(-100.0, 50.0)), 0.0);
        assert_eq!(divider.position(Point::new(120.0, 50.0)), 0.5);
        assert_eq!(divider.position(Point::new(400.0, 50.0)), 1.0);
    }
}
