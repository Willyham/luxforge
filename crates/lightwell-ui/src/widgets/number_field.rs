//! A labelled value that switches between formatted display and editable text.

use crate::theme;
use crate::widgets::text::{error_caption, value_text};
use iced::alignment::Horizontal;
use iced::widget::{button, column, mouse_area, row, text, text_input};
use iced::{Alignment, Element, Length};

/// The value field's visible state. Parsing and validation belong to the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueEdit {
    Display,
    Editing {
        text: String,
        invalid: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumberFieldModel {
    pub label: String,
    pub display: String,
    pub edit: ValueEdit,
    pub unit: Option<String>,
    pub enabled: bool,
}

pub fn number_field<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> Element<'a, M> {
    let (header, invalid) = field_header(model, on_edit_start, on_text, on_submit, on_reset);
    let mut body = column![header].spacing(4.0);
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

pub(crate) fn field_header<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> (Element<'a, M>, Option<String>) {
    let invalid = match &model.edit {
        ValueEdit::Editing { invalid, .. } => invalid.clone(),
        ValueEdit::Display => None,
    };
    let color = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    let label = mouse_area(
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .color(color),
    )
    .on_double_click(on_reset);
    let label: Element<'a, M> = if model.enabled {
        label.into()
    } else {
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .color(color)
            .into()
    };
    let value: Element<'a, M> = match &model.edit {
        ValueEdit::Display => {
            let display = match &model.unit {
                Some(unit) => format!("{}{unit}", model.display),
                None => model.display.clone(),
            };
            button(value_text(display))
                .padding(0)
                .style(theme::button_plain)
                .on_press_maybe(model.enabled.then_some(on_edit_start))
                .into()
        }
        ValueEdit::Editing { text, .. } => text_input("", text)
            .size(theme::SIZE_CONTROL)
            .width(Length::Fixed(theme::VALUE_WIDTH))
            .align_x(Horizontal::Right)
            .style(theme::text_input_style(invalid.is_some()))
            .on_input_maybe(model.enabled.then_some(on_text))
            .on_submit_maybe(model.enabled.then_some(on_submit))
            .into(),
    };
    (
        row![iced::widget::container(label).width(Length::Fill), value]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .into(),
        invalid,
    )
}
