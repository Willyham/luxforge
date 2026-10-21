//! A download's setup sheet over the whole window: the sheet centred on an opaque scrim, which
//! takes every press beside it, so nothing behind it is pressed while it asks
//! ([`crate::state::setup_sheet`]).
use crate::{
    app::message::{Message, capability::CapabilityMessage},
    state::setup_sheet::{FactKind, IntroductionPicture, SetupSheet},
};
use iced::{
    Alignment, Length,
    widget::{Space, container, opaque},
};
use luxforge_ui::{
    Element, FactTone, Icon, KeyHint, SetupFactModel, SetupPicture, SetupSheetModel,
    SetupStepModel, setup_sheet as sheet, theme,
};

/// The sheet and its scrim, while one asks.
pub(crate) fn setup_sheet(model: Option<&SetupSheet>) -> Option<Element<'_, Message>> {
    let model = model?;
    let card = sheet(
        &SetupSheetModel {
            eyebrow: model.eyebrow.clone(),
            title: model.title.clone(),
            lead: model.lead.clone(),
            hero: model.hero.map(picture),
            steps: model
                .steps
                .iter()
                .map(|step| SetupStepModel {
                    title: step.title.clone(),
                    body: step.body.clone(),
                    picture: picture(step.picture),
                    hints: step
                        .hints
                        .iter()
                        .map(|(key, action)| KeyHint {
                            key: key.clone(),
                            action: action.clone(),
                        })
                        .collect(),
                })
                .collect(),
            facts_label: model.facts_label.clone(),
            facts: model
                .facts
                .iter()
                .map(|fact| SetupFactModel {
                    glyph: match fact.kind {
                        FactKind::Size => Icon::Drive,
                        FactKind::Origin => Icon::Information,
                        FactKind::Memory(Some(false)) => Icon::Warning,
                        FactKind::Memory(_) => Icon::Stack,
                    },
                    headline: fact.headline.clone(),
                    detail: fact.detail.clone(),
                    tone: match fact.kind {
                        FactKind::Memory(Some(true)) => FactTone::Met,
                        kind if kind.unmet() => FactTone::Unmet,
                        _ => FactTone::Plain,
                    },
                })
                .collect(),
            notes: model.notes.clone(),
            promise: model.promise.clone(),
            allow: model.allow.clone(),
            dismiss: model.dismiss.clone(),
        },
        Message::Capability(CapabilityMessage::Consent(model.allow.is_some())),
        Message::Capability(CapabilityMessage::Consent(false)),
    );
    Some(
        iced::widget::stack![
            opaque(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::scrim_surface)
            ),
            container(opaque(card))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(theme::SPACING * 2.0)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    )
}

/// The picture the widget draws for one the introduction names.
fn picture(picture: IntroductionPicture) -> SetupPicture {
    match picture {
        IntroductionPicture::PaintOver => SetupPicture::PaintOver,
        IntroductionPicture::Candidates => SetupPicture::Candidates,
        IntroductionPicture::History => SetupPicture::History,
        IntroductionPicture::Describe => SetupPicture::Describe,
    }
}
