//! A notice card shown over the canvas: a leading icon, a title, a body and its actions.

use super::button_row::{ButtonSize, ButtonTone, text_button};
use super::icon_button::{Icon, icon};
use crate::theme;
use crate::{Element, Theme, Token};
use iced::widget::{Row, column, container, row, text};
use iced::{Alignment, Length};

/// A notice's tone, which sets its outline and its icon's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// An informational notice (e.g. a stale preview): a neutral outline and icon.
    Neutral,
    /// A notice that needs a decision (e.g. a conflict): the accent outline and icon, and its last
    /// action is the primary one.
    Warning,
    /// A notice that reports a failure (e.g. a missing original): the error ink's outline and
    /// icon.
    Error,
}

impl Tone {
    /// The card's outline.
    pub fn border(self) -> Token {
        match self {
            Self::Neutral => Token::StripRule,
            Self::Warning => Token::NoticeWarningBorder,
            Self::Error => Token::NoticeErrorBorder,
        }
    }

    /// The leading icon's colour.
    pub fn ink(self) -> Token {
        match self {
            Self::Neutral => Token::TextSecondary,
            Self::Warning => Token::Accent,
            Self::Error => Token::Error,
        }
    }
}

/// Plain data for one notice card.
#[derive(Debug, Clone, PartialEq)]
pub struct NoticeCardModel {
    /// The leading icon: the spark for a change made elsewhere, the triangle for the rest.
    pub icon: Icon,
    pub title: String,
    pub body: String,
    pub tone: Tone,
}

/// Renders one notice card, [`theme::NOTICE_WIDTH`] wide. Each of `actions` is a `(label, message)`
/// pair, rendered as a regular labelled button in order at the right; in a notice that needs a
/// decision the last action, the one that keeps the work (Reapply), is the primary one.
pub fn notice_card<'a, M: Clone + 'a>(
    model: &NoticeCardModel,
    actions: Vec<(String, M)>,
) -> Element<'a, M> {
    let header = text(model.title.clone())
        .size(theme::SIZE_NOTICE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .style(theme::ink(Token::TextBright));
    let body = text(model.body.clone())
        .size(theme::SIZE_NOTICE_BODY)
        .style(theme::ink(Token::TextSecondary));

    let count = actions.len();
    let mut action_row = Row::new()
        .spacing(theme::BUTTON_ROW_SPACING)
        .align_y(Alignment::Center);
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
        icon(model.icon, theme::ICON_SIZE, model.tone.ink()),
        column![header, body].spacing(2.0).width(Length::Fill),
        action_row,
    ]
    .spacing(theme::NOTICE_SPACING)
    .align_y(Alignment::Center);

    let border = model.tone.border();
    container(content)
        .padding(theme::NOTICE_PADDING)
        .width(Length::Fixed(theme::NOTICE_WIDTH))
        .style(move |theme: &Theme| theme::chrome_surface(theme, border, theme::CHROME_RADIUS))
        .into()
}
