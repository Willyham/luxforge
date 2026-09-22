//! A value field with explicit decrement and increment buttons.

use super::icon_button::{Icon, IconButtonModel, icon_button};
use super::number_field::{NumberFieldModel, ValueEdit, field_header};
use crate::theme;
use crate::widgets::text::error_caption;
use iced::widget::{column, row};
use iced::{Alignment, Element};

#[derive(Debug, Clone, PartialEq)]
pub struct StepperModel {
    pub field: NumberFieldModel,
    pub decrement_enabled: bool,
    pub increment_enabled: bool,
    pub decrement_tooltip: String,
    pub increment_tooltip: String,
}

#[allow(clippy::too_many_arguments)]
pub fn stepper<'a, M: Clone + 'a>(
    model: &StepperModel,
    on_decrement: M,
    on_increment: M,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> Element<'a, M> {
    let (field, invalid) = field_header(&model.field, on_edit_start, on_text, on_submit, on_reset);
    let minus = icon_button(
        &IconButtonModel {
            icon: Icon::Minus,
            tooltip: model.decrement_tooltip.clone(),
            enabled: model.field.enabled && model.decrement_enabled,
            selected: false,
        },
        Some(on_decrement),
    );
    let plus = icon_button(
        &IconButtonModel {
            icon: Icon::Plus,
            tooltip: model.increment_tooltip.clone(),
            enabled: model.field.enabled && model.increment_enabled,
            selected: false,
        },
        Some(on_increment),
    );
    let mut body = column![
        row![field, minus, plus]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center)
    ]
    .spacing(4.0);
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

// Keep these imports visible in generated API documentation.
const _: Option<ValueEdit> = None;
