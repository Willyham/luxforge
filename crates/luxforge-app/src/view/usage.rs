//! The usage statistics startup notice: a small card over the bottom of the workspace, above the
//! status bar, with the design's words and its two neutral buttons. It is not modal: it takes no
//! focus and holds no key, the rest of the window stays usable around it, and Escape answers No
//! thanks only when nothing else in the window takes Escape
//! ([design](../../../../docs/design/usage-statistics.md#consent-and-first-launch)).
use crate::{
    app::message::{Message, usage::UsageMessage},
    layout::STATUS_BAR_HEIGHT,
    state::usage::UsageNotice,
};
use iced::{Alignment, Length, Padding, widget::container};
use luxforge_ui::{Element, Icon, NoticeCardModel, Tone, notice_card, theme};

pub(crate) fn notice(model: &UsageNotice) -> Element<'_, Message> {
    let [share, decline] = model.actions;
    // A neutral card, so neither button is drawn as the primary one: neither is preselected.
    let card = notice_card(
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
    );
    // The layer fills the window but only the card takes the pointer, so everything around it
    // stays usable.
    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::End)
        .padding(Padding {
            bottom: STATUS_BAR_HEIGHT + theme::SPACING * 2.0,
            ..Padding::new(theme::SPACING * 2.0)
        })
        .into()
}
