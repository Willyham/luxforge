//! A rounded, selectable label chip (versions, ratio presets, filters), and the wrapping row that
//! holds a set of them.
//!
//! A chip is [`theme::CHIP_HEIGHT`] tall on the Control surface; a selected chip is tinted with the
//! accent ([`theme::SELECTED_FILL`]) and its label is in the accent, as the crop reference draws
//! the chosen ratio.

use super::button_row::RowPlacement;
use super::text::caption;
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, button, container, mouse_area, row, text};
use iced::{Alignment, Element, Length, Padding};

/// Plain data for one chip.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipModel {
    pub label: String,
    /// A short trailing caption, e.g. an entry number.
    pub trailing: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}

/// Renders one chip. `on_context` fires on a right-click, e.g. to open a context menu.
pub fn chip<'a, M: Clone + 'a>(
    model: &ChipModel,
    on_press: Option<M>,
    on_context: Option<M>,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::chip_selected
    } else {
        theme::button_control
    };
    let ink = match (model.enabled, model.selected) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => theme::ACCENT,
        (true, false) => theme::CHIP_LABEL,
    };
    let mut content = row![
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(ink)
    ]
    .spacing(theme::CHIP_TRAILING_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);

    if let Some(trailing) = &model.trailing {
        content = content.push(caption(trailing.clone()));
    }

    let control = button(content)
        .padding(Padding {
            top: 0.0,
            right: theme::CHIP_PADDING,
            bottom: 0.0,
            left: theme::CHIP_PADDING,
        })
        .height(Length::Fixed(theme::CHIP_HEIGHT))
        .style(style)
        .on_press_maybe(model.enabled.then_some(on_press).flatten());

    let area = mouse_area(control);

    match on_context.filter(|_| model.enabled) {
        Some(message) => area.on_right_press(message).into(),
        None => area.into(),
    }
}

/// Renders one compact chip, as the state panel draws a version: [`theme::VERSION_CHIP_HEIGHT`]
/// tall with its label and its trailing caption as one caption-sized line (`Warm · 5`) in the
/// chip's ink. `on_context` fires on a right-click.
pub fn compact_chip<'a, M: Clone + 'a>(
    model: &ChipModel,
    on_press: Option<M>,
    on_context: Option<M>,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::chip_selected
    } else {
        theme::button_control
    };
    let ink = match (model.enabled, model.selected) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => theme::ACCENT,
        (true, false) => theme::CHIP_LABEL,
    };
    let label = match &model.trailing {
        Some(trailing) => format!("{} \u{b7} {trailing}", model.label),
        None => model.label.clone(),
    };
    let control = button(
        container(
            text(label)
                .size(theme::SIZE_CAPTION)
                .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
                .wrapping(Wrapping::None)
                .color(ink),
        )
        .center_y(Length::Fill),
    )
    .padding([0.0, theme::VERSION_CHIP_PADDING])
    .height(Length::Fixed(theme::VERSION_CHIP_HEIGHT))
    .style(move |iced_theme: &iced::Theme, status| {
        let mut style = style(iced_theme, status);
        style.border.radius = theme::VERSION_CHIP_RADIUS.into();
        style
    })
    .on_press_maybe(model.enabled.then_some(on_press).flatten());
    let area = mouse_area(control);
    match on_context.filter(|_| model.enabled) {
        Some(message) => area.on_right_press(message).into(),
        None => area.into(),
    }
}

/// Sets chips in rows that wrap at the panel's width, [`theme::CHIP_SPACING`] apart both ways,
/// with the button row's margin above and, when another row follows, [`theme::CHIP_ROW_BOTTOM`]
/// below.
pub fn chip_row<'a, M: 'a>(chips: Vec<Element<'a, M>>, placement: RowPlacement) -> Element<'a, M> {
    let mut padding = placement.padding();
    padding.bottom = if placement.followed {
        theme::CHIP_ROW_BOTTOM
    } else {
        0.0
    };
    container(chip_wrap(chips))
        .padding(padding)
        .width(Length::Fill)
        .into()
}

/// Chips set in rows that wrap at the available width, [`theme::CHIP_SPACING`] apart both ways,
/// with no margin of their own: an enum's choices under its label.
pub fn chip_wrap<'a, M: 'a>(chips: Vec<Element<'a, M>>) -> Element<'a, M> {
    Row::with_children(chips)
        .spacing(theme::CHIP_SPACING)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(theme::CHIP_SPACING)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// crop-and-straighten.png: 22 pt chips 4 pt apart, the selected one tinted.
    #[test]
    fn chips_match_the_crop_reference() {
        assert_eq!(theme::CHIP_HEIGHT, 22.0);
        assert_eq!(theme::CHIP_SPACING, 4.0);
        assert_eq!(theme::SELECTED_FILL, iced::Color::from_rgb8(62, 55, 46));
        for selected in [false, true] {
            let _: Element<'_, ()> = chip_row(
                vec![chip(
                    &ChipModel {
                        label: "Original".into(),
                        trailing: None,
                        selected,
                        enabled: true,
                    },
                    Some(()),
                    None,
                )],
                RowPlacement::default(),
            );
        }
    }
}
