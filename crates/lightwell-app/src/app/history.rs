//! History and versions: undo, redo and restore, selecting an entry for preview and returning to
//! current, holding the Original for comparison, loading older rows, and naming versions.
use super::{
    Editor,
    gesture::Starting,
    message::{HistoryMessage, Message, PreviewMessage},
    tasks::{self, mutation, older_task, preview_task, versions_task},
};
use iced::Task;
use lightwell_core::HistorySelection;
use serde_json::{Value, json};

impl Editor {
    /// One history or versions message.
    pub(super) fn history_update(&mut self, message: HistoryMessage) -> Task<Message> {
        match message {
            HistoryMessage::Selected(result) => {
                // The selection's own answer ends the request that set `busy`, whether or not
                // something newer overtook the frame it carries.
                self.busy = false;
                return self.dispatch(Message::Preview(PreviewMessage::Loaded(result)));
            }
            HistoryMessage::VersionsLoaded(result) => {
                self.busy = false;
                match result {
                    Ok((versions, request)) => {
                        self.versions = versions;
                        self.read_back(request);
                        self.version_name.clear();
                        self.status = "Versions updated".into();
                    }
                    Err(error) => self.status = error,
                }
            }
            HistoryMessage::OlderLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(page) => {
                        self.history.entries.extend(page.entries);
                        self.history.next_before_sequence = page.next_before_sequence;
                        self.status = "Loaded older history".into();
                    }
                    Err(error) => self.status = error,
                }
            }
            HistoryMessage::CompareBegin => {
                // Compare selects the Original entry, which pauses an open draft: the draft would
                // have to be resumed on release, and the design keeps one draft and one preview
                // selection at a time. Refuse it and say so rather than pausing silently.
                if let Some(reason) = self.gesture_refusal(Starting::Compare) {
                    self.status = reason;
                    return Task::none();
                }
                let (Some(state), Some(original)) = (&self.state, self.original_entry.clone())
                else {
                    return Task::none();
                };
                if self.compare_return.is_some() {
                    return Task::none();
                }
                self.compare_return = Some(self.session.preview.selection.clone());
                let asset = state.asset.id.clone();
                self.status = "Comparing with the original…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset.clone(),
                    Some(original.clone()),
                    "preview.select",
                    json!({"asset_id":asset,"entry_id":original}),
                    proxy,
                    |value| Message::Preview(PreviewMessage::Loaded(value)),
                );
            }
            HistoryMessage::CompareEnd => {
                let (Some(state), Some(previous)) = (&self.state, self.compare_return.take())
                else {
                    return Task::none();
                };
                let asset = state.asset.id.clone();
                let proxy = self.proxy_bounds();
                return match previous {
                    HistorySelection::Current => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset,
                        None,
                        "preview.return-current",
                        json!({}),
                        proxy,
                        |value| Message::Preview(PreviewMessage::Loaded(value)),
                    ),
                    HistorySelection::Entry(entry_id) => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset.clone(),
                        Some(entry_id.clone()),
                        "preview.select",
                        json!({"asset_id":asset,"entry_id":entry_id}),
                        proxy,
                        |value| Message::Preview(PreviewMessage::Loaded(value)),
                    ),
                };
            }
            HistoryMessage::VersionName(value) => self.version_name = value,
            HistoryMessage::ToggleVersionForm => self.version_form_open = !self.version_form_open,
            HistoryMessage::Undo | HistoryMessage::Redo => {
                let undo = matches!(message, HistoryMessage::Undo);
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let method = if undo { "history.undo" } else { "history.redo" };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
                return self.command(method, params);
            }
            HistoryMessage::Select(entry_id) => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                // The current row is the live state: selecting it returns to current instead of
                // starting a historical preview, which is also what the API does with that entry.
                if state.current_entry.id == entry_id {
                    return self.update(Message::History(HistoryMessage::ReturnCurrent));
                }
                let params = json!({"asset_id":state.asset.id,"entry_id":entry_id});
                let asset = state.asset.id.clone();
                self.busy = true;
                self.status = "Selecting history state…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    Some(entry_id),
                    "preview.select",
                    params,
                    proxy,
                    |value| Message::History(HistoryMessage::Selected(value)),
                );
            }
            HistoryMessage::ReturnCurrent => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                let asset = state.asset.id.clone();
                self.busy = true;
                self.status = "Returning to current state…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    None,
                    "preview.return-current",
                    json!({}),
                    proxy,
                    |value| Message::History(HistoryMessage::Selected(value)),
                );
            }
            HistoryMessage::Restore => {
                let (Some(state), HistorySelection::Entry(entry_id)) =
                    (&self.state, &self.session.preview.selection)
                else {
                    return Task::none();
                };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision),"entry_id":entry_id});
                return self.command("history.restore", params);
            }
            HistoryMessage::SaveVersion => {
                let (Some(state), Some(entry_id)) = (&self.state, &self.display_entry) else {
                    return Task::none();
                };
                let name = self.version_name.trim().to_string();
                if name.is_empty() {
                    self.status = "Enter a version name first".into();
                    return Task::none();
                }
                let params = json!({"asset_id":state.asset.id,"name":name,"mutation":tasks::request(),"entry_id":entry_id});
                self.version_form_open = false;
                return self.version_command("version.create", params);
            }
            HistoryMessage::DeleteVersion(name) => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let params =
                    json!({"asset_id":state.asset.id,"name":name,"mutation":tasks::request()});
                return self.version_command("version.delete", params);
            }
            HistoryMessage::LoadOlder => {
                let (Some(state), Some(before)) = (&self.state, self.history.next_before_sequence)
                else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                let asset = state.asset.id.clone();
                self.busy = true;
                return older_task(self.owner.clone(), self.client, asset, before);
            }
        }
        Task::none()
    }

    pub(super) fn version_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status = format!("Running {method}…");
        versions_task(self.owner.clone(), self.client, asset, method, params)
    }
}
