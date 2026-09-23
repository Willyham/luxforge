//! A notice card shown over the canvas: a title, a body and its actions.

use super::button_row::{ButtonSize, ButtonTone, text_button};
use super::text::caption;
use crate::theme;
use iced::widget::{Row, column, container, row, text};
use iced::{Alignment, Element, Length};

/// A notice's tone, which affects its title colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// An informational notice (e.g. a stale preview).
    Neutral,
    /// A notice that needs a decision (e.g. a conflict): the accent-coloured title.
    Warning,
}

/// Plain data for one notice card.
#[derive(Debug, Clone, PartialEq)]
pub struct NoticeCardModel {
    pub title: String,
    pub body: String,
    pub tone: Tone,
}

/// Renders one notice card. Each of `actions` is a `(label, message)` pair, rendered as a regular
/// labelled button in order; in a notice that needs a decision the last action, the one that
/// keeps the work (Reapply), is the primary one.
pub fn notice_card<'a, M: Clone + 'a>(
    model: &NoticeCardModel,
    actions: Vec<(String, M)>,
) -> Element<'a, M> {
    let title_color = match model.tone {
        Tone::Neutral => theme::TEXT_PRIMARY,
        Tone::Warning => theme::ACCENT,
    };

    let header = text(model.title.clone())
        .size(theme::SIZE_TITLE)
        .color(title_color);
    let body = caption(model.body.clone());

    let count = actions.len();
    let mut action_row = Row::new().spacing(theme::BUTTON_ROW_SPACING);
    for (index, (label, message)) in actions.into_iter().enumerate() {
        let tone = if model.tone == Tone::Warning && index + 1 == count {
            ButtonTone::Primary
        } else {
            ButtonTone::Control
        };
        action_row = action_row.push(text_button(
            &label,
            tone,
            ButtonSize::Regular,
            Some(message),
        ));
    }

    let content = row![
        column![header, body].spacing(4.0).width(Length::Fill),
        action_row,
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);

    container(content)
        .padding(theme::SPACING)
        .style(theme::bar_surface)
        .into()
}
