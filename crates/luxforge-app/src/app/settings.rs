//! The Settings sheet: opening and closing it, reading the preferences and the flags through
//! `preferences.read` and `flags.list`, and writing each change through `preferences.set` or
//! `flags.set`, one at a time in the order made. The sheet changes no recipe, so it neither needs
//! nor displaces a draft.
use super::{
    Editor,
    message::{Message, settings::SettingsMessage},
    outcome::Outcome,
    tasks::{call, call_own, owner_task},
};
use crate::state::settings::{FlagControl, GeneralPreferences, SettingsTab, parse_number};
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
                    None => SettingsMessage::Open(SettingsTab::General),
                };
                return self.settings_update(message);
            }
            SettingsMessage::Listed { flags, preferences } => {
                self.settings.reading = false;
                match flags {
                    Ok(flags) => {
                        self.settings.flags = Some(flags);
                        self.settings.error = None;
                    }
                    Err(reason) => self.settings.error = Some(reason),
                }
                match preferences {
                    Ok(preferences) => {
                        self.settings.preferences = Some(preferences);
                        self.settings.preferences_error = None;
                    }
                    Err(reason) => self.settings.preferences_error = Some(reason),
                }
                self.outcome(Outcome::FlagsRead);
            }
            SettingsMessage::SetAutoCollapse(on) => {
                self.settings.collapse_waiting = Some(on);
                return self.write_preferences();
            }
            SettingsMessage::PreferencesSaved(result) => {
                self.settings.collapse_writing = None;
                let mut read = Task::none();
                match result {
                    Ok((preferences, request)) => {
                        self.settings.preferences = Some(preferences);
                        self.settings.preferences_error = None;
                        self.read_back(request);
                    }
                    Err(reason) => {
                        // The write may have landed before its answer was lost: the poll reads
                        // its event like another client's.
                        self.resync();
                        self.status.text =
                            format!("Could not change Auto collapse history: {reason}");
                        self.event("preference_set_failed", || json!({"reason": reason}));
                        self.settings.preferences_error = Some(reason);
                        // The switch showed the change it asked for; read back what is stored.
                        read = self.read_flags();
                    }
                }
                if self.settings.closing && self.settings.idle() {
                    return self.close();
                }
                if self.settings.idle() {
                    self.outcome(Outcome::FlagsWritten);
                }
                return Task::batch([read, self.write_preferences()]);
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

    /// Read the preferences and the flags, unless a read is already out: it answers for this
    /// request too.
    fn read_flags(&mut self) -> Task<Message> {
        if self.settings.reading {
            return Task::none();
        }
        self.settings.reading = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let preferences = general(call(&owner, client, "preferences.read", json!({})));
                (
                    listed(call(&owner, client, "flags.list", json!({}))),
                    preferences,
                )
            },
            |(flags, preferences)| {
                Message::Settings(SettingsMessage::Listed { flags, preferences })
            },
        )
    }

    /// Send the newest Auto collapse history value asked for, once nothing is in flight.
    fn write_preferences(&mut self) -> Task<Message> {
        if self.settings.collapse_writing.is_some() {
            return Task::none();
        }
        let Some(on) = self.settings.collapse_waiting.take() else {
            return Task::none();
        };
        self.settings.collapse_writing = Some(on);
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (answer, request) = call_own(
                    &owner,
                    client,
                    "preferences.set",
                    json!({"auto_collapse_history": on}),
                )?;
                Ok((general(Ok((answer, 0)))?, request))
            },
            |result| Message::Settings(SettingsMessage::PreferencesSaved(result)),
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
        let general = &self.workspace.settings;
        json!({
            "open": self.settings.open.map(SettingsTab::name),
            "general": {"auto_collapse_history": general.auto_collapse,
                        "saving": general.auto_collapse_saving,
                        "error": general.preferences_error},
            "flags": self.settings.flags,
            "rows": rows,
            "unrecognized": self.workspace.settings.unrecognized,
            "error": self.settings.error,
            "writes_outstanding": usize::from(self.settings.writing.is_some())
                + self.settings.waiting.len()
                + usize::from(self.settings.collapse_writing.is_some())
                + usize::from(self.settings.collapse_waiting.is_some()),
        })
    }

    /// Another client changed a flag or a preference: read them again, while the sheet shows them.
    pub(crate) fn flags_changed_elsewhere(&mut self) -> Task<Message> {
        if self.settings.open.is_none() {
            return Task::none();
        }
        self.read_flags()
    }
}

/// An owner answer as the preferences the General tab shows.
fn general(answer: Result<(Value, u64), String>) -> Result<GeneralPreferences, String> {
    let (value, _) = answer?;
    serde_json::from_value(value).map_err(|error| format!("unreadable preferences: {error}"))
}

/// An owner answer as the flags it lists.
fn listed(answer: Result<(Value, u64), String>) -> Result<FlagList, String> {
    let (value, _) = answer?;
    serde_json::from_value(value).map_err(|error| format!("unreadable flags: {error}"))
}
