//! Labelled buttons in a module section, and the row that holds them under a group's sliders.
//!
//! A picker (Basic's Neutral picker, RAW's Neutral WB) and an action (As shot, Crop) are the same
//! button: an optional icon, the label, and an optional key hint in the tertiary colour, on the
//! Control surface at [`theme::BUTTON_HEIGHT`]. The row sets them side by side with
//! [`theme::BUTTON_ROW_MARGIN`] above.

use super::icon_button::{Icon, icon};
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, button, container, text};
use iced::{Alignment, Element, Length, Padding};

/// How a labelled button reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonTone {
    /// The Control surface: an ordinary picker or action.
    #[default]
    Control,
    /// The accent fill: the one primary action on a surface, such as Apply.
    Primary,
    /// The accent tint: a picker whose mode is active.
    Selected,
}

/// Plain data for one labelled button.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelledButtonModel {
    pub label: String,
    pub icon: Option<Icon>,
    /// The key that does the same thing, such as `W` for Basic's neutral picker.
    pub key_hint: Option<String>,
    pub tone: ButtonTone,
    pub enabled: bool,
}

/// Renders one labelled button.
pub fn labelled_button<'a, M: Clone + 'a>(
    model: &LabelledButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (model.tone, model.enabled) {
        (_, false) => theme::TEXT_TERTIARY,
        (ButtonTone::Primary, true) => theme::CANVAS,
        (ButtonTone::Selected, true) => theme::ACCENT,
        (ButtonTone::Control, true) => theme::TEXT_PRIMARY,
    };
    let mut content = Row::new().align_y(Alignment::Center);
    if let Some(glyph) = model.icon {
        content = content.push(
            container(icon(glyph, theme::BUTTON_ICON_SIZE, ink))
                .padding(Padding::default().right(theme::BUTTON_ICON_SPACING)),
        );
    }
    content = content.push(
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(ink),
    );
    if let Some(key) = &model.key_hint {
        content = content.push(
            container(
                text(key.clone())
                    .size(theme::SIZE_CAPTION)
                    .wrapping(Wrapping::None)
                    .color(theme::TEXT_TERTIARY),
            )
            .padding(Padding::default().left(theme::BUTTON_HINT_SPACING)),
        );
    }
    let style = match model.tone {
        ButtonTone::Control => theme::button_control,
        ButtonTone::Primary => theme::button_accent,
        ButtonTone::Selected => theme::button_selected,
    };
    button(content.height(Length::Fill))
        .padding(Padding {
            top: 0.0,
            right: theme::BUTTON_PADDING_RIGHT,
            bottom: 0.0,
            left: theme::BUTTON_PADDING_LEFT,
        })
        .height(Length::Fixed(theme::BUTTON_HEIGHT))
        .style(style)
        .on_press_maybe(if model.enabled { on_press } else { None })
        .into()
}

/// The height a button row takes in a section body, margins included.
pub const fn button_row_height() -> f32 {
    theme::BUTTON_ROW_MARGIN + theme::BUTTON_HEIGHT + theme::BUTTON_ROW_BOTTOM
}

/// Sets buttons side by side as one row under a group's sliders. Buttons that do not fit the
/// panel's width wrap onto another line at the same spacing rather than being clipped.
pub fn button_row<'a, M: 'a>(buttons: Vec<Element<'a, M>>) -> Element<'a, M> {
    container(
        Row::with_children(buttons)
            .spacing(theme::BUTTON_ROW_SPACING)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(theme::BUTTON_ROW_SPACING),
    )
    .padding(Padding {
        top: theme::BUTTON_ROW_MARGIN,
        right: 0.0,
        bottom: theme::BUTTON_ROW_BOTTOM,
        left: 0.0,
    })
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_row_is_its_button_and_margins() {
        assert_eq!(button_row_height(), 28.0);
        assert_eq!(theme::BUTTON_HEIGHT, 22.0);
    }

    #[test]
    fn every_tone_builds_with_and_without_its_parts() {
        for tone in [
            ButtonTone::Control,
            ButtonTone::Primary,
            ButtonTone::Selected,
        ] {
            for (icon, key_hint) in [(None, None), (Some(Icon::Picker), Some("W".to_string()))] {
                let _: Element<'_, ()> = labelled_button(
                    &LabelledButtonModel {
                        label: "Neutral picker".into(),
                        icon,
                        key_hint,
                        tone,
                        enabled: true,
                    },
                    Some(()),
                );
            }
        }
        let _: Element<'_, ()> = button_row(vec![iced::widget::row![].into()]);
    }
}
