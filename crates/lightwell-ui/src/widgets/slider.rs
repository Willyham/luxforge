//! The bipolar/unipolar slider row: label, editable value and a rail that fills from a zero tick.

use crate::geometry;
use crate::theme;
use crate::widgets::double_click::double_click;
use crate::widgets::text::{error_caption, value_text};
use iced::alignment::Horizontal;
use iced::widget::{button, column, mouse_area, row, slider as iced_slider, text, text_input};
use iced::{Alignment, Element, Length};

/// What the slider's value field currently shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueEdit {
    /// The formatted `display` value; clicking it starts editing.
    Display,
    /// A text field is open. `invalid`, when set, is the declared-range message shown under the
    /// field; while it is set the field stays open and commits nothing.
    Editing {
        /// The text currently typed, which may not parse to a valid value yet.
        text: String,
        /// The declared-range message to show, or `None` while the typed text is valid.
        invalid: Option<String>,
    },
}

/// Plain data for one slider row. Holds no editing logic: the host validates typed text and
/// supplies `display` and `edit` already formatted; this widget only lays them out.
#[derive(Debug, Clone, PartialEq)]
pub struct SliderModel {
    /// The control's label, at the row's leading edge. Double-clicking it resets the field.
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    /// The rail's increment: the parameter's declared step when it declares one, else the step the
    /// host derives from the range.
    pub step: f64,
    /// The step used while Shift is held.
    pub shift_step: f64,
    /// How many decimals the value carries. Every value a drag produces is rounded to it before
    /// the message leaves this widget, so the host never receives a number it would not display.
    pub decimals: usize,
    /// The tick the fill grows from; `None` fills from `min` (a unipolar slider).
    pub zero: Option<f64>,
    /// A short unit shown after the value, e.g. `"%"` or `"°"`.
    pub unit: Option<String>,
    /// The value, already formatted by the host (sign, decimals, rounding).
    pub display: String,
    pub edit: ValueEdit,
    /// Whether a draft is open on this field: the rail's one accent state, the handle.
    pub dragging: bool,
    /// Whether the field accepts input. Iced's slider has no built-in disabled state, so a
    /// disabled slider still reports pointer drags; this crate cannot change that, and a caller
    /// that must forbid it has to ignore the resulting messages upstream. The value field (a
    /// button or a text input) is genuinely disabled, since both those widgets support it.
    pub enabled: bool,
}

/// Renders one slider row: a label, a value field and the rail.
///
/// Double-clicking either the label or the rail publishes `on_reset`. The rail needs the
/// [`crate::double_click`] wrapper for it: iced's slider captures the left press over its own
/// bounds, so a `mouse_area` around it would never see one.
///
/// Every value `on_change` carries has been through [`geometry::quantize`], so the host receives
/// the value the row displays rather than the float the pointer mapping produced.
///
/// There is no `on_cancel` callback. Escape cancels a draft through the app's keymap, not through
/// this row: iced's `text_input` consumes Escape internally (it drops its own focus) without
/// producing a message, so no widget here can emit one.
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
    let invalid_message = match &model.edit {
        ValueEdit::Editing { invalid, .. } => invalid.clone(),
        ValueEdit::Display => None,
    };

    let label_color = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    let label = mouse_area(
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .color(label_color),
    )
    .on_double_click(on_reset.clone());

    let value_field: Element<'a, M> = match &model.edit {
        ValueEdit::Display => button(value_text(display_with_unit(&model.display, &model.unit)))
            .padding(0)
            .style(theme::button_plain)
            .on_press_maybe(model.enabled.then_some(on_edit_start))
            .into(),
        ValueEdit::Editing { text, .. } => text_input("", text)
            .size(theme::SIZE_CONTROL)
            .width(Length::Fixed(theme::VALUE_WIDTH))
            .align_x(Horizontal::Right)
            .style(theme::text_input_style(invalid_message.is_some()))
            .on_input_maybe(model.enabled.then_some(on_text))
            .on_submit_maybe(model.enabled.then_some(on_submit))
            .into(),
    };

    let header = row![label, value_field]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let fill = geometry::fill_stops(model.min, model.max, model.zero, model.value);
    let (min, max, step, decimals) = (model.min, model.max, model.step, model.decimals);
    let rail = iced_slider(min..=max, model.value, move |value| {
        on_change(geometry::quantize(value, min, max, step, decimals))
    })
    .step(model.step)
    .shift_step(model.shift_step)
    .on_release(on_release)
    .height(16.0)
    .style(theme::slider_style(fill, model.dragging));

    let mut body = column![header, double_click(rail, on_reset)].spacing(4.0);

    if let Some(message) = invalid_message {
        body = body.push(error_caption(message));
    }

    body.into()
}

/// Appends the unit suffix to a formatted display value, if any. A symbol (`°`, `%`, `×`) sits
/// against the number; a word (`EV`, `K`, `px`) is separated from it by a space.
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
    use super::*;

    #[test]
    fn display_edit_shows_the_formatted_value() {
        let edit = ValueEdit::Display;
        assert!(matches!(edit, ValueEdit::Display));
    }

    #[test]
    fn editing_shows_typed_text_regardless_of_validity() {
        let valid = ValueEdit::Editing {
            text: "18".into(),
            invalid: None,
        };
        let invalid = ValueEdit::Editing {
            text: "9999".into(),
            invalid: Some("Range is -100 to 100".into()),
        };

        match (valid, invalid) {
            (
                ValueEdit::Editing {
                    text: valid_text,
                    invalid: valid_invalid,
                },
                ValueEdit::Editing {
                    text: invalid_text,
                    invalid: invalid_invalid,
                },
            ) => {
                assert_eq!(valid_text, "18");
                assert!(valid_invalid.is_none());
                assert_eq!(invalid_text, "9999");
                assert_eq!(invalid_invalid.as_deref(), Some("Range is -100 to 100"));
            }
            _ => panic!("expected both values to be Editing"),
        }
    }

    #[test]
    fn unit_is_appended_only_when_present() {
        assert_eq!(display_with_unit("140", &None), "140");
        assert_eq!(display_with_unit("2.4", &Some("°".to_string())), "2.4°");
        assert_eq!(
            display_with_unit("1.00", &Some("EV".to_string())),
            "1.00 EV"
        );
        assert_eq!(display_with_unit("6500", &Some("K".to_string())), "6500 K");
    }
}
