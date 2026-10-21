//! The range picker's line in the Masks panel, as the range-rows board draws it: the first line
//! under an open luminance or colour range row. At rest it is the Pick button; while the eyedropper
//! is armed the same line is the pick bar — the accent eyedropper, the hint and Done — so arming it
//! moves nothing under it. The caller's trailing control (the kind's help button) ends the line in
//! both states.
//!
//! Both are plain data. What a click on the photograph does while the bar is up belongs to the
//! editor, which decides it; the bar only says so and offers the way out.

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::icon_button::{Icon, icon, with_tooltip};
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, Space, button, container, row, text, tooltip};
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
    /// Done is offered; it is not while a request the editor is waiting on holds the panel.
    pub enabled: bool,
}

/// Renders the pick bar across its width: the eyedropper in the accent, the hint and Done, on the
/// accent's tint. A press on Done publishes `on_done`.
pub fn pick_bar<'a, M: Clone + 'a>(model: &PickBarModel, on_done: M) -> Element<'a, M> {
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
    let content = row![
        icon(Icon::Picker, theme::DROPDOWN_ICON_SIZE, Token::Accent),
        text(model.hint.clone())
            .size(theme::SIZE_SMALL_CAPTION)
            .line_height(LineHeight::Relative(1.3))
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextSecondary))
            .width(Length::Fill),
        container(done).center_y(Length::Fill),
    ]
    .spacing(theme::PICK_BAR_SPACING)
    .align_y(Alignment::Center);
    container(content.height(Length::Fill))
        .padding(theme::PICK_BAR_PADDING)
        .width(Length::Fill)
        .height(Length::Fixed(theme::PICK_BAR_HEIGHT))
        .style(theme::pick_bar_surface)
        .into()
}

/// Plain data for a range row's pick line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickLineModel {
    /// The Pick button at rest; while it is armed the line is the bar instead.
    pub pick: PickButtonModel,
    /// What the Pick button's tooltip says it picks: `Pick colour range`.
    pub tooltip: String,
    pub bar: PickBarModel,
}

/// Renders the pick line, inset under its component row: Pick at rest, the bar while armed, and
/// `trailing` at its end either way. A press on Pick publishes `on_pick`; on Done, `on_done`.
pub fn pick_line<'a, M: Clone + 'a>(
    model: &PickLineModel,
    on_pick: M,
    on_done: M,
    trailing: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let mut line = Row::new()
        .spacing(theme::PICK_LINE_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::PICK_BAR_HEIGHT))
        .width(Length::Fill);
    if model.pick.armed {
        line = line.push(pick_bar(&model.bar, on_done));
    } else {
        line = line
            .push(with_tooltip(
                pick_button(&model.pick, on_pick),
                model.tooltip.clone(),
                tooltip::Position::Top,
            ))
            .push(Space::new().width(Length::Fill));
    }
    if let Some(trailing) = trailing {
        line = line.push(trailing);
    }
    container(line)
        .padding(theme::PICK_LINE_PADDING)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        for (armed, enabled) in [(false, true), (true, true), (false, false), (true, false)] {
            let model = PickLineModel {
                pick: PickButtonModel {
                    label: "Pick".into(),
                    armed,
                    enabled,
                },
                tooltip: "Pick colour range".into(),
                bar: PickBarModel {
                    hint: "Click or drag on the photo".into(),
                    enabled,
                },
            };
            let _: Element<'_, u8> = pick_line(&model, 1, 2, Some(text("i").into()));
            let _: Element<'_, u8> = pick_line(&model, 1, 2, None);
        }
    }
}
