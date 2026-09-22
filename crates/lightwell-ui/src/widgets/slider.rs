//! A slider row whose rail reports fractions and whose value field is shared with the stepper.
//!
//! The row is two lines: the label line (the label, and the value right-aligned in a fixed box)
//! and the rail line (the rail, its fill, the zero tick and the handle). Iced's slider draws only
//! the handle and owns the pointer; the rail under it is drawn here from
//! [`geometry::rail_geometry`], on the same scale Iced places the handle on, so the fill always
//! meets the handle and the tick always sits where the handle rests at zero.

use crate::geometry::{self, Side};
use crate::theme;
use crate::widgets::double_click::double_click;
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
    let range = model.soft_min..=model.soft_max;
    let soft_min = model.soft_min;
    let soft_max = model.soft_max;
    let value = model.value.clamp(soft_min, soft_max);
    // Iced needs a numeric rail internally. Its output is immediately reduced to a fraction;
    // declared value mapping and validation remain entirely in the host.
    let on_change: Rc<dyn Fn(f64) -> M + 'a> = Rc::new(on_change);
    let slider_change = Rc::clone(&on_change);
    let fine_change = Rc::clone(&on_change);
    let handle = iced_slider(range, value, move |v| {
        slider_change(geometry::fraction_from_value(soft_min, soft_max, v))
    })
    .step(model.step)
    .shift_step(model.shift_step)
    .on_release(on_release.clone())
    .height(theme::SLIDER_RAIL_HEIGHT)
    .style(theme::slider_style(model.dragging));
    let handle: Element<'a, M> = SliderGuard {
        content: handle.into(),
        enabled: model.enabled,
        value,
        min: soft_min,
        max: soft_max,
        fine_step: model.fine_step,
        on_fine: Box::new(move |v| {
            fine_change(geometry::fraction_from_value(soft_min, soft_max, v))
        }),
        on_release,
    }
    .into();
    let handle = if model.enabled {
        double_click(handle, on_reset)
    } else {
        handle
    };
    let rail = RailDrawing::from_model(model);
    let mut layers = stack![
        canvas(rail)
            .width(Length::Fill)
            .height(theme::SLIDER_RAIL_HEIGHT),
        handle
    ];
    // The over-range mark sits on the rail's end, where the pinned handle is, so it is drawn over
    // the handle rather than under it.
    if let Some(side) = model.over_range {
        layers = layers.push(
            canvas(OverRangeMark { side })
                .width(Length::Fill)
                .height(theme::SLIDER_RAIL_HEIGHT),
        );
    }
    let rail_line = container(layers)
        .width(Length::Fill)
        .height(theme::SLIDER_RAIL_HEIGHT);
    let mut body = column![header, rail_line].spacing(theme::SLIDER_GAP);
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

/// Everything the rail line draws under the handle, as plain values.
#[derive(Debug, Clone, PartialEq)]
struct RailDrawing {
    /// The value as a fraction of the soft rail.
    value: f64,
    /// The zero as a fraction of the soft rail, for a bipolar slider.
    zero: Option<f64>,
    rail: RailDecoration,
}

impl RailDrawing {
    fn from_model(model: &SliderModel) -> Self {
        let fraction = |v: f64| geometry::fraction_from_value(model.soft_min, model.soft_max, v);
        Self {
            value: fraction(model.value),
            zero: geometry::zero_fraction(model.soft_min, model.soft_max, model.zero),
            rail: model.rail.clone(),
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
        vec![
            state
                .cache
                .draw(renderer, bounds.size(), |frame| draw_rail(frame, self)),
        ]
    }
}

fn draw_rail(frame: &mut canvas::Frame, rail: &RailDrawing) {
    let width = frame.width();
    let middle = frame.height() / 2.0;
    let geometry = geometry::rail_geometry(width, theme::THUMB_RADIUS, rail.value, rail.zero);
    let band = |thickness: f32, from: f32, to: f32| {
        (
            Point::new(from, middle - thickness / 2.0),
            Size::new((to - from).max(0.0), thickness),
        )
    };
    match &rail.rail {
        RailDecoration::Colors(colors) if !colors.is_empty() => {
            // A declared colour rail replaces the fill: its colour is the module's meaning. The
            // stops are mixed in sRGB and laid over the panel at the rail's opacity, as the module
            // references draw them; Iced's gradients mix in linear light, so the rail is drawn as
            // short two-stop pieces between exactly computed colours.
            let stops: Vec<[f32; 3]> = colors.iter().map(|c| [c.r, c.g, c.b]).collect();
            let panel = [theme::PANEL.r, theme::PANEL.g, theme::PANEL.b];
            let at = |t: f32| {
                let [r, g, b] = geometry::over(
                    geometry::rail_colour_at(&stops, t),
                    panel,
                    theme::DECORATED_RAIL_OPACITY,
                );
                Color::from_rgb(r, g, b)
            };
            let pieces = geometry::rail_pieces(width, theme::RAIL_PIECE_LENGTH);
            for pair in pieces.windows(2) {
                let (from, to) = (pair[0] * width, pair[1] * width);
                let (origin, size) = band(theme::DECORATED_RAIL_WIDTH, from, to);
                let linear =
                    gradient::Linear::new(Point::new(from, middle), Point::new(to, middle))
                        .add_stop(0.0, at(pair[0]))
                        .add_stop(1.0, at(pair[1]));
                frame.fill_rectangle(origin, size, canvas::Fill::from(linear));
            }
        }
        _ => {
            let (origin, size) = band(theme::RAIL_WIDTH, 0.0, width);
            frame.fill_rectangle(origin, size, theme::RAIL);
            if let Some((from, to)) = geometry.fill {
                let (origin, size) = band(theme::RAIL_WIDTH, from, to);
                frame.fill_rectangle(origin, size, theme::RAIL_FILL);
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
