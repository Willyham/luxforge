//! The status bar: the last message and the one control that makes it usable elsewhere.
use crate::{app::message::Message, state::status::StatusBarModel};
use iced::{
    Alignment, Element, Length,
    widget::{button, row, text},
};

pub(crate) fn status_bar(model: &StatusBarModel) -> Element<'_, Message> {
    row![
        text(&model.message).size(12).width(Length::Fill),
        button(text("Copy message").size(12))
            .style(button::secondary)
            .on_press(Message::CopyStatus),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}
