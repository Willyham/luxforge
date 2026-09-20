//! The state panel: history, named versions and the recipe of the displayed entry.
use crate::{
    app::message::Message,
    state::panel::{Marker, StatePanelModel},
};
use iced::{
    Element, Length,
    widget::{button, column, row, text, text_input},
};

pub(crate) fn state_panel(model: &StatePanelModel) -> Element<'_, Message> {
    column![
        history(model),
        versions(model),
        text("Layer stack").size(18),
        text(recipe(model)).size(11),
    ]
    .spacing(18)
    .into()
}

fn history(model: &StatePanelModel) -> Element<'_, Message> {
    let editable = !model.busy && model.preview.is_none() && !model.history.is_empty();
    let mut rows = column![
        row![
            text("History").size(18),
            button("Undo").on_press_maybe(editable.then_some(Message::Undo)),
            button("Redo").on_press_maybe(editable.then_some(Message::Redo)),
        ]
        .spacing(6)
    ]
    .spacing(5);
    for entry in &model.history {
        let marker = match entry.marker {
            Marker::Current => "●",
            Marker::Previewed => "◉",
            Marker::Plain => "○",
        };
        let branch = if entry.branch { " · branch" } else { "" };
        let label = format!(
            "{marker} {} · {} · {}{branch}",
            entry.sequence, entry.label, entry.actor
        );
        rows = rows.push(
            button(text(label).size(12))
                .width(Length::Fill)
                .on_press_maybe((!model.busy).then(|| Message::Preview(entry.entry_id.clone()))),
        );
    }
    if model.can_load_older {
        rows = rows.push(
            button("Load older history")
                .on_press_maybe((!model.busy).then_some(Message::LoadOlder)),
        );
    }
    if let Some(preview) = model.preview {
        rows = rows.push(
            row![
                button("Return to current")
                    .on_press_maybe(preview.can_return.then_some(Message::ReturnCurrent)),
                button("Restore this state")
                    .on_press_maybe(preview.can_restore.then_some(Message::Restore)),
            ]
            .spacing(6),
        );
    }
    rows.into()
}

fn versions(model: &StatePanelModel) -> Element<'_, Message> {
    let mut rows = column![
        text("Versions").size(18),
        row![
            text_input("Name the displayed state", &model.version_name)
                .on_input(Message::VersionName)
                .on_submit(Message::SaveVersion)
                .width(Length::Fill),
            button("Save").on_press_maybe(model.can_save.then_some(Message::SaveVersion)),
        ]
        .spacing(6),
    ]
    .spacing(5);
    for version in &model.versions {
        let marker = if version.selected { "◉" } else { "○" };
        let label = format!(
            "{marker} {} · entry {}",
            version.name, version.entry_sequence
        );
        rows = rows.push(
            row![
                button(text(label).size(12))
                    .width(Length::Fill)
                    .on_press_maybe(
                        (!model.busy).then(|| Message::Preview(version.entry_id.clone()))
                    ),
                button(text("Delete").size(12)).on_press_maybe(
                    (!model.busy).then(|| Message::DeleteVersion(version.name.clone()))
                ),
            ]
            .spacing(6),
        );
    }
    rows.into()
}

/// The recipe as the owner described it: one row per stored layer, in processing order.
fn recipe(model: &StatePanelModel) -> String {
    if model.recipe.is_empty() {
        return if model.history.is_empty() {
            "No layer selected".into()
        } else {
            "Original · no edit layers".into()
        };
    }
    model
        .recipe
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            let availability = if layer.available {
                ""
            } else {
                " · unavailable"
            };
            format!(
                "{}: {} · {}{availability}",
                index + 1,
                layer.title,
                layer.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
