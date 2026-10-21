//! The range picker's two pieces in the Masks panel, as the range-masks board draws them: the Pick
//! button a luminance or colour range row carries, lit while its eyedropper is armed, and the
//! one-line pick bar under the selected row while it is — the accent eyedropper, the hint, the
//! colour count and Done.
//!
//! Both are plain data. What a click on the photograph does while the bar is up belongs to the
//! editor, which decides it; the bar only says so and offers the way out.

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::icon_button::{Icon, icon};
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{button, container, row, text};
use iced::{Alignment, Length};

/// Plain data for one Pick button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickButtonModel {
    pub label: String,
    /// The eyedropper is armed for this row: the button is drawn lit, in the accent.
    pub armed: bool,
    pub enabled: bool,
}

/// Renders a Pick button: the eyedropper and the label on a compact Control button, or on the
/// accent tint while armed. A press publishes `on_pick`.
pub fn pick_button<'a, M: Clone + 'a>(model: &PickButtonModel, on_pick: M) -> Element<'a, M> {
    labelled_button(
        &LabelledButtonModel {
            label: model.label.clone(),
            icon: Some(Icon::Picker),
            key_hint: None,
            tone: if model.armed {
                ButtonTone::Selected
            } else {
                ButtonTone::Control
            },
            size: ButtonSize::Compact,
            fill: false,
            enabled: model.enabled,
        },
        Some(on_pick),
    )
}

/// Plain data for the pick bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickBarModel {
    /// What a click and a drag do here, in one short line: `Click or drag on the photo`.
    pub hint: String,
    /// The colours held of the most the range can hold (`2 of 5`), for a kind that holds them.
    pub count: Option<String>,
    /// Done is offered; it is not while a request the editor is waiting on holds the panel.
    pub enabled: bool,
}

/// Renders the pick bar: the eyedropper in the accent, the hint, the count in the accent and Done,
/// on the accent's tint. A press on Done publishes `on_done`.
pub fn pick_bar<'a, M: Clone + 'a>(model: &PickBarModel, on_done: M) -> Element<'a, M> {
    let mut content = row![
        icon(Icon::Picker, theme::DROPDOWN_ICON_SIZE, Token::Accent),
        text(model.hint.clone())
            .size(theme::SIZE_SMALL_CAPTION)
            .line_height(LineHeight::Relative(1.3))
            .style(theme::ink(Token::TextSecondary))
            .width(Length::Fill),
    ]
    .spacing(theme::PICK_BAR_SPACING)
    .align_y(Alignment::Center);
    if let Some(count) = &model.count {
        content = content.push(
            text(count.clone())
                .size(theme::SIZE_SMALL_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::Accent)),
        );
    }
    let done = button(
        text("Done")
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(if model.enabled {
                Token::Text
            } else {
                Token::TextTertiary
            })),
    )
    .padding([0.0, theme::PICK_BAR_DONE_PADDING])
    .height(Length::Fixed(theme::PICK_BAR_DONE_HEIGHT))
    .style(theme::button_control)
    .on_press_maybe(model.enabled.then_some(on_done));
    content = content.push(container(done).center_y(Length::Fill));
    container(
        container(content.height(Length::Fill))
            .padding(theme::PICK_BAR_PADDING)
            .width(Length::Fill)
            .height(Length::Fixed(theme::PICK_BAR_HEIGHT))
            .style(theme::pick_bar_surface),
    )
    .padding(theme::PICK_BAR_MARGIN)
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        for (count, enabled) in [
            (None, true),
            (Some("2 of 5".to_owned()), true),
            (None, false),
        ] {
            let _: Element<'_, u8> = pick_bar(
                &PickBarModel {
                    hint: "Click or drag on the photo".into(),
                    count,
                    enabled,
                },
                1,
            );
        }
        for (armed, enabled) in [(false, true), (true, true), (false, false)] {
            let _: Element<'_, u8> = pick_button(
                &PickButtonModel {
                    label: "Pick".into(),
                    armed,
                    enabled,
                },
                2,
            );
        }
    }
}
