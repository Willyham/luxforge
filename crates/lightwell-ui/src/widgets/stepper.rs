//! A value field with explicit decrement and increment buttons: one [`theme::FIELD_ROW_HEIGHT`]
//! row, the label at the left, then the decrement and increment buttons and the value box at the
//! right, as a number field has it.

use super::icon_button::{Icon, IconButtonModel, header_icon_button};
use super::number_field::{NumberFieldModel, ValueEdit, field_box, field_label, outside_unit};
use crate::theme;
use crate::widgets::text::error_caption;
use iced::widget::{Row, column, container};
use iced::{Alignment, Element, Length};

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
    let invalid = match &model.field.edit {
        ValueEdit::Editing { invalid, .. } => invalid.clone(),
        ValueEdit::Display => None,
    };
    let minus = header_icon_button(
        &IconButtonModel {
            icon: Icon::Minus,
            tooltip: model.decrement_tooltip.clone(),
            enabled: model.field.enabled && model.decrement_enabled,
            selected: false,
        },
        Some(on_decrement),
    );
    let plus = header_icon_button(
        &IconButtonModel {
            icon: Icon::Plus,
            tooltip: model.increment_tooltip.clone(),
            enabled: model.field.enabled && model.increment_enabled,
            selected: false,
        },
        Some(on_increment),
    );
    let mut line = Row::new()
        .push(container(field_label(&model.field, on_reset)).width(Length::Fill))
        .push(minus)
        .push(plus)
        .push(field_box(&model.field, on_edit_start, on_text, on_submit));
    if let Some(unit) = outside_unit(&model.field.unit) {
        line = line.push(unit);
    }
    let line = line
        .spacing(theme::FIELD_UNIT_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::FIELD_ROW_HEIGHT))
        .width(Length::Fill);
    let mut body = column![line];
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}
