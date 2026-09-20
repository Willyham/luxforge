//! A notice card shown over the canvas: a title, a body and its actions.

use super::text::caption;
use crate::theme;
use iced::widget::{Row, button, column, container, row, text};
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

/// Renders one notice card. Each of `actions` is a `(label, message)` pair, rendered as a button
/// in order.
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

    let mut action_row = Row::new().spacing(theme::SPACING / 2.0);
    for (label, message) in actions {
        action_row = action_row.push(
            button(text(label).size(theme::SIZE_CONTROL))
                .padding([4.0, 10.0])
                .style(theme::button_plain)
                .on_press(message),
        );
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
