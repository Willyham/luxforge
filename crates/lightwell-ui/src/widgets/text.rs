//! Text helpers at the sizes and colours the visual language defines.

use crate::theme;
use iced::widget::{container, text};
use iced::{Element, Font, Length, font::Weight};

/// 13 pt semibold text: module and section titles.
pub fn title<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_TITLE)
        .font(Font {
            weight: Weight::Semibold,
            ..Font::DEFAULT
        })
        .color(theme::TEXT_PRIMARY)
        .into()
}

/// 12 pt primary text: control labels and body copy.
pub fn label<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CONTROL)
        .color(theme::TEXT_PRIMARY)
        .into()
}

/// 11 pt tertiary text: hints, readouts and secondary detail.
pub fn caption<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CAPTION)
        .color(theme::TEXT_TERTIARY)
        .into()
}

/// An 11 pt caption in the clipping-highlight red: an invalid value's range message or an
/// unavailable provider's reason. The visual language reserves red for clipping in principle, but
/// the components board renders both of these states in exactly that colour, so this widget set
/// follows the rendered evidence rather than the stated principle.
pub fn error_caption<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into())
        .size(theme::SIZE_CAPTION)
        .color(theme::CLIPPING_HIGHLIGHT)
        .into()
}

/// 10.5 pt capitalised sub-group label (White balance, Tone, Color).
pub fn section_label<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    text(content.into().to_uppercase())
        .size(theme::SIZE_SECTION_LABEL)
        .color(theme::TEXT_TERTIARY)
        .into()
}

/// A value right-aligned in a fixed-width box, so its last digit never moves as its width
/// changes, without depending on true tabular (fixed-width) numerals.
///
/// Iced's text renderer has no OpenType feature switch, so it cannot request the `tnum` font
/// feature the visual language calls for. Right-aligning in a fixed box gives the property that
/// matters here — a value's units place stays put as its tens or sign change — without it.
pub fn value_text<'a, M: Clone + 'a>(content: impl Into<String>) -> Element<'a, M> {
    container(
        text(content.into())
            .size(theme::SIZE_CONTROL)
            .color(theme::TEXT_PRIMARY),
    )
    .align_right(Length::Fixed(theme::VALUE_WIDTH))
    .into()
}
