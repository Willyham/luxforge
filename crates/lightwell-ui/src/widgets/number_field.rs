//! A labelled value that switches between formatted display and editable text.

use crate::theme;
use crate::widgets::text::{control_label, error_caption, value_text};
use iced::alignment::Horizontal;
use iced::widget::{button, column, mouse_area, row, text_input};
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
    /// Stable host-supplied focus target for the editing input.
    pub id: Option<String>,
    pub label: String,
    pub display: String,
    pub edit: ValueEdit,
    pub unit: Option<String>,
    pub enabled: bool,
}

/// The editable input shared by number fields, RGB/hex fields, curve coordinates and zoom.
/// Layout stays with the containing widget; parsing, validation and commit handling stay with
/// the caller. This is an input primitive, not a descriptor-level text control.
pub fn value_input<'a, M: Clone + 'a>(
    placeholder: &str,
    value: &str,
    invalid: bool,
    enabled: bool,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
) -> iced::widget::TextInput<'a, M> {
    text_input(placeholder, value)
        .size(theme::SIZE_CONTROL)
        .style(theme::text_input_style(invalid))
        .on_input_maybe(enabled.then_some(on_text))
        .on_submit_maybe(enabled.then_some(on_submit))
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
    let label: Element<'a, M> = if model.enabled {
        mouse_area(control_label(model.label.clone(), true))
            .on_double_click(on_reset)
            .into()
    } else {
        control_label(model.label.clone(), false).into()
    };
    let value: Element<'a, M> = match &model.edit {
        ValueEdit::Display => {
            let display = display_with_unit(&model.display, &model.unit);
            button(value_text(display))
                .padding(0)
                .style(theme::button_plain)
                .on_press_maybe(model.enabled.then_some(on_edit_start))
                .into()
        }
        ValueEdit::Editing { text, .. } => {
            let input = value_input(
                "",
                text,
                invalid.is_some(),
                model.enabled,
                on_text,
                on_submit,
            )
            .width(Length::Fixed(theme::VALUE_WIDTH))
            .align_x(Horizontal::Right);
            match &model.id {
                Some(id) => input.id(iced::widget::Id::from(id.clone())).into(),
                None => input.into(),
            }
        }
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

/// Symbols sit against a number; word units have a separating space.
fn display_with_unit(display: &str, unit: &Option<String>) -> String {
    match unit {
        Some(unit) if unit.starts_with(|c: char| c.is_ascii_alphanumeric()) => {
            format!("{display} {unit}")
        }
        Some(unit) => format!("{display}{unit}"),
        None => display.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::display_with_unit;

    #[test]
    fn symbol_and_word_units_keep_their_spacing() {
        assert_eq!(display_with_unit("140", &None), "140");
        assert_eq!(display_with_unit("2.4", &Some("°".into())), "2.4°");
        assert_eq!(display_with_unit("1.00", &Some("EV".into())), "1.00 EV");
        assert_eq!(display_with_unit("6500", &Some("K".into())), "6500 K");
    }
}
