//! A rail of fixed, labelled stops: the title bar's zoom stops.
//!
//! The stops are evenly spaced whatever values they stand for, one equal cell each with its notch
//! on the rail at the cell's centre and its label under it, so the whole cell, label included, is
//! the stop's target. A press takes the thumb to the stop under the pointer and a drag carries it
//! from stop to stop; the stop it rests on when released is published, once. Nothing is published
//! mid-drag, so passing over several stops on the way asks the host for one change, not one each.
//!
//! The widget owns no value. The host says where the thumb is, as a fractional stop index (between
//! two notches for a value between their stops, `None` for one outside them, which draws no
//! thumb), and which stop, if any, the value is exactly, whose label is set bright.
//!
//! A rail that the host lets answer the arrow keys ([`StepKeys`]) publishes a step of one stop up
//! or down for each press, repeats included; the host decides which stop that is from its own
//! value. It hears the keys wherever the focus is, so only a rail that is showing for a moment, as
//! the zoom stops' panel is, should be given them.

use crate::geometry;
use crate::theme;
use crate::{Element, Palette, Theme};
use iced::alignment::Vertical;
use iced::keyboard::{self, Key, Modifiers, key::Named};
use iced::widget::canvas::{self, Action, Event, Path, Stroke};
use iced::widget::container;
use iced::widget::text::{Alignment as TextAlignment, LineHeight};
use iced::{Color, Length, Point, Rectangle, Renderer, Size, mouse};
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq)]
pub struct NotchedSliderModel {
    /// Each stop's label, left to right: `50%`.
    pub labels: Vec<String>,
    /// Where the thumb sits as a fractional stop index, or `None` to draw no thumb.
    pub position: Option<f32>,
    /// The stop the value is exactly, whose label is set bright.
    pub selected: Option<usize>,
    /// Which arrow keys step the rail.
    pub keys: StepKeys,
    pub enabled: bool,
}

/// Which arrow keys step a notched rail, a stop up or down per press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StepKeys {
    /// None: the keys are left to whatever has the focus.
    #[default]
    Off,
    /// Up and Down only, leaving Left and Right to a text field's caret.
    Vertical,
    /// Up and Right step up; Down and Left step down.
    All,
}

/// A notched rail. `on_select` receives the stop a press or drag was released on, unless it is
/// already the selected one; `on_step` receives `1` or `-1` for each arrow key the rail answers.
pub fn notched_slider<'a, M: 'a>(
    model: &NotchedSliderModel,
    on_select: impl Fn(usize) -> M + 'a,
    on_step: impl Fn(i32) -> M + 'a,
) -> Element<'a, M> {
    let width = theme::NOTCH_CELL_WIDTH * model.labels.len() as f32;
    canvas::Canvas::new(NotchCanvas {
        model: model.clone(),
        on_select: Box::new(on_select),
        on_step: Box::new(on_step),
    })
    .width(Length::Fixed(width))
    .height(Length::Fixed(theme::NOTCH_HEIGHT))
    .into()
}

/// A notched rail on a dropped menu's surface, as the title bar's zoom stops drop under the zoom
/// control.
pub fn notched_panel<'a, M: 'a>(
    model: &NotchedSliderModel,
    on_select: impl Fn(usize) -> M + 'a,
    on_step: impl Fn(i32) -> M + 'a,
) -> Element<'a, M> {
    container(notched_slider(model, on_select, on_step))
        .padding(theme::NOTCH_PANEL_PADDING)
        .style(theme::menu_surface)
        .into()
}

// ---- geometry ----------------------------------------------------------------------------------

/// Where the rail puts fractional stop `position` among `count` equal cells across `width`: a
/// whole position at its cell's centre, anything between on the straight line between two.
pub fn stop_x(width: f32, count: usize, position: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let cell = width / count as f32;
    (position.clamp(0.0, (count - 1) as f32) + 0.5) * cell
}

/// The stop whose cell holds `x`, the nearer end's for a point off the rail.
pub fn stop_at(width: f32, count: usize, x: f32) -> usize {
    if count == 0 || width <= 0.0 {
        return 0;
    }
    let cell = width / count as f32;
    ((x / cell).floor().max(0.0) as usize).min(count - 1)
}

/// The step an arrow key makes on a rail answering `keys`: `1` up, `-1` down, or `None` for a key
/// it leaves alone. A key held with Command, Control or Option is a shortcut, never a step.
pub fn arrow_step(key: &Key, modifiers: Modifiers, keys: StepKeys) -> Option<i32> {
    if keys == StepKeys::Off || modifiers.command() || modifiers.control() || modifiers.alt() {
        return None;
    }
    match key {
        Key::Named(Named::ArrowUp) => Some(1),
        Key::Named(Named::ArrowDown) => Some(-1),
        Key::Named(Named::ArrowRight) if keys == StepKeys::All => Some(1),
        Key::Named(Named::ArrowLeft) if keys == StepKeys::All => Some(-1),
        _ => None,
    }
}

// ---- the canvas --------------------------------------------------------------------------------

struct NotchCanvas<'a, M> {
    model: NotchedSliderModel,
    on_select: Box<dyn Fn(usize) -> M + 'a>,
    on_step: Box<dyn Fn(i32) -> M + 'a>,
}

/// What the drawing depends on besides the model: the size, the stop under the pointer, the stop
/// a gesture holds and the theme's generation.
type DrawKey = (NotchedSliderModel, Size, Option<usize>, Option<usize>, u64);

#[derive(Default)]
struct NotchState {
    cache: canvas::Cache,
    key: RefCell<Option<DrawKey>>,
    /// The stop a press or drag holds the thumb on.
    held: Option<usize>,
    /// The stop under the pointer when it last moved, to redraw only when that changes.
    hovered: Option<usize>,
}

impl<M> NotchCanvas<'_, M> {
    /// Calls `clear` when anything the drawing depends on changed since the cache was drawn.
    fn refresh(
        &self,
        key: &RefCell<Option<DrawKey>>,
        size: Size,
        hovered: Option<usize>,
        held: Option<usize>,
        theme: &Theme,
        clear: impl FnOnce(),
    ) {
        let next = Some((self.model.clone(), size, hovered, held, theme.generation()));
        if *key.borrow() != next {
            clear();
            *key.borrow_mut() = next;
        }
    }
}

impl<M> canvas::Program<M, Theme> for NotchCanvas<'_, M> {
    type State = NotchState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<M>> {
        let count = self.model.labels.len();
        if !self.model.enabled || count == 0 {
            state.held = None;
            state.hovered = None;
            return None;
        }
        let under = |cursor: mouse::Cursor| {
            cursor
                .position_in(bounds)
                .map(|point| stop_at(bounds.width, count, point.x))
        };
        match event {
            // A gesture in progress has the rail; a key mid-drag would fight it.
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if state.held.is_none() =>
            {
                let step = arrow_step(key, *modifiers, self.model.keys)?;
                Some(Action::publish((self.on_step)(step)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.held = Some(under(cursor)?);
                Some(Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.held.is_some() {
                    // A drag past either end holds the end stop, as a slider's handle stays at its end.
                    let x = cursor.position()?.x - bounds.x;
                    let stop = stop_at(bounds.width, count, x);
                    let moved = state.held != Some(stop);
                    state.held = Some(stop);
                    return Some(if moved {
                        Action::request_redraw().and_capture()
                    } else {
                        Action::capture()
                    });
                }
                let hovered = under(cursor);
                (hovered != state.hovered).then(|| {
                    state.hovered = hovered;
                    Action::request_redraw()
                })
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let stop = state.held.take()?;
                Some(if Some(stop) == self.model.selected {
                    Action::request_redraw().and_capture()
                } else {
                    Action::publish((self.on_select)(stop)).and_capture()
                })
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let count = self.model.labels.len();
        if bounds.width <= 0.0 || bounds.height <= 0.0 || count == 0 {
            return Vec::new();
        }
        let hovered = if self.model.enabled && state.held.is_none() {
            cursor
                .position_in(bounds)
                .map(|point| stop_at(bounds.width, count, point.x))
        } else {
            None
        };
        self.refresh(
            &state.key,
            bounds.size(),
            hovered,
            state.held,
            theme,
            || state.cache.clear(),
        );
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            draw_notches(
                frame,
                &self.model,
                bounds.size(),
                hovered,
                state.held,
                theme.palette(),
            )
        })]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if !self.model.enabled {
            mouse::Interaction::None
        } else if state.held.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

fn rgb(color: Color) -> [f32; 3] {
    [color.r, color.g, color.b]
}

/// `colour` laid over the menu surface at `opacity`, mixed in sRGB and drawn opaque.
fn over(palette: &Palette, colour: Color, opacity: f32) -> Color {
    let [r, g, b] = geometry::over(rgb(colour), rgb(palette.menu_surface), opacity);
    Color::from_rgb(r, g, b)
}

fn draw_notches(
    frame: &mut canvas::Frame,
    model: &NotchedSliderModel,
    size: Size,
    hovered: Option<usize>,
    held: Option<usize>,
    palette: &Palette,
) {
    let count = model.labels.len();
    let width = size.width;
    let cell = width / count as f32;
    let middle = theme::NOTCH_RAIL_HEIGHT / 2.0;
    let x = |position: f32| stop_x(width, count, position);
    let ink = |color: Color| {
        if model.enabled {
            color
        } else {
            over(palette, color, 0.4)
        }
    };
    // A held thumb is drawn where the gesture holds it; otherwise where the host says it is.
    let thumb = held.map(|stop| stop as f32).or(model.position).map(x);
    let (first, last) = (x(0.0), x((count - 1) as f32));
    let band = |from: f32, to: f32| {
        (
            Point::new(from, middle - theme::RAIL_WIDTH / 2.0),
            Size::new((to - from).max(0.0), theme::RAIL_WIDTH),
        )
    };
    let (origin, rail) = band(first, last);
    frame.fill_rectangle(origin, rail, palette.rail);
    if let Some(thumb) = thumb {
        let (origin, filled) = band(first, thumb);
        frame.fill_rectangle(origin, filled, ink(palette.rail_fill));
    }
    for stop in 0..count {
        let centre = x(stop as f32);
        let lit = Some(stop) == hovered || Some(stop) == held;
        // The label's cell, lifted under the pointer as a menu item is.
        if Some(stop) == hovered {
            let top = theme::NOTCH_RAIL_HEIGHT + theme::NOTCH_LABEL_GAP - 2.0;
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(stop as f32 * cell + 1.0, top),
                    Size::new(cell - 2.0, theme::NOTCH_LABEL_HEIGHT + 4.0),
                    theme::MENU_ITEM_RADIUS.into(),
                ),
                palette.menu_item_hover,
            );
        }
        let tick = if lit {
            palette.text
        } else if thumb.is_some_and(|thumb| centre <= thumb + 0.5) {
            palette.rail_fill
        } else {
            palette.text_tertiary
        };
        frame.fill_rectangle(
            Point::new(
                centre - theme::NOTCH_TICK_WIDTH / 2.0,
                middle - theme::NOTCH_TICK_HEIGHT / 2.0,
            ),
            Size::new(theme::NOTCH_TICK_WIDTH, theme::NOTCH_TICK_HEIGHT),
            ink(tick),
        );
        let selected = Some(stop) == model.selected && held.is_none_or(|held| held == stop);
        let color = if selected {
            palette.text_bright
        } else if lit {
            palette.text
        } else {
            palette.text_secondary
        };
        frame.fill_text(canvas::Text {
            content: model.labels[stop].clone(),
            position: Point::new(
                centre,
                theme::NOTCH_RAIL_HEIGHT + theme::NOTCH_LABEL_GAP + theme::NOTCH_LABEL_HEIGHT / 2.0,
            ),
            color: ink(color),
            size: theme::SIZE_CAPTION.into(),
            line_height: LineHeight::Absolute(theme::NOTCH_LABEL_HEIGHT.into()),
            font: if selected {
                theme::FONT_SEMIBOLD
            } else {
                theme::FONT
            },
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
    }
    let Some(thumb) = thumb else { return };
    let centre = Point::new(thumb, middle);
    if held.is_some() {
        frame.fill(
            &Path::circle(centre, theme::THUMB_HALO_RADIUS),
            over(palette, palette.accent, theme::THUMB_HALO_OPACITY),
        );
        frame.fill(&Path::circle(centre, theme::THUMB_RADIUS), palette.accent);
    } else {
        let inner = theme::THUMB_RADIUS - theme::THUMB_OUTLINE_WIDTH;
        frame.fill(&Path::circle(centre, inner), ink(palette.thumb));
        frame.stroke(
            &Path::circle(centre, inner + theme::THUMB_OUTLINE_WIDTH / 2.0),
            Stroke::default()
                .with_color(palette.thumb_outline)
                .with_width(theme::THUMB_OUTLINE_WIDTH),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail is drawn again when the theme changes, and only then while nothing else moves.
    #[test]
    fn a_theme_change_redraws_the_notches() {
        let canvas = NotchCanvas {
            model: NotchedSliderModel {
                labels: vec!["50%".into(), "100%".into()],
                position: Some(1.0),
                selected: Some(1),
                keys: StepKeys::Off,
                enabled: true,
            },
            on_select: Box::new(|_| ()),
            on_step: Box::new(|_| ()),
        };
        let size = Size::new(76.0, theme::NOTCH_HEIGHT);
        let (first, second) = (Theme::luxforge_dark(), Theme::luxforge_dark());
        let key = RefCell::new(None);
        let mut clears = 0;
        canvas.refresh(&key, size, None, None, &first, || clears += 1);
        canvas.refresh(&key, size, None, None, &first, || clears += 1);
        assert_eq!(clears, 1, "an unchanged theme keeps the drawing");
        canvas.refresh(&key, size, None, None, &second, || clears += 1);
        assert_eq!(clears, 2, "a new generation redraws it");
    }

    /// Nine stops 36 pt apart: each notch at its cell's centre, a fraction between two on the line
    /// joining them, and a position off the rail at its end.
    #[test]
    fn stops_sit_at_their_cells_centres() {
        let width = 9.0 * 36.0;
        assert_eq!(stop_x(width, 9, 0.0), 18.0);
        assert_eq!(stop_x(width, 9, 8.0), width - 18.0);
        assert_eq!(stop_x(width, 9, 2.5), 18.0 + 2.5 * 36.0);
        assert_eq!(stop_x(width, 9, -1.0), 18.0);
        assert_eq!(stop_x(width, 9, 12.0), width - 18.0);
        assert_eq!(stop_x(width, 0, 1.0), 0.0);
    }

    /// The whole cell is its stop's target, and a point off the rail takes the nearer end.
    #[test]
    fn a_point_takes_the_stop_whose_cell_holds_it() {
        let width = 9.0 * 36.0;
        assert_eq!(stop_at(width, 9, 0.0), 0);
        assert_eq!(stop_at(width, 9, 35.9), 0);
        assert_eq!(stop_at(width, 9, 36.0), 1);
        assert_eq!(stop_at(width, 9, width - 0.1), 8);
        assert_eq!(stop_at(width, 9, -40.0), 0);
        assert_eq!(stop_at(width, 9, width + 40.0), 8);
        for stop in 0..9 {
            assert_eq!(stop_at(width, 9, stop_x(width, 9, stop as f32)), stop);
        }
        assert_eq!(stop_at(width, 0, 10.0), 0);
    }

    /// Up and Down step whenever the rail answers keys, Left and Right only when it answers all of
    /// them, and no key steps a rail that answers none or a key held as a shortcut.
    #[test]
    fn the_arrow_keys_step_the_rail_it_answers() {
        let key = |named| Key::Named(named);
        let none = Modifiers::empty();
        for keys in [StepKeys::Vertical, StepKeys::All] {
            assert_eq!(arrow_step(&key(Named::ArrowUp), none, keys), Some(1));
            assert_eq!(arrow_step(&key(Named::ArrowDown), none, keys), Some(-1));
            assert_eq!(
                arrow_step(&key(Named::ArrowUp), Modifiers::SHIFT, keys),
                Some(1)
            );
            for shortcut in [Modifiers::COMMAND, Modifiers::ALT, Modifiers::CTRL] {
                assert_eq!(arrow_step(&key(Named::ArrowUp), shortcut, keys), None);
            }
            assert_eq!(arrow_step(&key(Named::Enter), none, keys), None);
        }
        assert_eq!(
            arrow_step(&key(Named::ArrowRight), none, StepKeys::All),
            Some(1)
        );
        assert_eq!(
            arrow_step(&key(Named::ArrowLeft), none, StepKeys::All),
            Some(-1)
        );
        assert_eq!(
            arrow_step(&key(Named::ArrowRight), none, StepKeys::Vertical),
            None
        );
        assert_eq!(
            arrow_step(&key(Named::ArrowLeft), none, StepKeys::Vertical),
            None
        );
        assert_eq!(arrow_step(&key(Named::ArrowUp), none, StepKeys::Off), None);
    }

    #[test]
    fn a_notched_slider_builds_resting_between_stops_and_disabled() {
        let labels: Vec<String> = ["50%", "100%", "200%"].map(String::from).to_vec();
        for (position, selected, enabled) in [
            (Some(1.0), Some(1), true),
            (Some(0.4), None, true),
            (None, None, false),
        ] {
            let _: Element<'_, ()> = notched_slider(
                &NotchedSliderModel {
                    labels: labels.clone(),
                    position,
                    selected,
                    keys: StepKeys::All,
                    enabled,
                },
                |_| (),
                |_| (),
            );
        }
    }
}
