//! The move question's card, in the stack of workspace notices over the bottom of the workspace
//! ([`super::usage::layer`]), with the design's words, its buttons and the Don't ask again switch.
//! It is not modal: it takes no focus and holds no key, and the rest of the window stays usable
//! around it ([design](../../../../docs/design/auto-update.md#moving-into-applications)).
use crate::{
    app::message::{Message, move_to_applications::MoveMessage},
    state::move_to_applications::{DONT_ASK, MoveAction, MoveNotice},
};
use luxforge_ui::{
    Element, Icon, NoticeCardModel, ToggleModel, Tone, compact_toggle, notice_card_with,
};

fn message(action: MoveAction) -> Message {
    Message::MoveToApplications(match action {
        MoveAction::Move => MoveMessage::Move,
        MoveAction::NotNow => MoveMessage::NotNow,
        MoveAction::Replace => MoveMessage::Replace,
        MoveAction::Cancel => MoveMessage::Cancel,
    })
}

pub(crate) fn card(model: &MoveNotice) -> Element<'_, Message> {
    let footer = model.dont_ask.map(|on| {
        compact_toggle(
            &ToggleModel {
                label: DONT_ASK.into(),
                on,
                enabled: true,
            },
            None,
            |on| Message::MoveToApplications(MoveMessage::DontAsk(on)),
        )
    });
    // A neutral card: Move and relaunch is offered first, and nothing is preselected.
    notice_card_with(
        &NoticeCardModel {
            icon: Icon::Information,
            title: model.title.to_owned(),
            body: model.body.clone(),
            tone: Tone::Neutral,
        },
        model
            .actions
            .iter()
            .map(|(label, action)| ((*label).to_owned(), message(*action)))
            .collect(),
        footer,
    )
}
