//! A slider row whose rail reports fractions and whose value field is shared with the stepper.
//!
//! The row is two lines: the label line (the label, and the value right-aligned in a fixed box)
//! and the rail line (the rail, its fill, the zero tick and the handle). Iced's slider draws only
//! the handle and owns the pointer; the rail under it is drawn here from
//! [`geometry::rail_geometry`], on the same scale Iced places the handle on, so the fill always
//! meets the handle and the tick always sits where the handle rests at zero.

use crate::geometry::{self, Side};
use crate::theme;
use crate::widgets::double_click::double_click_when;
use crate::widgets::number_field::{NumberFieldModel, field_header};
use crate::widgets::slider_guard::SliderGuard;
use crate::widgets::text::error_caption;
use iced::widget::canvas::gradient;
use iced::widget::{canvas, column, container, slider as iced_slider, stack};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};
use std::cell::RefCell;
use std::rc::Rc;

pub use crate::widgets::number_field::ValueEdit;

#[derive(Debug, Clone, Default, PartialEq)]
pub enum RailDecoration {
    #[default]
    Plain,
    /// Already chosen colours, evenly spaced from left to right and mixed in sRGB.
    Colors(Vec<Color>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SliderModel {
    pub id: Option<String>,
    pub label: String,
    /// Hard range, used only to display out-of-soft-range status; the rail spans `soft_*`.
    pub min: f64,
    pub max: f64,
    pub soft_min: f64,
    pub soft_max: f64,
    pub value: f64,
    pub step: f64,
    pub shift_step: f64,
    pub fine_step: f64,
    pub zero: Option<f64>,
    pub rail: RailDecoration,
    pub over_range: Option<Side>,
    pub unit: Option<String>,
    pub display: String,
    pub edit: ValueEdit,
    pub dragging: bool,
    pub enabled: bool,
}

/// The callback receives a rail fraction. The host maps it to a declared value, applies its
/// declared step and display precision, and owns validation. Double-clicking the label or an
/// enabled rail publishes `on_reset`; the rail wrapper sees the press before Iced's slider
/// captures it.
#[allow(clippy::too_many_arguments)]
pub fn slider<'a, M: Clone + 'a>(
    model: &SliderModel,
    on_change: impl Fn(f64) -> M + 'a,
    on_release: M,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> Element<'a, M> {
    let field = NumberFieldModel {
        id: model.id.clone(),
        label: model.label.clone(),
        display: model.display.clone(),
        edit: model.edit.clone(),
        unit: model.unit.clone(),
        enabled: model.enabled,
    };
    let (header, invalid) =
        field_header(&field, on_edit_start, on_text, on_submit, on_reset.clone());
    let rail_line = rail_line(
        &RailLine::from_model(model),
        on_change,
        on_release,
        on_reset,
    );
    let mut body = column![header, rail_line].spacing(theme::SLIDER_GAP);
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

/// One rail line as plain data: the soft range the rail spans, the value, its steps, the zero the
/// fill grows from, the rail's colours and the gesture state. The slider row and the stepper's
/// rail both draw from it, so a rail looks and behaves the same wherever it is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RailLine {
    pub soft_min: f64,
    pub soft_max: f64,
    pub value: f64,
    pub step: f64,
    pub shift_step: f64,
    pub fine_step: f64,
    pub zero: Option<f64>,
    pub decoration: RailDecoration,
    pub over_range: Option<Side>,
    pub dragging: bool,
    pub enabled: bool,
}

impl RailLine {
    fn from_model(model: &SliderModel) -> Self {
        Self {
            soft_min: model.soft_min,
            soft_max: model.soft_max,
            value: model.value,
            step: model.step,
            shift_step: model.shift_step,
            fine_step: model.fine_step,
            zero: model.zero,
            decoration: model.rail.clone(),
            over_range: model.over_range,
            dragging: model.dragging,
            enabled: model.enabled,
        }
    }
}

/// The rail line: the rail, its fill, the zero tick and, while dragging, the thumb's halo, drawn
/// under Iced's handle on the scale Iced places the handle on. The callback receives a rail
/// fraction; double-clicking an enabled rail publishes `on_reset`.
pub(crate) fn rail_line<'a, M: Clone + 'a>(
    rail: &RailLine,
    on_change: impl Fn(f64) -> M + 'a,
    on_release: M,
    on_reset: M,
) -> Element<'a, M> {
    let soft_min = rail.soft_min;
    let soft_max = rail.soft_max;
    let value = rail.value.clamp(soft_min, soft_max);
    // Iced needs a numeric rail internally. Its output is immediately reduced to a fraction;
    // declared value mapping and validation remain entirely in the host.
    let on_change: Rc<dyn Fn(f64) -> M + 'a> = Rc::new(on_change);
    let slider_change = Rc::clone(&on_change);
    let fine_change = Rc::clone(&on_change);
    let handle = iced_slider(soft_min..=soft_max, value, move |v| {
        slider_change(geometry::fraction_from_value(soft_min, soft_max, v))
    })
    .step(rail.step)
    .shift_step(rail.shift_step)
    .on_release(on_release.clone())
    .height(theme::SLIDER_RAIL_HEIGHT)
    .style(theme::slider_style(rail.dragging));
    let handle: Element<'a, M> = SliderGuard {
        content: handle.into(),
        enabled: rail.enabled,
        value,
        min: soft_min,
        max: soft_max,
        fine_step: rail.fine_step,
        on_fine: Box::new(move |v| {
            fine_change(geometry::fraction_from_value(soft_min, soft_max, v))
        }),
        on_release,
    }
    .into();
    // Always wrapped, and told whether to listen, so the tree keeps one shape: a rail disabled for
    // a moment between the two presses of a double-click keeps the first press.
    let handle = double_click_when(handle, on_reset, rail.enabled);
    let drawing = RailDrawing::from_rail(rail);
    let mut layers = stack![
        canvas(drawing)
            .width(Length::Fill)
            .height(theme::SLIDER_RAIL_HEIGHT),
        handle
    ];
    // The over-range mark sits on the rail's end, where the pinned handle is, so it is drawn over
    // the handle rather than under it.
    if let Some(side) = rail.over_range {
        layers = layers.push(
            canvas(OverRangeMark { side })
                .width(Length::Fill)
                .height(theme::SLIDER_RAIL_HEIGHT),
        );
    }
    container(layers)
        .width(Length::Fill)
        .height(theme::SLIDER_RAIL_HEIGHT)
        .into()
}

/// Everything the rail line draws under the handle, as plain values.
#[derive(Debug, Clone, PartialEq)]
struct RailDrawing {
    /// The value as a fraction of the soft rail.
    value: f64,
    /// The zero as a fraction of the soft rail, for a bipolar slider.
    zero: Option<f64>,
    rail: RailDecoration,
    /// A gesture is live: the thumb's halo is drawn around the handle.
    dragging: bool,
}

impl RailDrawing {
    fn from_rail(rail: &RailLine) -> Self {
        let fraction = |v: f64| geometry::fraction_from_value(rail.soft_min, rail.soft_max, v);
        Self {
            value: fraction(rail.value),
            zero: geometry::zero_fraction(rail.soft_min, rail.soft_max, rail.zero),
            rail: rail.decoration.clone(),
            dragging: rail.dragging,
        }
    }
}

#[derive(Default)]
struct RailCache {
    cache: canvas::Cache,
    key: RefCell<Option<(RailDrawing, Size)>>,
}

impl<M> canvas::Program<M> for RailDrawing {
    type State = RailCache;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        let key = Some((self.clone(), bounds.size()));
        if *state.key.borrow() != key {
            state.cache.clear();
            *state.key.borrow_mut() = key;
        }
        // The halo is taller than the rail line, so the frame's clip reaches past the line above
        // and below; the rail line's layout, and so the row's pitch, stays as it is.
        let size = bounds.size();
        let clip = geometry::rail_clip(size.width, size.height, theme::THUMB_HALO_RADIUS);
        let clip = Rectangle::new(Point::new(clip.0, clip.1), Size::new(clip.2, clip.3));
        vec![
            state
                .cache
                .draw_with_bounds(renderer, clip, |frame| draw_rail(frame, self, size)),
        ]
    }
}

/// Draws the rail line into a `size` rail line; the frame's clip may reach beyond it.
fn draw_rail(frame: &mut canvas::Frame, rail: &RailDrawing, size: Size) {
    let width = size.width;
    let middle = size.height / 2.0;
    let geometry = geometry::rail_geometry(width, theme::THUMB_RADIUS, rail.value, rail.zero);
    let band = |thickness: f32, from: f32, to: f32| {
        (
            Point::new(from, middle - thickness / 2.0),
            Size::new((to - from).max(0.0), thickness),
        )
    };
    let colors = match &rail.rail {
        RailDecoration::Colors(colors) if !colors.is_empty() => Some(colors),
        _ => None,
    };
    // The colour a decorated rail shows at fraction `t`: its stops mixed in sRGB and laid over the
    // panel at the rail's opacity.
    let at = |colors: &Vec<Color>, t: f32| {
        let stops: Vec<[f32; 3]> = colors.iter().map(|c| [c.r, c.g, c.b]).collect();
        let [r, g, b] = geometry::over(
            geometry::rail_colour_at(&stops, t),
            rgb(theme::PANEL),
            theme::DECORATED_RAIL_OPACITY,
        );
        Color::from_rgb(r, g, b)
    };
    let gradient_band =
        |frame: &mut canvas::Frame, from: f32, to: f32, start: Color, end: Color| {
            let (origin, size) = band(theme::DECORATED_RAIL_WIDTH, from, to);
            let linear = gradient::Linear::new(Point::new(from, middle), Point::new(to, middle))
                .add_stop(0.0, start)
                .add_stop(1.0, end);
            frame.fill_rectangle(origin, size, canvas::Fill::from(linear));
        };
    match colors {
        Some(colors) => {
            // A declared colour rail replaces the fill: its colour is the module's meaning. The
            // stops are mixed in sRGB and laid over the panel at the rail's opacity, as the module
            // references draw them; Iced's gradients mix in linear light, so the rail is drawn as
            // short two-stop pieces between exactly computed colours.
            let pieces = geometry::rail_pieces(width, theme::RAIL_PIECE_LENGTH);
            for pair in pieces.windows(2) {
                let (from, to) = (pair[0] * width, pair[1] * width);
                gradient_band(frame, from, to, at(colors, pair[0]), at(colors, pair[1]));
            }
        }
        None => {
            let (origin, size) = band(theme::RAIL_WIDTH, 0.0, width);
            frame.fill_rectangle(origin, size, theme::RAIL);
            if let Some((from, to)) = geometry.fill {
                let (origin, size) = band(theme::RAIL_WIDTH, from, to);
                frame.fill_rectangle(origin, size, theme::RAIL_FILL);
            }
        }
    }
    // While a gesture is live the thumb sits in a halo of the accent: a disc laid over the panel
    // and over the rail it crosses, each composited in sRGB as the references draw it.
    if rail.dragging {
        let handle = geometry.handle;
        let radius = theme::THUMB_HALO_RADIUS;
        frame.fill(
            &canvas::Path::circle(Point::new(handle, middle), radius),
            halo_over(theme::PANEL),
        );
        let (from, to) = ((handle - radius).max(0.0), (handle + radius).min(width));
        match colors {
            Some(colors) => {
                let tinted = |x: f32| halo_over(at(colors, x / width.max(f32::EPSILON)));
                gradient_band(frame, from, to, tinted(from), tinted(to));
            }
            None => {
                let (origin, size) = band(theme::RAIL_WIDTH, from, to);
                frame.fill_rectangle(origin, size, halo_over(theme::RAIL));
                if let Some((fill_from, fill_to)) = geometry.fill {
                    let (fill_from, fill_to) = (fill_from.max(from), fill_to.min(to));
                    if fill_to > fill_from {
                        let (origin, size) = band(theme::RAIL_WIDTH, fill_from, fill_to);
                        frame.fill_rectangle(origin, size, halo_over(theme::RAIL_FILL));
                    }
                }
            }
        }
    }
    if let Some(tick) = geometry.tick {
        frame.fill_rectangle(
            Point::new(
                tick - theme::ZERO_TICK_WIDTH / 2.0,
                middle - theme::ZERO_TICK_HEIGHT / 2.0,
            ),
            Size::new(theme::ZERO_TICK_WIDTH, theme::ZERO_TICK_HEIGHT),
            theme::ZERO_TICK,
        );
    }
}

fn rgb(color: Color) -> [f32; 3] {
    [color.r, color.g, color.b]
}

/// What the thumb's halo makes of `under`: the accent laid over it at
/// [`theme::THUMB_HALO_OPACITY`], mixed in sRGB and drawn opaque, since Iced blends a translucent
/// fill in linear light and would draw the halo far brighter than the references do.
fn halo_over(under: Color) -> Color {
    let [r, g, b] = geometry::over(rgb(theme::ACCENT), rgb(under), theme::THUMB_HALO_OPACITY);
    Color::from_rgb(r, g, b)
}

/// The accent mark at the end of the rail beyond which the value lies.
struct OverRangeMark {
    side: Side,
}

impl<M> canvas::Program<M> for OverRangeMark {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let mark = theme::OVER_RANGE_MARK;
        let x = match self.side {
            Side::Low => 0.0,
            Side::High => bounds.width - mark.width,
        };
        frame.fill_rectangle(
            Point::new(x, (bounds.height - mark.height) / 2.0),
            mark,
            theme::ACCENT,
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::halo_over;
    use crate::theme;

    fn rgb8(color: iced::Color) -> [u8; 3] {
        [color.r, color.g, color.b].map(|c| (c * 255.0).round() as u8)
    }

    /// crop-and-straighten.png (drafting) and components.png (Exposure dragging): the thumb sits
    /// in an 18 pt halo, (81, 69, 53) over the panel and (100, 88, 75) over the empty rail, each
    /// within one code of rounding.
    #[test]
    fn the_dragging_halo_matches_the_references() {
        assert_eq!(2.0 * theme::THUMB_HALO_RADIUS, 18.0);
        for (under, reference) in [(theme::PANEL, [81, 69, 53]), (theme::RAIL, [100, 88, 75])] {
            let drawn = rgb8(halo_over(under));
            assert!(
                drawn.iter().zip(reference).all(|(a, b)| a.abs_diff(b) <= 1),
                "{drawn:?} against {reference:?}"
            );
        }
    }
}
