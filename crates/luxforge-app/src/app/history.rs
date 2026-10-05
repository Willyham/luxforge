//! History and versions: undo, redo and restore, selecting an entry for preview and returning to
//! current, holding the Original for comparison, loading older rows, and naming versions.
use super::{
    Editor,
    gesture::Starting,
    message::{Message, history::HistoryMessage, preview::PreviewMessage},
    tasks::{self, mutation, older_task, preview_task, versions_task},
};
use iced::Task;
use luxforge_core::HistorySelection;
use serde_json::{Value, json};
use std::time::Instant;

impl Editor {
    /// What the desktop shows of its photograph: the session's selection of the asset it holds
    /// ([`crate::state::shown_selection`]).
    pub(crate) fn shown_selection(&self) -> HistorySelection {
        crate::state::shown_selection(self.document.state.as_ref(), &self.session)
    }

    /// Whether the photograph is at its current state, so it may be edited
    /// ([`crate::state::at_current`]).
    pub(crate) fn at_current(&self) -> bool {
        crate::state::at_current(self.document.state.as_ref(), &self.session)
    }

    /// One history or versions message.
    pub(super) fn history_update(&mut self, message: HistoryMessage) -> Task<Message> {
        // Undo, Redo and Restore move the current entry at once, so an open draft or a request in
        // flight refuses them as it refuses every other commit, from the title bar, a shortcut, the
        // palette and the panel alike: each of them sends one of these messages. Nothing is sent.
        if matches!(
            message,
            HistoryMessage::Undo | HistoryMessage::Redo | HistoryMessage::Restore
        ) && let Some(reason) = self.gesture_refusal(Starting::History)
        {
            self.status.text = reason;
            return Task::none();
        }
        match message {
            HistoryMessage::CompareKeyPressed { uncropped } => {
                if self.compare_key.press(Instant::now(), uncropped) && uncropped {
                    return self.history_update(HistoryMessage::CompareUncropped);
                }
            }
            HistoryMessage::CompareHoldElapsed(sequence) => {
                if self.gallery_page().is_some() {
                    self.compare_key.cancel();
                } else if self.compare_key.elapsed(sequence) {
                    self.event("comparison_hold_started", || json!({"press":sequence}));
                    return self.history_update(HistoryMessage::CompareBegin);
                }
            }
            HistoryMessage::CompareKeyReleased => {
                return match self.compare_key.release(Instant::now()) {
                    Some(super::keymap::CompareRelease::Tap) => self.compare_toggle(),
                    Some(super::keymap::CompareRelease::Hold) => {
                        self.history_update(HistoryMessage::CompareEnd)
                    }
                    None => Task::none(),
                };
            }
            HistoryMessage::CompareKeyCancelled => {
                self.compare_key.cancel();
                return self.history_update(HistoryMessage::CompareEnd);
            }
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
                        self.document.versions = versions;
                        self.read_back(request);
                        self.version_form.name.clear();
                        self.status.text = "Versions updated".into();
                    }
                    Err(error) => self.status.text = error,
                }
            }
            HistoryMessage::OlderLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(page) => {
                        self.document.history.entries.extend(page.entries);
                        self.document.history.next_before_sequence = page.next_before_sequence;
                        self.status.text = "Loaded older history".into();
                    }
                    Err(error) => self.status.text = error,
                }
            }
            HistoryMessage::CompareBegin => return self.compare_begin(true),
            HistoryMessage::CompareUncropped => return self.compare_begin(false),
            HistoryMessage::CompareToggle => return self.compare_toggle(),
            HistoryMessage::ComparePosition(position) => {
                if self.presentation.compare_after.is_some() {
                    self.compare_call(json!({"position":position}));
                }
            }
            HistoryMessage::CompareExit => {
                self.compare_key.cancel();
                if self.presentation.compare_after.is_some() {
                    return self.compare_exit();
                }
                return self.history_update(HistoryMessage::CompareEnd);
            }
            HistoryMessage::CompareEnd => {
                if self.presentation.compare_after.is_some() {
                    self.document.compare_hold = false;
                    return Task::none();
                }
                let (Some(state), Some(previous)) =
                    (&self.document.state, self.document.compare_return.clone())
                else {
                    return Task::none();
                };
                let (method, params) = match previous {
                    HistorySelection::Current => ("preview.return-current", json!({})),
                    HistorySelection::Entry(entry_id) => (
                        "preview.select",
                        json!({"asset_id":state.asset.id,"entry_id":entry_id}),
                    ),
                };
                if !self.compare_session(method, params) {
                    return Task::none();
                }
                self.document.compare_return = None;
                return self.comparison_preview();
            }
            HistoryMessage::VersionName(value) => self.version_form.name = value,
            HistoryMessage::ToggleVersionForm => self.version_form.open = !self.version_form.open,
            HistoryMessage::Undo | HistoryMessage::Redo => {
                let undo = matches!(message, HistoryMessage::Undo);
                let Some(state) = &self.document.state else {
                    return Task::none();
                };
                let method = if undo { "history.undo" } else { "history.redo" };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
                return self.command(method, params);
            }
            HistoryMessage::Select(entry_id) => {
                let Some(state) = &self.document.state else {
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
                self.status.text = "Selecting history state…".into();
                let proxy = self.drawn();
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
                let Some(state) = &self.document.state else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                let asset = state.asset.id.clone();
                // Said once the current entry's frame is back on screen.
                self.status.happened = Some(crate::state::status::Happened::Returned {
                    label: state.current_entry.label.clone(),
                    sequence: state.current_entry.sequence,
                });
                self.busy = true;
                self.status.text = "Returning to current state…".into();
                let proxy = self.drawn();
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
                    (&self.document.state, self.shown_selection())
                else {
                    return Task::none();
                };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision),"entry_id":entry_id});
                return self.command("history.restore", params);
            }
            HistoryMessage::SaveVersion => {
                let (Some(state), Some(entry_id)) =
                    (&self.document.state, &self.document.display_entry)
                else {
                    return Task::none();
                };
                let name = self.version_form.name.trim().to_string();
                if name.is_empty() {
                    self.status.text = "Enter a version name first".into();
                    return Task::none();
                }
                let params = json!({"asset_id":state.asset.id,"name":name,"mutation":tasks::request(),"entry_id":entry_id});
                self.version_form.open = false;
                return self.version_command("version.create", params);
            }
            HistoryMessage::DeleteVersion(name) => {
                let Some(state) = &self.document.state else {
                    return Task::none();
                };
                let params =
                    json!({"asset_id":state.asset.id,"name":name,"mutation":tasks::request()});
                return self.version_command("version.delete", params);
            }
            HistoryMessage::LoadOlder => {
                let (Some(state), Some(before)) = (
                    &self.document.state,
                    self.document.history.next_before_sequence,
                ) else {
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
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status.text = format!("Running {method}…");
        versions_task(self.owner.clone(), self.client, asset, method, params)
    }
}

impl Editor {
    /// Hold the Original entry's preview, remembering the selection it replaces. `keep_geometry`
    /// frames the Original with the geometry of the entry displayed now, so the framing matches and
    /// only the adjustments differ; without it the whole original is shown. Either way it is the
    /// read-only `preview.select` an API client calls.
    fn compare_begin(&mut self, keep_geometry: bool) -> Task<Message> {
        if self.presentation.compare_after.is_some() {
            self.document.compare_hold = true;
            return Task::none();
        }
        // Compare selects the Original entry, which pauses an open draft: the draft would have to
        // be resumed on release, and the design keeps one draft and one preview selection at a
        // time. Refuse it and say so rather than pausing silently.
        if let Some(reason) = self.gesture_refusal(Starting::Compare) {
            self.status.text = reason;
            return Task::none();
        }
        let (Some(state), Some(original)) =
            (&self.document.state, self.document.original_entry.clone())
        else {
            return Task::none();
        };
        if self.document.compare_return.is_some() {
            return Task::none();
        }
        let previous = self.shown_selection();
        let asset = state.asset.id.clone();
        if !self.compare_session(
            "preview.select",
            json!({"asset_id":asset,"entry_id":original,"keep_geometry":keep_geometry}),
        ) {
            return Task::none();
        }
        self.document.compare_return = Some(previous);
        self.status.text = "Comparing with the original…".into();
        self.comparison_preview()
    }

    /// Change bounded session/selection data in the input's own update. A release advances the
    /// generation before any late Before preparation can answer; source and image work stay on
    /// the preview task and worker.
    fn compare_session(&mut self, method: &str, params: Value) -> bool {
        let answer =
            tasks::call(&self.owner, self.client, method, params).and_then(|(value, _)| {
                serde_json::from_value(value["session"].clone()).map_err(|error| error.to_string())
            });
        match answer {
            Ok(session) => {
                self.adopt(session);
                true
            }
            Err(error) => {
                self.status.text = error;
                false
            }
        }
    }

    /// A divider move changes only session state: no runtime hop, preview job or image work.
    fn compare_call(&mut self, mut params: Value) -> bool {
        let Some(state) = &self.document.state else {
            return false;
        };
        params["asset_id"] = json!(state.asset.id);
        let request = params.clone();
        if !self.compare_session("preview.compare", params) {
            return false;
        }
        self.event("comparison_view", || request);
        true
    }

    fn compare_toggle(&mut self) -> Task<Message> {
        if self.presentation.compare_after.is_some() {
            return self.compare_exit();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Compare) {
            self.status.text = reason;
            return Task::none();
        }
        if self.document.compare_return.is_some() {
            return Task::none();
        }
        if self.document.display_entry != self.presentation.presented_entry {
            self.status.text = "Wait for the photograph before comparing".into();
            return Task::none();
        }
        // Prefer an already-rendered whole-detail frame. Retaining it clones only its Arc;
        // the existing 512 MiB raster and aggregate photo-texture limits still bound both sides.
        let after = self
            .presentation
            .exact()
            .map(|exact| &exact.raster)
            .or_else(|| self.presentation.proxy().map(|proxy| &proxy.raster))
            .and_then(|raster| {
                luxforge_ui::Frame::new(
                    raster.rgba.clone(),
                    raster.width,
                    raster.height,
                    self.presentation.presenter.photo_version(),
                )
            })
            .or_else(|| {
                self.presentation
                    .presenter
                    .photo_for(self.presentation.presented_content)
                    .cloned()
            });
        let Some(after) = after else {
            self.status.text = "Wait for the photograph before comparing".into();
            return Task::none();
        };
        // A frame the surface cannot give mip levels (one held in tiles) would alias at Fit, so
        // the display reduction of the same exact photograph, when it is the one on screen, is
        // kept to draw there instead.
        let after = super::compare_after::CompareAfter::new(
            after,
            super::compare_after::display_reduction(&self.presentation).cloned(),
            super::compare_after::DEVICE_TEXTURE_LIMIT,
        );
        let previous = self.shown_selection();
        if !self.compare_call(json!({"enabled":true,"position":0.5})) {
            return Task::none();
        }
        self.document.compare_return = Some(previous);
        self.presentation.compare_after = Some(after);
        self.document.compare_hold = false;
        self.status.text = "Drag to compare Before and After · tap \\ or Escape to exit".into();
        self.comparison_preview()
    }

    fn compare_exit(&mut self) -> Task<Message> {
        if !self.compare_call(json!({"enabled":false})) {
            return Task::none();
        }
        self.document.compare_return = None;
        self.presentation.compare_after = None;
        self.document.compare_hold = false;
        self.comparison_preview()
    }

    fn comparison_preview(&self) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        tasks::comparison_preview_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            self.session.clone(),
            self.drawn(),
        )
    }
}
