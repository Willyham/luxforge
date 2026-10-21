//! Keys drawn as caps: the loupe's key hints (`.hint`) and the keys in a help sheet's steps.

use crate::theme;
use crate::{Element, Token};
use iced::Alignment;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, container, row, text};

/// One key hint: the key's cap and what it does.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyHint {
    pub key: String,
    pub action: String,
}

/// One key in its cap: 10.5 pt semibold in [`Token::TextLabel`] on the tag surface.
pub fn key_cap<'a, M: 'a>(key: impl Into<String>) -> Element<'a, M> {
    container(
        text(key.into())
            .size(theme::SIZE_SECTION_LABEL)
            .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextLabel)),
    )
    .padding(theme::KEY_CAP_PADDING)
    .style(theme::tag_surface(theme::KEY_CAP_RADIUS))
    .into()
}

/// Renders key hints on one line, [`theme::KEY_HINT_SPACING`] apart: each key in its cap, then its
/// action in [`Token::TextIdentity`].
pub fn key_hints<'a, M: 'a>(hints: &[KeyHint]) -> Element<'a, M> {
    Row::with_children(hints.iter().map(|hint| {
        row![
            key_cap(hint.key.clone()),
            text(hint.action.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::TextIdentity)),
        ]
        .spacing(theme::KEY_CAP_SPACING)
        .align_y(Alignment::Center)
        .into()
    }))
    .spacing(theme::KEY_HINT_SPACING)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        let _: Element<'_, ()> = key_cap("Esc");
        let _: Element<'_, ()> = key_hints(&[KeyHint {
            key: "Esc".into(),
            action: "grid".into(),
        }]);
    }
}
