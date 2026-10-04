//! The Settings sheet: opening and closing it, reading the flags through `flags.list` and writing
//! each change through `flags.set`, one at a time in the order made. The sheet changes no recipe,
//! so it neither needs nor displaces a draft.
use super::{
    Editor,
    message::{Message, settings::SettingsMessage},
    outcome::Outcome,
    tasks::{call, call_own, owner_task},
};
use crate::state::settings::{FlagControl, SettingsTab, parse_number};
use iced::Task;
use luxforge_core::flags::FlagList;
use serde_json::{Value, json};

impl Editor {
    /// One Settings sheet message.
    pub(super) fn settings_update(&mut self, message: SettingsMessage) -> Task<Message> {
        match message {
            SettingsMessage::Open(tab) => {
                // The sheet is modal: nothing behind it stays open over it.
                self.palette.open = false;
                self.view_state.menu = None;
                self.settings.open = Some(tab);
                self.settings.number_text.clear();
                return self.read_flags();
            }
            SettingsMessage::Close => {
                self.settings.open = None;
                self.settings.number_text.clear();
            }
            SettingsMessage::Toggle => {
                let message = match self.settings.open {
                    Some(_) => SettingsMessage::Close,
                    None => SettingsMessage::Open(SettingsTab::Experiments),
                };
                return self.settings_update(message);
            }
            SettingsMessage::Listed(result) => {
                self.settings.reading = false;
                match result {
                    Ok(flags) => {
                        self.settings.flags = Some(flags);
                        self.settings.error = None;
                    }
                    Err(reason) => self.settings.error = Some(reason),
                }
                self.outcome(Outcome::FlagsRead);
            }
            SettingsMessage::Set { flag, value } => {
                self.settings.number_text.remove(&flag);
                self.settings.waiting.push_back((flag, value));
                return self.write_flags();
            }
            SettingsMessage::Saved(result) => {
                let written = self.settings.writing.take();
                let mut read = Task::none();
                match result {
                    Ok((flags, request)) => {
                        self.settings.flags = Some(flags);
                        self.settings.error = None;
                        self.read_back(request);
                    }
                    Err(reason) => {
                        // The write may have landed before its answer was lost: the poll reads
                        // its event like another client's.
                        self.resync();
                        let flag = written.map(|(flag, _)| flag).unwrap_or_default();
                        self.status.text = format!("Could not change {flag}: {reason}");
                        self.event(
                            "flag_set_failed",
                            || json!({"flag": flag, "reason": reason}),
                        );
                        self.settings.error = Some(reason);
                        // The row showed the change it asked for; read back what is stored.
                        read = self.read_flags();
                    }
                }
                if self.settings.closing && self.settings.idle() {
                    return self.close();
                }
                if self.settings.idle() {
                    self.outcome(Outcome::FlagsWritten);
                }
                return Task::batch([read, self.write_flags()]);
            }
            SettingsMessage::NumberText { flag, text } => {
                self.settings.number_text.insert(flag, text);
            }
            SettingsMessage::NumberSubmit(flag) => {
                let listed = self
                    .settings
                    .flags
                    .as_ref()
                    .and_then(|flags| flags.flag(&flag));
                let Some((listed, text)) = listed.zip(self.settings.number_text.get(&flag)) else {
                    return Task::none();
                };
                match parse_number(listed, text) {
                    Some(number) => {
                        let value = Some(Value::from(number));
                        return self.settings_update(SettingsMessage::Set { flag, value });
                    }
                    None => {
                        self.status.text = format!(
                            "{} takes a number from {} to {} in steps of {}",
                            listed.title,
                            listed.min.unwrap_or_default(),
                            listed.max.unwrap_or_default(),
                            listed.step.unwrap_or_default()
                        );
                    }
                }
            }
        }
        Task::none()
    }

    /// Read the flags, unless a read is already out: it answers for this request too.
    fn read_flags(&mut self) -> Task<Message> {
        if self.settings.reading {
            return Task::none();
        }
        self.settings.reading = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || listed(call(&owner, client, "flags.list", json!({}))),
            |result| Message::Settings(SettingsMessage::Listed(result)),
        )
    }

    /// Send the oldest waiting write, once nothing is in flight.
    fn write_flags(&mut self) -> Task<Message> {
        if self.settings.writing.is_some() {
            return Task::none();
        }
        let Some((flag, value)) = self.settings.waiting.pop_front() else {
            return Task::none();
        };
        self.settings.writing = Some((flag.clone(), value.clone()));
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (answer, request) = call_own(
                    &owner,
                    client,
                    "flags.set",
                    json!({"flag": flag, "value": value}),
                )?;
                Ok((listed(Ok((answer, 0)))?, request))
            },
            |result| Message::Settings(SettingsMessage::Saved(result)),
        )
    }

    /// What a captured frame records of the sheet: the tab, the flags as last read, every row as
    /// drawn and the writes outstanding.
    pub(crate) fn settings_summary(&self) -> Value {
        let rows: Vec<Value> = self
            .workspace
            .settings
            .rows
            .iter()
            .map(|row| {
                let control = match &row.control {
                    FlagControl::Toggle(on) => json!({"toggle": on}),
                    FlagControl::Choice {
                        values, selected, ..
                    } => json!({"choice": selected.and_then(|index| values.get(index))}),
                    FlagControl::Number { text, invalid, .. } => {
                        json!({"number": text, "invalid": invalid})
                    }
                };
                json!({"id": row.id, "control": control, "can_reset": row.can_reset,
                       "notes": row.notes, "error": row.error, "saving": row.saving})
            })
            .collect();
        json!({
            "open": self.settings.open.map(SettingsTab::name),
            "flags": self.settings.flags,
            "rows": rows,
            "unrecognized": self.workspace.settings.unrecognized,
            "error": self.settings.error,
            "writes_outstanding": usize::from(self.settings.writing.is_some()) + self.settings.waiting.len(),
        })
    }

    /// Another client changed a flag: read the flags again, while the sheet shows them.
    pub(crate) fn flags_changed_elsewhere(&mut self) -> Task<Message> {
        if self.settings.open.is_none() {
            return Task::none();
        }
        self.read_flags()
    }
}

/// An owner answer as the flags it lists.
fn listed(answer: Result<(Value, u64), String>) -> Result<FlagList, String> {
    let (value, _) = answer?;
    serde_json::from_value(value).map_err(|error| format!("unreadable flags: {error}"))
}
