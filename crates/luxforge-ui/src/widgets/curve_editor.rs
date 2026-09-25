//! Point curve plot. The host supplies sampled geometry and owns all point validation.

use crate::{
    Icon, IconButtonModel, SegmentedModel, ValueEdit, icon_button, segmented, theme, value_input,
};
use iced::{
    Alignment, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    advanced::{
        self, Clipboard, Shell, Widget, layout, renderer,
        widget::{Operation, Tree, operation, tree},
    },
    keyboard::{self, Key, key::Named},
    mouse::{self, Cursor},
    widget::{
        button,
        canvas::{self, Action, Event, Path, Stroke},
        column, row, text,
    },
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

pub const POINT_HIT_RADIUS: f32 = 9.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

/// Keep the cache across unrelated redraws; Iced's cache handles size changes itself.
pub(crate) fn invalidate_on_version_change<K: Copy + PartialEq>(
    current: &Cell<Option<K>>,
    next: K,
    clear: impl FnOnce(),
) {
    if current.get() != Some(next) {
        clear();
        current.set(Some(next));
    }
}

/// The point nearest `pointer` in plot pixels, within `radius` inclusive.
pub fn hit_test(points: &[[f32; 2]], pointer: Point, size: Size, radius: f32) -> Option<usize> {
    let mut best = None;
    let mut best_distance = radius.max(0.0).powi(2);
    for (index, [x, y]) in points.iter().copied().enumerate() {
        let dx = pointer.x - x * size.width;
        let dy = pointer.y - (1.0 - y) * size.height;
        let distance = dx * dx + dy * dy;
        if distance <= best_distance {
            best = Some(index);
            best_distance = distance;
        }
    }
    best
}

pub fn point_fraction(pointer: Point, bounds: Rectangle) -> [f32; 2] {
    [
        ((pointer.x - bounds.x) / bounds.width).clamp(0.0, 1.0),
        (1.0 - (pointer.y - bounds.y) / bounds.height).clamp(0.0, 1.0),
    ]
}

/// Fraction snapping is a host decision; this helper gives consistent 0..1 rounding for fields.
pub fn round_fraction(value: f32, decimals: u32) -> f32 {
    let scale = 10_f32.powi(decimals.min(6) as i32);
    (value.clamp(0.0, 1.0) * scale).round() / scale
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurvePointRow {
    pub display: [String; 2],
    pub edit: [ValueEdit; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurveEditorModel {
    pub points: Vec<[f32; 2]>,
    /// Interpolated samples are supplied by the host; this widget never computes a spline.
    pub sampled: Vec<[f32; 2]>,
    pub point_rows: Vec<CurvePointRow>,
    pub selected: Option<usize>,
    pub background: Option<[f32; 256]>,
    pub identity: bool,
    pub channels: Vec<String>,
    pub selected_channel: usize,
    pub dragging: bool,
    pub enabled: bool,
    /// Change when the plot's points, samples, background, identity or selection changes.
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CurveEditorEvent {
    Move {
        index: usize,
        position: [f32; 2],
    },
    Select(usize),
    Add([f32; 2]),
    Remove(usize),
    Release,
    Cancel,
    Channel(usize),
    Text {
        index: usize,
        axis: usize,
        text: String,
    },
    Submit {
        index: usize,
        axis: usize,
    },
    /// A keyboard direction. The host applies its declared step and modifiers.
    Nudge {
        index: usize,
        dx: i8,
        dy: i8,
        shift: bool,
        option: bool,
    },
}

pub fn curve_editor<'a, M: Clone + 'a>(
    model: &CurveEditorModel,
    on_event: impl Fn(CurveEditorEvent) -> M + 'a,
) -> Element<'a, M> {
    let on_event: Rc<dyn Fn(CurveEditorEvent) -> M + 'a> = Rc::new(on_event);
    let mut body = column![].spacing(5.0);
    if model.channels.len() > 1 {
        let callback = on_event.clone();
        body = body.push(segmented(
            &SegmentedModel {
                options: model.channels.clone(),
                selected: model.selected_channel,
                enabled: model.enabled,
            },
            move |index| callback(CurveEditorEvent::Channel(index)),
        ));
    }
    body = body.push(FocusableCurveCanvas {
        model: model.clone(),
        on_event: on_event.clone(),
    });
    for (index, fields) in model.point_rows.iter().enumerate() {
        let select_callback = on_event.clone();
        let mut point_row = row![
            button(text(format!("{}", index + 1)).size(theme::SIZE_CAPTION))
                .style(if model.selected == Some(index) {
                    theme::button_selected
                } else {
                    theme::button_plain
                })
                .on_press_maybe(
                    model
                        .enabled
                        .then_some(select_callback(CurveEditorEvent::Select(index)))
                )
        ]
        .spacing(4.0)
        .align_y(Alignment::Center);
        for axis in 0..2 {
            let (value, invalid) = match &fields.edit[axis] {
                ValueEdit::Display => (fields.display[axis].clone(), false),
                ValueEdit::Editing { text, invalid } => (text.clone(), invalid.is_some()),
            };
            let text_callback = on_event.clone();
            let submit_callback = on_event.clone();
            point_row = point_row
                .push(text(if axis == 0 { "X" } else { "Y" }).size(theme::SIZE_CAPTION))
                .push(
                    value_input(
                        "",
                        &value,
                        invalid,
                        model.enabled,
                        move |text| text_callback(CurveEditorEvent::Text { index, axis, text }),
                        submit_callback(CurveEditorEvent::Submit { index, axis }),
                    )
                    .width(Length::FillPortion(1)),
                );
        }
        let remove_callback = on_event.clone();
        point_row = point_row.push(icon_button(
            &IconButtonModel {
                icon: Icon::Minus,
                tooltip: format!("Remove point {}", index + 1),
                enabled: model.enabled,
                selected: false,
            },
            model
                .enabled
                .then_some(remove_callback(CurveEditorEvent::Remove(index))),
        ));
        body = body.push(point_row);
    }
    body.into()
}

struct FocusableCurveCanvas<'a, M> {
    model: CurveEditorModel,
    on_event: Rc<dyn Fn(CurveEditorEvent) -> M + 'a>,
}

#[derive(Default)]
struct CurveState {
    cache: canvas::Cache,
    version: Cell<Option<(u64, bool)>>,
    active: Option<usize>,
    last_click: Option<(Instant, Point)>,
    focused: bool,
    nudge_active: bool,
}

impl operation::Focusable for CurveState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

impl<M: Clone> canvas::Program<M> for FocusableCurveCanvas<'_, M> {
    type State = CurveState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<M>> {
        if !self.model.enabled {
            state.active = None;
            state.nudge_active = false;
            state.focused = false;
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(point) = cursor.position_in(bounds) else {
                    state.focused = false;
                    return None;
                };
                state.focused = true;
                let hit = hit_test(&self.model.points, point, bounds.size(), POINT_HIT_RADIUS);
                let double = state.last_click.is_some_and(|(when, previous)| {
                    when.elapsed() <= DOUBLE_CLICK
                        && (point.x - previous.x).hypot(point.y - previous.y) <= POINT_HIT_RADIUS
                });
                state.last_click = Some((Instant::now(), point));
                if let Some(index) = hit {
                    state.active = Some(index);
                    Some(
                        Action::publish((self.on_event)(CurveEditorEvent::Select(index)))
                            .and_capture(),
                    )
                } else if double {
                    state.active = None;
                    Some(
                        Action::publish((self.on_event)(CurveEditorEvent::Add(point_fraction(
                            point,
                            Rectangle::new(Point::ORIGIN, bounds.size()),
                        ))))
                        .and_capture(),
                    )
                } else {
                    None
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let index = state.active?;
                let point = cursor.position()?;
                Some(
                    Action::publish((self.on_event)(CurveEditorEvent::Move {
                        index,
                        position: point_fraction(point, bounds),
                    }))
                    .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.active.take()?;
                Some(Action::publish((self.on_event)(CurveEditorEvent::Release)).and_capture())
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if state.focused =>
            {
                if matches!(key, Key::Named(Named::Tab)) {
                    if state.nudge_active {
                        state.nudge_active = false;
                        return Some(Action::publish((self.on_event)(CurveEditorEvent::Release)));
                    }
                    return None;
                }
                if matches!(key, Key::Named(Named::Escape)) {
                    state.active = None;
                    state.nudge_active = false;
                    return Some(
                        Action::publish((self.on_event)(CurveEditorEvent::Cancel)).and_capture(),
                    );
                }
                let index = self.model.selected?;
                let event = match key {
                    Key::Named(Named::Delete | Named::Backspace) => CurveEditorEvent::Remove(index),
                    Key::Named(
                        named @ (Named::ArrowLeft
                        | Named::ArrowRight
                        | Named::ArrowUp
                        | Named::ArrowDown),
                    ) => {
                        let (dx, dy) = match named {
                            Named::ArrowLeft => (-1, 0),
                            Named::ArrowRight => (1, 0),
                            Named::ArrowUp => (0, 1),
                            _ => (0, -1),
                        };
                        state.nudge_active = true;
                        CurveEditorEvent::Nudge {
                            index,
                            dx,
                            dy,
                            shift: modifiers.shift(),
                            option: modifiers.alt(),
                        }
                    }
                    _ => return None,
                };
                Some(Action::publish((self.on_event)(event)).and_capture())
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. })
                if state.focused && state.nudge_active =>
            {
                if matches!(
                    key,
                    Key::Named(
                        Named::ArrowLeft | Named::ArrowRight | Named::ArrowUp | Named::ArrowDown
                    )
                ) {
                    state.nudge_active = false;
                    Some(Action::publish((self.on_event)(CurveEditorEvent::Release)).and_capture())
                } else {
                    None
                }
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
        _cursor: Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        invalidate_on_version_change(&state.version, (self.model.version, state.focused), || {
            state.cache.clear()
        });
        let model = &self.model;
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            let size = frame.size();
            frame.fill_rectangle(Point::ORIGIN, size, theme::CONTROL);
            if state.focused {
                frame.stroke_rectangle(
                    Point::ORIGIN,
                    size,
                    Stroke::default().with_color(theme::ACCENT).with_width(1.0),
                );
            }
            if let Some(background) = &model.background {
                let mut path = canvas::path::Builder::new();
                path.move_to(Point::new(0.0, size.height));
                for (index, value) in background.iter().enumerate() {
                    let value = if value.is_finite() {
                        value.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    path.line_to(Point::new(
                        index as f32 / 255.0 * size.width,
                        (1.0 - value) * size.height,
                    ));
                }
                path.line_to(Point::new(size.width, size.height));
                path.close();
                frame.fill(
                    &path.build(),
                    iced::Color {
                        a: 0.16,
                        ..theme::TEXT_SECONDARY
                    },
                );
            }
            for n in 1..4 {
                let f = n as f32 / 4.0;
                let grid = iced::Color {
                    a: 0.2,
                    ..theme::TEXT_TERTIARY
                };
                frame.stroke(
                    &Path::line(
                        Point::new(f * size.width, 0.0),
                        Point::new(f * size.width, size.height),
                    ),
                    Stroke::default().with_color(grid).with_width(1.0),
                );
                frame.stroke(
                    &Path::line(
                        Point::new(0.0, f * size.height),
                        Point::new(size.width, f * size.height),
                    ),
                    Stroke::default().with_color(grid).with_width(1.0),
                );
            }
            if model.identity {
                frame.stroke(
                    &Path::line(Point::new(0.0, size.height), Point::new(size.width, 0.0)),
                    Stroke::default()
                        .with_color(theme::TEXT_TERTIARY)
                        .with_width(1.0),
                );
            }
            if let Some(first) = model.sampled.first() {
                let mut path = canvas::path::Builder::new();
                path.move_to(plot_point(*first, size));
                for point in &model.sampled[1..] {
                    path.line_to(plot_point(*point, size));
                }
                frame.stroke(
                    &path.build(),
                    Stroke::default()
                        .with_color(theme::TEXT_PRIMARY)
                        .with_width(2.0),
                );
            }
            for (index, point) in model.points.iter().enumerate() {
                frame.fill(
                    &Path::circle(
                        plot_point(*point, size),
                        if model.selected == Some(index) {
                            5.0
                        } else {
                            3.5
                        },
                    ),
                    if model.selected == Some(index) && model.dragging {
                        theme::ACCENT
                    } else {
                        theme::TEXT_PRIMARY
                    },
                );
            }
        })]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.model.enabled && cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

impl<M: Clone> Widget<M, Theme, Renderer> for FocusableCurveCanvas<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<CurveState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(CurveState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(200.0), Length::Fixed(200.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fixed(200.0), Length::Fixed(200.0))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: advanced::Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<CurveState>();
        if self.model.enabled {
            operation.focusable(None, layout.bounds(), state);
        } else {
            state.focused = false;
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: advanced::Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<CurveState>();
        if let Some(action) = canvas::Program::update(self, state, event, layout.bounds(), cursor) {
            let (message, redraw, status) = action.into_inner();
            shell.request_redraw_at(redraw);
            if let Some(message) = message {
                shell.publish(message);
            }
            if status == iced::event::Status::Captured {
                shell.capture_event();
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: advanced::Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        use iced::advanced::graphics::geometry::Renderer as _;
        let bounds = layout.bounds();
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let state = tree.state.downcast_ref::<CurveState>();
        renderer.with_translation(iced::Vector::new(bounds.x, bounds.y), |renderer| {
            for geometry in canvas::Program::draw(self, state, renderer, theme, bounds, cursor) {
                renderer.draw_geometry(geometry);
            }
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: advanced::Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        canvas::Program::mouse_interaction(
            self,
            tree.state.downcast_ref::<CurveState>(),
            layout.bounds(),
            cursor,
        )
    }
}

impl<'a, M: Clone + 'a> From<FocusableCurveCanvas<'a, M>> for Element<'a, M> {
    fn from(widget: FocusableCurveCanvas<'a, M>) -> Self {
        Element::new(widget)
    }
}

fn plot_point(point: [f32; 2], size: Size) -> Point {
    Point::new(
        point[0].clamp(0.0, 1.0) * size.width,
        (1.0 - point[1].clamp(0.0, 1.0)) * size.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::canvas::Program;

    fn model() -> CurveEditorModel {
        CurveEditorModel {
            points: vec![[0.2, 0.2]],
            sampled: vec![[0.0, 0.0], [1.0, 1.0]],
            point_rows: vec![],
            selected: Some(0),
            background: None,
            identity: true,
            channels: vec!["RGB".into()],
            selected_channel: 0,
            dragging: false,
            enabled: true,
            version: 0,
        }
    }

    fn message(action: Option<Action<CurveEditorEvent>>) -> Option<CurveEditorEvent> {
        action.and_then(|action| action.into_inner().0)
    }

    #[test]
    fn hit_test_picks_nearest_point_inside_radius_and_none_outside() {
        let points = [[0.2, 0.2], [0.25, 0.25], [0.8, 0.8]];
        let size = Size::new(100.0, 100.0);
        assert_eq!(
            hit_test(&points, Point::new(24.0, 75.0), size, 9.0),
            Some(1)
        );
        assert_eq!(hit_test(&points, Point::new(50.0, 50.0), size, 9.0), None);
        assert_eq!(
            hit_test(&points, Point::new(20.0, 80.0), size, 0.0),
            Some(0)
        );
    }

    #[test]
    fn fractions_and_rounding_are_bounded() {
        let bounds = Rectangle::new(Point::new(10.0, 10.0), Size::new(100.0, 100.0));
        assert_eq!(point_fraction(Point::new(60.0, 60.0), bounds), [0.5, 0.5]);
        assert_eq!(point_fraction(Point::new(-10.0, 200.0), bounds), [0.0, 0.0]);
        assert_eq!(round_fraction(0.12346, 3), 0.123);
    }

    #[test]
    fn version_key_only_invalidates_when_model_changes() {
        let version = Cell::new(Some(7));
        let mut invalidations = 0;
        for next in [7, 7, 8, 8] {
            invalidate_on_version_change(&version, next, || invalidations += 1);
        }
        assert_eq!(invalidations, 1);
    }

    #[test]
    fn double_click_uses_local_coordinates_even_with_offset_bounds() {
        let canvas = FocusableCurveCanvas {
            model: model(),
            on_event: Rc::new(|event| event),
        };
        let mut state = CurveState::default();
        let bounds = Rectangle::new(Point::new(50.0, 70.0), Size::new(100.0, 100.0));
        let cursor = Cursor::Available(Point::new(110.0, 120.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(
            message(canvas.update(&mut state, &press, bounds, cursor)),
            None
        );
        assert_eq!(
            message(canvas.update(&mut state, &press, bounds, cursor)),
            Some(CurveEditorEvent::Add([0.6, 0.5]))
        );
    }

    #[test]
    fn arrow_nudge_releases_on_key_up_after_pointer_leaves_plot() {
        use keyboard::{
            Location, Modifiers,
            key::{Code, Physical},
        };
        let canvas = FocusableCurveCanvas {
            model: model(),
            on_event: Rc::new(|event| event),
        };
        let mut state = CurveState::default();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(100.0, 100.0));
        let cursor = Cursor::Available(Point::new(20.0, 80.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(
            message(canvas.update(&mut state, &press, bounds, cursor)),
            Some(CurveEditorEvent::Select(0))
        );
        let key = Key::Named(Named::ArrowRight);
        let physical_key = Physical::Code(Code::ArrowRight);
        let down = Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key,
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert_eq!(
            message(canvas.update(&mut state, &down, bounds, Cursor::Unavailable)),
            Some(CurveEditorEvent::Nudge {
                index: 0,
                dx: 1,
                dy: 0,
                shift: false,
                option: false
            })
        );
        let up = Event::Keyboard(keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: Location::Standard,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(
            message(canvas.update(&mut state, &up, bounds, Cursor::Unavailable)),
            Some(CurveEditorEvent::Release)
        );
        assert_eq!(
            message(canvas.update(&mut state, &up, bounds, Cursor::Unavailable)),
            None
        );
    }

    #[test]
    fn focus_operation_enables_keyboard_nudge_without_pointer_hover() {
        use keyboard::{
            Location, Modifiers,
            key::{Code, Physical},
        };
        let canvas = FocusableCurveCanvas {
            model: model(),
            on_event: Rc::new(|event| event),
        };
        let mut state = CurveState::default();
        operation::Focusable::focus(&mut state);
        assert!(operation::Focusable::is_focused(&state));
        let key = Key::Named(Named::ArrowUp);
        let down = Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Code(Code::ArrowUp),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert_eq!(
            message(canvas.update(
                &mut state,
                &down,
                Rectangle::new(Point::ORIGIN, Size::new(200.0, 200.0)),
                Cursor::Unavailable
            )),
            Some(CurveEditorEvent::Nudge {
                index: 0,
                dx: 0,
                dy: 1,
                shift: false,
                option: false
            })
        );
        operation::Focusable::unfocus(&mut state);
        assert!(!operation::Focusable::is_focused(&state));
    }
}
