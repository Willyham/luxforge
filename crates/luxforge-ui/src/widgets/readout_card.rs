//! A card of exact values: one row per value, its name at the left in the tertiary colour and the
//! value at the right in the secondary colour, on the Canvas surface. The crop draft's readout
//! (input stage, rectangle, output, what Apply commits) is one.

use super::button_row::RowPlacement;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, container, row, text};
use iced::{Element, Length, Padding};

/// The card's height for `rows` rows, its padding included.
pub fn readout_card_height(rows: usize) -> f32 {
    2.0 * theme::READOUT_PADDING_Y + rows as f32 * theme::READOUT_LINE_HEIGHT
}

/// Renders the card. Its margins follow the rows around it like a button row's: `placement.followed`
/// keeps [`theme::BUTTON_ROW_BOTTOM`] under it.
pub fn readout_card<'a, M: 'a>(
    rows: &[(String, String)],
    placement: RowPlacement,
) -> Element<'a, M> {
    let line = |content: &str, color, align| {
        text(content.to_owned())
            .size(theme::SIZE_CAPTION)
            .line_height(LineHeight::Absolute(theme::READOUT_LINE_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .align_x(align)
            .color(color)
    };
    let body = Column::with_children(rows.iter().map(|(name, value)| {
        row![
            line(name, theme::TEXT_TERTIARY, Horizontal::Left),
            line(value, theme::TEXT_SECONDARY, Horizontal::Right).width(Length::Fill),
        ]
        .spacing(theme::SPACING)
        .into()
    }));
    let card = container(body)
        .padding(Padding {
            top: theme::READOUT_PADDING_Y,
            right: theme::READOUT_PADDING_X,
            bottom: theme::READOUT_PADDING_Y,
            left: theme::READOUT_PADDING_X,
        })
        .width(Length::Fill)
        .style(theme::readout_surface);
    let mut padding = placement.padding();
    padding.top = theme::READOUT_MARGIN;
    container(card).padding(padding).width(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// crop-and-straighten.png: four rows on a 16 pt pitch in a 75 pt card.
    #[test]
    fn the_card_matches_the_crop_reference() {
        assert_eq!(readout_card_height(4), 75.0);
        let _: Element<'_, ()> = readout_card(
            &[("Input stage".into(), "480 × 320".into())],
            RowPlacement::default(),
        );
    }
}
