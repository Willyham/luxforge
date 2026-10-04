//! The Presets section's driver. Every library change goes through [`Editor::preset_update`], so the
//! section's buttons, its row menus and an evidence script share one path. Applying a preset is not
//! here: a row's click and its palette entry are the section action's own
//! [`ActionMessage::Run`](crate::app::message::action::ActionMessage::Run),
//! the path every declared action takes, with its one mutation, `asset.state` and preview job.
//!
//! The library calls are catalog work for the owner and file work for the task: none of them
//! renders, opens a source or touches the recipe, so none sets the editor's `busy` flag. They run
//! one at a time under the library's own `pending` flag instead, and each reads the listing again
//! before it answers.
use crate::state::MenuTarget;
use crate::{
    app::{
        Editor,
        message::{Message, preset::PresetMessage},
        outcome::Outcome,
        tasks::{
            PresetChange, preset_create_task, preset_delete_task, preset_export_task,
            preset_import_task, preset_report_task, request,
        },
    },
    state::{
        IN_FLIGHT,
        presets::{
            PresetForm, PresetLibrary, PresetsModel, capture_fields, import_status,
            presets_control, presettable_groups,
        },
        tools,
    },
};
use iced::Task;
use luxforge_core::{PresetSummary, ReportCounts};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The Presets section's own state: the library as `preset.list` last answered it, and the
/// section's create form.
#[derive(Default)]
pub(crate) struct Presets {
    pub(crate) library: PresetLibrary,
    pub(crate) form: PresetForm,
}

impl Editor {
    /// The Presets section's model, whether or not the panel draws the section. The derive builds
    /// it only while the section is expanded (a library clones every preset's settings and
    /// strings), so a reader outside the view, an evidence step naming a row, builds it here from
    /// the state as it stands, by the function the derive uses.
    pub(crate) fn presets_model_now(&self) -> Option<PresetsModel> {
        let (module, _) = presets_control(&self.modules)?;
        let section = self
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == module.id)?;
        tools::presets_in(&tools::controls_of(section, &self.inputs())).cloned()
    }

    /// Every Presets-section change goes through here.
    pub(crate) fn preset_update(&mut self, message: PresetMessage) -> Task<Message> {
        match message {
            PresetMessage::Listed(result) => match result {
                Ok((presets, sequence)) => self.adopt_presets(presets, sequence),
                Err(error) => {
                    self.status.text = format!("Presets unavailable: {error}");
                    self.presets.library.failed(error);
                }
            },
            PresetMessage::ToggleForm => {
                self.presets.form.open = !self.presets.form.open;
                self.presets.form.error = None;
            }
            PresetMessage::Name(name) => self.presets.form.name = name,
            PresetMessage::Group(group) => self.presets.form.group = group,
            PresetMessage::Check { label, checked } => {
                self.presets.form.checked.insert(label, checked);
            }
            PresetMessage::Cancel => self.presets.form = PresetForm::default(),
            PresetMessage::Create => {
                if self.presets.library.pending {
                    return self.preset_refused("Waiting for the last preset request".into());
                }
                let (capture, create) = match self.preset_create_requests() {
                    Ok(requests) => requests,
                    Err(error) => {
                        self.presets.form.error = Some(error.clone());
                        return self.preset_refused(error);
                    }
                };
                self.presets.form.error = None;
                self.presets.library.pending = true;
                self.status.text = "Saving the preset…".into();
                return preset_create_task(self.owner.clone(), self.client, capture, create);
            }
            PresetMessage::Created(result) => {
                self.presets.library.pending = false;
                match result {
                    Ok(change) => {
                        let (name, group) = named(&change.result["preset"]);
                        self.adopt_change(*change);
                        self.status.text =
                            format!("Saved preset \u{201c}{name}\u{201d} in {group}");
                        self.presets.form = PresetForm::default();
                        self.outcome(Outcome::PresetsAnswered { failure: None });
                    }
                    Err(error) => {
                        // A duplicate name is the core's `conflict`, shown as it is, in the form
                        // where the name can be changed and in the status bar.
                        self.presets.form.error = Some(error.clone());
                        self.preset_failed(error);
                    }
                }
            }
            PresetMessage::Import => {
                if self.view_state.picker_open
                    || self.presets.library.pending
                    || self.evidence.is_some()
                {
                    return Task::none();
                }
                self.view_state.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Presets", &["xmp", "lrtemplate", "lfpreset"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    |path| Message::Preset(PresetMessage::ImportPicked(path)),
                );
            }
            PresetMessage::ImportPicked(path) => {
                self.view_state.picker_open = false;
                if let Some(reason) = self.mask_tool_refusal() {
                    return self.preset_refused(reason);
                }
                if let Some(path) = path {
                    return self.preset_import(path);
                }
            }
            PresetMessage::Imported(result) => {
                self.presets.library.pending = false;
                match result {
                    Ok(change) => {
                        let (name, _) = named(&change.result["preset"]);
                        let report = change.result["report"].clone();
                        let counts = report_counts(&report);
                        self.adopt_change(*change);
                        self.status.text = match counts {
                            Some(counts) => import_status(&name, &counts),
                            None => format!("Imported \u{201c}{name}\u{201d}"),
                        };
                        // Copy in the status bar copies the whole report while this line stands.
                        self.status.copy = Some((
                            self.status.text.clone(),
                            serde_json::to_string_pretty(&report).unwrap_or_default(),
                        ));
                        self.outcome(Outcome::PresetsAnswered { failure: None });
                    }
                    Err(error) => self.preset_failed(error),
                }
            }
            PresetMessage::Delete(id) => {
                self.view_state.menu = None;
                if self.presets.library.pending {
                    return self.preset_refused("Waiting for the last preset request".into());
                }
                self.presets.library.pending = true;
                self.status.text = "Deleting the preset…".into();
                return preset_delete_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::Deleted(result) => {
                self.presets.library.pending = false;
                match result {
                    Ok(change) => {
                        self.status.text = if change.result["deleted"] == json!(true) {
                            "Deleted the preset".into()
                        } else {
                            "The preset was already gone".into()
                        };
                        self.adopt_change(*change);
                        self.outcome(Outcome::PresetsAnswered { failure: None });
                    }
                    Err(error) => self.preset_failed(error),
                }
            }
            PresetMessage::CopyReport(id) => {
                self.view_state.menu = None;
                return preset_report_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::ReportRead(result) => match result {
                Ok(report) => {
                    self.status.text = "Copied the import report".into();
                    return iced::clipboard::write(report);
                }
                Err(error) => self.status.text = error,
            },
            PresetMessage::Export(id) => {
                self.view_state.menu = None;
                if self.evidence.is_some() {
                    return Task::none();
                }
                return preset_export_task(self.owner.clone(), self.client, id);
            }
            PresetMessage::Exported(result) => {
                self.status.text = match result {
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
        if self.presets.library.pending {
            return self.preset_refused("Waiting for the last preset request".into());
        }
        self.presets.library.pending = true;
        self.status.text = "Importing the preset…".into();
        preset_import_task(self.owner.clone(), self.client, path)
    }

    /// The two requests Create sends, in order: `preset.capture` of the checked groups' fields for
    /// the displayed entry, and the `preset.create` those captured settings complete. The name and
    /// group go as typed; the library trims them and applies its own rules.
    pub(crate) fn preset_create_requests(&self) -> Result<(Value, Value), String> {
        let state = self
            .document
            .state
            .as_ref()
            .ok_or("No photograph is open")?;
        let entry = self
            .displayed_entry()
            .ok_or("No history entry is displayed")?;
        if self.busy {
            return Err(IN_FLIGHT.into());
        }
        let form = &self.presets.form;
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
        self.presets.library.adopt(presets, sequence);
        if let Some(MenuTarget::Preset(id)) = &self.view_state.menu
            && self.presets.library.find(id).is_none()
        {
            self.view_state.menu = None;
        }
    }

    pub(crate) fn adopt_change(&mut self, change: PresetChange) {
        self.read_back(change.request);
        self.adopt_presets(change.presets, change.sequence);
    }

    /// A library request that cannot be sent: say why.
    pub(super) fn preset_refused(&mut self, reason: String) -> Task<Message> {
        self.preset_failed(reason);
        Task::none()
    }

    /// A library request failed or was refused: the status bar shows the core's own error, and the
    /// failure is reported with it.
    fn preset_failed(&mut self, error: String) {
        self.outcome(Outcome::PresetsAnswered {
            failure: Some(&error),
        });
        self.status.text = error;
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
