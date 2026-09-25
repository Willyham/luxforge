//! The Presets section's driver. Every library change goes through [`Editor::preset_update`], so the
//! section's buttons, its row menus and an evidence script share one path. Applying a preset is not
//! here: a row's click and its palette entry are the section action's own [`Message::RunAction`],
//! the path every declared action takes, with its one mutation, `asset.state` and preview job.
//!
//! The library calls are catalog work for the owner and file work for the task: none of them
//! renders, opens a source or touches the recipe, so none sets the editor's `busy` flag. They run
//! one at a time under the library's own `pending` flag instead, and each reads the listing again
//! before it answers.
use crate::{
    app::{
        Editor,
        evidence::Settle,
        message::{MenuTarget, Message, PresetMessage},
        tasks::{
            PresetChange, preset_create_task, preset_delete_task, preset_export_task,
            preset_import_task, preset_report_task, request,
        },
    },
    state::presets::{PresetForm, capture_fields, import_status, presettable_groups},
};
use iced::Task;
use lightwell_core::{PresetSummary, ReportCounts};
use serde_json::{Value, json};
use std::path::PathBuf;

impl Editor {
    /// Every Presets-section change goes through here.
    pub(crate) fn preset_update(&mut self, message: PresetMessage) -> Task<Message> {
        match message {
            PresetMessage::Listed(result) => match result {
                Ok((presets, sequence)) => self.adopt_presets(presets, sequence),
                Err(error) => {
                    self.status = format!("Presets unavailable: {error}");
                    self.presets.failed(error);
                }
            },
            PresetMessage::ToggleForm => {
                self.preset_form.open = !self.preset_form.open;
                self.preset_form.error = None;
            }
            PresetMessage::Name(name) => self.preset_form.name = name,
            PresetMessage::Group(group) => self.preset_form.group = group,
            PresetMessage::Check { label, checked } => {
                self.preset_form.checked.insert(label, checked);
            }
            PresetMessage::Cancel => self.preset_form = PresetForm::default(),
            PresetMessage::Create => {
                if self.presets.pending {
                    return self.preset_refused("Waiting for the last preset request".into());
                }
                let (capture, create) = match self.preset_create_requests() {
                    Ok(requests) => requests,
                    Err(error) => {
                        self.preset_form.error = Some(error.clone());
                        return self.preset_refused(error);
                    }
                };
                self.preset_form.error = None;
                self.presets.pending = true;
                self.status = "Saving the preset…".into();
                return preset_create_task(self.owner.clone(), self.client, capture, create);
            }
            PresetMessage::Created(result) => {
                self.presets.pending = false;
                match result {
                    Ok(change) => {
                        let (name, group) = named(&change.result["preset"]);
                        self.adopt_change(*change);
                        self.status = format!("Saved preset \u{201c}{name}\u{201d} in {group}");
                        self.preset_form = PresetForm::default();
                    }
                    Err(error) => {
                        // A duplicate name is the core's `conflict`, shown as it is, in the form
                        // where the name can be changed and in the status bar.
                        self.preset_form.error = Some(error.clone());
                        self.preset_failed(error);
                    }
                }
                self.settle_step(Settle::Presets);
            }
            PresetMessage::Import => {
                if self.picker_open || self.presets.pending || self.evidence.is_some() {
                    return Task::none();
                }
                self.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Presets", &["xmp", "lrtemplate", "lwpreset"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    |path| Message::Preset(PresetMessage::ImportPicked(path)),
                );
            }
            PresetMessage::ImportPicked(path) => {
                self.picker_open = false;
                if let Some(path) = path {
                    return self.preset_import(path);
                }
            }
            PresetMessage::Imported(result) => {
                self.presets.pending = false;
                match result {
                    Ok(change) => {
                        let (name, _) = named(&change.result["preset"]);
                        let report = change.result["report"].clone();
                        let counts = report_counts(&report);
                        self.adopt_change(*change);
                        self.status = match counts {
                            Some(counts) => import_status(&name, &counts),
                            None => format!("Imported \u{201c}{name}\u{201d}"),
                        };
                        // Copy in the status bar copies the whole report while this line stands.
                        self.status_copy = Some((
                            self.status.clone(),
                            serde_json::to_string_pretty(&report).unwrap_or_default(),
                        ));
                    }
                    Err(error) => self.preset_failed(error),
                }
                self.settle_step(Settle::Presets);
            }
            PresetMessage::Delete(id) => {
                self.menu = None;
                if self.presets.pending {
                    return self.preset_refused("Waiting for the last preset request".into());
                }
                self.presets.pending = true;
                self.status = "Deleting the preset…".into();
                return preset_delete_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::Deleted(result) => {
                self.presets.pending = false;
                match result {
                    Ok(change) => {
                        self.status = if change.result["deleted"] == json!(true) {
                            "Deleted the preset".into()
                        } else {
                            "The preset was already gone".into()
                        };
                        self.adopt_change(*change);
                    }
                    Err(error) => self.preset_failed(error),
                }
                self.settle_step(Settle::Presets);
            }
            PresetMessage::CopyReport(id) => {
                self.menu = None;
                return preset_report_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::ReportRead(result) => match result {
                Ok(report) => {
                    self.status = "Copied the import report".into();
                    return iced::clipboard::write(report);
                }
                Err(error) => self.status = error,
            },
            PresetMessage::Export(id) => {
                self.menu = None;
                if self.evidence.is_some() {
                    return Task::none();
                }
                return preset_export_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::Exported(result) => {
                self.status = match result {
                    Ok(Some(file)) => format!("Exported {file}"),
                    Ok(None) => "Export cancelled".into(),
                    Err(error) => error,
                };
            }
        }
        Task::none()
    }

    /// Import one file, chosen in the dialog or named by an evidence script: the same task either
    /// way, which reads the file off the update loop and refuses it there when it is too large.
    pub(crate) fn preset_import(&mut self, path: PathBuf) -> Task<Message> {
        if self.presets.pending {
            return self.preset_refused("Waiting for the last preset request".into());
        }
        self.presets.pending = true;
        self.status = "Importing the preset…".into();
        preset_import_task(self.owner.clone(), self.client, path)
    }

    /// The two requests Create sends, in order: `preset.capture` of the checked groups' fields for
    /// the displayed entry, and the `preset.create` those captured settings complete. The name and
    /// group go as typed; the library trims them and applies its own rules.
    pub(crate) fn preset_create_requests(&self) -> Result<(Value, Value), String> {
        let state = self.state.as_ref().ok_or("No photograph is open")?;
        let entry = self
            .displayed_entry()
            .ok_or("No history entry is displayed")?;
        if self.busy {
            return Err("Waiting for the last request".into());
        }
        let form = &self.preset_form;
        if form.name.trim().is_empty() {
            return Err("Name the preset before creating it".into());
        }
        if form.group.trim().is_empty() {
            return Err("Name the preset's group before creating it".into());
        }
        let fields = capture_fields(&presettable_groups(&self.modules, self.developer), form);
        if fields.is_empty() {
            return Err("Choose at least one group of settings to keep".into());
        }
        Ok((
            json!({"asset_id": state.asset.id, "entry_id": entry, "fields": fields}),
            json!({"name": form.name, "group": form.group, "mutation": request()}),
        ))
    }

    /// Adopt a listing, and close a row menu whose preset it no longer holds.
    pub(crate) fn adopt_presets(&mut self, presets: Vec<PresetSummary>, sequence: u64) {
        self.presets.adopt(presets, sequence);
        if let Some(MenuTarget::Preset(id)) = &self.menu
            && self.presets.find(id).is_none()
        {
            self.menu = None;
        }
    }

    pub(crate) fn adopt_change(&mut self, change: PresetChange) {
        self.read_back(change.request);
        self.adopt_presets(change.presets, change.sequence);
    }

    /// A library request that cannot be sent: say why, and let a waiting script step capture it.
    fn preset_refused(&mut self, reason: String) -> Task<Message> {
        self.preset_failed(reason);
        self.settle_step(Settle::Presets);
        Task::none()
    }

    /// A library request failed: the status bar shows the core's own error, and a script step that
    /// sent it is recorded as failed.
    fn preset_failed(&mut self, error: String) {
        self.refuse_step(&error);
        self.status = error;
    }
}

/// A record's name and group, as the status line quotes them.
fn named(record: &Value) -> (String, String) {
    (
        record["name"].as_str().unwrap_or_default().to_owned(),
        record["group"].as_str().unwrap_or_default().to_owned(),
    )
}

/// The four counts of a full import report.
fn report_counts(report: &Value) -> Option<ReportCounts> {
    let count = |list: &str| report[list].as_array().map(Vec::len);
    Some(ReportCounts {
        mapped: count("mapped")?,
        neutral: count("neutral")?,
        unsupported: count("unsupported")?,
        refused: count("refused")?,
    })
}
