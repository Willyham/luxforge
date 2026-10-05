//! A notice inside a module's panel rather than over the canvas: the warning triangle beside one
//! wrapped line of text, or the text alone for information. It holds no state and decides nothing.

use super::icon_button::{Icon, icon};
use super::notice_card::Tone;
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::LineHeight;
use iced::widget::{row, text};
use iced::{Alignment, Length};

/// Renders `body` at caption size: a [`Tone::Warning`] or [`Tone::Error`] notice leads with the
/// triangle in its tone's ink and reads in primary text; a [`Tone::Neutral`] one is secondary text
/// alone.
pub fn inline_notice<'a, M: 'a>(tone: Tone, body: impl Into<String>) -> Element<'a, M> {
    let neutral = tone == Tone::Neutral;
    let body = text(body.into())
        .size(theme::SIZE_CAPTION)
        .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
        .style(theme::ink(if neutral {
            Token::TextSecondary
        } else {
            Token::Text
        }))
        .width(Length::Fill);
    if neutral {
        return body.into();
    }
    row![
        icon(Icon::Clipping, theme::HEADER_ICON_SIZE, tone.ink()),
        body
    ]
    .spacing(theme::BUTTON_ROW_SPACING)
    .align_y(Alignment::Start)
    .width(Length::Fill)
    .into()
}
