//! A band on one axis: two thumbs for its low and high edges and, when the host declares them, a
//! shoulder grip outside each for the width the band fades over.
//!
//! The row is two lines, as a slider's is: the label with the band's readout right-aligned, then
//! the rail line. The rail is plain or a colour rail (a luminance band's runs black to white); the
//! band between the thumbs is filled, and each shoulder is drawn as a fade from the band outwards
//! to its grip, as mask-panels.png draws the luminance range.
//!
//! The widget owns no value. It reports which grip moved and the value that grip now stands for,
//! already clamped so the low edge never passes the high one and a shoulder never leaves the rail,
//! and snapped to the host's step; the host maps that onto its declared parameter exactly as it
//! maps a slider's fraction. Every position is computed by the pure functions below on the scale a
//! slider places its handle on ([`geometry::handle_center`]), so a range and a slider over the same
//! axis put the same value at the same point.

use super::double_click::ClickRun;
use crate::geometry;
use crate::theme;
use crate::widgets::slider::RailDecoration;
use crate::widgets::text::control_label;
use crate::{Element, Palette, Theme, Token};
use iced::alignment::Horizontal;
use iced::widget::canvas::{self, Action, Event, Path, Stroke, gradient};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{column, container, row, text};
use iced::{Alignment, Color, Length, Point, Rectangle, Renderer, Size, mouse};
use std::cell::RefCell;
use std::rc::Rc;

/// One of the four things a pointer can hold on a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeGrip {
    /// The band's low edge.
    Low,
    /// The band's high edge.
    High,
    /// The outer end of the low shoulder: a width below the low edge.
    LowShoulder,
    /// The outer end of the high shoulder: a width above the high edge.
    HighShoulder,
}

impl RangeGrip {
    /// Whether this is one of the band's two edges rather than a shoulder.
    pub fn is_thumb(self) -> bool {
        matches!(self, Self::Low | Self::High)
    }
}

/// The band's numbers, in the host's own units: the edges on the axis, the shoulders as widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeValues {
    pub low: f64,
    pub high: f64,
    /// `None` when the host declares no low shoulder, which draws no fade and no grip.
    pub low_feather: Option<f64>,
    pub high_feather: Option<f64>,
}

impl RangeValues {
    /// The value `grip` holds now.
    pub fn get(&self, grip: RangeGrip) -> Option<f64> {
        match grip {
            RangeGrip::Low => Some(self.low),
            RangeGrip::High => Some(self.high),
            RangeGrip::LowShoulder => self.low_feather,
            RangeGrip::HighShoulder => self.high_feather,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RangeSliderModel {
    pub label: String,
    /// What the label line shows at its right, already formatted: `62 – 88`.
    pub readout: String,
    /// The axis the rail spans.
    pub min: f64,
    pub max: f64,
    pub values: RangeValues,
    /// What one drag moves in: every value a drag reports is a whole number of steps, so a
    /// shoulder is never a fraction of one.
    pub step: f64,
    pub rail: RailDecoration,
    /// The grip a gesture holds, which is drawn in the accent inside its halo.
    pub dragging: Option<RangeGrip>,
    pub enabled: bool,
}

/// A range row. `on_change` receives the grip that moved and the value it now stands for;
/// `on_release` ends that grip's gesture; a double-click on a grip publishes `on_reset` for it.
pub fn range_slider<'a, M: Clone + 'a>(
    model: &RangeSliderModel,
    on_change: impl Fn(RangeGrip, f64) -> M + 'a,
    on_release: impl Fn(RangeGrip) -> M + 'a,
    on_reset: impl Fn(RangeGrip) -> M + 'a,
) -> Element<'a, M> {
    let readout = text(model.readout.clone())
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .wrapping(Wrapping::None)
        .align_x(Horizontal::Right)
        .style(theme::ink(if model.enabled {
            Token::Text
        } else {
            Token::TextTertiary
        }));
    let header = row![
        container(control_label(model.label.clone(), model.enabled)).width(Length::Fill),
        container(readout).padding(iced::Padding::default().right(theme::VALUE_INSET)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    let rail = iced::widget::canvas(RangeCanvas {
        model: model.clone(),
        on_change: Rc::new(on_change),
        on_release: Rc::new(on_release),
        on_reset: Rc::new(on_reset),
    })
    .width(Length::Fill)
    .height(theme::SLIDER_RAIL_HEIGHT);
    column![header, rail].spacing(theme::SLIDER_GAP).into()
}

// ---- geometry ----------------------------------------------------------------------------------

/// Where `value` sits on a rail `width` wide whose thumbs have `radius`: the slider's own scale, so
/// the ends of the axis are a radius in from the rail's ends. A value off the axis sits at its end.
pub fn value_x(width: f32, radius: f32, min: f64, max: f64, value: f64) -> f32 {
    geometry::handle_center(
        width,
        radius,
        geometry::fraction_from_value(min, max, value),
    )
}

/// The value at `x` on that rail, the inverse of [`value_x`], clamped to the axis.
pub fn x_value(width: f32, radius: f32, min: f64, max: f64, x: f32) -> f64 {
    let travel = (width - 2.0 * radius).max(0.0);
    if travel <= 0.0 || !min.is_finite() || !max.is_finite() || max <= min {
        return min;
    }
    let fraction = f64::from((x - radius) / travel).clamp(0.0, 1.0);
    min + fraction * (max - min)
}

/// Where each grip sits, in points from the rail's left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeLayout {
    pub low: f32,
    pub high: f32,
    /// The low shoulder's outer end, or `None` when the host declares no low shoulder. A shoulder
    /// reaching past the axis's end is drawn to that end.
    pub low_shoulder: Option<f32>,
    pub high_shoulder: Option<f32>,
}

impl RangeLayout {
    fn at(&self, grip: RangeGrip) -> Option<f32> {
        match grip {
            RangeGrip::Low => Some(self.low),
            RangeGrip::High => Some(self.high),
            RangeGrip::LowShoulder => self.low_shoulder,
            RangeGrip::HighShoulder => self.high_shoulder,
        }
    }
}

/// The grips' positions on a rail `width` wide for these values.
pub fn range_layout(
    width: f32,
    radius: f32,
    min: f64,
    max: f64,
    values: &RangeValues,
) -> RangeLayout {
    let x = |value: f64| value_x(width, radius, min, max, value);
    RangeLayout {
        low: x(values.low),
        high: x(values.high),
        low_shoulder: values.low_feather.map(|width| x(values.low - width)),
        high_shoulder: values.high_feather.map(|width| x(values.high + width)),
    }
}

/// How far from a thumb's centre a press still takes it.
pub const THUMB_REACH: f32 = theme::THUMB_RADIUS + 2.0;
/// How far from a shoulder grip's centre a press still takes it.
pub const SHOULDER_REACH: f32 = theme::SHOULDER_GRIP_RADIUS + 3.0;
/// Grips closer to the pointer than this to each other are one place: which one a press takes is
/// decided by the direction it then moves ([`resolve_tie`]).
pub const TIE: f32 = 1.0;

/// The grips a press at `x` takes: the nearest within its reach, and every other grip at the same
/// place, thumbs before shoulders. Empty when the press is on the rail away from every grip.
pub fn hit_grips(layout: &RangeLayout, x: f32) -> Vec<RangeGrip> {
    let mut found: Vec<(RangeGrip, f32)> = [
        (RangeGrip::Low, THUMB_REACH),
        (RangeGrip::High, THUMB_REACH),
        (RangeGrip::LowShoulder, SHOULDER_REACH),
        (RangeGrip::HighShoulder, SHOULDER_REACH),
    ]
    .into_iter()
    .filter_map(|(grip, reach)| {
        let distance = (layout.at(grip)? - x).abs();
        (distance <= reach).then_some((grip, distance))
    })
    .collect();
    let Some(nearest) = found.iter().map(|(_, distance)| *distance).reduce(f32::min) else {
        return Vec::new();
    };
    found.retain(|(_, distance)| *distance <= nearest + TIE);
    // Stable: the table order above already puts the thumbs first.
    found.into_iter().map(|(grip, _)| grip).collect()
}

/// Which of several grips at one place a gesture takes, from the direction it first moves (`dx`,
/// negative to the left). Two thumbs at one value part as the pointer goes: left is the low edge
/// and right the high one. A thumb whose shoulder is closed opens the shoulder when pulled outwards
/// and moves the edge when pushed inwards, so a closed shoulder is never out of reach.
pub fn resolve_tie(tied: &[RangeGrip], dx: f32) -> Option<RangeGrip> {
    let has = |grip| tied.contains(&grip);
    let left = dx < 0.0;
    let chosen = if has(RangeGrip::Low) && has(RangeGrip::High) {
        if left {
            RangeGrip::Low
        } else {
            RangeGrip::High
        }
    } else if has(RangeGrip::Low) && has(RangeGrip::LowShoulder) {
        if left {
            RangeGrip::LowShoulder
        } else {
            RangeGrip::Low
        }
    } else if has(RangeGrip::High) && has(RangeGrip::HighShoulder) {
        if left {
            RangeGrip::High
        } else {
            RangeGrip::HighShoulder
        }
    } else {
        return tied.first().copied();
    };
    Some(chosen)
}

/// The thumb a press on the bare rail at `x` moves: the nearer edge, and at a tie the one on the
/// pointer's side.
pub fn nearest_thumb(layout: &RangeLayout, x: f32) -> RangeGrip {
    let (low, high) = ((layout.low - x).abs(), (layout.high - x).abs());
    if low < high || (low == high && x < layout.low) {
        RangeGrip::Low
    } else {
        RangeGrip::High
    }
}

/// What `grip` becomes when the pointer stands for `pointer` on the axis: a thumb takes the
/// pointer's value, a shoulder the width from its edge to the pointer. Each is snapped to whole
/// `step`s and clamped so the band stays a band: `low` never above `high`, each edge on the axis,
/// and each shoulder non-negative and no wider than the rail beyond its edge (a shoulder already
/// wider than that keeps its value until it is dragged).
pub fn dragged_value(
    grip: RangeGrip,
    pointer: f64,
    values: &RangeValues,
    min: f64,
    max: f64,
    step: f64,
) -> f64 {
    let snap = |value: f64| {
        if step.is_finite() && step > 0.0 {
            min + ((value - min) / step).round() * step
        } else {
            value
        }
    };
    // A width snapped to whole steps and held within `limit`, rounding down at the limit so a
    // snapped width never passes it.
    let width = |raw: f64, limit: f64| {
        let limit = limit.max(0.0);
        let snapped = if step.is_finite() && step > 0.0 {
            let whole = (raw / step).round() * step;
            if whole > limit {
                (limit / step + 1e-9).floor() * step
            } else {
                whole
            }
        } else {
            raw.min(limit)
        };
        snapped.max(0.0) + 0.0
    };
    match grip {
        RangeGrip::Low => snap(pointer).clamp(min, values.high.clamp(min, max)),
        RangeGrip::High => snap(pointer).clamp(values.low.clamp(min, max), max),
        RangeGrip::LowShoulder => width(values.low - pointer, values.low - min),
        RangeGrip::HighShoulder => width(pointer - values.high, max - values.high),
    }
}

// ---- the rail canvas ---------------------------------------------------------------------------

struct RangeCanvas<'a, M> {
    model: RangeSliderModel,
    on_change: Rc<dyn Fn(RangeGrip, f64) -> M + 'a>,
    on_release: Rc<dyn Fn(RangeGrip) -> M + 'a>,
    on_reset: Rc<dyn Fn(RangeGrip) -> M + 'a>,
}

/// A gesture in progress: the grip it holds, or the grips it may hold until it first moves, and
/// how far from the grip's centre the pointer took it, so a grip never jumps under the pointer.
#[derive(Debug, Clone, PartialEq)]
enum Held {
    Grip(RangeGrip),
    Tied(Vec<RangeGrip>),
}

#[derive(Default)]
struct RangeState {
    cache: canvas::Cache,
    key: RefCell<Option<RangeKey>>,
    held: Option<Held>,
    press_x: f32,
    offset: f32,
    clicks: ClickRun,
}

/// What a range's drawing depends on: the model, its size and the theme's generation.
type RangeKey = (RangeSliderModel, Size, u64);

impl<M> RangeCanvas<'_, M> {
    /// Calls `clear` when the model, its size or the theme changed since the cache was drawn.
    fn refresh(
        &self,
        key: &RefCell<Option<RangeKey>>,
        size: Size,
        theme: &Theme,
        clear: impl FnOnce(),
    ) {
        let next = Some((self.model.clone(), size, theme.generation()));
        if *key.borrow() != next {
            clear();
            *key.borrow_mut() = next;
        }
    }

    fn layout(&self, width: f32) -> RangeLayout {
        range_layout(
            width,
            theme::THUMB_RADIUS,
            self.model.min,
            self.model.max,
            &self.model.values,
        )
    }

    /// The change a pointer at `x` makes to `grip`, when it makes one.
    fn moved(&self, grip: RangeGrip, x: f32, width: f32) -> Option<M> {
        let model = &self.model;
        let pointer = x_value(width, theme::THUMB_RADIUS, model.min, model.max, x);
        let value = dragged_value(
            grip,
            pointer,
            &model.values,
            model.min,
            model.max,
            model.step,
        );
        (model.values.get(grip) != Some(value)).then(|| (self.on_change)(grip, value))
    }
}

impl<M> canvas::Program<M, Theme> for RangeCanvas<'_, M> {
    type State = RangeState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<M>> {
        if !self.model.enabled {
            state.held = None;
            return None;
        }
        let width = bounds.width;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                let layout = self.layout(width);
                let tied = hit_grips(&layout, point.x);
                if state.clicks.double(point)
                    && let Some(grip) = tied.first().copied()
                {
                    state.held = None;
                    return Some(Action::publish((self.on_reset)(grip)).and_capture());
                }
                state.press_x = point.x;
                match tied.as_slice() {
                    [] => {
                        // A press on the bare rail moves the nearer edge there, as a slider's
                        // handle jumps to a press on its rail, and holds it.
                        let grip = nearest_thumb(&layout, point.x);
                        state.held = Some(Held::Grip(grip));
                        state.offset = 0.0;
                        Some(match self.moved(grip, point.x, width) {
                            Some(message) => Action::publish(message).and_capture(),
                            None => Action::capture(),
                        })
                    }
                    [grip] => {
                        state.offset = point.x - layout.at(*grip).unwrap_or(point.x);
                        state.held = Some(Held::Grip(*grip));
                        Some(Action::capture())
                    }
                    [first, ..] => {
                        state.offset = point.x - layout.at(*first).unwrap_or(point.x);
                        state.held = Some(Held::Tied(tied));
                        Some(Action::capture())
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let held = state.held.clone()?;
                let x = cursor.position()?.x - bounds.x;
                let grip = match held {
                    Held::Grip(grip) => grip,
                    Held::Tied(tied) => {
                        let dx = x - state.press_x;
                        if dx.abs() < TIE {
                            return Some(Action::capture());
                        }
                        let grip = resolve_tie(&tied, dx)?;
                        state.held = Some(Held::Grip(grip));
                        grip
                    }
                };
                Some(match self.moved(grip, x - state.offset, width) {
                    Some(message) => Action::publish(message).and_capture(),
                    None => Action::capture(),
                })
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                match state.held.take()? {
                    Held::Grip(grip) => {
                        Some(Action::publish((self.on_release)(grip)).and_capture())
                    }
                    // Pressed on two grips at one place and never moved: nothing was held.
                    Held::Tied(_) => Some(Action::capture()),
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
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        self.refresh(&state.key, bounds.size(), theme, || state.cache.clear());
        let size = bounds.size();
        let clip = geometry::rail_clip(size.width, size.height, theme::THUMB_HALO_RADIUS);
        let clip = Rectangle::new(Point::new(clip.0, clip.1), Size::new(clip.2, clip.3));
        vec![state.cache.draw_with_bounds(renderer, clip, |frame| {
            draw_range(
                frame,
                &self.model,
                &self.layout(size.width),
                size,
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
            return mouse::Interaction::None;
        }
        if state.held.is_some() {
            return mouse::Interaction::Grabbing;
        }
        match cursor.position_in(bounds) {
            Some(point) if !hit_grips(&self.layout(bounds.width), point.x).is_empty() => {
                mouse::Interaction::Grab
            }
            Some(_) => mouse::Interaction::Pointer,
            None => mouse::Interaction::None,
        }
    }
}

fn rgb(color: Color) -> [f32; 3] {
    [color.r, color.g, color.b]
}

/// `colour` laid over `under` at `opacity`, mixed in sRGB and drawn opaque, as the slider's rail
/// and halo are.
fn over(colour: Color, under: Color, opacity: f32) -> Color {
    let [r, g, b] = geometry::over(rgb(colour), rgb(under), opacity);
    Color::from_rgb(r, g, b)
}

fn draw_range(
    frame: &mut canvas::Frame,
    model: &RangeSliderModel,
    layout: &RangeLayout,
    size: Size,
    palette: &Palette,
) {
    let width = size.width;
    let middle = size.height / 2.0;
    let colors = match &model.rail {
        RailDecoration::Colors(colors) if !colors.is_empty() => Some(colors),
        _ => None,
    };
    let thickness = if colors.is_some() {
        theme::DECORATED_RAIL_WIDTH
    } else {
        theme::RAIL_WIDTH
    };
    let band = |from: f32, to: f32| {
        (
            Point::new(from.min(to), middle - thickness / 2.0),
            Size::new((to - from).abs(), thickness),
        )
    };
    // The rail: a declared colour rail mixed in sRGB and laid over its backdrop, in short exact
    // pieces as the slider draws one, or the plain rail.
    match colors {
        Some(colors) => {
            let stops: Vec<[f32; 3]> = colors.iter().map(|c| rgb(*c)).collect();
            let at = |t: f32| {
                let [r, g, b] = geometry::over(
                    geometry::rail_colour_at(&stops, t),
                    rgb(palette.rail_backdrop),
                    theme::DECORATED_RAIL_OPACITY,
                );
                Color::from_rgb(r, g, b)
            };
            let pieces = geometry::rail_pieces(width, theme::RAIL_PIECE_LENGTH);
            for pair in pieces.windows(2) {
                let (from, to) = (pair[0] * width, pair[1] * width);
                let (origin, piece) = band(from, to);
                let linear =
                    gradient::Linear::new(Point::new(from, middle), Point::new(to, middle))
                        .add_stop(0.0, at(pair[0]))
                        .add_stop(1.0, at(pair[1]));
                frame.fill_rectangle(origin, piece, canvas::Fill::from(linear));
            }
        }
        None => {
            let (origin, rail) = band(0.0, width);
            frame.fill_rectangle(origin, rail, palette.rail);
        }
    }
    // The shoulders fade from the band's fill at its edge to nothing at the grip.
    let transparent = Color {
        a: 0.0,
        ..palette.rail_fill
    };
    for (edge, end) in [
        (layout.low, layout.low_shoulder),
        (layout.high, layout.high_shoulder),
    ] {
        let Some(end) = end.filter(|end| (end - edge).abs() > f32::EPSILON) else {
            continue;
        };
        let (origin, shoulder) = band(edge, end);
        let linear = gradient::Linear::new(Point::new(edge, middle), Point::new(end, middle))
            .add_stop(0.0, palette.rail_fill)
            .add_stop(1.0, transparent);
        frame.fill_rectangle(origin, shoulder, canvas::Fill::from(linear));
    }
    // The band itself.
    if layout.high > layout.low {
        let (origin, filled) = band(layout.low, layout.high);
        frame.fill_rectangle(origin, filled, palette.rail_fill);
    }
    let dragging = |grip| model.dragging == Some(grip);
    // The halo sits under whichever grip a gesture holds.
    if let Some(x) = model.dragging.and_then(|grip| layout.at(grip)) {
        frame.fill(
            &Path::circle(Point::new(x, middle), theme::THUMB_HALO_RADIUS),
            over(
                palette.accent,
                palette.background,
                theme::THUMB_HALO_OPACITY,
            ),
        );
    }
    let ink = |color: Color| {
        if model.enabled {
            color
        } else {
            over(color, palette.background, 0.4)
        }
    };
    for (grip, x) in [
        (RangeGrip::LowShoulder, layout.low_shoulder),
        (RangeGrip::HighShoulder, layout.high_shoulder),
    ] {
        let Some(x) = x else { continue };
        let fill = if dragging(grip) {
            palette.accent
        } else {
            palette.text_tertiary
        };
        frame.fill(
            &Path::circle(Point::new(x, middle), theme::SHOULDER_GRIP_RADIUS),
            ink(fill),
        );
    }
    for grip in [RangeGrip::Low, RangeGrip::High] {
        let Some(x) = layout.at(grip) else { continue };
        let centre = Point::new(x, middle);
        if dragging(grip) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f32 = 214.0;
    const RADIUS: f32 = 7.0;

    fn band() -> RangeValues {
        RangeValues {
            low: 62.0,
            high: 88.0,
            low_feather: Some(10.0),
            high_feather: Some(6.0),
        }
    }

    /// The range is drawn again when the theme changes, and only then while it stands still.
    #[test]
    fn a_theme_change_redraws_the_range() {
        let canvas = RangeCanvas {
            model: RangeSliderModel {
                label: "Luminance".into(),
                readout: "62 \u{2013} 88".into(),
                min: 0.0,
                max: 100.0,
                values: band(),
                step: 1.0,
                rail: RailDecoration::Plain,
                dragging: None,
                enabled: true,
            },
            on_change: Rc::new(|_, _| ()),
            on_release: Rc::new(|_| ()),
            on_reset: Rc::new(|_| ()),
        };
        let size = Size::new(WIDTH, 2.0 * RADIUS);
        let (first, second) = (Theme::luxforge_dark(), Theme::luxforge_dark());
        let key = RefCell::new(None);
        let mut clears = 0;
        canvas.refresh(&key, size, &first, || clears += 1);
        canvas.refresh(&key, size, &first, || clears += 1);
        assert_eq!(clears, 1, "an unchanged theme keeps the drawing");
        canvas.refresh(&key, size, &second, || clears += 1);
        assert_eq!(clears, 2, "a new generation redraws it");
    }

    /// The axis's ends are a radius in from the rail's, as a slider's handle travels, and the two
    /// mappings are each other's inverse across the axis.
    #[test]
    fn values_and_positions_map_on_the_sliders_scale() {
        assert_eq!(value_x(WIDTH, RADIUS, 0.0, 100.0, 0.0), RADIUS);
        assert_eq!(value_x(WIDTH, RADIUS, 0.0, 100.0, 100.0), WIDTH - RADIUS);
        assert_eq!(value_x(WIDTH, RADIUS, 0.0, 100.0, 50.0), WIDTH / 2.0);
        for value in [0.0, 12.5, 62.0, 88.0, 100.0] {
            let x = value_x(WIDTH, RADIUS, 0.0, 100.0, value);
            assert!((x_value(WIDTH, RADIUS, 0.0, 100.0, x) - value).abs() < 1e-4);
        }
        // Off the rail, and a rail with no travel, answer the axis's ends.
        assert_eq!(x_value(WIDTH, RADIUS, 0.0, 100.0, -20.0), 0.0);
        assert_eq!(x_value(WIDTH, RADIUS, 0.0, 100.0, WIDTH + 5.0), 100.0);
        assert_eq!(x_value(10.0, RADIUS, 0.0, 100.0, 5.0), 0.0);
        // A value off the axis sits at its end, where a slider pins its handle.
        assert_eq!(value_x(WIDTH, RADIUS, 0.0, 100.0, -5.0), RADIUS);
    }

    /// Each shoulder's grip is its width away from its edge, and one reaching past the axis is
    /// drawn at the axis's end.
    #[test]
    fn the_layout_places_each_grip_at_its_value() {
        let layout = range_layout(WIDTH, RADIUS, 0.0, 100.0, &band());
        let x = |value| value_x(WIDTH, RADIUS, 0.0, 100.0, value);
        assert_eq!(layout.low, x(62.0));
        assert_eq!(layout.high, x(88.0));
        assert_eq!(layout.low_shoulder, Some(x(52.0)));
        assert_eq!(layout.high_shoulder, Some(x(94.0)));
        let wide = RangeValues {
            high_feather: Some(40.0),
            ..band()
        };
        assert_eq!(
            range_layout(WIDTH, RADIUS, 0.0, 100.0, &wide).high_shoulder,
            Some(WIDTH - RADIUS)
        );
        let bare = RangeValues {
            low_feather: None,
            high_feather: None,
            ..band()
        };
        let layout = range_layout(WIDTH, RADIUS, 0.0, 100.0, &bare);
        assert_eq!((layout.low_shoulder, layout.high_shoulder), (None, None));
    }

    /// A press takes the nearest grip within its reach; a press on the bare rail takes nothing,
    /// and a missing shoulder can never be taken.
    #[test]
    fn a_press_takes_the_nearest_grip_within_reach() {
        let layout = range_layout(WIDTH, RADIUS, 0.0, 100.0, &band());
        assert_eq!(hit_grips(&layout, layout.low), vec![RangeGrip::Low]);
        assert_eq!(hit_grips(&layout, layout.high + 3.0), vec![RangeGrip::High]);
        assert_eq!(
            hit_grips(&layout, layout.low_shoulder.unwrap() - 2.0),
            vec![RangeGrip::LowShoulder]
        );
        assert_eq!(
            hit_grips(&layout, layout.high_shoulder.unwrap()),
            vec![RangeGrip::HighShoulder]
        );
        // Between the thumbs, far from both: the bare rail, whose nearer edge a press moves.
        let between = (layout.low + layout.high) / 2.0 - 2.0;
        assert!(hit_grips(&layout, between).is_empty());
        assert_eq!(nearest_thumb(&layout, between), RangeGrip::Low);
        assert_eq!(nearest_thumb(&layout, layout.high + 30.0), RangeGrip::High);
        assert!(hit_grips(&layout, RADIUS).is_empty(), "the axis's far end");
        let bare = range_layout(
            WIDTH,
            RADIUS,
            0.0,
            100.0,
            &RangeValues {
                low_feather: None,
                ..band()
            },
        );
        assert!(hit_grips(&bare, layout.low_shoulder.unwrap()).is_empty());
    }

    /// Grips at one place are all taken, thumbs first, and the first move says which one the
    /// gesture holds: coincident edges part left and right, and a closed shoulder opens outwards.
    #[test]
    fn grips_at_one_place_part_by_the_direction_of_the_first_move() {
        let closed = RangeValues {
            low: 50.0,
            high: 50.0,
            low_feather: Some(0.0),
            high_feather: Some(10.0),
        };
        let layout = range_layout(WIDTH, RADIUS, 0.0, 100.0, &closed);
        let tied = hit_grips(&layout, layout.low);
        assert_eq!(
            tied,
            vec![RangeGrip::Low, RangeGrip::High, RangeGrip::LowShoulder]
        );
        assert_eq!(resolve_tie(&tied, -3.0), Some(RangeGrip::Low));
        assert_eq!(resolve_tie(&tied, 3.0), Some(RangeGrip::High));
        let shoulder = [RangeGrip::Low, RangeGrip::LowShoulder];
        assert_eq!(resolve_tie(&shoulder, -2.0), Some(RangeGrip::LowShoulder));
        assert_eq!(resolve_tie(&shoulder, 2.0), Some(RangeGrip::Low));
        let high = [RangeGrip::High, RangeGrip::HighShoulder];
        assert_eq!(resolve_tie(&high, 2.0), Some(RangeGrip::HighShoulder));
        assert_eq!(resolve_tie(&high, -2.0), Some(RangeGrip::High));
        assert_eq!(resolve_tie(&[RangeGrip::High], -1.0), Some(RangeGrip::High));
        assert_eq!(resolve_tie(&[], 1.0), None);
    }

    /// A drag snaps to whole steps and keeps the band a band: the edges never cross and stay on
    /// the axis, and a shoulder is a non-negative width no wider than the rail beyond its edge.
    #[test]
    fn a_drag_is_snapped_and_clamped_so_the_band_stays_a_band() {
        let values = band();
        let drag = |grip, pointer| dragged_value(grip, pointer, &values, 0.0, 100.0, 1.0);
        assert_eq!(drag(RangeGrip::Low, 40.4), 40.0);
        assert_eq!(drag(RangeGrip::Low, 95.0), 88.0, "low stops at high");
        assert_eq!(drag(RangeGrip::Low, -3.0), 0.0);
        assert_eq!(drag(RangeGrip::High, 70.6), 71.0);
        assert_eq!(drag(RangeGrip::High, 10.0), 62.0, "high stops at low");
        assert_eq!(drag(RangeGrip::High, 140.0), 100.0);
        assert_eq!(drag(RangeGrip::LowShoulder, 49.7), 12.0);
        assert_eq!(drag(RangeGrip::LowShoulder, 70.0), 0.0, "never negative");
        assert_eq!(
            drag(RangeGrip::LowShoulder, -10.0),
            62.0,
            "the rail below low"
        );
        assert_eq!(drag(RangeGrip::HighShoulder, 91.2), 3.0);
        assert_eq!(drag(RangeGrip::HighShoulder, 80.0), 0.0);
        assert_eq!(
            drag(RangeGrip::HighShoulder, 130.0),
            12.0,
            "the rail above high"
        );
        // A limit that is not a whole step rounds down, so the width stays on the grid and
        // inside the rail.
        let near = RangeValues { low: 0.5, ..values };
        assert_eq!(
            dragged_value(RangeGrip::LowShoulder, -5.0, &near, 0.0, 100.0, 1.0),
            0.0
        );
        // A non-positive step does not snap.
        assert_eq!(
            dragged_value(RangeGrip::Low, 40.4, &values, 0.0, 100.0, 0.0),
            40.4
        );
        // Every width reported is a positive zero rather than -0.
        assert!(drag(RangeGrip::HighShoulder, 88.0).is_sign_positive());
    }

    #[test]
    fn a_grip_names_whether_it_is_an_edge() {
        assert!(RangeGrip::Low.is_thumb() && RangeGrip::High.is_thumb());
        assert!(!RangeGrip::LowShoulder.is_thumb() && !RangeGrip::HighShoulder.is_thumb());
        let values = band();
        assert_eq!(values.get(RangeGrip::HighShoulder), Some(6.0));
        assert_eq!(
            RangeValues {
                low_feather: None,
                ..values
            }
            .get(RangeGrip::LowShoulder),
            None
        );
    }
}
