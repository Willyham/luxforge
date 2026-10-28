//! Settings › Modules: reading `module.list` and `module.layout.read` as the tab opens and when
//! another client changes the layout, writing each gesture through exactly one of
//! `module.enable`, `module.disable`, `module.order.set` or `module.layout.reset`, one at a time in
//! the order made, adopting each answer, and the one-step Undo toast, which writes back what the
//! last change replaced ([design](../../../../docs/design/module-manager.md#desktop)). The layout
//! is the person's preference, outside every catalog: nothing here changes a recipe, so the tab
//! neither needs nor displaces a draft.
use super::{
    Editor,
    message::{
        Message,
        settings::{ModuleDragEdit, ModulesKey, ModulesMessage, SettingsMessage},
    },
    tasks::{call, call_own, owner_task},
};
use crate::state::{
    module_settings::{self, LayoutWrite, ModulesFocus, ModulesRead, RowControl, UndoToast},
    settings::SettingsTab,
};
use iced::{Subscription, Task};
use luxforge_core::module_layout::LayoutView;
use serde_json::{Value, json};

impl Editor {
    /// One Modules tab message.
    pub(super) fn modules_tab_update(&mut self, message: ModulesMessage) -> Task<Message> {
        let tab = &mut self.settings.modules;
        match message {
            ModulesMessage::Read(result) => {
                tab.reading = false;
                match result {
                    Ok(read) => {
                        tab.read = Some(*read);
                        tab.read_error = None;
                    }
                    Err(reason) => tab.read_error = Some(reason),
                }
                let rows = tab.rows();
                if tab
                    .selected
                    .as_ref()
                    .is_some_and(|selected| !rows.contains(selected))
                {
                    tab.selected = None;
                }
                if let Some(focus) = tab.focus.take() {
                    return self.focus_module_row(&focus);
                }
            }
            ModulesMessage::Select(row) => tab.selected = Some(row),
            ModulesMessage::Switch { module, on } => {
                let change = tab.switched(&module, on);
                return self.layout_gesture(change, Some(module));
            }
            ModulesMessage::Locked(row) => {
                let change = tab.toggled(&row.module);
                tab.selected = Some(row.clone());
                return self.layout_gesture(change, Some(row.module));
            }
            ModulesMessage::Key(ModulesKey::Step(step)) => {
                if let Some(row) = tab.step_selection(step) {
                    tab.selected = Some(row);
                    return crate::view::module_settings::scroll_to_selected();
                }
            }
            ModulesMessage::Key(ModulesKey::Move(how)) => {
                let change = tab
                    .selected
                    .as_ref()
                    .filter(|row| tab.movable(row))
                    .and_then(|row| tab.moved(row, how));
                return self.layout_gesture(Ok(change), None);
            }
            ModulesMessage::Key(ModulesKey::Toggle) => {
                let Some(module) = tab.selected.as_ref().map(|row| row.module.clone()) else {
                    return Task::none();
                };
                let change = tab.toggled(&module);
                return self.layout_gesture(change, Some(module));
            }
            ModulesMessage::ResetOrder(toolset) => {
                let change = tab.reset(Some(toolset));
                return self.layout_gesture(Ok(change), None);
            }
            ModulesMessage::ResetAll => {
                let change = tab.reset(None);
                return self.layout_gesture(Ok(change), None);
            }
            ModulesMessage::Undo => {
                let Some(toast) = tab.toast.take() else {
                    return Task::none();
                };
                tab.refusal = None;
                tab.waiting.extend(toast.undo);
                self.status.text = format!("Undid \u{201c}{}\u{201d}", toast.text);
                self.event("module_layout_undo", || json!({"undid": toast.text}));
                return self.write_layout();
            }
            ModulesMessage::Drag(edit) => return self.module_drag(edit),
            ModulesMessage::Written(result) => return self.layout_written(result),
        }
        Task::none()
    }

    /// Open the sheet at Modules, at the row `focus` names once the tab has read what it lists.
    pub(super) fn open_modules(&mut self, focus: Option<ModulesFocus>) -> Task<Message> {
        let opened = self.settings_update(SettingsMessage::Open(SettingsTab::Modules));
        let Some(focus) = focus else {
            return opened;
        };
        // What the tab read last lists the row already: select it now. Otherwise the read the
        // opening makes chooses it.
        let focused = if self.settings.modules.resolve(&focus).is_some() {
            self.focus_module_row(&focus)
        } else {
            self.settings.modules.focus = Some(focus);
            Task::none()
        };
        Task::batch([opened, focused])
    }

    /// Select the row `focus` names and scroll the list to it.
    fn focus_module_row(&mut self, focus: &ModulesFocus) -> Task<Message> {
        let tab = &mut self.settings.modules;
        match tab.resolve(focus) {
            Some(row) => {
                tab.selected = Some(row);
                crate::view::module_settings::scroll_to_selected()
            }
            None => Task::none(),
        }
    }

    /// One gesture's change: queue its one write and show its toast, which replaces the last; or
    /// say in the detail pane and the status bar why it cannot be made. A gesture that changes
    /// nothing writes nothing.
    fn layout_gesture(
        &mut self,
        change: Result<Option<(LayoutWrite, UndoToast)>, String>,
        module: Option<String>,
    ) -> Task<Message> {
        let tab = &mut self.settings.modules;
        match change {
            Ok(Some((write, toast))) => {
                tab.refusal = None;
                self.status.text = format!("{} \u{b7} rendering unchanged", toast.text);
                tab.toast = Some(toast);
                tab.waiting.push_back(write);
                self.write_layout()
            }
            Ok(None) => Task::none(),
            Err(reason) => {
                self.status.text.clone_from(&reason);
                tab.refusal = Some((module, reason));
                Task::none()
            }
        }
    }

    fn module_drag(&mut self, edit: ModuleDragEdit) -> Task<Message> {
        let tab = &mut self.settings.modules;
        match edit {
            ModuleDragEdit::Start(row) => {
                if tab.movable(&row) {
                    tab.selected = Some(row.clone());
                    tab.drag = Some(module_settings::ModuleDrag { row, over: None });
                }
            }
            ModuleDragEdit::Over(module) => {
                if let Some(drag) = &mut tab.drag {
                    drag.over = Some(module);
                }
            }
            ModuleDragEdit::End => {
                let Some(drag) = tab.drag.take() else {
                    return Task::none();
                };
                let change = drag.over.and_then(|target| tab.dropped(&drag.row, &target));
                return self.layout_gesture(Ok(change), None);
            }
            ModuleDragEdit::Cancel => tab.drag = None,
        }
        Task::none()
    }

    /// Read what the tab lists, unless a read is already out: it answers for this request too.
    pub(crate) fn read_modules_tab(&mut self) -> Task<Message> {
        let tab = &mut self.settings.modules;
        if tab.reading {
            return Task::none();
        }
        tab.reading = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (listed, _) = call(&owner, client, "module.list", json!({}))?;
                let (layout, _) = call(&owner, client, "module.layout.read", json!({}))?;
                ModulesRead::parse(listed, layout).map(Box::new)
            },
            |result| Message::Settings(SettingsMessage::Modules(ModulesMessage::Read(result))),
        )
    }

    /// Send the oldest waiting layout write, once nothing is in flight.
    fn write_layout(&mut self) -> Task<Message> {
        let tab = &mut self.settings.modules;
        if tab.writing.is_some() {
            return Task::none();
        }
        let Some(write) = tab.waiting.pop_front() else {
            return Task::none();
        };
        let (method, params) = (write.method(), write.params());
        tab.writing = Some(write);
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (answer, request) = call_own(&owner, client, method, params)?;
                let layout: LayoutView = serde_json::from_value(answer)
                    .map_err(|error| format!("unreadable module layout: {error}"))?;
                Ok((layout, request))
            },
            |result| Message::Settings(SettingsMessage::Modules(ModulesMessage::Written(result))),
        )
    }

    /// The write in flight answered: adopt the layout it left, or say why it was refused and read
    /// back what is stored; then send the next.
    fn layout_written(&mut self, result: Result<(LayoutView, String), String>) -> Task<Message> {
        let written = self.settings.modules.writing.take();
        let read = match result {
            Ok((layout, request)) => {
                if let Some(read) = &mut self.settings.modules.read {
                    read.layout = layout.clone();
                }
                // The event sync skips this request's event, so the workspace adopts the answer
                // here: the panel, the strip and the palette follow the tab's change.
                self.read_back(request);
                self.adopt_layout(layout, "written")
            }
            Err(reason) => {
                // The write may have landed before its answer was lost: the poll reads its event
                // like another client's.
                self.resync();
                let module = written
                    .as_ref()
                    .and_then(LayoutWrite::module)
                    .map(str::to_owned);
                self.status.text = format!("Could not change the module layout: {reason}");
                self.event("module_layout_failed", || {
                    json!({
                        "method": written.as_ref().map(LayoutWrite::method),
                        "module": module,
                        "reason": reason,
                    })
                });
                let tab = &mut self.settings.modules;
                // What the toast offered to undo did not happen as it said.
                tab.toast = None;
                tab.refusal = Some((module, reason));
                self.read_modules_tab()
            }
        };
        if self.settings.closing && self.settings.idle() {
            return self.close();
        }
        Task::batch([read, self.write_layout()])
    }

    /// Another client changed the module layout, or events were missed: the workspace reads it
    /// again, and so does an open Modules tab. A closed one reads it as it opens.
    pub(crate) fn module_layout_changed_elsewhere(&mut self, changed: bool) -> Task<Message> {
        if !changed {
            return Task::none();
        }
        let tab = if self.settings.open == Some(SettingsTab::Modules) {
            self.read_modules_tab()
        } else {
            Task::none()
        };
        Task::batch([self.read_layout(), tab])
    }

    /// The workspace's own layout write answered: the tab's last read adopts it, as the event
    /// sync skips the write's event.
    pub(crate) fn modules_tab_follow(&mut self, layout: &LayoutView) {
        if let Some(read) = &mut self.settings.modules.read {
            read.layout = layout.clone();
        }
    }

    /// What a captured frame records of the Modules tab while it is open: every group and row as
    /// drawn, the detail pane, the toast and the layout drawn, with the writes outstanding. `null`
    /// while the tab is closed.
    pub(crate) fn modules_summary(&self) -> Value {
        let Some(model) = &self.workspace.settings.modules else {
            return Value::Null;
        };
        let tab = &self.settings.modules;
        let groups: Vec<Value> = model
            .groups
            .iter()
            .map(|group| {
                let rows: Vec<Value> = group
                    .rows
                    .iter()
                    .map(|row| {
                        let control = match &row.control {
                            RowControl::Switch(on) => json!({"switch": on}),
                            RowControl::Follows(on) => json!({"follows": on}),
                            RowControl::Lock(reason) => json!({"lock": reason}),
                        };
                        json!({"module": row.key.module, "title": row.title,
                               "qualifier": row.qualifier, "letter": row.letter,
                               "control": control, "grip": row.grip, "off": row.off,
                               "selected": row.selected})
                    })
                    .collect();
                json!({"toolset": group.toolset.as_str(), "can_reset": group.can_reset,
                       "rows": rows})
            })
            .collect();
        let detail = model.detail.as_ref().map(|detail| {
            json!({"title": detail.title, "origin": detail.origin, "hint": detail.hint,
                   "shown": detail.shown, "lives": detail.lives,
                   "stages": detail.stages.iter().filter(|(_, own)| *own)
                       .map(|(name, _)| name).collect::<Vec<_>>(),
                   "photos": detail.photos, "api": detail.api, "refusal": detail.refusal})
        });
        json!({
            "groups": groups,
            "detail": detail,
            "toast": model.toast,
            "can_reset_all": model.can_reset_all,
            "unrecognized": model.unrecognized,
            "loading": model.loading,
            "error": model.error,
            "dragging": model.dragging,
            "layout": tab.layout(),
            "writes_outstanding": usize::from(tab.writing.is_some()) + tab.waiting.len(),
        })
    }
}

/// A reorder by a row's grip ends wherever the button comes up, so its release is heard
/// window-wide, and only while a row is held.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if editor.settings.modules.drag.is_none() {
        return Subscription::none();
    }
    iced::event::listen_with(release)
}

fn release(event: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    matches!(
        event,
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
            iced::mouse::Button::Left
        ))
    )
    .then_some(Message::Settings(SettingsMessage::Modules(
        ModulesMessage::Drag(ModuleDragEdit::End),
    )))
}
