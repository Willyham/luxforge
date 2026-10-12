//! The usage statistics startup notice, and the layer the workspace's notices share: a small card
//! over the bottom of the workspace, above the
//! floating strip at the foot of the canvas or the grid (and Develop's filmstrip, when it is
//! shown), so it covers none of their controls, with the design's words and its two neutral
//! buttons. It is not modal: it takes no
//! focus and holds no key, the rest of the window stays usable around it, and Escape answers No
//! thanks only when nothing else in the window takes Escape
//! ([design](../../../../docs/design/usage-statistics.md#consent-and-first-launch)).
use crate::{
    app::message::{Message, usage::UsageMessage},
    layout::{DIVIDER_WIDTH, FILMSTRIP_HEIGHT, STATUS_BAR_HEIGHT},
    state::usage::UsageNotice,
};
use iced::{
    Alignment, Length, Padding,
    widget::{column, container, mouse_area},
};
use luxforge_ui::{Element, Icon, NoticeCardModel, Tone, notice_card, theme};

/// How far above the window's bottom edge the card sits: the status bar and its rule, the
/// filmstrip when `filmstrip` is shown, and the floating strip with its inset above and below.
pub(crate) fn clearance(filmstrip: bool) -> f32 {
    STATUS_BAR_HEIGHT
        + DIVIDER_WIDTH
        + if filmstrip { FILMSTRIP_HEIGHT } else { 0.0 }
        + 2.0 * theme::CHROME_INSET
        + theme::STRIP_HEIGHT
}

/// The startup notice's card.
pub(crate) fn card(model: &UsageNotice) -> Element<'_, Message> {
    let [share, decline] = model.actions;
    // A neutral card, so neither button is drawn as the primary one: neither is preselected.
    notice_card(
        &NoticeCardModel {
            icon: Icon::Information,
            title: model.title.to_owned(),
            body: model.body.clone(),
            tone: Tone::Neutral,
        },
        vec![
            (share.to_owned(), Message::Usage(UsageMessage::Share)),
            (decline.to_owned(), Message::Usage(UsageMessage::Decline)),
        ],
    )
}

/// The workspace's notices, stacked over the bottom of the workspace above the floating strip
/// (and Develop's filmstrip when `filmstrip` is shown): the update offer over the usage statistics
/// notice when both are up.
pub(crate) fn layer<'a>(cards: Vec<Element<'a, Message>>, filmstrip: bool) -> Element<'a, Message> {
    // The layer fills the window but only the cards take the pointer, so everything around them
    // stays usable. Each card claims it over all of itself, so a brush under it draws no cursor of
    // its own there.
    let cards = cards.into_iter().fold(
        column![].spacing(theme::SPACING).align_x(Alignment::Center),
        |cards, card| cards.push(mouse_area(card).interaction(iced::mouse::Interaction::Idle)),
    );
    container(cards)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::End)
        .padding(Padding {
            bottom: clearance(filmstrip),
            ..Padding::new(theme::SPACING * 2.0)
        })
        .into()
}
