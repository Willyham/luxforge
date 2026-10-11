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
            PresetForm, PresetLibrary, PresetsModel, import_status, presets_control,
            settings_groups,
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
    /// Double-click reset may arrive while the first click's commit is still answering.
    reset_after: Option<luxforge_core::AssetId>,
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
            PresetMessage::Hover(id) => return self.preset_hover(id),
            PresetMessage::Leave(id) => {
                if self.presets.library.interaction.hovered.as_ref() == Some(&id) {
                    return self.cancel_preset_hover();
                }
            }
            PresetMessage::Amount(value) => return self.preset_amount(value),
            PresetMessage::AmountReleased => {
                if self.preset_drafting() && self.presets.library.interaction.hovered.is_none() {
                    return self.release();
                }
            }
            PresetMessage::AmountEdit => {
                self.presets.library.interaction.editing = tools::ValueEdit::Typing(format!(
                    "{:.0}",
                    self.presets.library.interaction.amount
                ));
                self.presets.library.interaction.invalid = None;
                return iced::widget::operation::focus(iced::widget::Id::new("preset-amount"));
            }
            PresetMessage::AmountText(text) => {
                self.presets.library.interaction.editing = tools::ValueEdit::Typing(text);
                self.presets.library.interaction.invalid = None;
            }
            PresetMessage::AmountSubmit => {
                if let tools::ValueEdit::Typing(text) = &self.presets.library.interaction.editing {
                    if let Ok(value) = text.trim().parse::<f64>()
                        && value.is_finite()
                        && (0.0..=200.0).contains(&value)
                    {
                        let moved = self.preset_amount(value);
                        let released = self.preset_update(PresetMessage::AmountReleased);
                        return Task::batch([moved, released]);
                    }
                    self.presets.library.interaction.invalid =
                        Some("Amount must be between 0 and 200".into());
                }
            }
            PresetMessage::AmountCancel => {
                self.presets.library.interaction.editing = tools::ValueEdit::None;
                self.presets.library.interaction.invalid = None;
            }
            PresetMessage::AmountReset => {
                if self.busy
                    || self
                        .core_gesture()
                        .is_some_and(|gesture| gesture.draft.released())
                {
                    self.presets.reset_after = self
                        .document
                        .state
                        .as_ref()
                        .map(|state| state.asset.id.clone());
                    return Task::none();
                }
                let moved = self.preset_amount(100.0);
                let released = self.preset_update(PresetMessage::AmountReleased);
                return Task::batch([moved, released]);
            }
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
            PresetMessage::Check { id, checked } => {
                self.presets.form.checked.insert(id, checked);
            }
            PresetMessage::Analysis { id, checked } => {
                if checked {
                    // The groups the step overwrites are cleared, so clearing the step again does
                    // not bring them back checked.
                    for group in settings_groups(&self.modules, self.developer).groups {
                        if group.overwritten_by.contains(&id) {
                            self.presets.form.checked.insert(group.id, false);
                        }
                    }
                    self.presets.form.analysis.insert(id);
                } else {
                    self.presets.form.analysis.remove(&id);
                }
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
        let groups = form.capture_ids(&settings_groups(&self.modules, self.developer));
        if groups.is_empty() {
            return Err("Choose at least one group of settings to keep".into());
        }
        Ok((
            json!({"asset_id": state.asset.id, "entry_id": entry, "groups": groups}),
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

    pub(crate) fn preset_drafting(&self) -> bool {
        self.slider_gesture().is_some_and(|slider| {
            slider.action == "apply-settings"
                && ["amount", "preview"].contains(&slider.parameter.as_str())
        })
    }

    pub(crate) fn cancel_preset_hover(&mut self) -> Task<Message> {
        if self.presets.library.interaction.hovered.take().is_some() && self.preset_drafting() {
            self.event("preset_hover_cancelled", || json!({}));
            return self.discard();
        }
        Task::none()
    }

    fn preset_hover(&mut self, id: String) -> Task<Message> {
        if self.presets.library.interaction.hovered.as_ref() == Some(&id) {
            return Task::none();
        }
        if self.presets.library.interaction.hovered.is_none()
            && self
                .gesture_refusal(super::gesture::Starting::Preset)
                .is_some()
        {
            return Task::none();
        }
        if self.busy || !self.at_current() || self.mask_tool_refusal().is_some() {
            return Task::none();
        }
        let Some(preset) = self.presets.library.find(&id).cloned() else {
            return Task::none();
        };
        if !preset.unavailable.is_empty()
            || !luxforge_core::settings_can_preview(&self.modules, &preset.settings)
        {
            return self.cancel_preset_hover();
        }
        let Some((module, action)) = presets_control(&self.modules) else {
            return Task::none();
        };
        let Some(mut fields) = module
            .action(action)
            .and_then(|a| crate::state::presets::apply_fields(a, &preset))
        else {
            return Task::none();
        };
        let amount =
            if luxforge_core::settings_at_amount(&self.modules, &preset.settings, 0.0).is_ok() {
                self.presets.library.interaction.amount
            } else {
                100.0
            };
        fields.insert("amount".into(), json!(amount));
        self.presets.library.interaction.hovered = Some(id.clone());
        self.event("preset_hover", || json!({"preset_id":id,"amount":amount}));
        self.preset_offer(fields)
    }

    fn preset_offer(&mut self, fields: serde_json::Map<String, Value>) -> Task<Message> {
        if self.preset_drafting() {
            return self.drive(super::draft::Event::Offer(Value::Object(fields)));
        }
        let gesture = self.next_gesture();
        self.open_core(
            gesture,
            super::gesture::Kind::Slider(super::gesture::SliderGesture {
                action: "apply-settings".into(),
                parameter: if self.presets.library.interaction.hovered.is_some() {
                    "preview"
                } else {
                    "amount"
                }
                .into(),
                label: "Preset Amount".into(),
                target: Default::default(),
                unpreviewed: false,
            }),
            Some(Value::Object(fields)),
        )
    }

    fn preset_amount(&mut self, amount: f64) -> Task<Message> {
        if !amount.is_finite() || !(0.0..=200.0).contains(&amount) {
            return self.preset_refused("Amount must be between 0 and 200".into());
        }
        if self.busy || !self.at_current() || self.mask_tool_refusal().is_some() {
            return Task::none();
        }
        if self.core_gesture().is_some() && !self.preset_drafting() {
            return Task::none();
        }
        if self.gesture_conflicted() {
            return Task::none();
        }
        // Keep the request pinned for a continuing Amount gesture, even if another client edits
        // the library meanwhile. It is the same immutable settings snapshot the draft began with.
        if self.preset_drafting()
            && self.presets.library.interaction.hovered.is_none()
            && let Some(mut fields) = self
                .core_gesture()
                .and_then(|gesture| gesture.draft.sent())
                .and_then(Value::as_object)
                .cloned()
        {
            let Some(settings) = fields.get("settings").and_then(Value::as_object) else {
                return Task::none();
            };
            if let Err(error) = luxforge_core::settings_at_amount(&self.modules, settings, amount) {
                return self.preset_refused(error.detail);
            }
            fields.insert("amount".into(), json!(amount));
            self.presets.library.interaction.amount = amount;
            self.presets.library.interaction.editing = tools::ValueEdit::None;
            self.presets.library.interaction.invalid = None;
            return self.preset_offer(fields);
        }
        let hovering = self.presets.library.interaction.hovered.is_some();
        let preset = self
            .presets
            .library
            .interaction
            .hovered
            .as_deref()
            .and_then(|id| self.presets.library.find(id))
            .or_else(|| {
                self.document.state.as_ref().and_then(|state| {
                    crate::state::presets::selected_preset(
                        &self.presets.library,
                        &state.current_entry,
                    )
                })
            })
            .cloned();
        if let Some(preset) = &preset
            && let Err(error) =
                luxforge_core::settings_at_amount(&self.modules, &preset.settings, amount)
        {
            return self.preset_refused(error.detail);
        }
        self.presets.library.interaction.amount = amount;
        self.presets.library.interaction.editing = tools::ValueEdit::None;
        self.presets.library.interaction.invalid = None;
        let Some(preset) = preset else {
            return Task::none();
        };
        let Some((module, action)) = presets_control(&self.modules) else {
            return Task::none();
        };
        let Some(mut fields) = module
            .action(action)
            .and_then(|a| crate::state::presets::apply_fields(a, &preset))
        else {
            return Task::none();
        };
        fields.insert("amount".into(), json!(amount));
        if !hovering {
            self.controls.dragging = Some((action.to_owned(), "amount".into()));
        }
        self.preset_offer(fields)
    }

    /// A row click over its hover preview commits that exact draft, rather than applying over it.
    pub(crate) fn commit_preset_hover(
        &mut self,
        action: &str,
        fields: &serde_json::Map<String, Value>,
    ) -> Option<Task<Message>> {
        if action != "apply-settings" || !self.preset_drafting() {
            return None;
        }
        let hovered = self.presets.library.interaction.hovered.as_deref()?;
        if fields.get("origin")?["preset_id"].as_str() != Some(hovered) {
            return None;
        }
        self.presets.library.interaction.hovered = None;
        Some(self.release())
    }
}

/// Passive previews yield to the next explicit operation, including focus loss and other edits.
pub(super) fn before_message(editor: &mut Editor, message: &Message) -> Task<Message> {
    if editor.presets.library.interaction.hovered.is_none() {
        return Task::none();
    }
    if matches!(
        message,
        Message::Preset(
            PresetMessage::Hover(_)
                | PresetMessage::Leave(_)
                | PresetMessage::Amount(_)
                | PresetMessage::AmountReleased
        )
    ) {
        return Task::none();
    }
    if let Message::Action(super::message::action::ActionMessage::Run { action, preset }) = message
        && action == "apply-settings"
        && preset.get("origin").is_some_and(|origin| {
            origin["preset_id"].as_str() == editor.presets.library.interaction.hovered.as_deref()
        })
    {
        return Task::none();
    }
    if message.yields_to_mask_tool()
        || matches!(
            message,
            Message::Close | Message::Key(iced::Event::Window(iced::window::Event::Unfocused), _)
        )
    {
        return editor.cancel_preset_hover();
    }
    Task::none()
}

pub(super) fn after_message(editor: &mut Editor, before: &super::Before) -> Task<Message> {
    if !editor.preset_drafting() {
        editor.presets.library.interaction.hovered = None;
    }
    let changed = before.entry != editor.displayed_entry();
    let invalid_hover = editor
        .presets
        .library
        .interaction
        .hovered
        .as_deref()
        .is_some_and(|id| {
            editor.presets.library.find(id).is_none_or(|preset| {
                editor
                    .core_gesture()
                    .and_then(|gesture| gesture.draft.sent())
                    .is_some_and(|sent| sent["settings"] != Value::Object(preset.settings.clone()))
            })
        })
        || editor.gesture_conflicted();
    let cancelled = if changed || invalid_hover {
        editor.cancel_preset_hover()
    } else {
        Task::none()
    };
    if changed && !editor.preset_drafting() {
        editor.presets.library.interaction.amount = editor
            .document
            .state
            .as_ref()
            .and_then(|state| {
                crate::state::presets::selected_preset(
                    &editor.presets.library,
                    &state.current_entry,
                )
                .map(|_| {
                    state.current_entry.parameters["amount"]
                        .as_f64()
                        .unwrap_or(100.0)
                })
            })
            .unwrap_or(100.0);
        editor.presets.library.interaction.editing = tools::ValueEdit::None;
        editor.presets.library.interaction.invalid = None;
    }
    if !editor.busy
        && editor.core_gesture().is_none()
        && let Some(asset) = editor.presets.reset_after.take()
        && editor
            .document
            .state
            .as_ref()
            .is_some_and(|state| state.asset.id == asset)
    {
        let reset = editor.preset_update(PresetMessage::AmountReset);
        return Task::batch([cancelled, reset]);
    }
    cancelled
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
