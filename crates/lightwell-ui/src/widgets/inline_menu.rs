//! A small vertical menu of labelled actions on the control surface.
//!
//! The caller places it: this widget only renders the list, not a popover or its position.

use crate::theme;
use iced::widget::{Column, button, container, text};
use iced::{Element, Length};

/// Renders an inline menu: one `(label, message)` action per row, in order.
pub fn inline_menu<'a, M: Clone + 'a>(items: Vec<(String, M)>) -> Element<'a, M> {
    let mut list = Column::new();

    for (label, message) in items {
        list = list.push(
            button(text(label).size(theme::SIZE_CONTROL).width(Length::Fill))
                .padding([theme::SPACING / 2.0, theme::SPACING])
                .width(Length::Fill)
                .style(theme::button_plain)
                .on_press(message),
        );
    }

    container(list)
        .padding(4.0)
        .style(theme::control_surface)
        .into()
}
