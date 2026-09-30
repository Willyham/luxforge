//! Evidence steps on the Select workspace (**lane D**): each gesture sent through the message its
//! control or the key table sends, and captured once nothing Select asked the owner for is in
//! flight. An agent's pick is sent through the run's second client and captured once the desktop
//! has evaluated its view again, which it learns of only through its own event sync. Each settled
//! step records the owner's own answer for this client's session (`session.state`'s `browse`), so
//! a frame's selection can be checked against what the owner holds. A folder is browsed as Browse a
//! folder… does, bypassing only the native dialog. Library undo and redo are pressed through the key
//! table, and a bracket's Pick all through its header's message once the grid is scrolled to it;
//! each is captured once the view it made stale has been evaluated again.
use super::{AGENT_ACTOR, Settle};
use crate::app::{
    Editor,
    keymap::keymap,
    message::{Message, evidence::EvidenceMessage, select::SelectMessage},
    select::session_now,
    tasks::{call, owner_task, request},
};
use crate::state::select::{Block, PickFilter, QueryChange, SelectMenu, Shown, SourcePress};
use iced::Task;
use luxforge_core::{MutationRequest, catalog_types::RowItem};
use luxforge_evidence::{
    ArrowKey, LibraryKey, SelectMenu as ScriptMenu, SelectStep, SelectWorkspace,
};
use luxforge_ui::{GridPress, PressModifiers};
use serde_json::{Value, json};

impl Editor {
    /// Run one Select step.
    pub(super) fn select_step(&mut self, step: SelectStep) -> Task<Message> {
        self.select.evidence_after = None;
        match step {
            SelectStep::Switch(SelectWorkspace::Select) => {
                let task = self.update(Message::Select(SelectMessage::Switch(Shown::Select)));
                if !self.select_shown() {
                    let reason = self.status.text.clone();
                    return self.fail_step(format!("the switch was refused: {reason}"));
                }
                self.await_select(task)
            }
            SelectStep::Switch(SelectWorkspace::Develop) => {
                let task = self.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
                self.capture_next_frame();
                task
            }
            SelectStep::Source(name) => self.source_step(&name),
            SelectStep::Folder(path) => {
                if !self.select_shown() {
                    return self.fail_step("Select is not shown");
                }
                let task = self.update(Message::Select(SelectMessage::FolderPicked(Some(
                    path.into(),
                ))));
                self.await_select(task)
            }
            SelectStep::Arrow { direction, extend } => self.arrow_step(direction, extend),
            SelectStep::Choose { menu, item } => self.choose_step(menu, &item),
            SelectStep::Click {
                position,
                shift,
                command,
            } => self.click_step(position, PressModifiers { shift, command }),
            SelectStep::AgentPick { positions, picked } => self.agent_pick_step(&positions, picked),
            SelectStep::Library(key) => self.library_step(key),
            SelectStep::PickAll { position } => self.pick_all_step(position),
            SelectStep::FirstLook(path) => self.first_look_step(path.into()),
            SelectStep::ContinueInBackground => self.background_step(),
            SelectStep::CancelWork => self.cancel_work_step(),
        }
    }

    /// Wait for Select to settle after `task`'s message. Select's hook after this message reports
    /// it settled when the message left nothing in flight.
    fn await_select(&mut self, task: Task<Message>) -> Task<Message> {
        self.await_step(Settle::Select);
        task
    }

    /// Press the source row that shows `name`, or its name and its dates as `Name · dates`.
    fn source_step(&mut self, name: &str) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        let sources = &self.workspace.select.sources;
        let found = sources
            .months
            .iter()
            .flat_map(|month| month.rows.iter())
            .chain(&sources.on_disk)
            .chain(&sources.catalog)
            .find(|row| {
                row.name == name
                    || row
                        .secondary
                        .as_ref()
                        .is_some_and(|dates| format!("{} \u{b7} {dates}", row.name) == name)
            })
            .map(|row| row.press.clone());
        match found {
            Some(Some(SourcePress::View(source))) => {
                let task = self.update(Message::Select(SelectMessage::Source(source)));
                self.await_select(task)
            }
            Some(Some(SourcePress::Read(source))) => {
                let task = self.update(Message::Select(SelectMessage::Read(source)));
                self.await_select(task)
            }
            Some(Some(SourcePress::Toggle(path))) => {
                let task = self.update(Message::Select(SelectMessage::Toggle(path)));
                self.await_select(task)
            }
            Some(Some(SourcePress::BrowseFolder)) => self.fail_step(
                "Browse a folder… opens the native dialog, which a script cannot answer",
            ),
            Some(None) => self.fail_step(format!("the source row {name:?} does nothing")),
            None => self.fail_step(format!("no source row shows {name:?}")),
        }
    }

    /// Press `Cmd+Z` or `Shift+Cmd+Z` through the key table, as the keyboard does.
    fn library_step(&mut self, key: LibraryKey) -> Task<Message> {
        use iced::keyboard::{
            Event as KeyEvent, Key, Location, Modifiers,
            key::{NativeCode, Physical},
        };
        let pressed = Key::Character("z".into());
        let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: pressed.clone(),
            modified_key: pressed,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: match key {
                LibraryKey::Undo => Modifiers::COMMAND,
                LibraryKey::Redo => Modifiers::COMMAND | Modifiers::SHIFT,
            },
            text: None,
            repeat: false,
        });
        let status = iced::event::Status::Ignored;
        match keymap(&event, status, &self.key_context()) {
            Some(Message::Select(SelectMessage::Undo | SelectMessage::Redo)) => {
                let task = self.dispatch(Message::Key(event, status));
                self.await_select(task)
            }
            _ => self.fail_step("the key does not undo or redo here"),
        }
    }

    /// Press the Pick all action of the bracket holding view position `position`, as its header
    /// does: the grid's moment number is its place among the grid's moments.
    fn pick_all_step(&mut self, position: u32) -> Task<Message> {
        let state = &self.select.state;
        let Some(summary) = &state.summary else {
            return self.fail_step("Select holds no view");
        };
        let found = state
            .content
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Moment { index, action, .. } => Some((*index, action.is_some())),
                _ => None,
            })
            .enumerate()
            .find(|(_, (index, _))| {
                summary
                    .groups
                    .moments
                    .get(*index as usize)
                    .is_some_and(|moment| {
                        (moment.start..moment.start + moment.len).contains(&position)
                    })
            });
        match found {
            Some((number, (_, true))) => {
                // Scrolled to the bracket first, as a person scrolls to the header they press.
                let scroll = self.select.layout.reveal(
                    position,
                    self.select.scroll,
                    self.select.viewport.height,
                );
                let scrolled = self.update(Message::Select(SelectMessage::Scrolled(scroll)));
                let task = self.update(Message::Select(SelectMessage::PickAll(number as u32)));
                self.await_select(Task::batch([scrolled, task]))
            }
            Some(_) => self.fail_step(format!(
                "the moment holding position {position} offers no Pick all"
            )),
            None => self.fail_step(format!("no moment holds position {position}")),
        }
    }

    /// Press an arrow key through the key table, as the keyboard does.
    fn arrow_step(&mut self, direction: ArrowKey, extend: bool) -> Task<Message> {
        use iced::keyboard::{
            Event as KeyEvent, Key, Location, Modifiers,
            key::{Named, NativeCode, Physical},
        };
        let named = match direction {
            ArrowKey::Left => Named::ArrowLeft,
            ArrowKey::Right => Named::ArrowRight,
            ArrowKey::Up => Named::ArrowUp,
            ArrowKey::Down => Named::ArrowDown,
        };
        let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: if extend {
                Modifiers::SHIFT
            } else {
                Modifiers::empty()
            },
            text: None,
            repeat: false,
        });
        let status = iced::event::Status::Ignored;
        match keymap(&event, status, &self.key_context()) {
            // In the loupe the arrows step its frames and moments.
            Some(Message::Select(SelectMessage::Move { .. } | SelectMessage::Loupe(_))) => {
                let task = self.dispatch(Message::Key(event, status));
                self.await_select(task)
            }
            _ => self.fail_step("the arrow key moves nothing here"),
        }
    }

    /// Press a pick segment, or open a chip's or the sort's menu and choose the item labelled
    /// `item`, as the filter bar and the strip do.
    fn choose_step(&mut self, menu: ScriptMenu, item: &str) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        let change = match menu {
            ScriptMenu::Pick => PickFilter::ALL
                .into_iter()
                .find(|filter| filter.label() == item)
                .map(QueryChange::Pick),
            ScriptMenu::Camera | ScriptMenu::Kind | ScriptMenu::Group | ScriptMenu::Sort => {
                let menu = match menu {
                    ScriptMenu::Camera => SelectMenu::Camera,
                    ScriptMenu::Kind => SelectMenu::Kind,
                    ScriptMenu::Group => SelectMenu::Group,
                    _ => SelectMenu::Sort,
                };
                let _ = self.update(Message::Select(SelectMessage::Menu(Some(menu))));
                let model = &self.workspace.select;
                let choices = match menu {
                    SelectMenu::Camera => model.filter.camera.menu.as_ref(),
                    SelectMenu::Kind => model.filter.kind.menu.as_ref(),
                    SelectMenu::Group => model
                        .filter
                        .group
                        .as_ref()
                        .and_then(|chip| chip.menu.as_ref()),
                    SelectMenu::Sort => model.strip.sort_menu.as_ref(),
                };
                choices
                    .into_iter()
                    .flatten()
                    .find(|choice| choice.label == item)
                    .and_then(|choice| choice.change.clone())
            }
        };
        let Some(change) = change else {
            let _ = self.update(Message::Select(SelectMessage::Menu(None)));
            return self.fail_step(format!("no {menu:?} choice is labelled {item:?}"));
        };
        let task = self.update(Message::Select(SelectMessage::Change(change)));
        self.await_select(task)
    }

    /// Press the grid cell that shows view position `position`.
    fn click_step(&mut self, position: u32, modifiers: PressModifiers) -> Task<Message> {
        let layout = &self.select.layout;
        let Some(cell) = layout.cell_of_item(position).map(|cell| layout.cell(cell)) else {
            return self.fail_step(format!("no grid cell shows position {position}"));
        };
        let press = GridPress {
            cell: cell.cell,
            item: cell.item,
            span: cell.span,
            modifiers,
            double: false,
        };
        let task = self.update(Message::Select(SelectMessage::Press(press)));
        self.await_select(task)
    }

    /// Have the run's second client pick (or clear) the files at `positions` of the view on screen,
    /// named by the index rows the desktop has read for them.
    fn agent_pick_step(&mut self, positions: &[u32], picked: bool) -> Task<Message> {
        let Some(revision) = self.select.state.revision() else {
            return self.fail_step("Select holds no view");
        };
        let mut file_ids = Vec::with_capacity(positions.len());
        for position in positions {
            match self.select.state.rows.row(*position).map(|row| &row.item) {
                Some(RowItem::File { file_id }) => file_ids.push(*file_id),
                Some(RowItem::Photo { .. }) => {
                    return self.fail_step(format!(
                        "position {position} is a developed photograph, not a file to pick"
                    ));
                }
                None => {
                    return self.fail_step(format!("position {position}'s row has not been read"));
                }
            }
        }
        let owner = self.owner.clone();
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        let agent = *evidence.agent.get_or_insert_with(|| owner.register());
        let mutation = MutationRequest {
            actor: AGENT_ACTOR.into(),
            ..request()
        };
        self.note_step(json!({
            "file_ids": file_ids,
            "picked": picked,
            "request_id": mutation.request_id,
            "actor": AGENT_ACTOR,
            "view_revision": revision,
        }));
        self.select.evidence_after = Some(revision);
        self.await_step(Settle::Select);
        let params = json!({
            "targets": {"kind": "files", "file_ids": file_ids},
            "picked": picked,
            "mutation": mutation,
        });
        owner_task(
            move || call(&owner, agent, "pick.set", params).map(|(answer, _)| answer),
            |result| Message::Evidence(EvidenceMessage::SelectAgentAnswered(result)),
        )
    }

    /// The agent's pick answered. A refusal changes nothing, so no view will be evaluated again:
    /// the step is captured now, as failed. Otherwise it waits for the desktop's own event sync.
    pub(super) fn select_agent_answered(&mut self, result: Result<Value, String>) {
        match result {
            Ok(answer) => self.note_step(json!({ "agent_answer": answer })),
            Err(reason) => {
                self.select.evidence_after = None;
                let _ = self.fail_step(format!("the agent's pick was refused: {reason}"));
            }
        }
    }

    /// Nothing Select asked for is in flight: settle a waiting Select step, once the view has been
    /// evaluated again if the step waits for that, recording the owner's own answer for this
    /// client's session.
    pub(super) fn select_settled(&mut self, by: &str) {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.awaiting != Some(Settle::Select))
        {
            return;
        }
        if let Some(after) = self.select.evidence_after
            && self
                .select
                .state
                .revision()
                .is_none_or(|revision| revision <= after)
        {
            return;
        }
        self.select.evidence_after = None;
        let owner = match session_now(&self.owner, self.client) {
            Ok(session) => json!({ "owner_browse": session.browse }),
            Err(error) => json!({ "owner_browse_error": error }),
        };
        self.note_step(owner);
        self.settle_step(Settle::Select, by);
    }
}
