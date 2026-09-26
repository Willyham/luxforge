//! The status bar: a plain sentence of what last happened and the one control that makes it usable
//! elsewhere, then the pointer readout, then the run's own facts at the trailing edge — who else is
//! connected, what kind of frame the renderer put on screen and how long it took, and what the
//! current zoom comes to on this display.
use crate::{
    app::message::{Message, ViewMessage},
    state::status::StatusBarModel,
};
use iced::{
    Alignment, Element, Length, Theme,
    alignment::Horizontal,
    widget::{Space, container, row, text},
};
use luxforge_ui::{Icon, IconButtonModel, caption, header_icon_button, theme, truncated_text};

/// The separator between the readout and the facts after it.
const DOT: &str = "\u{00b7}";

/// The pointer readout's slot. It is always laid out, empty while the pointer is off the
/// photograph, so the readout appearing, changing width or clearing never moves the message or the
/// trailing facts. Wide enough for the longest readout there can be — three codes of 255 at
/// coordinates of five digits, the 16384 px side limit — with its separator.
pub(crate) const READOUT_WIDTH: f32 = 240.0;

pub(crate) fn status_bar(model: &StatusBarModel) -> Element<'_, Message> {
    // The sentence hugs its text, so Copy follows it, and ends in an ellipsis before it would push
    // the readout or the facts along.
    let message = row![
        truncated_text(
            model.message.clone(),
            theme::SIZE_CAPTION,
            theme::FONT,
            theme::TEXT_SECONDARY,
        ),
        header_icon_button(
            &IconButtonModel {
                icon: Icon::Copy,
                tooltip: "Copy the status".into(),
                enabled: true,
                selected: false,
            },
            Some(Message::View(ViewMessage::CopyStatus)),
        ),
    ]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center);
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
    let connected = model.agents_connected;
    let dot = container(Space::new())
        .width(Length::Fixed(theme::STATUS_DOT_SIZE))
        .height(Length::Fixed(theme::STATUS_DOT_SIZE))
        .style(move |_: &Theme| {
            container::Style::default()
                .background(if connected {
                    theme::AGENT_CONNECTED
                } else {
                    theme::TEXT_TERTIARY
                })
                .border(iced::Border {
                    radius: (theme::STATUS_DOT_SIZE / 2.0).into(),
                    ..iced::Border::default()
                })
        });
    let fact = |value: &str| {
        text(value.to_owned())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_TERTIARY)
            .wrapping(text::Wrapping::None)
    };
    let facts = row![
        row![dot, fact(&model.clients)]
            .spacing(theme::STATUS_DOT_SIZE)
            .align_y(Alignment::Center),
        fact(&model.render),
        fact(&model.view),
    ]
    .spacing(theme::STATUS_FACT_SPACING)
    .align_y(Alignment::Center);
    // A shrinking container lays the sentence out at its own width, where a filling one would give
    // it the whole slot and push Copy to the slot's far end.
    row![
        container(container(message).width(Length::Shrink)).width(Length::Fill),
        readout,
        facts,
    ]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center)
    .into()
}
