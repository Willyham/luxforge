//! The status bar: the last message and the one control that makes it usable elsewhere, then the
//! run's own facts at the trailing edge — who else is connected, what the renderer did and what the
//! current zoom means on this display.
use crate::{app::message::Message, state::status::StatusBarModel};
use iced::{
    Alignment, Element, Length,
    widget::{Row, button, row, text},
};
use lightwell_ui::{caption, theme};

/// The separator between the trailing captions.
const DOT: &str = "\u{00b7}";

pub(crate) fn status_bar(model: &StatusBarModel) -> Element<'_, Message> {
    let mut trailing = Row::new()
        .spacing(theme::SPACING / 2.0)
        .align_y(Alignment::Center);
    let fields = [
        model.clients.clone(),
        model.render.clone(),
        format!("{} {DOT} {}", model.zoom_text, model.scale_text),
    ];
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            trailing = trailing.push(caption(DOT));
        }
        trailing = trailing.push(caption(field));
    }
    row![
        text(&model.message)
            .size(theme::SIZE_CONTROL)
            .color(theme::TEXT_PRIMARY),
        button(text("Copy").size(theme::SIZE_CAPTION))
            .padding([2.0, 6.0])
            .style(theme::button_plain)
            .on_press(Message::CopyStatus),
        iced::widget::Space::new().width(Length::Fill),
        trailing,
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center)
    .into()
}
