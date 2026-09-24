//! A labelled value that switches between formatted display and editable text.
//!
//! Two shapes share one model. A slider's label line ([`field_header`]) shows the value as bare
//! text right-aligned in its box. A number field ([`number_field`]) is its own row of
//! [`theme::FIELD_ROW_HEIGHT`]: the label at the left and the value in a [`theme::FIELD_WIDTH`] box
//! on the Control surface at the right, with a word unit (`px`) after the box and a symbol unit
//! (`°`) inside it.

use crate::theme;
use crate::widgets::double_click::double_click_when;
use crate::widgets::text::{control_label, error_caption, value_text};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, button, column, container, row, text, text_input};
use iced::{Alignment, Element, Length, Padding};

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

/// A [`value_input`] sized as a field box: [`theme::FIELD_HEIGHT`] tall, `width` wide, the value
/// right-aligned at [`theme::VALUE_INSET`] from the box's edge, so typing into a field does not
/// move its digits.
pub fn boxed_input<'a, M: Clone + 'a>(
    placeholder: &str,
    value: &str,
    width: f32,
    invalid: bool,
    enabled: bool,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
) -> iced::widget::TextInput<'a, M> {
    value_input(placeholder, value, invalid, enabled, on_text, on_submit)
        .width(Length::Fixed(width))
        .line_height(LineHeight::Absolute(
            (theme::FIELD_HEIGHT - 2.0 * theme::FIELD_PADDING_Y).into(),
        ))
        .padding(Padding {
            top: theme::FIELD_PADDING_Y,
            right: theme::FIELD_INSET,
            bottom: theme::FIELD_PADDING_Y,
            left: theme::FIELD_INSET,
        })
        .align_x(Horizontal::Right)
        .style(theme::field_input_style(invalid))
}

/// A number field: one row, the label at the left and the value boxed at the right.
pub fn number_field<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> Element<'a, M> {
    let invalid = invalid(model);
    let mut line = Row::new()
        .push(container(field_label(model, on_reset)).width(Length::Fill))
        .push(field_box(model, on_edit_start, on_text, on_submit));
    if let Some(unit) = outside_unit(&model.unit) {
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

/// A colour's field row, as developer-pixel.png draws RGB: the label at the left, then the swatch
/// and the channel boxes ([`boxed_input`] at [`theme::CHANNEL_FIELD_WIDTH`]) at the right.
pub fn channel_row<'a, M: 'a>(
    label: String,
    enabled: bool,
    swatch: Element<'a, M>,
    channels: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let boxes = Row::with_children(channels).spacing(theme::CHANNEL_FIELD_SPACING);
    row![
        container(control_label(label, enabled)).width(Length::Fill),
        swatch,
        boxes
    ]
    .spacing(theme::SWATCH_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fixed(theme::FIELD_ROW_HEIGHT))
    .width(Length::Fill)
    .into()
}

/// A label line over a control that is not a slider (a choice, a curve): the label alone, on the
/// slider's label line, so its pitch matches.
pub fn label_line<'a, M: 'a>(label: String, enabled: bool) -> Element<'a, M> {
    control_label(label, enabled).into()
}

fn invalid(model: &NumberFieldModel) -> Option<String> {
    match &model.edit {
        ValueEdit::Editing { invalid, .. } => invalid.clone(),
        ValueEdit::Display => None,
    }
}

/// The field's label: double-clicking it resets the field. The wrapper stays in place while the
/// field is disabled, so a press made just before a moment of disablement still counts.
pub(crate) fn field_label<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_reset: M,
) -> Element<'a, M> {
    double_click_when(
        control_label(model.label.clone(), model.enabled),
        on_reset,
        model.enabled,
    )
}

/// The value in its box: a press opens it for typing; typing shows the text as typed.
pub(crate) fn field_box<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
) -> Element<'a, M> {
    match &model.edit {
        ValueEdit::Display => {
            let shown = box_text(&model.display, &model.unit);
            button(
                text(shown)
                    .size(theme::SIZE_CONTROL)
                    .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
                    .wrapping(Wrapping::None)
                    .align_x(Horizontal::Right)
                    .align_y(Vertical::Center)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .color(if model.enabled {
                        theme::TEXT_PRIMARY
                    } else {
                        theme::TEXT_TERTIARY
                    }),
            )
            .padding(Padding {
                top: 0.0,
                right: theme::FIELD_INSET,
                bottom: 0.0,
                left: theme::FIELD_INSET,
            })
            .width(Length::Fixed(theme::FIELD_WIDTH))
            .height(Length::Fixed(theme::FIELD_HEIGHT))
            .style(theme::button_field)
            .on_press_maybe(model.enabled.then_some(on_edit_start))
            .into()
        }
        ValueEdit::Editing { text, invalid } => {
            let input = boxed_input(
                "",
                text,
                theme::FIELD_WIDTH,
                invalid.is_some(),
                model.enabled,
                on_text,
                on_submit,
            );
            match &model.id {
                Some(id) => input.id(iced::widget::Id::from(id.clone())).into(),
                None => input.into(),
            }
        }
    }
}

/// A word unit, drawn after the box in the tertiary colour, [`theme::FIELD_UNIT_GAP`] from it; a
/// symbol unit is inside the box.
pub(crate) fn outside_unit<'a, M: 'a>(unit: &Option<String>) -> Option<Element<'a, M>> {
    match unit {
        Some(unit) if is_word(unit) => Some(
            container(
                text(unit.clone())
                    .size(theme::SIZE_CAPTION)
                    .wrapping(Wrapping::None)
                    .color(theme::TEXT_TERTIARY),
            )
            .padding(Padding {
                left: unit_lead(),
                ..Padding::ZERO
            })
            .into(),
        ),
        _ => None,
    }
}

/// What a word unit adds to the row's own spacing, so it sits [`theme::FIELD_UNIT_GAP`] from the
/// box.
fn unit_lead() -> f32 {
    (theme::FIELD_UNIT_GAP - theme::FIELD_UNIT_SPACING).max(0.0)
}

fn is_word(unit: &str) -> bool {
    unit.starts_with(|c: char| c.is_ascii_alphanumeric())
}

/// What the box shows: the value, with a symbol unit against it.
fn box_text(display: &str, unit: &Option<String>) -> String {
    match unit {
        Some(unit) if !is_word(unit) => format!("{display}{unit}"),
        _ => display.to_string(),
    }
}

/// A slider's label line: the label, and the value right-aligned in its fixed box with its unit.
pub(crate) fn field_header<'a, M: Clone + 'a>(
    model: &NumberFieldModel,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
) -> (Element<'a, M>, Option<String>) {
    let invalid = invalid(model);
    let label = field_label(model, on_reset);
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
        Some(unit) if is_word(unit) => format!("{display} {unit}"),
        Some(unit) => format!("{display}{unit}"),
        None => display.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{box_text, display_with_unit};
    use crate::theme;

    #[test]
    fn symbol_and_word_units_keep_their_spacing() {
        assert_eq!(display_with_unit("140", &None), "140");
        assert_eq!(display_with_unit("2.4", &Some("°".into())), "2.4°");
        assert_eq!(display_with_unit("1.00", &Some("EV".into())), "1.00 EV");
        assert_eq!(display_with_unit("6500", &Some("K".into())), "6500 K");
    }

    #[test]
    fn a_box_holds_a_symbol_unit_and_leaves_a_word_unit_outside() {
        assert_eq!(box_text("2.40", &Some("°".into())), "2.40°");
        assert_eq!(box_text("1204", &Some("px".into())), "1204");
        assert_eq!(box_text("877", &None), "877");
    }

    /// developer-pixel.png: `px` sits 8 pt after its box, 2 pt more than the row's 6 pt spacing
    /// between a stepper's buttons and its box.
    #[test]
    fn a_word_unit_sits_its_own_gap_from_the_box() {
        assert_eq!(theme::FIELD_UNIT_GAP, 8.0);
        assert_eq!(theme::FIELD_UNIT_SPACING + super::unit_lead(), 8.0);
    }

    /// developer-pixel.png and crop-and-straighten.png: a 56 × 20 pt box on a 24 pt row.
    #[test]
    fn a_field_is_its_box_on_its_row() {
        assert_eq!((theme::FIELD_WIDTH, theme::FIELD_HEIGHT), (56.0, 20.0));
        assert_eq!(theme::FIELD_ROW_HEIGHT, 24.0);
        assert_eq!(
            theme::FIELD_HEIGHT - 2.0 * theme::FIELD_PADDING_Y,
            theme::SLIDER_LABEL_HEIGHT + 2.0,
            "the editing input keeps the box's height"
        );
    }
}
