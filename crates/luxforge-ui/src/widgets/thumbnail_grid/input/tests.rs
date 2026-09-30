use super::*;
use crate::widgets::thumbnail_grid::paint::TextStyle;
use crate::{CellView, GridLayout, GridMetrics, MomentHeader, MomentKind, thumbnail_grid};
use iced::window;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Message {
    Press(GridPress),
    Scroll(f32),
    Action(u32),
    Viewport(Size),
}

/// Half the text's size per character, as the drawing tests measure.
struct Half;

impl Measure for Half {
    fn measure(&mut self, content: &str, style: TextStyle) -> f32 {
        content.chars().count() as f32 * style.size * 0.5
    }
}

/// Where the widget sits in the window.
const BOUNDS: Rectangle = Rectangle {
    x: 100.0,
    y: 50.0,
    width: 594.0,
    height: 300.0,
};

/// Four cells a line: a bracket with Pick all 3 (its cells 0–2) on a line of its own, then 41
/// singles in eleven lines.
fn layout() -> GridLayout {
    GridLayout::new(
        vec![
            GridBlock::Moment {
                header: MomentHeader {
                    kind: MomentKind::Bracket,
                    title: "Bracket".into(),
                    detail: "".into(),
                    evidence: None,
                    picked: None,
                    action: Some("Pick all 3".into()),
                },
                frames: 3,
                collapsed: false,
            },
            GridBlock::Singles(41),
        ],
        GridMetrics::select(136.0),
        BOUNDS.width,
    )
}

fn grid(layout: &GridLayout, scroll: f32) -> ThumbnailGrid<'_, Message> {
    thumbnail_grid(layout, scroll, |_| CellView::default())
        .on_press(Message::Press)
        .on_scroll(Message::Scroll)
        .on_moment_action(Message::Action)
        .viewport(BOUNDS.size())
        .on_viewport(Message::Viewport)
}

/// Sends `event` with the pointer at `at` (widget coordinates).
fn send(
    grid: &ThumbnailGrid<'_, Message>,
    input: &mut Input,
    event: Event,
    at: Point,
) -> (Vec<Message>, Response) {
    let mut messages = Vec::new();
    let cursor = mouse::Cursor::Available(Point::new(BOUNDS.x + at.x, BOUNDS.y + at.y));
    let response = grid.respond(input, &mut Half, &event, BOUNDS, cursor, &mut |message| {
        messages.push(message)
    });
    (messages, response)
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

fn moved(at: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(BOUNDS.x + at.x, BOUNDS.y + at.y),
    })
}

fn wheel(delta: mouse::ScrollDelta) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled { delta })
}

/// The centre of `item`'s cell in widget coordinates at `scroll`.
fn over(layout: &GridLayout, item: u32, scroll: f32) -> Point {
    let rect = layout.item_rect(item).unwrap();
    Point::new(rect.center_x(), rect.center_y() - scroll)
}

#[test]
fn a_press_on_a_cell_publishes_its_item_modifiers_and_double_click() {
    let layout = layout();
    let grid = grid(&layout, 0.0);
    let mut input = Input::default();
    let (messages, response) = send(&grid, &mut input, press(), over(&layout, 3, 0.0));
    assert_eq!(
        messages,
        [Message::Press(GridPress {
            cell: 3,
            item: 3,
            span: 1,
            modifiers: PressModifiers::default(),
            double: false,
        })]
    );
    assert!(response.capture);
    // Again on the same cell at once: a double click.
    let (messages, _) = send(&grid, &mut input, press(), over(&layout, 3, 0.0));
    assert!(matches!(
        messages[..],
        [Message::Press(GridPress { double: true, .. })]
    ));
    // Then another cell, with Shift and Command held: a single press with both.
    let modifiers = keyboard::Modifiers::SHIFT | keyboard::Modifiers::COMMAND;
    let _ = send(
        &grid,
        &mut input,
        Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)),
        Point::ORIGIN,
    );
    let (messages, _) = send(&grid, &mut input, press(), over(&layout, 5, 0.0));
    assert_eq!(
        messages,
        [Message::Press(GridPress {
            cell: 5,
            item: 5,
            span: 1,
            modifiers: PressModifiers {
                shift: true,
                command: true
            },
            double: false,
        })]
    );
    // A gap between cells and a press outside the widget publish nothing.
    let (messages, response) = send(&grid, &mut input, press(), Point::new(8.0, 100.0));
    assert!(messages.is_empty() && !response.capture);
    let (messages, _) = send(&grid, &mut input, press(), Point::new(-20.0, 100.0));
    assert!(messages.is_empty());
}

#[test]
fn the_action_button_publishes_its_moment_and_the_rest_of_the_header_nothing() {
    let layout = layout();
    let grid = grid(&layout, 0.0);
    let mut input = Input::default();
    // The frame starts at (16, 10); its header strip is 28 pt tall. The button's right edge is
    // 10 pt inside the frame's: "Pick all 3" at 5.5 pt a character, 8 pt each side, 71 pt wide.
    let frame = layout.frames()[0].rect;
    let right = frame.x + frame.width - 10.0;
    let (messages, response) = send(
        &grid,
        &mut input,
        press(),
        Point::new(right - 30.0, frame.y + 14.0),
    );
    assert_eq!(messages, [Message::Action(0)]);
    assert!(response.capture);
    let (messages, _) = send(
        &grid,
        &mut input,
        press(),
        Point::new(frame.x + 40.0, frame.y + 14.0),
    );
    assert!(messages.is_empty(), "the title is not a button");
    // The pointer over the button hovers it; moving off redraws once.
    let (_, response) = send(
        &grid,
        &mut input,
        moved(Point::new(right - 30.0, frame.y + 14.0)),
        Point::new(right - 30.0, frame.y + 14.0),
    );
    assert_eq!(input.hover, Hover::Action(0));
    assert!(response.redraw);
    let (_, response) = send(
        &grid,
        &mut input,
        moved(Point::new(200.0, 200.0)),
        Point::new(200.0, 200.0),
    );
    assert_eq!(input.hover, Hover::None);
    assert!(response.redraw);
    let (_, response) = send(
        &grid,
        &mut input,
        moved(Point::new(210.0, 200.0)),
        Point::new(210.0, 200.0),
    );
    assert!(!response.redraw, "nothing changed");
}

#[test]
fn the_wheel_scrolls_by_lines_and_pixels_and_adds_up_within_a_frame() {
    let layout = layout();
    let mut input = Input::default();
    let grid = grid(&layout, 0.0);
    let at = Point::new(200.0, 100.0);
    // One line down is one row of cells and its gap.
    let (messages, response) = send(
        &grid,
        &mut input,
        wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }),
        at,
    );
    assert_eq!(messages, [Message::Scroll(128.0)]);
    assert!(response.capture);
    // A second delta before the caller has applied the first continues from it.
    let (messages, _) = send(
        &grid,
        &mut input,
        wheel(mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 }),
        at,
    );
    assert_eq!(messages, [Message::Scroll(158.0)]);
    // Once the caller passes its offset, that is where scrolling continues from.
    let grid = self::grid(&layout, 20.0);
    let (messages, _) = send(
        &grid,
        &mut input,
        wheel(mouse::ScrollDelta::Pixels { x: 0.0, y: 5.0 }),
        at,
    );
    assert_eq!(messages, [Message::Scroll(15.0)]);
    // At the top, scrolling up publishes nothing and leaves the event to others.
    let grid = self::grid(&layout, 0.0);
    let mut input = Input::default();
    let (messages, response) = send(
        &grid,
        &mut input,
        wheel(mouse::ScrollDelta::Lines { x: 0.0, y: 3.0 }),
        at,
    );
    assert!(messages.is_empty() && !response.capture);
    // Far down, the offset is clamped to the content's end.
    let (messages, _) = send(
        &grid,
        &mut input,
        wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -100.0 }),
        at,
    );
    assert_eq!(messages, [Message::Scroll(layout.height() - BOUNDS.height)]);
    // Outside the widget the wheel is not the grid's.
    let (messages, _) = send(
        &grid,
        &mut Input::default(),
        wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }),
        Point::new(-5.0, 100.0),
    );
    assert!(messages.is_empty());
}

#[test]
fn the_scrollbar_pages_from_its_track_and_follows_a_drag() {
    let layout = layout();
    let grid = grid(&layout, 0.0);
    let mut input = Input::default();
    let bar = paint::scrollbar(layout.height(), 0.0, BOUNDS.size()).unwrap();
    let x = BOUNDS.width - 3.0;
    // Below the scroller: a page down.
    let (messages, response) = send(&grid, &mut input, press(), Point::new(x, 250.0));
    assert_eq!(messages, [Message::Scroll(BOUNDS.height)]);
    assert!(response.capture);
    // On the scroller: a drag, which follows the pointer and ends on release.
    let grab = bar.scroller.y + 5.0;
    let grid = self::grid(&layout, 0.0);
    let mut input = Input::default();
    let (messages, response) = send(&grid, &mut input, press(), Point::new(x, grab));
    assert!(messages.is_empty());
    assert!(response.capture && response.redraw);
    assert_eq!(input.drag, Some(5.0));
    let (messages, _) = send(
        &grid,
        &mut input,
        moved(Point::new(x, grab + bar.travel / 2.0)),
        Point::new(x, grab + bar.travel / 2.0),
    );
    let [Message::Scroll(scroll)] = messages[..] else {
        panic!("{messages:?}");
    };
    assert!((scroll - bar.range / 2.0).abs() < 0.01, "{scroll}");
    // Past the widget, the drag still follows, clamped.
    let (messages, _) = send(
        &grid,
        &mut input,
        moved(Point::new(x + 300.0, 2000.0)),
        Point::new(x + 300.0, 2000.0),
    );
    assert_eq!(messages, [Message::Scroll(bar.range)]);
    let (_, response) = send(&grid, &mut input, release(), Point::new(x, 0.0));
    assert!(response.capture);
    assert_eq!(input.drag, None);
}

#[test]
fn a_new_size_is_published_once_on_any_event() {
    let layout = layout();
    let redraw = Event::Window(window::Event::RedrawRequested(std::time::Instant::now()));
    // The caller knows the size: nothing.
    let mut input = Input::default();
    let (messages, _) = send(
        &grid(&layout, 0.0),
        &mut input,
        redraw.clone(),
        Point::ORIGIN,
    );
    assert!(messages.is_empty());
    // The caller laid out for another size: the real one, once.
    let stale = grid(&layout, 0.0).viewport(Size::new(400.0, 300.0));
    let (messages, _) = send(&stale, &mut input, redraw.clone(), Point::ORIGIN);
    assert_eq!(messages, [Message::Viewport(BOUNDS.size())]);
    let (messages, _) = send(&stale, &mut input, redraw.clone(), Point::ORIGIN);
    assert!(messages.is_empty());
    // A press still does its own work alongside.
    let (messages, _) = send(&stale, &mut input, press(), over(&layout, 3, 0.0));
    assert!(matches!(messages[..], [Message::Press(_)]));
}

#[test]
fn without_callbacks_nothing_is_published_or_captured() {
    let layout = layout();
    let quiet = thumbnail_grid::<Message>(&layout, 0.0, |_| CellView::default());
    let mut input = Input::default();
    for (event, at) in [
        (press(), over(&layout, 3, 0.0)),
        (
            wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }),
            Point::new(200.0, 100.0),
        ),
    ] {
        let (messages, response) = send(&quiet, &mut input, event, at);
        assert!(messages.is_empty());
        assert!(!response.capture);
    }
}
