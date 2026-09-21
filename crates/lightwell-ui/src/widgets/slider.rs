//! A slider row whose rail reports fractions and whose value field is shared with the stepper.

use crate::geometry::{self, Side};
use crate::theme;
use crate::widgets::number_field::{NumberFieldModel, field_header};
use crate::widgets::slider_guard::SliderGuard;
use crate::widgets::text::error_caption;
use iced::widget::{column, container, row, slider as iced_slider};
use iced::{Alignment, Color, Element, Length};
use std::rc::Rc;

pub use crate::widgets::number_field::ValueEdit;

#[derive(Debug, Clone, PartialEq)]
pub enum RailDecoration {
    Plain,
    /// Already chosen colours, ordered from left to right. At most eight are drawn by Iced.
    Colors(Vec<Color>),
}

impl Default for RailDecoration {
    fn default() -> Self {
        Self::Plain
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SliderModel {
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

/// The callback receives a rail fraction. The host maps it to a declared value and owns steps.
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
        label: model.label.clone(),
        display: model.display.clone(),
        edit: model.edit.clone(),
        unit: model.unit.clone(),
        enabled: model.enabled,
    };
    let (header, invalid) = field_header(&field, on_edit_start, on_text, on_submit, on_reset);
    let fill = geometry::fill_stops(model.soft_min, model.soft_max, model.zero, model.value);
    let range = model.soft_min..=model.soft_max;
    let soft_min = model.soft_min;
    let soft_max = model.soft_max;
    let value = model.value.clamp(soft_min, soft_max);
    // Iced needs a numeric rail internally. Its output is immediately reduced to a fraction;
    // declared value mapping and validation remain entirely in the host.
    let on_change: Rc<dyn Fn(f64) -> M + 'a> = Rc::new(on_change);
    let slider_change = Rc::clone(&on_change);
    let fine_change = Rc::clone(&on_change);
    let rail = iced_slider(range, value, move |v| {
        slider_change(geometry::fraction_from_value(soft_min, soft_max, v))
    })
    .step(model.step)
    .shift_step(model.shift_step)
    .on_release(on_release.clone())
    .height(16.0)
    .style(theme::slider_style_decorated(
        fill,
        model.dragging,
        model.rail.clone(),
        geometry::fraction_from_value(soft_min, soft_max, value) as f32,
    ));
    let rail: Element<'a, M> = SliderGuard {
        content: rail.into(),
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
    let low = over_range_mark(model.over_range == Some(Side::Low));
    let high = over_range_mark(model.over_range == Some(Side::High));
    let rail = row![low, rail, high]
        .spacing(3.0)
        .align_y(Alignment::Center);
    let mut body = column![header, rail].spacing(4.0);
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

fn over_range_mark<'a, M: 'a>(visible: bool) -> Element<'a, M> {
    container(iced::widget::Space::new())
        .width(Length::Fixed(2.0))
        .height(Length::Fixed(9.0))
        .style(move |_theme| {
            iced::widget::container::Style::default().background(if visible {
                theme::ACCENT
            } else {
                Color::TRANSPARENT
            })
        })
        .into()
}
