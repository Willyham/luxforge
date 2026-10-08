//! The status bar: a plain sentence of what last happened and the one control that makes it usable
//! elsewhere, then the run's own facts at the trailing edge — who else is
//! connected, what kind of frame the renderer put on screen and how long it took, why a gesture is
//! drawn on the slower CPU path while a reason for it lasts, and what the current zoom comes to on
//! this display.
use crate::{
    app::message::{Message, view::ViewMessage},
    state::{long_work::LongWorkModel, status::StatusBarModel},
};
use iced::{
    Alignment, Length,
    widget::{Space, container, row, text, tooltip},
};
use luxforge_ui::{Element, Theme, Token};
use luxforge_ui::{Icon, IconButtonModel, header_icon_button, theme, truncated_text};

/// The widest the notice's tooltip grows before its sentence wraps.
const NOTICE_TOOLTIP_WIDTH: f32 = 320.0;

pub(crate) fn status_bar<'a>(
    model: &'a StatusBarModel,
    work: &'a LongWorkModel,
    report: bool,
) -> Element<'a, Message> {
    // The sentence hugs its text, so Copy follows it, and ends in an ellipsis before it would push
    // the facts along.
    let mut message = row![
        truncated_text(
            model.message.clone(),
            theme::SIZE_CAPTION,
            theme::FONT,
            Token::TextSecondary,
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
    if report {
        message = message.push(crate::view::select_catalog::status_report());
    }
    let connected = model.agents_connected;
    let dot = container(Space::new())
        .width(Length::Fixed(theme::STATUS_DOT_SIZE))
        .height(Length::Fixed(theme::STATUS_DOT_SIZE))
        .style(move |theme: &Theme| {
            container::Style::default()
                .background(if connected {
                    theme::AGENT_CONNECTED
                } else {
                    theme.palette().text_tertiary
                })
                .border(iced::Border {
                    radius: (theme::STATUS_DOT_SIZE / 2.0).into(),
                    ..iced::Border::default()
                })
        });
    let fact = |value: &str| {
        text(value.to_owned())
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(Token::TextTertiary))
            .wrapping(text::Wrapping::None)
    };
    // Why the gesture is drawn on the CPU, a muted phrase right after the render slot with its
    // sentence on hover; nothing is drawn on the photograph.
    let notice: Option<Element<'a, Message>> = model.fallback.as_ref().map(|notice| {
        tooltip(
            text(notice.phrase.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextSecondary))
                .wrapping(text::Wrapping::None),
            container(
                text(notice.tooltip.clone())
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(Token::Text)),
            )
            .max_width(NOTICE_TOOLTIP_WIDTH)
            .padding(theme::TOOLTIP_PADDING)
            .style(theme::bar_surface),
            tooltip::Position::Top,
        )
        .into()
    });
    // Long-running work's busiest job leads the facts while any job runs.
    let mut facts = row![]
        .spacing(theme::STATUS_FACT_SPACING)
        .align_y(Alignment::Center);
    if let Some(job) = crate::view::long_work::busiest(work) {
        facts = facts.push(job);
    }
    let facts = facts
        .push(
            row![dot, fact(&model.clients)]
                .spacing(theme::STATUS_DOT_SIZE)
                .align_y(Alignment::Center),
        )
        .push(fact(&model.render))
        .extend(notice)
        .push(fact(&model.view));
    // A shrinking container lays the sentence out at its own width, where a filling one would give
    // it the whole slot and push Copy to the slot's far end.
    row![
        container(container(message).width(Length::Shrink)).width(Length::Fill),
        facts,
    ]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center)
    .into()
}
