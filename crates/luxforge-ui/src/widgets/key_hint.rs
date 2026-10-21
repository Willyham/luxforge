//! Keys drawn as caps: the loupe's key hints (`.hint`) and a sheet step's hint line (`.where`).

use crate::theme;
use crate::{Element, Token};
use iced::Alignment;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, container, text};

/// One key hint: the keys pressed together, each in its cap, and what they do. A hint without keys
/// is words alone, such as where a step is done.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyHint {
    pub keys: Vec<String>,
    pub action: String,
}

impl KeyHint {
    /// One key and what it does.
    pub fn new(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            keys: vec![key.into()],
            action: action.into(),
        }
    }
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

/// An action's words beside its keys, in `ink`.
fn action<'a, M: 'a>(words: &str, ink: Token) -> Element<'a, M> {
    text(words.to_owned())
        .size(theme::SIZE_CAPTION)
        .wrapping(Wrapping::None)
        .style(theme::ink(ink))
        .into()
}

/// Renders key hints on one line, [`theme::KEY_HINT_SPACING`] apart: each key in its cap, then its
/// action in [`Token::TextIdentity`].
pub fn key_hints<'a, M: 'a>(hints: &[KeyHint]) -> Element<'a, M> {
    Row::with_children(hints.iter().map(|hint| {
        let mut keys = Row::with_children(hint.keys.iter().cloned().map(key_cap))
            .spacing(theme::KEY_CAP_SPACING)
            .align_y(Alignment::Center);
        if !hint.action.is_empty() {
            keys = keys.push(action(&hint.action, Token::TextIdentity));
        }
        keys.into()
    }))
    .spacing(theme::KEY_HINT_SPACING)
    .align_y(Alignment::Center)
    .into()
}

/// Renders a sheet step's hint line: every cap and every action [`theme::KEY_CAP_SPACING`] apart in
/// [`Token::TextTertiary`], wrapping onto further lines in a narrow card.
pub fn hint_line<'a, M: 'a>(hints: &[KeyHint]) -> Element<'a, M> {
    Row::with_children(hints.iter().flat_map(|hint| {
        hint.keys
            .iter()
            .cloned()
            .map(key_cap)
            .chain((!hint.action.is_empty()).then(|| action(&hint.action, Token::TextTertiary)))
            .collect::<Vec<_>>()
    }))
    .spacing(theme::KEY_CAP_SPACING)
    .align_y(Alignment::Center)
    .wrap()
    .vertical_spacing(theme::KEY_CAP_SPACING)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        let hints = [
            KeyHint::new("Esc", "grid"),
            KeyHint {
                keys: vec!["Shift".into(), "Click".into()],
                action: "add".into(),
            },
            KeyHint {
                keys: Vec::new(),
                action: "Pick, on the row".into(),
            },
        ];
        let _: Element<'_, ()> = key_cap("Esc");
        let _: Element<'_, ()> = key_hints(&hints);
        let _: Element<'_, ()> = hint_line(&hints);
    }
}
