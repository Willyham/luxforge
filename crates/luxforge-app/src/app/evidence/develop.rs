//! Evidence steps on developing picks and Develop's development set: each gesture sent
//! through the message its control or key sends. A move through the set is captured in the frame
//! after its key when that frame draws the photograph's cached preview, and once the move has
//! settled otherwise; the other steps once nothing developing picks asked the owner for is in
//! flight.
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, develop::DevelopMessage},
};
use iced::Task;
use luxforge_evidence::{DevelopStep, SetStep};
use serde_json::json;

fn develop(message: DevelopMessage) -> Message {
    Message::Develop(message)
}

impl Editor {
    /// Run one develop step.
    pub(super) fn develop_step(&mut self, step: DevelopStep) -> Task<Message> {
        match step {
            DevelopStep::Picks => {
                if !self.select_shown() {
                    return self.fail_step("Select is not shown");
                }
                let task = self.update(develop(DevelopMessage::Open));
                if !self.develop.state.planning {
                    let reason = self.status.text.clone();
                    return self.fail_step(format!("Develop N did not open: {reason}"));
                }
                self.await_develop();
                task
            }
            DevelopStep::Active => {
                if !self.select_shown() || !self.select.state.over_catalog() {
                    return self.fail_step("A catalog view in Select is not shown");
                }
                let Some(active) = self.session.browse.selection.active else {
                    return self.fail_step("No catalog photograph is active");
                };
                let task = self.update(develop(DevelopMessage::OpenAt(active)));
                self.await_develop();
                task
            }
            DevelopStep::Name { event, text } => {
                let open = self
                    .develop
                    .state
                    .confirm
                    .as_ref()
                    .is_some_and(|confirm| event < confirm.events.len());
                if !open {
                    return self.fail_step(format!("the confirmation has no event {event}"));
                }
                let task = self.update(develop(DevelopMessage::Name { event, text }));
                self.capture_next_frame();
                task
            }
            DevelopStep::Existing { event, folder } => {
                if self.develop.state.confirm.is_none() {
                    return self.fail_step("the confirmation is not open");
                }
                let _ = self.update(develop(DevelopMessage::Menu(Some(event))));
                let chosen = self
                    .workspace
                    .develop
                    .confirm
                    .as_ref()
                    .and_then(|confirm| confirm.events.get(event))
                    .and_then(|row| row.menu.as_ref())
                    .and_then(|menu| menu.iter().find(|(_, label)| *label == folder))
                    .map(|(id, _)| id.clone());
                let Some(chosen) = chosen else {
                    return self.fail_step(format!("no existing folder is listed as {folder}"));
                };
                let task = self.update(develop(DevelopMessage::Existing {
                    event,
                    folder: chosen,
                }));
                self.capture_next_frame();
                task
            }
            DevelopStep::Copies(used) => {
                let offered = self
                    .workspace
                    .develop
                    .confirm
                    .as_ref()
                    .is_some_and(|confirm| confirm.copies.is_some());
                if !offered {
                    return self.fail_step("the confirmation offers no copies");
                }
                let task = self.update(develop(DevelopMessage::Copies(used)));
                self.capture_next_frame();
                task
            }
            DevelopStep::Confirm => {
                if self.develop.state.confirm.is_none() {
                    return self.fail_step("the confirmation is not open");
                }
                let task = self.update(develop(DevelopMessage::Confirm));
                if self.develop.state.developing.is_none() {
                    let reason = self.status.text.clone();
                    return self.fail_step(format!("Develop was refused: {reason}"));
                }
                self.await_develop();
                task
            }
            DevelopStep::Cancel => {
                let task = self.update(develop(DevelopMessage::Cancel));
                self.capture_next_frame();
                task
            }
            DevelopStep::Step(step) => {
                let delta = match step {
                    SetStep::Previous => -1,
                    SetStep::Next => 1,
                };
                if self.select_shown() || !self.filmstrip_shown() {
                    return self.fail_step("Develop shows no development set");
                }
                let task = self.update(develop(DevelopMessage::Step(delta)));
                self.after_move();
                task
            }
            DevelopStep::Cell(index) => {
                if self.select_shown() || !self.filmstrip_shown() {
                    return self.fail_step("Develop shows no filmstrip");
                }
                let task = self.update(develop(DevelopMessage::Show(index)));
                self.after_move();
                task
            }
            DevelopStep::Settle => {
                self.await_develop();
                Task::none()
            }
            DevelopStep::Ready => {
                if self.develop_ahead_ready() {
                    self.capture_next_frame();
                } else {
                    self.await_step(Settle::DevelopAhead);
                }
                Task::none()
            }
            DevelopStep::Strip => {
                if self.develop.state.set.is_none() {
                    return self.fail_step("Develop has no development set");
                }
                let task = self.update(develop(DevelopMessage::Collapse));
                self.await_develop();
                task
            }
        }
    }

    /// A move through the set: captured in the frame after the key when that frame draws the
    /// photograph's cached preview, which is what the step is evidence of; once the move has
    /// settled when nothing was decoded for it; and at once when it was refused.
    fn after_move(&mut self) {
        let preview = self.develop.state.preview.is_some();
        let moving = self.develop.switch.is_some();
        self.note_step(json!({"develop_move": {
            "moving": moving,
            "preview": preview,
            "status": self.status.text,
        }}));
        if preview || !moving {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Develop);
        }
    }

    /// Capture once developing picks is quiet: now, when nothing is in flight, or once what the
    /// step started has answered.
    pub(crate) fn await_develop(&mut self) {
        if self.develop_quiet() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Develop);
        }
    }

    /// After every message of an evidence run: settle a step waiting on developing picks or on the
    /// previews decoded ahead.
    pub(crate) fn develop_evidence_after(&mut self) {
        let awaiting = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.awaiting);
        match awaiting {
            Some(Settle::Develop) if self.develop_quiet() => {
                self.settle_step(Settle::Develop, "develop_quiet");
            }
            Some(Settle::DevelopAhead) if self.develop_ahead_ready() => {
                self.settle_step(Settle::DevelopAhead, "develop_ahead_ready");
            }
            _ => {}
        }
    }
}
