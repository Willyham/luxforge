//! The Corrections canvas (`docs/design/corrections-workspace.md`, "The canvas"): every
//! correction's pin, the hovered and selected corrections' outlines, sources and arrows, the
//! selected one's handles, the gesture in progress and the brush cursor, over the photograph, and
//! the pointer turned into the workspace's messages.
//!
//! The canvas owns no editing state. It borrows what the app holds for one `view` call, places
//! every figure through the core's map and the shared warped-path drawing
//! ([`super::brush_overlay`]), decides which target a press catches by the workspace's hit order
//! ([`crate::brush::hit`]) and publishes the press, its moves and its release; what they do is the
//! app's. Overlays are white over a translucent dark under-stroke so they read on any photograph,
//! and they are never themed: the accent marks only the selected pin, a set source and the gesture
//! being drafted. Pins and handles keep their point size at every zoom.
use super::{brush_overlay, mask_canvas::Placement, warped_path::Outline};
use crate::{
    app::message::{
        Message,
        corrections::{CanvasHit, CorrectionMessage},
    },
    brush::{
        ContentMap, Handle, HeldOperation, Hit, Modifiers, SourceChoice, ViewPoint, cursor_rings,
        hit,
    },
    state::corrections::Listed,
};
use iced::{
    Color, Point, Rectangle, Renderer, Size, keyboard,
    mouse::{self, Cursor},
    widget::canvas::{self, Action, Event, Frame, Geometry, LineDash, Path, Stroke},
};
use luxforge_ui::{Theme, theme};

/// A pin's ring: 12 pt across, on a translucent dark fill.
const PIN_RADIUS: f32 = 6.0;
/// The selected correction's hollow source pin.
const SOURCE_PIN_RADIUS: f32 = 4.0;
/// Half the size handle's side.
const SIZE_HANDLE: f32 = 3.5;
/// Outlines, the arrow and the under-stroke, in points.
const OUTLINE: f32 = 1.25;
const ARROW: f32 = 1.1;
const UNDER: f32 = 1.5;
/// The dashed source outline's pattern, in points.
const DASH: [f32; 2] = [4.0, 3.0];
/// The crosshair's arm and the `+` at the brush's centre.
const CROSSHAIR_ARM: f32 = 7.0;
const PLUS_ARM: f32 = 4.0;

const WHITE: Color = Color::WHITE;
const UNDER_INK: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
const PIN_FILL: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.35);
/// The faint tint of a footprint whose automatic source is picked on release.
const FOOTPRINT: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.16);

/// What the app lends the canvas for one frame.
#[derive(Clone, Copy)]
pub(crate) struct CanvasData<'a> {
    pub(crate) map: &'a ContentMap,
    /// Every stored correction, in application order.
    pub(crate) operations: &'a [Listed],
    pub(crate) selected: Option<&'a str>,
    pub(crate) hovered: Option<&'a str>,
    /// `H`: no pins, no outlines and no hit targets.
    pub(crate) hidden: bool,
    /// The next press's radius and feather.
    pub(crate) radius: f64,
    pub(crate) feather: f64,
    pub(crate) source: SourceChoice,
    /// The gesture in progress, drawn as it would commit now.
    pub(crate) gesture: Option<&'a crate::app::corrections::CorrectionGesture>,
    /// The gesture adds a new correction.
    pub(crate) painting: bool,
    /// Its source is picked on release, so it is drawn as a faint footprint.
    pub(crate) automatic: bool,
    /// A press would be refused: the limit, or another draft open.
    pub(crate) refused: bool,
    /// The map is for an older entry; a press waits for the fresh one.
    pub(crate) waiting: bool,
}

/// The canvas's own pointer bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct Interaction {
    modifiers: Modifiers,
    /// The button went down on the canvas and has not come up.
    pressed: bool,
    /// The last content position the pointer was placed at while pressed, for a release outside.
    last: Option<(ViewPoint, [f64; 2])>,
    /// The pin under the pointer, as last published.
    hovered: Option<String>,
}

/// The Corrections overlay. Borrowed from the app for one `view`.
pub(crate) struct CorrectionsCanvas<'a> {
    data: CanvasData<'a>,
    placement: Placement,
    /// The gesture in progress as it would commit now, and the stored correction it edits, which
    /// is drawn as `live` instead.
    live: Option<HeldOperation>,
    edited: Option<String>,
}

impl<'a> CorrectionsCanvas<'a> {
    pub(crate) fn new(data: CanvasData<'a>, placement: Placement) -> Self {
        let edited = data
            .gesture
            .and_then(|gesture| gesture.edited().map(str::to_owned));
        let live = data.gesture.and_then(|gesture| {
            gesture.drawn(edited.as_ref().and_then(|id| {
                data.operations
                    .iter()
                    .find(|listed| &listed.operation == id)
            }))
        });
        Self {
            data,
            placement,
            live,
            edited,
        }
    }
}

fn modifiers_of(modifiers: keyboard::Modifiers) -> Modifiers {
    Modifiers {
        shift: modifiers.shift(),
        command: modifiers.command(),
        alt: modifiers.alt(),
    }
}

/// The pointer in canvas-local points, even once a drag has left the bounds.
fn local(cursor: Cursor, bounds: Rectangle) -> Option<Point> {
    cursor
        .position()
        .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y))
}

/// Stroke an outline white over its dark under-stroke, dashed when `dash` is given.
fn stroke_outline(frame: &mut Frame, path: &Path, width: f32, colour: Color, dash: bool) {
    let dashes: &[f32] = if dash { &DASH } else { &[] };
    for (ink, extra) in [(UNDER_INK, UNDER), (colour, 0.0)] {
        frame.stroke(
            path,
            Stroke {
                line_dash: LineDash {
                    segments: dashes,
                    offset: 0,
                },
                ..Stroke::default().with_color(ink).with_width(width + extra)
            },
        );
    }
}

/// An outline's segments joined into polylines where they meet, so a dash runs along the figure
/// rather than restarting on every segment.
fn joined(outline: &Outline) -> Path {
    Path::new(|builder| {
        let mut end: Option<Point> = None;
        for (a, b) in &outline.segments {
            if end.is_none_or(|end| (end.x - a.x).abs() > 0.01 || (end.y - a.y).abs() > 0.01) {
                builder.move_to(*a);
            }
            builder.line_to(*b);
            end = Some(*b);
        }
    })
}

/// The same outline closed and filled.
fn filled(outline: &Outline) -> Path {
    Path::new(|builder| {
        let mut first = true;
        for (a, b) in &outline.segments {
            if first {
                builder.move_to(*a);
                first = false;
            }
            builder.line_to(*b);
        }
        builder.close();
    })
}

/// The operation moved to where its first point samples from, for its source outline.
fn at_source(held: &HeldOperation) -> Option<HeldOperation> {
    let (source, anchor) = (held.source?, *held.points.first()?);
    let (dx, dy) = (source[0] - anchor[0], source[1] - anchor[1]);
    Some(HeldOperation {
        points: held.points.iter().map(|[x, y]| [x + dx, y + dy]).collect(),
        source: None,
        ..held.clone()
    })
}

impl CorrectionsCanvas<'_> {
    /// Each stored correction as drawn now: the gesture's live shape in place of the one it edits.
    fn drawn(&self) -> impl Iterator<Item = (&Listed, HeldOperation)> + '_ {
        self.data.operations.iter().map(|listed| {
            let held = match (&self.edited, &self.live) {
                (Some(edited), Some(live)) if edited == &listed.operation => live.clone(),
                _ => listed.held(),
            };
            (listed, held)
        })
    }

    /// The selected correction's handles where they are drawn, in hit order.
    fn handles(&self) -> Vec<(Handle, ViewPoint)> {
        let Some(selected) = self.data.selected else {
            return Vec::new();
        };
        self.drawn()
            .find(|(listed, _)| listed.operation == selected)
            .map(|(_, held)| brush_overlay::handles(&self.placement, &held))
            .unwrap_or_default()
    }

    /// Every pin from the top of the stack down, by its place in application order.
    fn pins(&self) -> Vec<(usize, ViewPoint)> {
        let mut pins: Vec<(usize, ViewPoint)> = self
            .drawn()
            .enumerate()
            .filter_map(|(index, (_, held))| {
                Some((index, brush_overlay::pin(&self.placement, &held)?))
            })
            .collect();
        pins.reverse();
        pins
    }

    /// What a press at `point` with `modifiers` catches.
    fn hit_at(&self, point: Point, modifiers: Modifiers) -> CanvasHit {
        match hit(
            (point.x, point.y),
            modifiers,
            !self.data.hidden,
            self.handles(),
            self.pins(),
        ) {
            Hit::Handle(handle) => CanvasHit::Handle(handle),
            Hit::Pin(index) => self
                .data
                .operations
                .get(index)
                .map_or(CanvasHit::Add, |listed| {
                    CanvasHit::Pin(listed.operation.clone())
                }),
            Hit::Add => CanvasHit::Add,
        }
    }

    fn publish(message: CorrectionMessage) -> Action<Message> {
        Action::publish(Message::Corrections(message)).and_capture()
    }

    /// One correction's outline, dashed source outline and arrow.
    fn figure(&self, frame: &mut Frame, held: &HeldOperation, accent: bool) {
        let colour = if accent { theme::PHOTO_ANCHOR } else { WHITE };
        let outline = brush_overlay::outline(&self.placement, held);
        stroke_outline(frame, &joined(&outline), OUTLINE, colour, false);
        if let Some(source) = at_source(held) {
            let dashed = brush_overlay::outline(&self.placement, &source);
            stroke_outline(frame, &joined(&dashed), OUTLINE, WHITE, true);
            let arrow = brush_overlay::arrow(&self.placement, held);
            stroke_outline(frame, &arrow.path(), ARROW, WHITE, false);
        }
    }

    fn pin(frame: &mut Frame, at: ViewPoint, selected: bool) {
        let centre = Point::new(at.0, at.1);
        let fill = if selected {
            theme::PHOTO_ANCHOR
        } else {
            PIN_FILL
        };
        frame.fill(&Path::circle(centre, PIN_RADIUS), fill);
        frame.stroke(
            &Path::circle(centre, PIN_RADIUS + 0.75),
            Stroke::default().with_color(UNDER_INK).with_width(1.0),
        );
        frame.stroke(
            &Path::circle(centre, PIN_RADIUS),
            Stroke::default().with_color(WHITE).with_width(1.5),
        );
    }

    fn handle(frame: &mut Frame, handle: Handle, at: ViewPoint) {
        let centre = Point::new(at.0, at.1);
        match handle {
            Handle::Pin => {}
            Handle::Source => {
                frame.stroke(
                    &Path::circle(centre, SOURCE_PIN_RADIUS),
                    Stroke::default().with_color(UNDER_INK).with_width(2.5),
                );
                frame.stroke(
                    &Path::circle(centre, SOURCE_PIN_RADIUS),
                    Stroke::default().with_color(WHITE).with_width(1.25),
                );
            }
            Handle::Size => {
                let square = Path::rectangle(
                    Point::new(centre.x - SIZE_HANDLE, centre.y - SIZE_HANDLE),
                    Size::new(2.0 * SIZE_HANDLE, 2.0 * SIZE_HANDLE),
                );
                frame.fill(&square, WHITE);
                frame.stroke(
                    &square,
                    Stroke::default().with_color(UNDER_INK).with_width(1.0),
                );
            }
        }
    }

    /// A two-line tooltip above `at`, kept inside the canvas.
    fn tooltip(frame: &mut Frame, at: Point, lead: &str, detail: &str, size: Size) {
        const PAD: f32 = 6.0;
        const LINE: f32 = 15.0;
        let width =
            7.0 * detail.chars().count().max(lead.chars().count()) as f32 * 0.82 + 2.0 * PAD;
        let height = 2.0 * LINE + PAD;
        let x = (at.x + 14.0).min(size.width - width).max(0.0);
        let y = (at.y - height - 12.0).max(0.0);
        frame.fill(
            &Path::rounded_rectangle(Point::new(x, y), Size::new(width, height), 5.0.into()),
            Color::from_rgba(0.08, 0.08, 0.09, 0.92),
        );
        for (line, (text, colour)) in [
            (lead, WHITE),
            (detail, Color::from_rgba(1.0, 1.0, 1.0, 0.65)),
        ]
        .into_iter()
        .enumerate()
        {
            frame.fill_text(canvas::Text {
                content: text.to_owned(),
                position: Point::new(x + PAD, y + PAD / 2.0 + LINE * line as f32),
                color: colour,
                size: iced::Pixels(if line == 0 { 12.0 } else { 11.0 }),
                ..canvas::Text::default()
            });
        }
    }

    /// A crosshair of `arm` at a canvas point, in `colour`.
    fn crosshair(frame: &mut Frame, centre: Point, arm: f32, colour: Color) {
        let path = Path::new(|builder| {
            builder.move_to(Point::new(centre.x - arm, centre.y));
            builder.line_to(Point::new(centre.x + arm, centre.y));
            builder.move_to(Point::new(centre.x, centre.y - arm));
            builder.line_to(Point::new(centre.x, centre.y + arm));
        });
        stroke_outline(frame, &path, OUTLINE, colour, false);
    }

    /// The brush cursor at a content position: its two rings through the map, a `+` at its centre,
    /// and, with a set source, the accent crosshair where it samples. Held `⌥` swaps it for the
    /// source crosshair at brush size; a refused press draws a slash through it.
    fn cursor(&self, frame: &mut Frame, at: [f64; 2], point: Point, modifiers: Modifiers) {
        let spot = |radius: f64| HeldOperation {
            operation: String::new(),
            points: vec![at],
            radius,
            source: None,
        };
        let alt = modifiers.alt;
        for (radius, alpha, dashed) in cursor_rings(self.data.radius, self.data.feather) {
            let ring = brush_overlay::outline(&self.placement, &spot(radius));
            let colour = if alt {
                theme::PHOTO_ANCHOR
            } else {
                Color { a: alpha, ..WHITE }
            };
            stroke_outline(frame, &joined(&ring), OUTLINE, colour, dashed || alt);
        }
        if alt {
            Self::crosshair(frame, point, CROSSHAIR_ARM, theme::PHOTO_ANCHOR);
            return;
        }
        Self::crosshair(frame, point, PLUS_ARM, WHITE);
        if self.data.refused {
            let reach = 9.0;
            let slash = Path::line(
                Point::new(point.x - reach, point.y + reach),
                Point::new(point.x + reach, point.y - reach),
            );
            stroke_outline(frame, &slash, 1.5, WHITE, false);
        }
        if let Some(source) = self.data.source.crosshair(at)
            && let Some(centre) = self.placement.canvas_point(source[0], source[1])
        {
            Self::crosshair(frame, centre, CROSSHAIR_ARM, theme::PHOTO_ANCHOR);
            let tether = Path::line(point, centre);
            frame.stroke(
                &tether,
                Stroke {
                    line_dash: LineDash {
                        segments: &[2.0, 3.0],
                        offset: 0,
                    },
                    ..Stroke::default()
                        .with_color(Color { a: 0.6, ..WHITE })
                        .with_width(1.0)
                },
            );
        }
    }
}

impl canvas::Program<Message, Theme> for CorrectionsCanvas<'_> {
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
                Some(Action::request_redraw())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                let (x, y) = self.placement.content_point(point)?;
                let view = (point.x, point.y);
                state.pressed = true;
                state.last = Some((view, [x, y]));
                Some(Self::publish(CorrectionMessage::Press {
                    view,
                    content: [x, y],
                    modifiers: state.modifiers,
                    hit: self.hit_at(point, state.modifiers),
                }))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = local(cursor, bounds)?;
                if state.pressed {
                    let (x, y) = self.placement.content_point(point)?;
                    let view = (point.x, point.y);
                    state.last = Some((view, [x, y]));
                    return Some(Self::publish(CorrectionMessage::Drag {
                        view,
                        content: [x, y],
                    }));
                }
                let over = (!self.data.hidden && cursor.position_in(bounds).is_some())
                    .then(|| match self.hit_at(point, state.modifiers) {
                        CanvasHit::Pin(pin) => Some(pin),
                        CanvasHit::Handle(_) => self.data.selected.map(str::to_owned),
                        CanvasHit::Add => None,
                    })
                    .flatten();
                if over != state.hovered {
                    state.hovered = over.clone();
                    return Some(Action::publish(Message::Corrections(
                        CorrectionMessage::HoverPin(over),
                    )));
                }
                // The cursor follows the pointer without a message or a round trip.
                Some(Action::request_redraw())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.pressed => {
                state.pressed = false;
                let (view, content) = local(cursor, bounds)
                    .and_then(|point| {
                        let (x, y) = self.placement.content_point(point)?;
                        Some(((point.x, point.y), [x, y]))
                    })
                    .or(state.last)?;
                state.last = None;
                Some(Self::publish(CorrectionMessage::Release { view, content }))
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if state.hovered.take().is_some() {
                    return Some(Action::publish(Message::Corrections(
                        CorrectionMessage::HoverPin(None),
                    )));
                }
                Some(Action::request_redraw())
            }
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
        let shown = !self.data.hidden;
        // The gesture in progress: a faint footprint while its source waits for the release, or
        // its live outline and source.
        if let (true, Some(live)) = (self.data.painting, &self.live) {
            let outline = brush_overlay::outline(&self.placement, live);
            if self.data.automatic {
                frame.fill(&filled(&outline), FOOTPRINT);
            }
            stroke_outline(
                &mut frame,
                &joined(&outline),
                OUTLINE,
                theme::PHOTO_ANCHOR,
                false,
            );
            if let Some(source) = at_source(live) {
                let dashed = brush_overlay::outline(&self.placement, &source);
                stroke_outline(
                    &mut frame,
                    &joined(&dashed),
                    OUTLINE,
                    theme::PHOTO_ANCHOR,
                    true,
                );
                if let Some(at) = live.source
                    && let Some(centre) = self.placement.canvas_point(at[0], at[1])
                {
                    Self::crosshair(&mut frame, centre, CROSSHAIR_ARM, theme::PHOTO_ANCHOR);
                }
            }
        }
        if shown {
            for (listed, held) in self.drawn() {
                let selected = self.data.selected == Some(listed.operation.as_str());
                let hovered = self.data.hovered == Some(listed.operation.as_str());
                if selected || hovered {
                    self.figure(&mut frame, &held, false);
                }
            }
            for (listed, held) in self.drawn() {
                if let Some(at) = brush_overlay::pin(&self.placement, &held) {
                    Self::pin(
                        &mut frame,
                        at,
                        self.data.selected == Some(listed.operation.as_str()),
                    );
                }
            }
            for (handle, at) in self.handles() {
                Self::handle(&mut frame, handle, at);
            }
            // The hovered pin's tooltip: what it is and what its handles do.
            if !state.pressed
                && let Some((listed, held)) = self
                    .drawn()
                    .find(|(listed, _)| self.data.hovered == Some(listed.operation.as_str()))
                && let Some(at) = brush_overlay::pin(&self.placement, &held)
            {
                Self::tooltip(
                    &mut frame,
                    Point::new(at.0, at.1),
                    &format!("{} \u{b7} drag to move", listed.name),
                    "Drag the dashed source to resample \u{b7} / new source \u{b7} \u{232b} delete",
                    bounds.size(),
                );
            }
        }
        // The brush cursor, over the photograph and away from a pin or handle, while nothing is
        // held down on one.
        if let Some(point) = cursor.position_in(bounds)
            && let Some((x, y)) = self.placement.content_point(point)
            && (!shown
                || state.pressed
                || matches!(self.hit_at(point, state.modifiers), CanvasHit::Add))
            && (!state.pressed || self.data.painting)
        {
            self.cursor(&mut frame, [x, y], point, state.modifiers);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        let Some(point) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        if self.data.waiting {
            return mouse::Interaction::Progress;
        }
        if self.placement.content_point(point).is_none() {
            return mouse::Interaction::NotAllowed;
        }
        match self.hit_at(point, state.modifiers) {
            CanvasHit::Handle(Handle::Size) => mouse::Interaction::ResizingHorizontally,
            CanvasHit::Handle(_) if state.pressed => mouse::Interaction::Grabbing,
            CanvasHit::Handle(_) => mouse::Interaction::Grab,
            CanvasHit::Pin(_) => mouse::Interaction::Pointer,
            // The brush draws its own cursor; a refused press says so with the system's.
            CanvasHit::Add if self.data.refused && !state.pressed => mouse::Interaction::NotAllowed,
            CanvasHit::Add => mouse::Interaction::Hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::canvas_view::CanvasView;
    use canvas::Program;

    fn listed(operation: &str, points: Vec<[f64; 2]>, source: [f64; 2]) -> Listed {
        serde_json::from_value(serde_json::json!({
            "operation": operation,
            "name": "Heal 1",
            "kind": "heal",
            "index": 0,
            "points": points,
            "radius": 0.05,
            "feather": 50,
            "opacity": 100,
            "source-x": source[0],
            "source-y": source[1],
            "source-origin": "auto",
        }))
        .unwrap()
    }

    fn placement() -> Placement {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        Placement {
            map: ContentMap::from_affine((480, 320), (480, 320), identity, identity).unwrap(),
            view: CanvasView::fit((480.0, 320.0), Size::new(480.0, 320.0)).unwrap(),
            scale_factor: 1.0,
        }
    }

    fn data<'a>(
        map: &'a ContentMap,
        operations: &'a [Listed],
        selected: Option<&'a str>,
    ) -> CanvasData<'a> {
        CanvasData {
            map,
            operations,
            selected,
            hovered: None,
            hidden: false,
            radius: 0.05,
            feather: 50.0,
            source: SourceChoice::Auto,
            gesture: None,
            painting: false,
            automatic: false,
            refused: false,
            waiting: false,
        }
    }

    /// A press on a pin selects it, a press on the selected correction's handles grabs them, and a
    /// press anywhere else — inside an earlier correction included — adds. `⌥` and `⌘` presses
    /// bypass the pins, and with pins hidden every press adds.
    #[test]
    fn the_hit_order_is_handles_then_pins_then_add() {
        let placement = placement();
        let operations = vec![
            listed("correction-1", vec![[0.3, 0.5]], [0.3, 0.2]),
            listed("correction-2", vec![[0.6, 0.5], [0.7, 0.5]], [0.6, 0.2]),
        ];
        let map = placement.map.clone();
        let canvas = CorrectionsCanvas::new(
            data(&map, &operations, Some("correction-2")),
            placement.clone(),
        );
        let pin_1 = placement.canvas_point(0.3, 0.5).unwrap();
        let pin_2 = placement.canvas_point(0.6, 0.5).unwrap();
        let source_2 = placement.canvas_point(0.6, 0.2).unwrap();
        let none = Modifiers::default();
        assert_eq!(
            canvas.hit_at(pin_1, none),
            CanvasHit::Pin("correction-1".into())
        );
        assert_eq!(canvas.hit_at(pin_2, none), CanvasHit::Handle(Handle::Pin));
        assert_eq!(
            canvas.hit_at(source_2, none),
            CanvasHit::Handle(Handle::Source)
        );
        // Inside correction 1's footprint but away from its pin: adds.
        let inside = placement.canvas_point(0.3 + 0.03, 0.5).unwrap();
        assert_eq!(canvas.hit_at(inside, none), CanvasHit::Add);
        let alt = Modifiers { alt: true, ..none };
        assert_eq!(canvas.hit_at(pin_1, alt), CanvasHit::Add);
        let hidden = CorrectionsCanvas::new(
            CanvasData {
                hidden: true,
                ..data(&map, &operations, Some("correction-2"))
            },
            placement.clone(),
        );
        assert_eq!(hidden.hit_at(pin_2, none), CanvasHit::Add);
    }

    /// A press publishes the press with what it caught, moves publish drags, and the release comes
    /// back even when the pointer left the canvas.
    #[test]
    fn a_press_drag_and_release_publish_one_message_each() {
        let placement = placement();
        let operations: Vec<Listed> = Vec::new();
        let map = placement.map.clone();
        let canvas = CorrectionsCanvas::new(data(&map, &operations, None), placement.clone());
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(480.0, 320.0));
        let mut state = Interaction::default();
        let at = Point::new(100.0, 100.0);
        let press = canvas.update(
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds,
            Cursor::Available(at),
        );
        assert!(press.is_some());
        assert!(state.pressed);
        let to = Point::new(140.0, 100.0);
        assert!(
            canvas
                .update(
                    &mut state,
                    &Event::Mouse(mouse::Event::CursorMoved { position: to }),
                    bounds,
                    Cursor::Available(to),
                )
                .is_some()
        );
        let outside = Point::new(900.0, 900.0);
        assert!(
            canvas
                .update(
                    &mut state,
                    &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    bounds,
                    Cursor::Available(outside),
                )
                .is_some()
        );
        assert!(!state.pressed);
    }
}
