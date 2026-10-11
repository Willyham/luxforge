//! Point curve plot. The host supplies sampled geometry and owns all point validation.
//!
//! The widget classifies gestures and publishes them; it never applies a limit or edits a point.
//! A left press is classified by Iced's [`Click`], as `double_click.rs` does: a single click on a
//! point selects it and arms a drag, a single click away from every point asks to add one and arms
//! a drag on the point the host adds, a double-click on a point asks to remove it, and a third
//! click publishes nothing. The second press of a double-click whose first press asked to add
//! publishes nothing, so a double-click on empty plot adds one point and never removes the point it
//! just added. An armed drag publishes no move until the pointer has left [`DRAG_SLOP`] of the
//! press, so a click, or the first press of a double-click, never drafts a move. The drag an add
//! arms moves the host's selected point once the model holds one more point than at the press,
//! which is the host's answer to the add; an add the host refused leaves the count, so that drag
//! moves nothing. An add within [`CURVE_SNAP_RADIUS`] of the drawn curve lands on the nearest
//! host-supplied sample.
//!
//! The plot is square and fills the width it is given up to [`PLOT_MAX_SIDE`], centred in any
//! width beyond that. Under it: the host's hint, when it gives one, then a Points disclosure row
//! whose numeric point rows are drawn only while it is open.

use crate::{Element, Theme, Token};
use crate::{
    Icon, IconButtonModel, SegmentedModel, SubGroupHeaderModel, ValueEdit, icon_button, segmented,
    sub_group_header, theme, value_input,
};
use iced::{
    Alignment, Length, Point, Rectangle, Renderer, Size,
    advanced::{
        self, Clipboard, Shell, Widget, layout,
        mouse::{Click, click::Kind},
        renderer,
        widget::{Operation, Tree, operation, tree},
    },
    keyboard::{self, Key, key::Named},
    mouse::{self, Cursor},
    widget::{
        button,
        canvas::{self, Action, Event, Path, Stroke},
        column, container, row, text,
    },
};
use std::{cell::Cell, rc::Rc};

pub(crate) const POINT_HIT_RADIUS: f32 = 9.0;
/// How far, in plot pixels, the pointer must move from a press on a point before a drag begins.
const DRAG_SLOP: f32 = 3.0;
/// How close, in plot pixels, a click must be to the drawn curve for the add to land on the
/// nearest sample rather than at the pointer.
const CURVE_SNAP_RADIUS: f32 = 4.0;
/// The widest the square plot grows; a wider column centres it.
pub(crate) const PLOT_MAX_SIDE: f32 = 320.0;

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
pub(crate) fn hit_test(
    points: &[[f32; 2]],
    pointer: Point,
    size: Size,
    radius: f32,
) -> Option<usize> {
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

pub(crate) fn point_fraction(pointer: Point, bounds: Rectangle) -> [f32; 2] {
    [
        ((pointer.x - bounds.x) / bounds.width).clamp(0.0, 1.0),
        (1.0 - (pointer.y - bounds.y) / bounds.height).clamp(0.0, 1.0),
    ]
}

/// The sample of the drawn curve nearest `pointer`, exactly as the host supplied it, when the
/// pointer is within `radius` plot pixels of the polyline drawn through `sampled`.
fn snap_to_sampled(
    sampled: &[[f32; 2]],
    pointer: Point,
    size: Size,
    radius: f32,
) -> Option<[f32; 2]> {
    let first = plot_point(*sampled.first()?, size);
    let squared = |a: Point, b: Point| (a.x - b.x).powi(2) + (a.y - b.y).powi(2);
    let mut line = squared(pointer, first);
    let mut nearest = (line, sampled[0]);
    let mut previous = first;
    for sample in &sampled[1..] {
        let point = plot_point(*sample, size);
        let (dx, dy) = (point.x - previous.x, point.y - previous.y);
        let length = dx * dx + dy * dy;
        let t = if length > 0.0 {
            (((pointer.x - previous.x) * dx + (pointer.y - previous.y) * dy) / length)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        line = line.min(squared(
            pointer,
            Point::new(previous.x + t * dx, previous.y + t * dy),
        ));
        let distance = squared(pointer, point);
        if distance < nearest.0 {
            nearest = (distance, *sample);
        }
        previous = point;
    }
    (line <= radius.max(0.0).powi(2)).then_some(nearest.1)
}

/// What a left press over the plot publishes, from Iced's classification of it, the point it hit
/// and whether the press before it in the same run asked to add. `None` publishes nothing.
fn press_event(
    kind: Kind,
    hit: Option<usize>,
    after_add: bool,
    add: impl FnOnce() -> [f32; 2],
) -> Option<CurveEditorEvent> {
    match (kind, hit) {
        (Kind::Single, Some(index)) => Some(CurveEditorEvent::Select(index)),
        (Kind::Single, None) => Some(CurveEditorEvent::Add(add())),
        // The point under a double-click's second press may be the one its first press added.
        (Kind::Double, Some(index)) if !after_add => Some(CurveEditorEvent::Remove(index)),
        (Kind::Double, _) | (Kind::Triple, _) => None,
    }
}

/// The square plot's side in a column `available` pixels wide.
fn plot_side(available: f32) -> f32 {
    if available.is_finite() {
        available.clamp(0.0, PLOT_MAX_SIDE)
    } else {
        PLOT_MAX_SIDE
    }
}

/// Fraction snapping is a host decision; this helper gives consistent 0..1 rounding for fields.
#[cfg(test)]
pub(crate) fn round_fraction(value: f32, decimals: u32) -> f32 {
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
    /// The Points disclosure is open, so the numeric point rows are drawn.
    pub points_open: bool,
    /// The most points the curve holds, shown in the Points row's count.
    pub points_max: usize,
    /// A line under the plot saying which gestures edit the points, when the host gives one.
    pub hint: Option<String>,
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
    /// Open (`true`) or close the Points disclosure. View state only: it edits nothing.
    Points(bool),
}

/// The Points disclosure row: its header and the event a press on it publishes.
fn points_header(model: &CurveEditorModel) -> (SubGroupHeaderModel, CurveEditorEvent) {
    (
        SubGroupHeaderModel {
            label: "Points".into(),
            state: Some(format!("{} of {}", model.points.len(), model.points_max)),
            state_accent: false,
            expanded: Some(model.points_open),
            reset: false,
            enabled: model.enabled,
        },
        CurveEditorEvent::Points(!model.points_open),
    )
}

/// The numeric point rows drawn: every row while the Points disclosure is open, none while closed.
fn shown_rows(model: &CurveEditorModel) -> &[CurvePointRow] {
    if model.points_open {
        &model.point_rows
    } else {
        &[]
    }
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
    body = body.push(
        container(FocusableCurveCanvas {
            model: model.clone(),
            on_event: on_event.clone(),
        })
        .center_x(Length::Fill),
    );
    if let Some(hint) = &model.hint {
        body = body.push(
            text(hint.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    let (header, toggle) = points_header(model);
    let toggle = on_event(toggle);
    body = body.push(sub_group_header(&header, Some(toggle.clone()), toggle));
    for (index, fields) in shown_rows(model).iter().enumerate() {
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

impl<M> FocusableCurveCanvas<'_, M> {
    /// Calls `clear` when the model, the focus or the theme changed since the cache was drawn.
    fn refresh(
        &self,
        key: &Cell<Option<(u64, bool, u64)>>,
        focused: bool,
        theme: &Theme,
        clear: impl FnOnce(),
    ) {
        invalidate_on_version_change(
            key,
            (self.model.version, focused, theme.generation()),
            clear,
        );
    }
}

#[derive(Default)]
struct CurveState {
    cache: canvas::Cache,
    /// The model version, focus and theme generation the cache was drawn for.
    version: Cell<Option<(u64, bool, u64)>>,
    /// The point a press armed a drag on.
    active: Option<usize>,
    /// A press asked to add a point when the curve held this many; its drag takes the host's
    /// selected point once the model holds one more.
    adding: Option<usize>,
    /// Where that press was, in window coordinates, until the pointer leaves the drag slop.
    press: Option<Point>,
    /// The armed drag has left the slop and published a move, so its release ends a gesture.
    dragging: bool,
    /// The last left press, which is all [`Click`] needs to classify the next one.
    last_click: Option<Click>,
    /// That press asked to add a point.
    last_added: bool,
    focused: bool,
    nudge_active: bool,
}

impl CurveState {
    fn disarm(&mut self) {
        self.active = None;
        self.adding = None;
        self.press = None;
        self.dragging = false;
    }
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

impl<M: Clone> canvas::Program<M, Theme> for FocusableCurveCanvas<'_, M> {
    type State = CurveState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<M>> {
        if !self.model.enabled {
            state.disarm();
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
                let click = Click::new(point, mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                state.disarm();
                let size = bounds.size();
                let hit = hit_test(&self.model.points, point, size, POINT_HIT_RADIUS);
                let event = press_event(click.kind(), hit, state.last_added, || {
                    snap_to_sampled(&self.model.sampled, point, size, CURVE_SNAP_RADIUS)
                        .unwrap_or_else(|| {
                            point_fraction(point, Rectangle::new(Point::ORIGIN, size))
                        })
                });
                state.last_added = matches!(event, Some(CurveEditorEvent::Add(_)));
                if let (Kind::Single, Some(index)) = (click.kind(), hit) {
                    state.active = Some(index);
                    state.press = cursor.position();
                } else if state.last_added {
                    state.adding = Some(self.model.points.len());
                    state.press = cursor.position();
                }
                match event {
                    Some(event) => Some(Action::publish((self.on_event)(event)).and_capture()),
                    None => Some(Action::capture()),
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.active.is_none() {
                    // An add's drag waits for the host to answer it with the point it selects.
                    let before = state.adding?;
                    if self.model.points.len() != before + 1 {
                        return None;
                    }
                    state.active = self.model.selected;
                    state.adding = None;
                }
                let index = state.active?;
                let point = cursor.position()?;
                if !state.dragging {
                    let press = state.press?;
                    if (point.x - press.x).hypot(point.y - press.y) <= DRAG_SLOP {
                        return None;
                    }
                    state.dragging = true;
                    state.press = None;
                }
                Some(
                    Action::publish((self.on_event)(CurveEditorEvent::Move {
                        index,
                        position: point_fraction(point, bounds),
                    }))
                    .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let dragged = state.dragging;
                state.disarm();
                // A press that never left the slop drafted nothing, so it has nothing to end.
                dragged.then(|| {
                    Action::publish((self.on_event)(CurveEditorEvent::Release)).and_capture()
                })
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
                    state.disarm();
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
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        self.refresh(&state.version, state.focused, theme, || state.cache.clear());
        let model = &self.model;
        let palette = theme.palette();
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            let size = frame.size();
            frame.fill_rectangle(Point::ORIGIN, size, palette.control);
            if state.focused {
                frame.stroke_rectangle(
                    Point::ORIGIN,
                    size,
                    Stroke::default().with_color(palette.accent).with_width(1.0),
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
                        ..palette.text_secondary
                    },
                );
            }
            for n in 1..4 {
                let f = n as f32 / 4.0;
                let grid = iced::Color {
                    a: 0.2,
                    ..palette.text_tertiary
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
                        .with_color(palette.text_tertiary)
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
                    Stroke::default().with_color(palette.text).with_width(2.0),
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
                        palette.accent
                    } else {
                        palette.text
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
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let side = plot_side(limits.max().width);
        layout::atomic(limits, Length::Fixed(side), Length::Fixed(side))
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
            points_open: false,
            points_max: 8,
            hint: None,
        }
    }

    /// The plot is drawn again when the theme changes, as well as when the model or the focus
    /// does.
    #[test]
    fn a_theme_change_redraws_the_plot() {
        let (canvas, state) = editor(model());
        let (first, second) = (Theme::luxforge_dark(), Theme::luxforge_dark());
        let mut clears = 0;
        canvas.refresh(&state.version, false, &first, || clears += 1);
        canvas.refresh(&state.version, false, &first, || clears += 1);
        assert_eq!(clears, 1, "an unchanged theme keeps the drawing");
        canvas.refresh(&state.version, false, &second, || clears += 1);
        assert_eq!(clears, 2, "a new generation redraws it");
    }

    fn message(action: Option<Action<CurveEditorEvent>>) -> Option<CurveEditorEvent> {
        action.and_then(|action| action.into_inner().0)
    }

    /// A curve editor over `model` publishing its own events, with fresh widget state.
    fn editor(
        model: CurveEditorModel,
    ) -> (FocusableCurveCanvas<'static, CurveEditorEvent>, CurveState) {
        (
            FocusableCurveCanvas {
                model,
                on_event: Rc::new(|event| event),
            },
            CurveState::default(),
        )
    }

    /// Every event one pointer event publishes, at `at` in window coordinates.
    fn send(
        canvas: &FocusableCurveCanvas<'static, CurveEditorEvent>,
        state: &mut CurveState,
        event: mouse::Event,
        bounds: Rectangle,
        at: Point,
    ) -> Option<CurveEditorEvent> {
        message(canvas.update(state, &Event::Mouse(event), bounds, Cursor::Available(at)))
    }

    const PRESS: mouse::Event = mouse::Event::ButtonPressed(mouse::Button::Left);
    const RELEASE: mouse::Event = mouse::Event::ButtonReleased(mouse::Button::Left);

    fn moved(position: Point) -> mouse::Event {
        mouse::Event::CursorMoved { position }
    }

    /// The last press as `mouse::Click` recorded it. Iced classifies a press as the next click of a
    /// run only when it arrives strictly later than the last (and within its own interval), so a
    /// test waits only until the clock has moved past it. A test that pressed twice with no wait
    /// could see two presses at one `Instant`, which Iced treats as unrelated.
    fn after_last_press() {
        let pressed = std::time::Instant::now();
        luxforge_testbase::wait_until("the clock moving past the last press", || {
            std::time::Instant::now() > pressed
        });
    }

    /// 257 samples of the identity, as a host's sample query answers for the neutral curve.
    fn identity_samples() -> Vec<[f32; 2]> {
        (0..=256)
            .map(|k| {
                let x = k as f32 / 256.0;
                [x, x]
            })
            .collect()
    }

    const PLOT: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 200.0,
    };

    #[test]
    fn a_press_and_release_without_motion_publishes_no_move() {
        let (canvas, mut state) = editor(model());
        let on_point = Point::new(40.0, 160.0);
        assert_eq!(
            send(&canvas, &mut state, PRESS, PLOT, on_point),
            Some(CurveEditorEvent::Select(0)),
            "a press on a point selects it"
        );
        assert_eq!(
            send(&canvas, &mut state, RELEASE, PLOT, on_point),
            None,
            "a release with no drag publishes neither a move nor a release to commit"
        );
        assert_eq!(state.active, None);
        assert!(!state.dragging);
    }

    #[test]
    fn motion_below_the_drag_slop_publishes_no_move() {
        let (canvas, mut state) = editor(model());
        let press = Point::new(40.0, 160.0);
        assert_eq!(
            send(&canvas, &mut state, PRESS, PLOT, press),
            Some(CurveEditorEvent::Select(0))
        );
        for jitter in [
            Point::new(41.0, 161.0),
            Point::new(43.0, 160.0),
            Point::new(40.0, 157.0),
            Point::new(42.0, 162.0),
        ] {
            assert_eq!(
                send(&canvas, &mut state, moved(jitter), PLOT, jitter),
                None,
                "{jitter:?} is within {DRAG_SLOP} px of the press"
            );
        }
        assert_eq!(send(&canvas, &mut state, RELEASE, PLOT, press), None);

        // Past the slop the drag begins, and from then on every motion moves the point, even back
        // inside the slop; its release ends the gesture.
        let (canvas, mut state) = editor(model());
        let _ = send(&canvas, &mut state, PRESS, PLOT, press);
        let beyond = Point::new(44.0, 160.0);
        assert_eq!(
            send(&canvas, &mut state, moved(beyond), PLOT, beyond),
            Some(CurveEditorEvent::Move {
                index: 0,
                position: point_fraction(beyond, PLOT)
            })
        );
        assert_eq!(
            send(&canvas, &mut state, moved(press), PLOT, press),
            Some(CurveEditorEvent::Move {
                index: 0,
                position: point_fraction(press, PLOT)
            })
        );
        assert_eq!(
            send(&canvas, &mut state, RELEASE, PLOT, press),
            Some(CurveEditorEvent::Release)
        );
    }

    #[test]
    fn a_double_click_on_a_point_publishes_remove_and_no_move() {
        let (canvas, mut state) = editor(model());
        let on_point = Point::new(40.0, 160.0);
        let mut published = Vec::new();
        published.extend(send(&canvas, &mut state, PRESS, PLOT, on_point));
        published.extend(send(&canvas, &mut state, RELEASE, PLOT, on_point));
        after_last_press();
        let jitter = Point::new(41.0, 161.0);
        published.extend(send(&canvas, &mut state, PRESS, PLOT, jitter));
        published.extend(send(
            &canvas,
            &mut state,
            moved(Point::new(49.0, 161.0)),
            PLOT,
            Point::new(49.0, 161.0),
        ));
        published.extend(send(&canvas, &mut state, RELEASE, PLOT, jitter));
        assert_eq!(
            published,
            vec![CurveEditorEvent::Select(0), CurveEditorEvent::Remove(0)],
            "the first press selects, the second removes, and nothing moves or releases"
        );
    }

    #[test]
    fn a_third_click_after_a_double_click_publishes_nothing() {
        // On a point: select, remove, then nothing.
        let (canvas, mut state) = editor(model());
        let on_point = Point::new(40.0, 160.0);
        let mut published = Vec::new();
        for _ in 0..3 {
            published.extend(send(&canvas, &mut state, PRESS, PLOT, on_point));
            published.extend(send(&canvas, &mut state, RELEASE, PLOT, on_point));
            after_last_press();
        }
        assert_eq!(
            published,
            vec![CurveEditorEvent::Select(0), CurveEditorEvent::Remove(0)]
        );

        // Away from every point: add, then nothing, then nothing.
        let (canvas, mut state) = editor(model());
        let away = Point::new(150.0, 150.0);
        let mut published = Vec::new();
        for _ in 0..3 {
            published.extend(send(&canvas, &mut state, PRESS, PLOT, away));
            after_last_press();
        }
        assert_eq!(published, vec![CurveEditorEvent::Add([0.75, 0.25])]);
        assert!(state.active.is_none(), "a third click arms no drag");
    }

    #[test]
    fn a_double_click_away_from_the_points_adds_one_and_never_removes_it() {
        // The host has not answered the add by the second press: nothing is under it.
        let (canvas, mut state) = editor(model());
        let away = Point::new(150.0, 150.0);
        let mut published = Vec::new();
        published.extend(send(&canvas, &mut state, PRESS, PLOT, away));
        published.extend(send(&canvas, &mut state, RELEASE, PLOT, away));
        after_last_press();
        published.extend(send(&canvas, &mut state, PRESS, PLOT, away));
        published.extend(send(&canvas, &mut state, RELEASE, PLOT, away));
        assert_eq!(published, vec![CurveEditorEvent::Add([0.75, 0.25])]);

        // The host answered the add between the presses, so the second lands on the new point:
        // it still publishes nothing, as the model's new point is the one the first press added.
        let (mut canvas, mut state) = editor(model());
        assert_eq!(
            send(&canvas, &mut state, PRESS, PLOT, away),
            Some(CurveEditorEvent::Add([0.75, 0.25]))
        );
        canvas.model.points = vec![[0.2, 0.2], [0.75, 0.25]];
        canvas.model.selected = Some(1);
        after_last_press();
        assert_eq!(send(&canvas, &mut state, PRESS, PLOT, away), None);
        assert!(state.active.is_none(), "the second press arms no drag");

        // A later double-click on that point, outside the last run's interval, removes it.
        state.last_click = None;
        let mut published = Vec::new();
        for _ in 0..2 {
            published.extend(send(&canvas, &mut state, PRESS, PLOT, away));
            published.extend(send(&canvas, &mut state, RELEASE, PLOT, away));
            after_last_press();
        }
        assert_eq!(
            published,
            vec![CurveEditorEvent::Select(1), CurveEditorEvent::Remove(1)]
        );
    }

    #[test]
    fn a_click_near_the_drawn_curve_adds_at_the_nearest_sample() {
        let model = CurveEditorModel {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            sampled: identity_samples(),
            ..model()
        };
        let expected = model.sampled[129];
        let (canvas, mut state) = editor(model);
        // 1.4 px off the drawn diagonal, whose pointer fraction would be (0.51, 0.5).
        let near = Point::new(102.0, 100.0);
        assert_eq!(
            send(&canvas, &mut state, PRESS, PLOT, near),
            Some(CurveEditorEvent::Add(expected)),
            "the add lands on the nearest sample exactly as the host supplied it"
        );
        assert_eq!(expected, [129.0 / 256.0, 129.0 / 256.0]);

        // The snap is measured to the drawn polyline, not to the samples alone: a curve sampled
        // only at its ends still snaps a pointer on its line, to the nearer end.
        let sparse = snap_to_sampled(
            &[[0.0, 0.0], [1.0, 1.0]],
            Point::new(60.0, 141.0),
            PLOT.size(),
            CURVE_SNAP_RADIUS,
        );
        assert_eq!(sparse, Some([0.0, 0.0]));
        assert_eq!(
            snap_to_sampled(
                &identity_samples(),
                Point::new(106.0, 100.0),
                PLOT.size(),
                CURVE_SNAP_RADIUS
            ),
            None,
            "4.2 px from the line is beyond the snap radius"
        );
    }

    #[test]
    fn a_click_away_from_the_curve_adds_at_the_pointer() {
        let (canvas, mut state) = editor(CurveEditorModel {
            sampled: identity_samples(),
            ..model()
        });
        // Offset bounds: the add is the pointer's fraction of the plot in local coordinates.
        let bounds = Rectangle::new(Point::new(50.0, 70.0), Size::new(100.0, 100.0));
        let pointer = Point::new(110.0, 120.0);
        assert_eq!(
            send(&canvas, &mut state, PRESS, bounds, pointer),
            Some(CurveEditorEvent::Add([0.6, 0.5]))
        );
        assert!(
            state.active.is_none(),
            "an add arms no drag on an existing point"
        );
        assert_eq!(send(&canvas, &mut state, RELEASE, bounds, pointer), None);
    }

    #[test]
    fn a_press_that_adds_drags_the_added_point_without_a_second_press() {
        let (mut canvas, mut state) = editor(model());
        let away = Point::new(150.0, 150.0);
        assert_eq!(
            send(&canvas, &mut state, PRESS, PLOT, away),
            Some(CurveEditorEvent::Add([0.75, 0.25]))
        );
        // Before the host answers the add there is no new point to move.
        let early = Point::new(160.0, 150.0);
        assert_eq!(send(&canvas, &mut state, moved(early), PLOT, early), None);

        // The host adds the point in sorted order and selects it.
        canvas.model.points = vec![[0.2, 0.2], [0.75, 0.25]];
        canvas.model.selected = Some(1);
        let within = Point::new(151.0, 151.0);
        assert_eq!(
            send(&canvas, &mut state, moved(within), PLOT, within),
            None,
            "the drag slop applies to an add's drag too"
        );
        let beyond = Point::new(160.0, 120.0);
        assert_eq!(
            send(&canvas, &mut state, moved(beyond), PLOT, beyond),
            Some(CurveEditorEvent::Move {
                index: 1,
                position: point_fraction(beyond, PLOT)
            })
        );
        assert_eq!(
            send(&canvas, &mut state, RELEASE, PLOT, beyond),
            Some(CurveEditorEvent::Release)
        );

        // An add the host refused leaves the count, so the drag moves the selected point not at
        // all.
        let (canvas, mut state) = editor(model());
        let _ = send(&canvas, &mut state, PRESS, PLOT, away);
        assert_eq!(send(&canvas, &mut state, moved(beyond), PLOT, beyond), None);
        assert_eq!(send(&canvas, &mut state, RELEASE, PLOT, beyond), None);
    }

    #[test]
    fn the_plot_fills_its_column_up_to_the_maximum_side() {
        assert_eq!(plot_side(268.0), 268.0);
        assert_eq!(plot_side(PLOT_MAX_SIDE + 200.0), PLOT_MAX_SIDE);
        assert_eq!(plot_side(f32::INFINITY), PLOT_MAX_SIDE);
        assert_eq!(plot_side(-1.0), 0.0);
    }

    #[test]
    fn the_points_row_publishes_points_and_hides_the_rows_while_closed() {
        let row = |x: &str, y: &str| CurvePointRow {
            display: [x.into(), y.into()],
            edit: [ValueEdit::Display, ValueEdit::Display],
        };
        let closed = CurveEditorModel {
            points: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            point_rows: vec![row("0", "0"), row("0.5", "0.6"), row("1", "1")],
            points_max: 16,
            hint: Some("Click to add a point, or double-click one to remove it".into()),
            ..model()
        };
        let (header, event) = points_header(&closed);
        assert_eq!(header.label, "Points");
        assert_eq!(header.state.as_deref(), Some("3 of 16"));
        assert_eq!(
            header.expanded,
            Some(false),
            "the chevron shows a closed list"
        );
        assert!(!header.reset);
        assert_eq!(event, CurveEditorEvent::Points(true));
        assert!(
            shown_rows(&closed).is_empty(),
            "no point row is drawn while closed"
        );

        let open = CurveEditorModel {
            points_open: true,
            ..closed.clone()
        };
        let (header, event) = points_header(&open);
        assert_eq!(header.expanded, Some(true));
        assert_eq!(event, CurveEditorEvent::Points(false));
        assert_eq!(shown_rows(&open), open.point_rows.as_slice());

        // Both states build, with and without the hint, and the row's press publishes through the
        // host's callback.
        for model in [
            closed.clone(),
            open,
            CurveEditorModel {
                hint: None,
                ..closed
            },
        ] {
            let published = Rc::new(Cell::new(None));
            let sink = published.clone();
            let _: Element<'_, ()> = curve_editor(&model, move |event| {
                if matches!(event, CurveEditorEvent::Points(_)) {
                    sink.set(Some(event));
                }
            });
            assert_eq!(
                published.take(),
                Some(CurveEditorEvent::Points(!model.points_open)),
                "the Points row's press is built from the model"
            );
        }
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
