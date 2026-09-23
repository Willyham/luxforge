//! The status bar: the last message and the one control that makes it usable elsewhere, then the
//! pointer readout, then the run's own facts at the trailing edge — who else is connected, what the
//! renderer did and what the current zoom means on this display.
use crate::{app::message::Message, state::status::StatusBarModel};
use iced::{
    Alignment, Element, Length,
    alignment::Horizontal,
    widget::{Row, Space, button, container, row, text},
};
use lightwell_ui::{caption, theme};

/// The separator between the trailing captions.
const DOT: &str = "\u{00b7}";

/// The pointer readout's slot. It is always laid out, empty while the pointer is off the
/// photograph, so the readout appearing, changing width or clearing never moves the message or the
/// trailing facts. Wide enough for the longest readout there can be — three codes of 255 at
/// coordinates of five digits, the 16384 px side limit — with its separator.
pub(crate) const READOUT_WIDTH: f32 = 240.0;

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
    let readout: Element<'_, Message> = match &model.readout {
        Some(readout) => row![
            text(readout.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_SECONDARY)
                .wrapping(text::Wrapping::None),
            caption(DOT),
        ]
        .spacing(theme::SPACING / 2.0)
        .align_y(Alignment::Center)
        .into(),
        None => Space::new().into(),
    };
    let readout = container(readout)
        .width(Length::Fixed(READOUT_WIDTH))
        .align_x(Horizontal::Right)
        .clip(true);
    row![
        text(&model.message)
            .size(theme::SIZE_CONTROL)
            .color(theme::TEXT_PRIMARY),
        button(text("Copy").size(theme::SIZE_CAPTION))
            .padding([2.0, 6.0])
            .style(theme::button_plain)
            .on_press(Message::CopyStatus),
        Space::new().width(Length::Fill),
        readout,
        trailing,
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center)
    .into()
}
