//! Text helpers at the sizes and colours the visual language defines.

use crate::Element;
use crate::theme::{self, Token};
use iced::Length;
use iced::alignment::Horizontal;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{container, text};

/// 13 pt semibold text: module and section titles.
pub fn title<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .style(theme::ink(Token::Text))
        .into()
}

/// 12 pt primary text: control labels and body copy.
pub fn label<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CONTROL)
        .style(theme::ink(Token::Text))
        .into()
}

/// 11 pt tertiary text: hints, readouts and secondary detail.
pub fn caption<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CAPTION)
        .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
        .style(theme::ink(Token::TextTertiary))
        .into()
}

/// An 11 pt caption in the error ink: an invalid value's range message or an unavailable
/// provider's reason. Luxforge Dark's error ink is the clipping-highlight red, as the components
/// board renders both of these states.
pub fn error_caption<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CAPTION)
        .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
        .style(theme::ink(Token::Error))
        .into()
}

/// 10.5 pt semibold capitalised section label (Versions, History, Adjustments). Iced has no letter
/// spacing, so the boards' tracking is not drawn.
pub fn section_label<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into().to_uppercase())
        .size(theme::SIZE_SECTION_LABEL)
        .font(theme::FONT_SEMIBOLD)
        .style(theme::ink(Token::TextTertiary))
        .wrapping(Wrapping::None)
        .into()
}

/// A value right-aligned in a fixed-width box, so its last digit never moves as its width
/// changes, without depending on true tabular (fixed-width) numerals.
///
/// Iced's text renderer has no OpenType feature switch, so it cannot request the `tnum` font
/// feature the visual language calls for. Right-aligning in a fixed box gives the property that
/// matters here — a value's units place stays put as its tens or sign change — without it.
pub(crate) fn value_text<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    container(
        text(content.into())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .align_x(Horizontal::Right)
            .width(Length::Fill)
            .style(theme::ink(Token::Text)),
    )
    .padding(iced::Padding::default().right(theme::VALUE_INSET))
    .width(Length::Fixed(theme::VALUE_WIDTH))
    .into()
}

/// A control's label on its label line: 12 pt in the label colour, one line
/// [`theme::SLIDER_LABEL_HEIGHT`] tall, so a slider row keeps its pitch.
pub(crate) fn control_label<'a>(
    content: impl Into<String>,
    enabled: bool,
) -> iced::widget::Text<'a, crate::Theme> {
    text(content.into())
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .style(theme::ink(if enabled {
            Token::TextLabel
        } else {
            Token::TextTertiary
        }))
}

/// 11 pt semibold secondary text in sentence case: a sub-group's label (White balance, Tone).
pub(crate) fn group_label<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CAPTION)
        .font(theme::FONT_SEMIBOLD)
        .style(theme::ink(Token::TextSecondary))
        .wrapping(Wrapping::None)
        .into()
}
