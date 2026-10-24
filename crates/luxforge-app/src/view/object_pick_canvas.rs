//! The object picker's canvas ([`crate::object_pick`]): while a tool picks objects, a crosshair
//! over the photograph, the object under the pointer washed white inside its outline, a loose
//! drag's dashed stroke, a stroke that found nothing still tinted, and the numbered pins of one
//! that found several. For a tool that holds what it selects (Corrections › Object) it also draws
//! the selected objects, tinted and numbered in pick order, the painted shadow with a dashed edge,
//! the brush while Paint shadow is armed, and a running generation's ring.
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
    brush::Modifiers,
    object_pick::{HoverOverlay, Pin},
    state::remove::{EffectStroke, SelectedObject},
    view::{brush_overlay::Traces, corrections_canvas, mask_canvas::Placement},
};
use iced::{
    Color, Point, Rectangle, Renderer, Size, Vector, alignment, keyboard,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, LineCap, LineDash, Path, Stroke, Text},
};
use luxforge_ui::{Theme, theme};

/// The dashes of a loose stroke's centre line, in logical points.
const DASH: [f32; 2] = [4.0, 3.0];
/// A pin's radius, and how far from its centre a press still catches it, in points.
const PIN_RADIUS: f32 = 13.0;
const PIN_CATCH: f32 = 16.0;
/// A selected object's numbered tag, just inside the top right of its bounds, dark like a pin so
/// it reads over the object's own colour and its tint.
const TAG_RADIUS: f32 = 11.0;
const TAG_INSET: f32 = 4.0;
/// A numeral's size, in points: large and semibold, in the interface's own bundled face, so it
/// reads on any photograph and on a host with no system fonts.
const NUMERAL: f32 = 15.0;
const TAG_NUMERAL: f32 = 13.0;
const WHITE: Color = Color::WHITE;
const UNDER_INK: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
/// A pin's disc: near-opaque dark, ringed white over a dark halo.
const PIN_FILL: Color = Color::from_rgba(0.06, 0.06, 0.07, 0.92);
const PIN_HALO: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.55);
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
    /// What the tool holds: its selection and the painted shadow, and the strokes it paints.
    pub(crate) held: Option<HeldData<'a>>,
}

/// What a tool that holds its selection lends the canvas for one frame.
#[derive(Clone, Copy)]
pub(crate) struct HeldData<'a> {
    /// The selected objects in pick order, each drawn from its held tint.
    pub(crate) objects: &'a [SelectedObject],
    pub(crate) effect: Option<&'a EffectStroke>,
    /// The shadow stroke being painted, at `brush`.
    pub(crate) live: Option<&'a [[f64; 2]]>,
    /// Paint shadow is armed: the brush's radius, which the cursor draws.
    pub(crate) brush: Option<f64>,
    /// A generation runs: its fraction, at this content position.
    pub(crate) progress: Option<(Option<f64>, [f64; 2])>,
    /// A candidate is shown in the photograph, so the selection is not tinted over it.
    pub(crate) shown: bool,
}

/// The canvas's own pointer bookkeeping: whether the button went down on it, and where it last was.
#[derive(Default)]
pub(crate) struct Interaction {
    pressed: bool,
    last: Option<((f32, f32), [f64; 2])>,
    modifiers: Modifiers,
    /// The painted figures last traced.
    traces: Traces,
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
            numbered(frame, centre, (PIN_RADIUS, NUMERAL), index + 1, pin.chosen);
        }
    }

    /// What the tool holds: each selected object's tint over its bounds and its number at the top
    /// right of them, the painted shadow with a dashed edge, the shadow being painted, and a
    /// running generation's ring. A candidate shown in the photograph hides the tints, as it hides
    /// Remove's selection.
    fn held(&self, frame: &mut Frame, held: &HeldData<'_>, traces: &Traces) {
        if !held.shown {
            if let Some(effect) = held.effect {
                corrections_canvas::painted_selection(
                    frame,
                    &self.placement,
                    (&effect.points, effect.radius),
                    false,
                    true,
                    traces,
                );
            }
            for object in held.objects {
                if let Some(kept) = &object.held {
                    self.image_over(frame, kept.bounds, &kept.tint);
                }
            }
            for (index, object) in held.objects.iter().enumerate() {
                let Some(kept) = &object.held else {
                    continue;
                };
                let [_, y0, x1, _] = kept.bounds;
                if let Some(corner) = self.placement.canvas_point_unbounded(x1, y0) {
                    let inset = TAG_RADIUS + TAG_INSET;
                    let at = Point::new(corner.x - inset, corner.y + inset);
                    numbered(frame, at, (TAG_RADIUS, TAG_NUMERAL), index + 1, false);
                }
            }
        }
        if let (Some(points), Some(radius)) = (held.live, held.brush) {
            corrections_canvas::painted_selection(
                frame,
                &self.placement,
                (points, radius),
                true,
                true,
                traces,
            );
        }
        if let Some((fraction, at)) = held.progress {
            corrections_canvas::progress_ring(frame, &self.placement, at, fraction);
        }
    }
}

/// A numbered disc of `(radius, numeral size)` at `centre`: white semibold numerals on near-opaque
/// dark, ringed white over a dark halo, or dark numerals on the accent when `accent`.
fn numbered(frame: &mut Frame, centre: Point, size: (f32, f32), number: usize, accent: bool) {
    let (radius, numeral) = size;
    frame.fill(&Path::circle(centre, radius + 2.0), PIN_HALO);
    let circle = Path::circle(centre, radius);
    frame.fill(
        &circle,
        if accent {
            theme::PHOTO_ANCHOR
        } else {
            PIN_FILL
        },
    );
    frame.stroke(&circle, Stroke::default().with_color(WHITE).with_width(1.5));
    frame.fill_text(Text {
        content: number.to_string(),
        position: centre,
        color: if accent { PIN_CHOSEN_INK } else { WHITE },
        size: iced::Pixels(numeral),
        font: theme::FONT_SEMIBOLD,
        align_x: alignment::Horizontal::Center.into(),
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

fn modifiers_of(modifiers: keyboard::Modifiers) -> Modifiers {
    Modifiers {
        shift: modifiers.shift(),
        command: modifiers.command(),
        alt: modifiers.alt(),
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
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = modifiers_of(*modifiers);
                None
            }
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
                    ObjectPointer::Press {
                        view,
                        at,
                        modifiers: state.modifiers,
                    },
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
                // Paint shadow's brush follows the pointer without a pick.
                if self.data.held.is_some_and(|held| held.brush.is_some()) {
                    return Some(Action::request_redraw());
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
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        if let Some(held) = &self.data.held {
            self.held(&mut frame, held, &state.traces);
        }
        let brush = self.data.held.and_then(|held| held.brush);
        if let Some(overlay) = self.data.overlay
            && self.data.pins.is_none()
            && brush.is_none()
        {
            self.image_over(&mut frame, overlay.bounds, &overlay.handle);
        }
        if let Some(stroke) = self.data.stroke {
            self.stroke(&mut frame, stroke);
        }
        if let Some(pins) = self.data.pins {
            self.pins(&mut frame, pins);
        }
        if let Some(radius) = brush
            && let Some(point) = super::brush_cursor::brush_cursor(cursor, bounds, self.covered)
            && let Some(at) = self.content(point)
        {
            corrections_canvas::brush_ring(&mut frame, &self.placement, at, radius);
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
        let brush = self.data.held.is_some_and(|held| held.brush.is_some());
        match super::brush_cursor::brush_cursor(cursor, bounds, self.covered) {
            Some(point) if self.pin_at(point).is_some() => mouse::Interaction::Pointer,
            // Paint shadow draws the brush as its own cursor.
            Some(point) if brush && self.content(point).is_some() => mouse::Interaction::Hidden,
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
            held: None,
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
            held: None,
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
