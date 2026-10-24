//! The Settings sheet: opening and closing it, reading the preferences and the flags through
//! `preferences.read` and `flags.list`, writing each flag change through `flags.set`, one at a
//! time in the order made, and handing each General row's change to the desktop's preference
//! writer. The sheet changes no recipe, so it neither needs nor displaces a draft.
use super::{
    Editor,
    message::{Message, settings::SettingsMessage},
    outcome::Outcome,
    tasks::{call, call_own, owner_task},
};
use crate::state::{
    MenuTarget,
    capabilities::Operation,
    fill_setup::SetupAction,
    models::{self, ModelRequest},
    preferences::{self, GeneralControl},
    settings::{FlagControl, SettingsTab, parse_number},
};
use iced::Task;
use luxforge_core::flags::FlagList;
use serde_json::{Value, json};

impl Editor {
    /// One Settings sheet message.
    pub(super) fn settings_update(&mut self, message: SettingsMessage) -> Task<Message> {
        match message {
            SettingsMessage::Open(tab) => {
                // A launch that is not eligible for usage statistics has no Privacy tab to open, and
                // one whose modules hold no model no Models tab.
                let models = models::model_modules(&self.modules, self.settings.flags.as_ref())
                    .next()
                    .is_some();
                let tab =
                    if SettingsTab::shown(self.usage.eligible, models).any(|shown| shown == tab) {
                        tab
                    } else {
                        SettingsTab::General
                    };
                // The sheet is modal: nothing behind it stays open over it.
                self.palette.open = false;
                self.view_state.menu = None;
                self.settings.open = Some(tab);
                self.settings.number_text.clear();
                // What a `luxforge-json` process stored in the library reaches the desktop by no
                // event, so opening the sheet lists the themes again too, and reads the usage
                // statistics consent, which another process may have changed. The Models tab reads
                // the status of every module that holds a model, which a load, an unload or an
                // idle release may have changed since it was last read.
                // The Updates group shows the update status, read again as the sheet opens.
                let statuses = if tab == SettingsTab::Models {
                    self.read_model_statuses()
                } else {
                    Task::none()
                };
                // The Agents tab looks for the server beside this build again, since a build may
                // have put it there since, and reads the connected clients.
                let agents = if tab == SettingsTab::Agents {
                    self.read_agents()
                } else {
                    Task::none()
                };
                return Task::batch([
                    self.read_flags(),
                    self.list_themes(),
                    self.read_usage(),
                    statuses,
                    agents,
                    self.read_updates(),
                ]);
            }
            SettingsMessage::Close => {
                self.settings.open = None;
                self.settings.number_text.clear();
                // A theme row's and a model row's menus belong to the sheet.
                if matches!(
                    self.view_state.menu,
                    Some(
                        MenuTarget::Theme(_)
                            | MenuTarget::Model { .. }
                            | MenuTarget::Resident { .. }
                    )
                ) {
                    self.view_state.menu = None;
                }
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
                let followed = match flags {
                    Ok(flags) => {
                        self.settings.flags = Some(flags);
                        self.settings.error = None;
                        self.flags_followed()
                    }
                    Err(reason) => {
                        self.settings.error = Some(reason);
                        Task::none()
                    }
                };
                let applied = self.preferences_read(preferences);
                self.outcome(Outcome::FlagsRead);
                return Task::batch([applied, followed]);
            }
            SettingsMessage::Flags { flags, change } => {
                if change {
                    self.settings.flag_reads = self.settings.flag_reads.saturating_sub(1);
                }
                match flags {
                    // A read the sheet's own read overtook leaves the sheet's answer.
                    Ok(flags) if !self.settings.reading => {
                        self.settings.flags = Some(flags);
                        return self.flags_followed();
                    }
                    Ok(_) => {}
                    Err(reason) => self.event("flags_read_failed", || json!({"reason": reason})),
                }
            }
            SettingsMessage::SetGeneral(row, value) => return self.set_general(row, value),
            SettingsMessage::CatalogFolder(folder) => return self.catalog_folder_chosen(folder),
            SettingsMessage::Set { flag, value } => {
                self.settings.number_text.remove(&flag);
                self.settings.waiting.push_back((flag, value));
                return self.write_flags();
            }
            SettingsMessage::Saved(result) => {
                let written = self.settings.writing.take();
                let read = match result {
                    Ok((flags, request)) => {
                        self.settings.flags = Some(flags);
                        self.settings.error = None;
                        self.read_back(request);
                        self.flags_followed()
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
                        self.read_flags()
                    }
                };
                if self.settings.closing && self.settings.idle() {
                    return self.close();
                }
                if self.settings.idle() {
                    self.outcome(Outcome::FlagsWritten);
                }
                return Task::batch([read, self.write_flags()]);
            }
            SettingsMessage::Agents(message) => return self.agents_update(message),
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

    /// Read `module.status` of every module that holds a model, as the Models tab shows them, one
    /// round trip each through the capability driver; a module with a round trip already out is
    /// left to its answer.
    fn read_model_statuses(&mut self) -> Task<Message> {
        let wanted: Vec<String> =
            models::model_modules(&self.modules, self.settings.flags.as_ref())
                .map(|module| module.id.clone())
                .filter(|module| self.capabilities.module(module).pending == 0)
                .collect();
        Task::batch(
            wanted
                .into_iter()
                .map(|module| self.capability_op(&module, Operation::Refresh))
                .collect::<Vec<_>>(),
        )
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
                let preferences = call(&owner, client, "preferences.read", json!({}))
                    .and_then(|(answer, _)| preferences::parse(answer));
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

    /// The flags as the host answered them changed: what a live flag gates follows. Remove leaves
    /// with its fill task's flag off and reads its tiers again when its status predates the change,
    /// and an open Models tab with no model left to list gives way to General.
    fn flags_followed(&mut self) -> Task<Message> {
        if self.settings.open == Some(SettingsTab::Models)
            && models::model_modules(&self.modules, self.settings.flags.as_ref())
                .next()
                .is_none()
        {
            self.settings.open = Some(SettingsTab::General);
        }
        self.remove_follow_flags();
        Task::none()
    }

    /// Read the flags alone, off the update loop: what a live flag gates outside the sheet (the
    /// object picker and the models a flagged feature uses)
    /// follows the person's choice from launch on. A read
    /// that follows another client's `change` is counted until it answers.
    pub(crate) fn read_flag_values(&mut self, change: bool) -> Task<Message> {
        if change {
            self.settings.flag_reads += 1;
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || listed(call(&owner, client, "flags.list", json!({}))),
            move |flags| Message::Settings(SettingsMessage::Flags { flags, change }),
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

    /// What a captured frame records of the sheet: the tab, every General row as drawn, the flags
    /// as last read, every flag row as drawn and the writes outstanding.
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
        let general: Vec<Value> = self
            .workspace
            .settings
            .general
            .iter()
            .map(|row| {
                let control = match &row.control {
                    GeneralControl::Toggle(on) => json!({"toggle": on}),
                    GeneralControl::Choice { .. } => json!({"choice": row.control.value()}),
                    GeneralControl::Catalog { path, notes, .. } => json!({
                        "catalog": path, "stored": row.control.value(), "notes": notes
                    }),
                };
                json!({"id": row.preference.field(), "control": control, "saving": row.saving})
            })
            .collect();
        let appearance = &self.workspace.settings.appearance;
        let themes: Vec<Value> = appearance
            .rows
            .iter()
            .map(|row| {
                json!({"id": row.id, "name": row.name, "mode": row.mode, "origin": row.origin,
                       "adjusted": row.adjusted, "active": row.active,
                       "deletable": row.deletable, "menu_open": row.menu_open})
            })
            .collect();
        json!({
            "models": self.models_summary(),
            "agents": self.agents_summary(),
            "open": self.settings.open.map(SettingsTab::name),
            "tabs": self.workspace.settings.tabs.iter().map(|tab| tab.name()).collect::<Vec<_>>(),
            "general": {"rows": general, "error": self.workspace.settings.preferences_error},
            "appearance": {"rows": themes, "unrecognized": appearance.unrecognized,
                           "loading": appearance.loading, "error": appearance.error,
                           "refusal": appearance.refusal},
            "flags": self.settings.flags,
            "rows": rows,
            "unrecognized": self.workspace.settings.unrecognized,
            "error": self.settings.error,
            "writes_outstanding": usize::from(self.settings.writing.is_some())
                + self.settings.waiting.len()
                + self.preferences.outstanding(),
        })
    }

    /// What a captured frame records of the Models tab while it is open: every row and resident row
    /// as drawn, with the request each one's button sends and Copy as JSON request copies, the
    /// resident and machine lines, and, for correlation, the `module.status` answers they were
    /// drawn from as the desktop holds them. `null` while the tab is closed.
    fn models_summary(&self) -> Value {
        let Some(tab) = &self.workspace.settings.models else {
            return Value::Null;
        };
        let mutation = json!({"request_id": "<per press>", "actor": crate::state::ACTOR});
        let rows: Vec<Value> = tab
            .rows
            .iter()
            .map(|row| {
                json!({
                    "module_id": row.module_id, "resource_id": row.resource_id,
                    "resources": row.resources, "title": row.title, "caption": row.caption,
                    "detail": row.detail, "state": row.state,
                    "tradeoff": row.tradeoff, "use_restriction": row.use_restriction,
                    "requirement": row.requirement, "fits": row.fits,
                    "removable": row.removable, "enabled": row.enabled,
                    "menu_open": row.menu_open,
                    "remove_requests": row.removable.then(|| ModelRequest::Remove.requests(
                        &row.module_id, &row.remove, || mutation.clone())),
                    "download": row.download.as_ref().map(|download| json!({
                        "setup": match download {
                            SetupAction::Install { setup, .. } | SetupAction::Cancel { setup, .. } => setup,
                        },
                        "requests": download.requests(|| mutation.clone()),
                    })),
                })
            })
            .collect();
        let resident: Vec<Value> = tab
            .resident
            .iter()
            .map(|row| {
                json!({
                    "module_id": row.module_id, "resource_id": row.resource_id,
                    "title": row.title, "detail": row.detail, "enabled": row.enabled,
                    "menu_open": row.menu_open,
                    "unload_request": ModelRequest::Unload.request(
                        &row.module_id, &row.resource_id, Value::Null),
                })
            })
            .collect();
        let statuses: serde_json::Map<String, Value> = models::model_modules(&self.modules, self.settings.flags.as_ref())
            .map(|module| {
                let status = self.capabilities.module(&module.id).status.as_ref();
                (
                    module.id.clone(),
                    status.map_or(Value::Null, |status| {
                        json!({
                            "resources": status.resources,
                            "machine": status.machine.as_ref().map(|machine| json!({"memory_bytes": machine.memory_bytes})),
                            "runtime": status.runtime.as_ref().map(|runtime| json!({
                                "held_bytes": runtime.held_bytes,
                                "budget_bytes": runtime.budget_bytes,
                                "models": runtime.models.iter().map(|model| json!({
                                    "resource_id": model.resource_id, "state": model.state,
                                    "resident_bytes": model.resident_bytes,
                                })).collect::<Vec<_>>(),
                            })),
                        })
                    }),
                )
            })
            .collect();
        json!({
            "rows": rows,
            "resident": resident,
            "resident_line": tab.resident_line,
            "machine": tab.machine,
            "loading": tab.loading,
            "errors": tab.errors,
            "tiers": [],
            "status": statuses,
        })
    }

    /// Another client changed a flag, a preference, the theme library or the usage statistics
    /// consent. An open sheet reads the flags and the preferences again; a closed one reads the flags
    /// alone for a flag, since a live flag gates what the desktop offers, and a preference is read
    /// again whatever is on screen, since the desktop applies some of them to this session, the
    /// active theme among them. A theme event lists the library
    /// again whether or not the sheet is open, since the palette lists the themes too. A consent
    /// change is read again in an eligible launch, so an open Privacy tab follows it.
    pub(crate) fn settings_changed_elsewhere(
        &mut self,
        flags: bool,
        preferences: bool,
        themes: bool,
        usage: bool,
    ) -> Task<Message> {
        let listed = if themes {
            self.list_themes()
        } else {
            Task::none()
        };
        let read = if self.settings.open.is_some() && (flags || preferences) {
            self.read_flags()
        } else {
            let flags = if flags {
                self.read_flag_values(true)
            } else {
                Task::none()
            };
            let preferences = if preferences {
                self.read_preferences()
            } else {
                Task::none()
            };
            Task::batch([flags, preferences])
        };
        let consent = if usage {
            self.read_usage()
        } else {
            Task::none()
        };
        Task::batch([listed, read, consent])
    }
}

/// An owner answer as the flags it lists.
fn listed(answer: Result<(Value, u64), String>) -> Result<FlagList, String> {
    let (value, _) = answer?;
    serde_json::from_value(value).map_err(|error| format!("unreadable flags: {error}"))
}
