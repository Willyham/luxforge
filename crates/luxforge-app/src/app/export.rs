//! Exporting ([export settings design](../../../../docs/design/export-settings.md#the-export-sheet)):
//! the Export menu, the Export sheet, Export again and Export with preset.
//!
//! The desktop holds no export rules. The sheet ([`crate::state::export_sheet`]) holds the settings
//! the person is choosing and shows what `export.plan` answers for them: the output size, the
//! physical size, the name it will write and every notice. It asks for a plan only when a setting
//! the plan depends on changes (the format, size, density, folder, suffix and the encoding choices
//! the estimate counts — never the quality slider or the metadata level), one plan in flight with
//! the newest change pending ([`Editor::sheet_changed`]). The presets are `export.presets.*`.
//!
//! Every export of one photograph is one chain of owner requests off the update loop
//! ([`write_task`]): `export.plan` for the settings, which names the destination (the first free
//! `<stem><suffix>.<ext>` in the settings' folder), and `export.write` with exactly those settings
//! and that destination, asked again once the source is prepared when the owner answers
//! `preparation-required` — the requests an API client sends for the same settings. The sheet's
//! Export, Export again (the remembered last export's settings) and Export with preset (the
//! preset's own) differ only in the settings they send. With no density set, the desktop on macOS
//! names the window's own ([`screen_pixels_per_inch`], export decision 7) in what it sends.
//!
//! The job runs on the core's export lane, and the desktop reads it with `job.read` until it ends,
//! through a reader that exists only while this window's export is queued or running (performance
//! rule 8) and sends a message only when the job's record changes or the job ends
//! ([`job_reads`]). The status bar shows its determinate progress — phase and percent — from the
//! start for an export whose plan warned `large-image`, and otherwise once the job has run for a
//! second, judged on its reads with no timer of its own ([`ExportRun::bar`]); the bar opens the
//! Performance section, whose export row has Cancel. Once an export the person started from the
//! sheet, the menu or the palette succeeds, its settings and preset are stored as the last export
//! (`export_last`).
//!
//! An evidence run's `export` file step bypasses only the sheet: it names a file in the run's
//! evidence directory, written with today's full-size settings, and is never remembered.
//!
//! An entry holding stale frozen patches (`docs/design/corrections.md`, "Input identity") is
//! refused `conflict` with `data.code` `stale-unacknowledged` until an export acknowledges exactly
//! them. The desktop never acknowledges on its own: it holds the choice and says on the canvas
//! which patches are stale and why, and **Export anyway** sends `export.write` again, as a request
//! of its own, with `acknowledge_stale` naming that entry and exactly those patches; **Cancel**
//! lets it go. A commit between the two makes the owner refuse the old acknowledgement, which is
//! held and offered again in the same way.
use crate::app::{
    Editor,
    gesture::Starting,
    job_reads::{self, Pass, Reader, Watch},
    message::{
        Message,
        export::{ExportMessage, SheetMessage},
    },
    outcome::Outcome,
    tasks::{CallError, call, call_detailed, owner_task, request, wait_source_job},
};
use crate::state::{
    MenuTarget,
    export_sheet::{
        self as sheet_model, BatchPlan, CardChoice, ExportSheet, NamingTarget, SheetPlan,
    },
};
use iced::{Subscription, Task, futures::stream::BoxStream};
#[cfg(test)]
use luxforge_core::jobs::JOB_READ;
use luxforge_core::{
    AssetId, ClientId, EntryId, ErrorKind, ExportFolder, ExportPreset, ExportSettings,
    MetadataLevel, OwnerHandle, preferences::ExportLast,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How many times `export.write` is asked again after its source was prepared.
const PREPARATION_RETRIES: usize = 4;

/// How long an export runs before the status bar shows its progress, unless its plan warned.
pub(crate) const BAR_DELAY: Duration = Duration::from_secs(1);

/// What `export.write` is sent: everything the request carries, with the plan that named its
/// destination.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportChoice {
    pub(crate) asset_id: AssetId,
    pub(crate) entry_id: EntryId,
    pub(crate) destination: PathBuf,
    /// The settings sent, as the request carries them.
    pub(crate) settings: ExportSettings,
    /// Ask for the reference renderer's export, `reference: true`; otherwise `reference` is not
    /// sent.
    pub(crate) reference: bool,
    /// `export.plan`'s answer, reported when the choice is taken up.
    pub(crate) plan: Value,
    /// The exact acknowledgement of the entry's stale patches, `{entry_id, operations}`, sent only
    /// by Export anyway, the person's answer to the refusal that named them.
    pub(crate) acknowledge_stale: Option<Value>,
    /// What is stored as the last export once it succeeds: none for an evidence file.
    pub(crate) remember: Option<ExportLast>,
}

/// One export to start: the entry, the settings it is written with, where (a destination named by
/// an evidence step, or the plan's suggestion) and what it is remembered as.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportRequest {
    pub(crate) asset_id: AssetId,
    pub(crate) entry_id: EntryId,
    pub(crate) settings: ExportSettings,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) reference: bool,
    pub(crate) remember: Option<ExportLast>,
}

/// An export refused for its entry's stale frozen patches, held until the person answers: the
/// choice, and the entry and patches the refusal named, which Export anyway acknowledges exactly.
#[derive(Clone, Debug)]
pub(crate) struct HeldStale {
    pub(crate) choice: Box<ExportChoice>,
    pub(crate) entry_id: Value,
    pub(crate) stale: Vec<luxforge_core::StaleCorrection>,
}

impl HeldStale {
    /// The `acknowledge_stale` Export anyway sends: the refused entry and its stale patches.
    pub(crate) fn acknowledgement(&self) -> Value {
        json!({
            "entry_id": self.entry_id,
            "operations": self.stale.iter().map(|patch| &patch.operation).collect::<Vec<_>>(),
        })
    }
}

/// The held export a refusal with `data` leaves, when `data` names stale patches.
pub(crate) fn held_stale(choice: ExportChoice, data: Option<&Value>) -> Option<HeldStale> {
    let data = data?;
    if data["code"] != "stale-unacknowledged" {
        return None;
    }
    let stale: Vec<luxforge_core::StaleCorrection> =
        serde_json::from_value(data["stale"].clone()).ok()?;
    (!stale.is_empty() && data["entry_id"].is_string()).then(|| HeldStale {
        choice: Box::new(choice),
        entry_id: data["entry_id"].clone(),
        stale,
    })
}

/// The one export this window runs, from its plan to its last read, an export held for its stale
/// patches, the open sheet and the presets as last listed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Exporting {
    pub(crate) run: Option<ExportRun>,
    pub(crate) held: Option<HeldStale>,
    pub(crate) sheet: Option<ExportSheet>,
    /// Each opening gets its own identity: a closed sheet's late plan cannot answer its successor.
    pub(crate) sheet_generation: u64,
    /// `export.presets.list` as last answered: built-in first, then the person's by name.
    pub(crate) presets: Option<Vec<ExportPreset>>,
    /// The last presets read failed.
    pub(crate) presets_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportRun {
    /// It asked for the reference renderer's export.
    pub(crate) reference: bool,
    /// The destination's file name, once the plan named it.
    pub(crate) file_name: Option<String>,
    /// The core's job, once `export.write` has queued it.
    pub(crate) job_id: Option<String>,
    /// What `export.write` was sent, kept so a refusal for stale patches can hold it.
    pub(crate) choice: Option<Box<ExportChoice>>,
    /// What is stored as the last export once it succeeds.
    pub(crate) remember: Option<ExportLast>,
    /// Its plan carried `large-image`, so the status bar shows its progress from the start.
    pub(crate) warned: bool,
    /// When the job was queued, from which its reads judge how long it has run.
    pub(crate) queued: Option<Instant>,
    /// The job's progress on its last read: the fraction and the phase.
    pub(crate) progress: Option<(f64, Option<String>)>,
    /// The status bar shows its progress: from the start for a warned export, otherwise from the
    /// first read after it has run [`BAR_DELAY`]. Once shown it stays until the export ends.
    pub(crate) bar: bool,
}

impl ExportRun {
    fn new(reference: bool, remember: Option<ExportLast>) -> Self {
        Self {
            reference,
            file_name: None,
            job_id: None,
            choice: None,
            remember,
            warned: false,
            queued: None,
            progress: None,
            bar: false,
        }
    }
}

impl Exporting {
    /// An export is under way, from the press to its last read.
    pub(crate) fn active(&self) -> bool {
        self.run.is_some()
    }

    /// The job this window follows is queued or running at the core.
    fn live(&self) -> Option<&str> {
        self.run.as_ref()?.job_id.as_deref()
    }

    /// The preset listed as `preset_id`.
    pub(crate) fn preset(&self, preset_id: &str) -> Option<&ExportPreset> {
        self.presets
            .as_deref()?
            .iter()
            .find(|preset| preset.preset_id == preset_id)
    }
}

impl Editor {
    pub(super) fn export_update(&mut self, message: ExportMessage) -> Task<Message> {
        match message {
            ExportMessage::Menu => {
                self.list_export_presets();
                self.view_state.menu = Some(MenuTarget::Export);
                Task::none()
            }
            ExportMessage::OpenSheet { reference } => self.open_export_sheet(reference),
            ExportMessage::Sheet(message) => self.sheet_update(message),
            ExportMessage::Planned {
                generation,
                settings,
                result,
            } => self.sheet_planned(generation, *settings, result),
            ExportMessage::Again => self.export_again(),
            ExportMessage::WithPreset(preset_id) => self.export_with_preset(&preset_id),
            ExportMessage::Sent { choice, result } => self.export_sent(choice, result),
            ExportMessage::Anyway => self.export_anyway(),
            ExportMessage::Dismiss => {
                if self.export.held.take().is_some() {
                    self.status.text = "Export cancelled".into();
                    self.event("export_stale_dismissed", || json!({}));
                }
                Task::none()
            }
            ExportMessage::Read { job_id, result } => self.export_read(job_id, result),
        }
    }

    /// Whether an export can start now: the one answer [`Editor::export_refusal`] gives the press,
    /// which the title bar's Export button reads as its enabled state.
    pub(crate) fn can_export(&self) -> bool {
        self.export_refusal().is_none()
    }

    /// Why an export cannot start now, in the words the status bar uses: a photograph is open,
    /// this window is not already exporting (one export per window is all the desktop runs; the
    /// core would queue more), no request is in flight — the one refusal's busy half
    /// ([`Starting::Export`]), with no one-draft rule, since an open draft does not change the
    /// displayed entry an export writes — and no file dialog is open.
    pub(crate) fn export_refusal(&self) -> Option<String> {
        if self.document.state.is_none() {
            return Some("Open a photograph to export it".into());
        }
        if self.export.active() {
            return Some("An export is already running".into());
        }
        if self.export.held.is_some() {
            return Some("Answer the stale patches notice first".into());
        }
        if let Some(reason) = self.gesture_refusal(Starting::Export) {
            return Some(reason);
        }
        self.view_state
            .picker_open
            .then(|| "A file dialog is already open".to_owned())
    }

    fn close_export_menu(&mut self) {
        if matches!(self.view_state.menu, Some(MenuTarget::Export)) {
            self.view_state.menu = None;
        }
    }

    /// The saved photograph being exported, including the comparison's fixed After entry.
    pub(super) fn export_entry(&self) -> Option<EntryId> {
        self.session
            .preview
            .comparison
            .as_ref()
            .filter(|comparison| {
                self.document.state.as_ref().map(|state| &state.asset.id)
                    == Some(&comparison.asset_id)
            })
            .map(|comparison| comparison.after_entry.clone())
            .or_else(|| self.displayed_entry())
    }

    /// The last export as the preferences remember it.
    pub(crate) fn export_last(&self) -> Option<ExportLast> {
        self.preferences.applied_export_last().cloned()
    }

    /// `export.presets.list`, read now: the sheet, the menu and the palette list exactly the
    /// presets the API lists. A small bounded document read on the owner, as a gesture's own call.
    pub(crate) fn list_export_presets(&mut self) {
        let result = presets_now(&self.owner, self.client);
        self.export_presets_listed(result);
    }

    fn export_presets_listed(&mut self, result: Result<Vec<ExportPreset>, String>) {
        match result {
            Ok(presets) => {
                self.export.presets = Some(presets);
                self.export.presets_error = None;
            }
            Err(error) => self.export.presets_error = Some(error),
        }
    }

    /// The settings a sheet opens on: the last export's, from the preset it named while that is
    /// still listed; the first time, Web's.
    fn opening_settings(&self) -> Option<(ExportSettings, Option<String>)> {
        if let Some(last) = self.export_last() {
            let preset = last.preset_id.filter(|id| self.export.preset(id).is_some());
            return Some((last.settings, preset));
        }
        self.export
            .preset(sheet_model::WEB)
            .map(|web| (web.settings.clone(), Some(web.preset_id.clone())))
    }

    /// `Cmd+E`, Export… or the palette: the sheet over the displayed entry.
    fn open_export_sheet(&mut self, reference: bool) -> Task<Message> {
        self.close_export_menu();
        if let Some(refusal) = self.export_refusal() {
            self.status.text = refusal;
            return Task::none();
        }
        let Some(entry_id) = self.export_entry() else {
            self.status.text = "No history entry is displayed".into();
            return Task::none();
        };
        let state = self.document.state.as_ref().expect("checked above");
        let file_name = file_name(&state.asset.locator);
        let stem = state
            .asset
            .locator
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".into());
        let target = sheet_model::SheetTarget::Single {
            asset_id: state.asset.id.clone(),
            entry_id,
            file_name,
            stem,
        };
        self.open_export_sheet_over(target, reference)
    }

    /// Open the sheet over `target`, reading the presets it shows and planning its settings.
    pub(crate) fn open_export_sheet_over(
        &mut self,
        target: sheet_model::SheetTarget,
        reference: bool,
    ) -> Task<Message> {
        self.list_export_presets();
        let Some((settings, preset_id)) = self.opening_settings() else {
            self.status.text = format!(
                "Export presets unavailable: {}",
                self.export.presets_error.clone().unwrap_or_default()
            );
            return Task::none();
        };
        self.event(
            "export_sheet_opened",
            || json!({"batch": target.batch(), "reference": reference, "preset_id": preset_id}),
        );
        self.export.sheet_generation += 1;
        self.export.sheet = Some(ExportSheet::new(target, reference, settings, preset_id));
        self.plan_sheet()
    }

    /// One gesture on the open sheet.
    fn sheet_update(&mut self, message: SheetMessage) -> Task<Message> {
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        let presets = self.export.presets.clone().unwrap_or_default();
        match message {
            SheetMessage::Preset(id) => {
                sheet.card_menu = None;
                if let Some(preset) = presets.iter().find(|preset| preset.preset_id == id) {
                    sheet.choose(preset);
                }
            }
            SheetMessage::CardMenu(id) => {
                sheet.card_menu = id;
                return Task::none();
            }
            SheetMessage::Card(id, choice) => {
                sheet.card_menu = None;
                let Some(preset) = presets.iter().find(|preset| preset.preset_id == id) else {
                    return Task::none();
                };
                match choice {
                    CardChoice::Update => {
                        let settings = sheet.settings.clone();
                        return self.change_preset(
                            "update",
                            "export.presets.update",
                            json!({"preset_id": id, "settings": settings}),
                        );
                    }
                    CardChoice::Rename => {
                        sheet.naming = Some(sheet_model::Naming {
                            target: NamingTarget::Rename(id),
                            text: preset.name.clone(),
                        });
                    }
                    CardChoice::Duplicate => {
                        sheet.naming = Some(sheet_model::Naming {
                            target: NamingTarget::Duplicate(id),
                            text: format!("{} copy", preset.name),
                        });
                    }
                    CardChoice::Delete => {
                        return self.change_preset(
                            "delete",
                            "export.presets.delete",
                            json!({"preset_id": id}),
                        );
                    }
                }
                return Task::none();
            }
            SheetMessage::Format(format) => sheet.set_format(format),
            SheetMessage::Fit(fit) => sheet.set_fit(fit),
            SheetMessage::Text(field, text) => sheet.type_text(field, text),
            SheetMessage::Quality(quality) => sheet.set_quality(quality),
            SheetMessage::Optimise(on) => sheet.set_optimise(on),
            SheetMessage::Chroma(chroma) => sheet.set_chroma(chroma),
            SheetMessage::Limit(on) => sheet.set_limit(on),
            SheetMessage::BitDepth(depth) => sheet.set_bit_depth(depth),
            SheetMessage::Metadata(level) => sheet.set_metadata(level),
            SheetMessage::Sharpen(medium) => sheet.set_sharpen(medium),
            SheetMessage::SharpenAmount(amount) => sheet.set_sharpen_amount(amount),
            SheetMessage::Density(on) => sheet.set_density(on),
            SheetMessage::OriginalFolder => sheet.set_original_folder(),
            SheetMessage::ChosenFolder { choose } => match (&sheet.chosen_folder, choose) {
                (Some(folder), false) => {
                    let folder = folder.clone();
                    sheet.set_folder(folder);
                }
                _ => return self.choose_export_folder(),
            },
            SheetMessage::FolderChosen(folder) => {
                self.view_state.picker_open = false;
                if let (Some(folder), Some(sheet)) = (folder, self.export.sheet.as_mut()) {
                    sheet.set_folder(folder);
                }
            }
            SheetMessage::Name(target) => {
                let text = match &target {
                    NamingTarget::New => String::new(),
                    NamingTarget::Rename(id) | NamingTarget::Duplicate(id) => presets
                        .iter()
                        .find(|preset| preset.preset_id == *id)
                        .map(|preset| preset.name.clone())
                        .unwrap_or_default(),
                };
                sheet.naming = Some(sheet_model::Naming { target, text });
                return Task::none();
            }
            SheetMessage::NameText(text) => {
                if let Some(naming) = &mut sheet.naming {
                    naming.text = text;
                }
                return Task::none();
            }
            SheetMessage::NameCancel => {
                sheet.naming = None;
                return Task::none();
            }
            SheetMessage::NameSubmit => return self.submit_preset_name(),
            SheetMessage::Update => {
                let Some(preset) = sheet.preset(&presets).filter(|preset| !preset.builtin) else {
                    return Task::none();
                };
                let params = json!({"preset_id": preset.preset_id, "settings": sheet.settings});
                return self.change_preset("update", "export.presets.update", params);
            }
            SheetMessage::Close => {
                self.export.sheet = None;
                self.event("export_sheet_closed", || json!({}));
                return Task::none();
            }
            SheetMessage::Export => return self.export_from_sheet(),
        }
        self.sheet_changed()
    }

    /// The native folder dialog for the sheet's Choose…, which answers [`SheetMessage::FolderChosen`].
    /// An evidence run names its folder with that message instead.
    fn choose_export_folder(&mut self) -> Task<Message> {
        if self.view_state.picker_open || self.evidence.is_some() {
            return Task::none();
        }
        self.view_state.picker_open = true;
        let start = self
            .export
            .sheet
            .as_ref()
            .and_then(|sheet| sheet.chosen_folder.clone());
        Task::perform(
            async move {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Export into");
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                dialog
                    .pick_folder()
                    .await
                    .map(|folder| folder.path().to_path_buf())
            },
            |folder| Message::Export(ExportMessage::Sheet(SheetMessage::FolderChosen(folder))),
        )
    }

    /// A setting changed: ask for a plan when one it depends on did, unless one is in flight, in
    /// which case the newest settings are planned once it answers.
    fn sheet_changed(&mut self) -> Task<Message> {
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        if sheet.wants_plan().is_none() {
            return Task::none();
        }
        if sheet.plan.in_flight {
            sheet.plan.pending = true;
            return Task::none();
        }
        self.plan_sheet()
    }

    /// Ask for the sheet's plan of its settings now: `export.plan` of the entry, or of each
    /// photograph a batch names whose row the desktop holds.
    fn plan_sheet(&mut self) -> Task<Message> {
        let generation = self.export.sheet_generation;
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        let Some(settings) = sheet.valid().cloned() else {
            return Task::none();
        };
        sheet.plan.in_flight = true;
        sheet.plan.pending = false;
        sheet.plan.sent += 1;
        sheet.plan.asked = Some(settings.clone());
        let (owner, client) = (self.owner.clone(), self.client);
        let target = sheet.target.clone();
        let sent = settings.clone();
        self.event(
            "export_plan_asked",
            || json!({"settings": settings, "sent": self.export.sheet.as_ref().map(|s| s.plan.sent)}),
        );
        owner_task(
            move || sheet_plan_now(&owner, client, &target, &sent),
            move |result| {
                Message::Export(ExportMessage::Planned {
                    generation,
                    settings: Box::new(settings),
                    result,
                })
            },
        )
    }

    fn sheet_planned(
        &mut self,
        generation: u64,
        settings: ExportSettings,
        result: Result<SheetPlan, String>,
    ) -> Task<Message> {
        if generation != self.export.sheet_generation {
            return Task::none();
        }
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        sheet.plan.in_flight = false;
        match result {
            Ok(plan) => {
                sheet.plan.answer = Some(plan);
                sheet.plan.error = None;
            }
            Err(error) => {
                sheet.plan.answer = None;
                sheet.plan.error = Some(error);
            }
        }
        sheet.plan.answered = Some(settings);
        let pending = std::mem::take(&mut sheet.plan.pending);
        if pending && sheet.wants_plan().is_some() {
            return self.plan_sheet();
        }
        self.outcome(Outcome::SheetPlanned);
        Task::none()
    }

    /// Save as preset…, Rename… or Duplicate's name submitted.
    fn submit_preset_name(&mut self) -> Task<Message> {
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        let Some(naming) = sheet.naming.clone() else {
            return Task::none();
        };
        let name = naming.text.trim().to_owned();
        let (what, method, params) = match naming.target {
            NamingTarget::New => (
                "create",
                "export.presets.create",
                json!({"name": name, "settings": sheet.settings}),
            ),
            NamingTarget::Rename(id) => (
                "rename",
                "export.presets.update",
                json!({"preset_id": id, "name": name}),
            ),
            NamingTarget::Duplicate(id) => {
                let Some(preset) = self.export.preset(&id) else {
                    return Task::none();
                };
                (
                    "duplicate",
                    "export.presets.create",
                    json!({"name": name, "settings": preset.settings}),
                )
            }
        };
        self.change_preset(what, method, params)
    }

    /// One preset change, sent synchronously as the gesture's own request with a new request id;
    /// the presets are listed again once it answers, so the sheet shows exactly what the API
    /// lists.
    fn change_preset(&mut self, what: &str, method: &str, mut params: Value) -> Task<Message> {
        params["mutation"] = json!(request());
        let answer = call_detailed(&self.owner, self.client, method, params.clone());
        self.event(
            "export_preset_change",
            || json!({"method": method, "params": params, "ok": answer.is_ok()}),
        );
        let result = answer
            .map(|answer| serde_json::from_value::<ExportPreset>(answer["preset"].clone()).ok())
            .map_err(|error| error.message);
        self.export_preset_changed(what.to_owned(), result)
    }

    fn export_preset_changed(
        &mut self,
        what: String,
        result: Result<Option<ExportPreset>, String>,
    ) -> Task<Message> {
        self.list_export_presets();
        let presets = self.export.presets.clone().unwrap_or_default();
        let Some(sheet) = self.export.sheet.as_mut() else {
            return Task::none();
        };
        sheet.changing = false;
        match result {
            Ok(preset) => {
                sheet.preset_error = None;
                sheet.naming = None;
                match (what.as_str(), preset) {
                    // A new preset holds the current settings, or the duplicated one's: it is the
                    // card selected now.
                    ("create" | "duplicate", Some(preset)) => {
                        self.status.text = format!("Saved export preset {}", preset.name);
                        sheet.choose(&preset);
                    }
                    ("update", Some(preset)) => {
                        self.status.text = format!("Updated export preset {}", preset.name);
                    }
                    ("rename", Some(preset)) => {
                        self.status.text = format!("Renamed export preset to {}", preset.name);
                    }
                    ("delete", _) => {
                        self.status.text = "Deleted the export preset".to_owned();
                        if sheet.preset(&presets).is_none() {
                            sheet.preset_id = None;
                        }
                    }
                    _ => {}
                }
            }
            Err(error) => {
                self.status.text = format!("Export preset not saved: {error}");
                sheet.preset_error = Some(error);
            }
        }
        // A new preset's settings may ask for a plan; the outcome follows, so a step waiting on the
        // change waits on that plan too.
        let task = self.sheet_changed();
        self.outcome(Outcome::ExportPresetChanged);
        task
    }

    /// Answer the sheet's plan in flight now, as its owner task would: for tests, which run no
    /// tasks.
    #[cfg(test)]
    pub(crate) fn answer_sheet_plan(&mut self) {
        while let Some(sheet) = self
            .export
            .sheet
            .as_ref()
            .filter(|sheet| sheet.plan.in_flight)
        {
            let Some(settings) = sheet.valid().cloned() else {
                return;
            };
            let result = sheet_plan_now(&self.owner, self.client, &sheet.target, &settings);
            let _ = self.update(Message::Export(ExportMessage::Planned {
                generation: self.export.sheet_generation,
                settings: Box::new(settings),
                result,
            }));
        }
    }

    /// The open sheet has a plan in flight.
    pub(crate) fn sheet_planning(&self) -> bool {
        self.export
            .sheet
            .as_ref()
            .is_some_and(|sheet| sheet.plan.in_flight)
    }

    /// The sheet's Export: write with its settings, directly into its folder, and close it. A batch
    /// sheet starts `batch.export` of the selection with them.
    fn export_from_sheet(&mut self) -> Task<Message> {
        let presets = self.export.presets.clone().unwrap_or_default();
        let Some(sheet) = self.export.sheet.as_ref() else {
            return Task::none();
        };
        let Some(settings) = sheet.valid().cloned() else {
            self.status.text = "Correct the highlighted setting first".into();
            return Task::none();
        };
        let remember = ExportLast {
            settings: settings.clone(),
            preset_id: sheet.remembered_preset(&presets),
        };
        if sheet.target.batch() {
            self.export.sheet = None;
            return self.start_batch_export(settings, remember);
        }
        match self.sheet_request() {
            Ok(request) => {
                self.export.sheet = None;
                self.start_export(request)
            }
            Err(refusal) => {
                self.status.text = refusal;
                Task::none()
            }
        }
    }

    /// What the open single-photograph sheet's Export starts: its entry with its settings, as the
    /// desktop sends them, remembered as the last export once written.
    pub(crate) fn sheet_request(&self) -> Result<ExportRequest, String> {
        let presets = self.export.presets.as_deref().unwrap_or_default();
        let sheet = self
            .export
            .sheet
            .as_ref()
            .ok_or("The Export sheet is not open")?;
        let settings = sheet
            .valid()
            .cloned()
            .ok_or("Correct the highlighted setting first")?;
        let sheet_model::SheetTarget::Single {
            asset_id, entry_id, ..
        } = &sheet.target
        else {
            return Err("A batch sheet exports through batch.export".into());
        };
        if let Some(refusal) = self.export_refusal() {
            return Err(refusal);
        }
        Ok(ExportRequest {
            asset_id: asset_id.clone(),
            entry_id: entry_id.clone(),
            settings: self.desktop_settings(settings.clone()),
            destination: None,
            reference: sheet.reference,
            remember: Some(ExportLast {
                settings,
                preset_id: sheet.remembered_preset(presets),
            }),
        })
    }

    /// The settings the desktop sends for these ([`settings_from_window`]).
    pub(crate) fn desktop_settings(&self, settings: ExportSettings) -> ExportSettings {
        settings_from_window(settings, self.view_state.system_scale_factor)
    }

    /// Export again (`Shift+Cmd+E`): the last export's settings, folder and suffix, at once,
    /// counting the name up when it is taken.
    fn export_again(&mut self) -> Task<Message> {
        self.close_export_menu();
        let request = self.again_request();
        self.start_or_say(request)
    }

    /// What Export again starts: the displayed entry with the last export's settings, remembered
    /// again as they were.
    pub(crate) fn again_request(&self) -> Result<ExportRequest, String> {
        let last = self.export_last().ok_or("Nothing has been exported yet")?;
        self.displayed_request(last.settings.clone(), Some(last), None)
    }

    /// Export with preset: that preset's own settings and folder, at once.
    fn export_with_preset(&mut self, preset_id: &str) -> Task<Message> {
        self.close_export_menu();
        if self.export.preset(preset_id).is_none() {
            self.list_export_presets();
        }
        let request = self.preset_request(preset_id);
        self.start_or_say(request)
    }

    /// What Export with preset starts: the displayed entry with the preset's own settings,
    /// remembered as that preset.
    pub(crate) fn preset_request(&self, preset_id: &str) -> Result<ExportRequest, String> {
        let preset = self
            .export
            .preset(preset_id)
            .ok_or("That export preset no longer exists")?;
        let remember = ExportLast {
            settings: preset.settings.clone(),
            preset_id: Some(preset.preset_id.clone()),
        };
        self.displayed_request(preset.settings.clone(), Some(remember), None)
    }

    /// The displayed entry exported with `settings`, as the desktop sends them, into
    /// `destination` or the name the plan suggests.
    fn displayed_request(
        &self,
        settings: ExportSettings,
        remember: Option<ExportLast>,
        destination: Option<PathBuf>,
    ) -> Result<ExportRequest, String> {
        if let Some(refusal) = self.export_refusal() {
            return Err(refusal);
        }
        let entry_id = self.export_entry().ok_or("No history entry is displayed")?;
        let asset_id = self
            .document
            .state
            .as_ref()
            .ok_or("Open a photograph to export it")?
            .asset
            .id
            .clone();
        Ok(ExportRequest {
            asset_id,
            entry_id,
            settings: self.desktop_settings(settings),
            destination,
            reference: false,
            remember,
        })
    }

    fn start_or_say(&mut self, request: Result<ExportRequest, String>) -> Task<Message> {
        match request {
            Ok(request) => self.start_export(request),
            Err(refusal) => {
                self.status.text = refusal;
                Task::none()
            }
        }
    }

    /// An evidence step's export into the run's evidence directory: today's full-size settings,
    /// with every supported field or none, written to the file the step names.
    pub(crate) fn export_file(
        &mut self,
        keep_metadata: bool,
        reference: bool,
        destination: PathBuf,
    ) -> Task<Message> {
        let settings = ExportSettings {
            folder: super::remembered::export_folder(&destination)
                .map_or(ExportFolder::Original, ExportFolder::Path),
            ..ExportSettings::baseline(if keep_metadata {
                MetadataLevel::All
            } else {
                MetadataLevel::None
            })
        };
        let request = self
            .displayed_request(settings, None, Some(destination))
            .map(|request| ExportRequest {
                reference,
                ..request
            });
        self.start_or_say(request)
    }

    /// Start one export: the plan, then `export.write`, in one chain off the update loop.
    pub(crate) fn start_export(&mut self, request: ExportRequest) -> Task<Message> {
        self.close_export_menu();
        if let Some(refusal) = self.export_refusal() {
            self.status.text = refusal;
            return Task::none();
        }
        self.export.run = Some(ExportRun::new(request.reference, request.remember.clone()));
        self.status.text = "Exporting\u{2026}".into();
        self.event(
            "export_started",
            || json!({"asset_id": request.asset_id, "entry_id": request.entry_id,
                "settings": request.settings, "reference": request.reference,
                "destination": request.destination.is_some(), "remember": request.remember.is_some()}),
        );
        write_task(self.owner.clone(), self.client, request)
    }

    fn export_sent(
        &mut self,
        choice: Option<Box<ExportChoice>>,
        result: Result<Value, CallError>,
    ) -> Task<Message> {
        if let Some(choice) = &choice {
            let file = file_name(&choice.destination);
            let warned = carries(&choice.plan, "large-image");
            if let Some(run) = &mut self.export.run {
                run.file_name = Some(file.clone());
                run.choice = Some(choice.clone());
                run.warned = warned;
            }
            self.status.text = format!("Exporting {file}\u{2026}");
            self.outcome(Outcome::ExportPlanned(&choice.plan));
        }
        let file = self
            .export
            .run
            .as_ref()
            .and_then(|run| run.file_name.clone())
            .unwrap_or_default();
        match result {
            Ok(answer) => {
                let Some(job_id) = answer["job_id"].as_str().map(str::to_owned) else {
                    let error = "export.write answered no job".to_owned();
                    self.export_finished(format!("Export failed: {error}"), Some(&error), None);
                    return Task::none();
                };
                if let Some(run) = &mut self.export.run {
                    run.job_id = Some(job_id);
                    run.queued = Some(Instant::now());
                    // A warned export shows its progress from the start.
                    run.bar = run.warned;
                    self.outcome(Outcome::ExportQueued(&answer));
                }
                // The reader starts with the next subscription rebuild and reads at once, so a
                // small photograph that is already written is found without waiting a tick.
                Task::none()
            }
            Err(error) => {
                let reason = error.to_string();
                let held = (error.code == ErrorKind::Conflict.code())
                    .then(|| held_stale((*choice.clone()?).clone(), error.data.as_ref()))
                    .flatten();
                if let Some(held) = held {
                    let status = stale_text(&file, held.stale.len());
                    self.event(
                        "export_stale_held",
                        || json!({"entry_id": held.entry_id, "stale": held.stale}),
                    );
                    self.export.held = Some(held);
                    self.export_finished(status, Some(&reason), None);
                    return Task::none();
                }
                let status = if error.code == ErrorKind::Conflict.code() {
                    refused_text(&file)
                } else {
                    format!("Not exported: {}", error.message)
                };
                self.export_finished(status, Some(&reason), None);
                Task::none()
            }
        }
    }

    fn export_read(&mut self, job_id: String, result: Result<Value, String>) -> Task<Message> {
        if self.export.live() != Some(job_id.as_str()) {
            return Task::none();
        }
        let file = self
            .export
            .run
            .as_ref()
            .and_then(|run| run.file_name.clone())
            .unwrap_or_default();
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                self.export_finished(format!("Export failed: {error}"), Some(&error), None);
                return Task::none();
            }
        };
        match record["status"].as_str() {
            // Still going: its progress, and whether it has run long enough to show it, judged on
            // this read.
            Some("queued" | "running") => {
                if let Some(run) = &mut self.export.run {
                    let progress = &record["progress"];
                    if let Some(fraction) = progress["fraction"].as_f64() {
                        run.progress =
                            Some((fraction, progress["message"].as_str().map(str::to_owned)));
                    }
                    run.bar |= run
                        .queued
                        .is_some_and(|queued| queued.elapsed() >= BAR_DELAY);
                }
                self.outcome(Outcome::ExportProgress);
            }
            Some("ready") => {
                let result = &record["result"];
                let written = result["path"]
                    .as_str()
                    .map(|path| file_name(Path::new(path)))
                    .unwrap_or(file);
                let limit = self
                    .export
                    .run
                    .as_ref()
                    .and_then(|run| run.choice.as_ref())
                    .and_then(|choice| choice.settings.limit_bytes());
                let status = exported_text(
                    &written,
                    result["width"].as_u64().unwrap_or(0),
                    result["height"].as_u64().unwrap_or(0),
                    result["bytes"].as_u64().unwrap_or(0),
                    result["quality"].as_u64(),
                    limit,
                );
                let last = self
                    .export
                    .run
                    .as_ref()
                    .and_then(|run| run.remember.clone());
                self.export_finished(status, None, Some(record));
                return self.remember_export_last(last);
            }
            Some("cancelled") => {
                self.export_finished("Export cancelled".into(), None, Some(record));
            }
            _ => {
                let reason = record["error"]["message"]
                    .as_str()
                    .or_else(|| record["error"].as_str())
                    .unwrap_or("the export job failed")
                    .to_owned();
                let code = record["error"]["code"].as_str().unwrap_or("export");
                self.export_finished(
                    format!("Export failed: {reason}"),
                    Some(&format!("{code}: {reason}")),
                    Some(record),
                );
            }
        }
        Task::none()
    }

    /// Export anyway: send the held export again, as a request of its own, acknowledging exactly
    /// the entry and the stale patches its refusal named. Nothing is acknowledged without this
    /// press, and a press acknowledges only what the notice showed.
    fn export_anyway(&mut self) -> Task<Message> {
        let Some(held) = self.export.held.take() else {
            return Task::none();
        };
        if let Some(refusal) = self.export_refusal() {
            self.status.text = refusal;
            self.export.held = Some(held);
            return Task::none();
        }
        let mut choice = *held.choice.clone();
        choice.acknowledge_stale = Some(held.acknowledgement());
        let file = file_name(&choice.destination);
        self.status.text = format!("Exporting {file}\u{2026}");
        let mut run = ExportRun::new(choice.reference, choice.remember.clone());
        run.file_name = Some(file);
        run.warned = carries(&choice.plan, "large-image");
        run.choice = Some(Box::new(choice.clone()));
        self.export.run = Some(run);
        self.event(
            "export_anyway",
            || json!({"acknowledge_stale": choice.acknowledge_stale}),
        );
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let result = send_now(&owner, client, &choice);
                (choice, result)
            },
            |(choice, result)| {
                Message::Export(ExportMessage::Sent {
                    choice: Some(Box::new(choice)),
                    result,
                })
            },
        )
    }

    /// The export ended, one way or another: say so, forget it, and report it with the job's last
    /// record and what the GPU tile worker has done and holds. `failure` marks an export that was
    /// refused or failed.
    fn export_finished(&mut self, status: String, failure: Option<&str>, record: Option<Value>) {
        self.status.text = status;
        let bar = self.export.run.take().is_some_and(|run| run.bar);
        self.event("export_finished", || {
            let tiles = self.renderer.tiles.as_ref();
            json!({"status":self.status.text,"record":record,"bar":bar,
                "tiles":tiles.map(|tiles| tiles.figures().record())})
        });
        self.outcome(Outcome::ExportEnded {
            record: record.as_ref(),
            failure,
            bar,
        });
    }

    /// The running export's reader, which exists only while its job is queued or running. It is
    /// identified by the job alone, so rebuilding the subscription after every message keeps the
    /// one reader and its memory of what it has sent.
    pub(crate) fn export_reader_subscription(&self) -> Option<Subscription<Message>> {
        self.export.live().map(|job_id| {
            Subscription::run_with(
                Reader {
                    identity: job_id.to_owned(),
                    owner: self.owner.clone(),
                    client: self.client,
                    presentation_visible: self.visibility.sampling_allowed(),
                },
                export_reads,
            )
        })
    }

    /// The status bar's export progress, while it shows: `Optimising DSC_0042-print.jpg` and the
    /// fraction the job's last read reported.
    pub(crate) fn export_progress(&self) -> Option<crate::state::long_work::BusiestJob> {
        let run = self.export.run.as_ref().filter(|run| run.bar)?;
        let (fraction, phase) = run.progress.clone().unwrap_or((0.0, None));
        Some(crate::state::long_work::BusiestJob {
            job_id: run.job_id.clone().unwrap_or_default(),
            label: format!(
                "{} {}",
                phase_label(phase.as_deref()),
                run.file_name.clone().unwrap_or_default()
            ),
            fraction: Some(fraction.clamp(0.0, 1.0) as f32),
            jobs: 1,
        })
    }

    /// The export as a captured frame records it: the menu, whether one can start, the sheet, the
    /// one running and the owner requests this desktop has made, by method. No path is recorded.
    pub(super) fn export_summary(&self) -> Value {
        let presets = self.export.presets.as_deref().unwrap_or_default();
        let model = &self.workspace.export_sheet;
        json!({
            "menu_open": self.workspace.title.export_menu_open,
            "menu": self.workspace.title.export_menu.as_ref().map(|menu| json!({
                "again": menu.again,
                "presets": menu.presets.iter().map(|(_, name, detail)| json!([name, detail])).collect::<Vec<_>>(),
            })),
            "can_export": self.workspace.title.can_export,
            "presets": presets.iter().map(|preset| json!({
                "preset_id": preset.preset_id, "name": preset.name, "builtin": preset.builtin,
            })).collect::<Vec<_>>(),
            "sheet": self.export.sheet.as_ref().map(|sheet| json!({
                "batch": sheet.target.batch(),
                "reference": sheet.reference,
                "preset_id": sheet.preset_id,
                "changed": sheet.changed(presets),
                "settings": sheet.settings,
                "valid": sheet.valid().is_some(),
                "plans_sent": sheet.plan.sent,
                "plan_in_flight": sheet.plan.in_flight,
                "plan_pending": sheet.plan.pending,
                "plan": match &sheet.plan.answer {
                    Some(SheetPlan::Single(plan)) => plan_record(plan),
                    Some(SheetPlan::Batch(plan)) => json!({
                        "planned": plan.planned, "large": plan.large, "largest": plan.largest,
                        "refused": plan.refused.iter().map(|(code, count, _)| json!([code, count])).collect::<Vec<_>>(),
                        "failed": plan.failed, "first_name": plan.first_name,
                    }),
                    None => Value::Null,
                },
                "model": model.as_ref().map(|model| json!({
                    "title": model.title, "subtitle": model.subtitle,
                    "cards": model.cards.iter().map(|card| json!({
                        "name": card.name, "detail": card.detail, "selected": card.selected,
                        "changed": card.changed, "builtin": card.builtin,
                    })).collect::<Vec<_>>(),
                    "readout": model.readout, "density": model.density,
                    "large_image": model.large_image, "refusals": model.refusals,
                    "name": model.name, "taken": model.taken, "summary": model.summary,
                    "update": model.update, "can_export": model.can_export,
                    "export_label": model.export_label, "invalid": model.invalid.as_ref().map(|(_, reason)| reason),
                    "sharpen": {
                        "medium": model.sharpen.medium, "amount": model.sharpen.amount,
                        "amount_enabled": model.sharpen.amount_enabled, "changed": model.sharpen.changed,
                    },
                })),
            })),
            "running": self.export.run.as_ref().map(|run| json!({
                "reference": run.reference,
                "file_name": run.file_name,
                "job_id": run.job_id,
                "warned": run.warned,
                "bar": run.bar,
                "progress": run.progress.as_ref().map(|(fraction, phase)| json!({"fraction": fraction, "phase": phase})),
                "remember": run.remember.is_some(),
                "settings": run.choice.as_ref().map(|choice| &choice.settings),
                "acknowledge_stale": run.choice.as_ref().and_then(|choice| choice.acknowledge_stale.clone()),
            })),
            "status_bar": self.workspace.long_work.export.as_ref().map(|job| json!({
                "label": job.label, "fraction": job.fraction,
            })),
            "held_stale": self.export.held.as_ref().map(|held| json!({
                "file_name": file_name(&held.choice.destination),
                "entry_id": held.entry_id,
                "stale": held.stale,
            })),
            "owner_calls": super::tasks::call_latency::counts(),
        })
    }
}

/// The phase a job's progress names, as the status bar says it.
pub(crate) fn phase_label(phase: Option<&str>) -> String {
    match phase {
        Some(phase) if !phase.is_empty() => {
            let mut characters = phase.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().chain(characters).collect())
                .unwrap_or_default()
        }
        _ => "Exporting".to_owned(),
    }
}

/// Whether a plan carries the notice `code`.
fn carries(plan: &Value, code: &str) -> bool {
    plan["notices"]
        .as_array()
        .is_some_and(|notices| notices.iter().any(|notice| notice["code"] == code))
}

/// The plan as an evidence step records it: everything but the suggested path, whose directory is
/// the original's, reduced to its file name.
pub(crate) fn plan_record(plan: &Value) -> Value {
    let mut record = plan.clone();
    if let Some(object) = record.as_object_mut()
        && let Some(suggested) = object.get("suggested").and_then(Value::as_str)
    {
        let name = file_name(Path::new(suggested));
        object.insert("suggested".into(), json!(name));
    }
    record
}

/// A path's file name, as the status bar names it.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The settings an export from a window at `system_scale_factor` sends: with no density set, on
/// macOS the window's own ([`screen_pixels_per_inch`]), unless they sharpen for paper, whose
/// radius follows the density: those send none, and sharpen for 300 ppi.
pub(crate) fn settings_from_window(
    mut settings: ExportSettings,
    system_scale_factor: f32,
) -> ExportSettings {
    let for_paper = settings
        .sharpen
        .is_some_and(|sharpen| sharpen.medium.print());
    if settings.pixels_per_inch.is_none() && !for_paper {
        settings.pixels_per_inch = screen_pixels_per_inch(system_scale_factor);
    }
    settings
}

/// The density an export from this window names when its settings set none. On macOS, 72 pixels
/// per inch for each physical pixel of a point on the window's display, 144 on a Retina display,
/// which is what makes Preview's Actual Size show one image pixel per physical pixel, as 100% does
/// here; Preview sizes a file that names no density at 72. Elsewhere none: the JFIF header keeps its
/// unitless aspect ratio.
pub(crate) fn screen_pixels_per_inch(system_scale_factor: f32) -> Option<u16> {
    (cfg!(target_os = "macos") && system_scale_factor.is_finite() && system_scale_factor > 0.0)
        .then(|| (72.0 * system_scale_factor).round().clamp(1.0, 65535.0) as u16)
}

/// `Exported DSC_0042-web.jpg · 2048 × 1638 · 612 KB · quality 80`, `… · quality 71 to fit 500
/// KB` under a size limit, and no quality for a TIFF.
pub(crate) fn exported_text(
    file: &str,
    width: u64,
    height: u64,
    bytes: u64,
    quality: Option<u64>,
    limit: Option<u64>,
) -> String {
    let mut text = format!(
        "Exported {file} \u{b7} {width} \u{d7} {height} \u{b7} {}",
        size_text(bytes)
    );
    match (quality, limit) {
        (Some(quality), Some(limit)) => text.push_str(&format!(
            " \u{b7} quality {quality} to fit {}",
            size_text(limit)
        )),
        (Some(quality), None) => text.push_str(&format!(" \u{b7} quality {quality}")),
        (None, _) => {}
    }
    text
}

/// A file's size in decimal units: megabytes to a tenth from 1 MB, whole kilobytes below.
fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", (bytes as f64 / 1000.0).round().max(1.0) as u64)
    }
}

/// What the status bar says while an export waits for its stale patches to be acknowledged.
pub(crate) fn stale_text(file: &str, count: usize) -> String {
    let patches = if count == 1 {
        "1 frozen patch is".to_owned()
    } else {
        format!("{count} frozen patches are")
    };
    format!("Not exported yet: {patches} stale in {file}; Export anyway or Cancel")
}

/// What the status bar says when the destination already exists.
pub(crate) fn refused_text(file: &str) -> String {
    format!("Not exported: {file} already exists; Luxforge never replaces a file")
}

/// `export.presets.list`, as the sheet, the menu and the palette list them.
pub(crate) fn presets_now(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<Vec<ExportPreset>, String> {
    let (answer, _) = call(owner, client, "export.presets.list", json!({}))?;
    serde_json::from_value(answer["presets"].clone()).map_err(|error| error.to_string())
}

/// The plan, then `export.write` of what it names, in one chain off the update loop: the requests
/// an API client sends for the same settings.
fn write_task(owner: OwnerHandle, client: ClientId, request: ExportRequest) -> Task<Message> {
    owner_task(
        move || write_now(&owner, client, request),
        |(choice, result)| {
            Message::Export(ExportMessage::Sent {
                choice: choice.map(Box::new),
                result,
            })
        },
    )
}

/// [`write_task`]'s work: the choice it sent, when the plan named a destination, and the answer.
pub(crate) fn write_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: ExportRequest,
) -> (Option<ExportChoice>, Result<Value, CallError>) {
    let plan = match plan_now(
        owner,
        client,
        &request.asset_id,
        Some(&request.entry_id),
        &request.settings,
    ) {
        Ok(plan) => plan,
        Err(message) => {
            return (
                None,
                Err(CallError {
                    code: "plan".into(),
                    message,
                    data: None,
                    job_id: None,
                }),
            );
        }
    };
    let destination = match request
        .destination
        .clone()
        .or_else(|| plan["suggested"].as_str().map(PathBuf::from))
    {
        Some(destination) => destination,
        None => {
            // The refusing notice says why no name can be suggested.
            let message = plan["notices"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|notice| notice["refuses"] == true)
                .and_then(|notice| notice["message"].as_str())
                .unwrap_or("no free name is left in the folder")
                .to_owned();
            return (
                None,
                Err(CallError {
                    code: "validation".into(),
                    message,
                    data: None,
                    job_id: None,
                }),
            );
        }
    };
    let choice = ExportChoice {
        asset_id: request.asset_id,
        entry_id: request.entry_id,
        destination,
        settings: request.settings,
        reference: request.reference,
        plan,
        acknowledge_stale: None,
        remember: request.remember,
    };
    let result = send_now(owner, client, &choice);
    (Some(choice), result)
}

/// The sheet's plan of `settings`: `export.plan` of its entry, or of each photograph of a batch
/// whose row the desktop holds, counted together.
pub(crate) fn sheet_plan_now(
    owner: &OwnerHandle,
    client: ClientId,
    target: &sheet_model::SheetTarget,
    settings: &ExportSettings,
) -> Result<SheetPlan, String> {
    match target {
        sheet_model::SheetTarget::Single {
            asset_id, entry_id, ..
        } => plan_now(owner, client, asset_id, Some(entry_id), settings).map(SheetPlan::Single),
        sheet_model::SheetTarget::Batch { assets, .. } => {
            let mut batch = BatchPlan::default();
            for asset in assets {
                let plan = plan_now(owner, client, asset, None, settings);
                batch.count(plan.as_ref().map_err(String::as_str));
            }
            Ok(SheetPlan::Batch(batch))
        }
    }
}

/// `export.plan` for one entry with `settings` — the current entry when `entry_id` is `None` —
/// blocking this thread until the owner answers.
pub(crate) fn plan_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: &AssetId,
    entry_id: Option<&EntryId>,
    settings: &ExportSettings,
) -> Result<Value, String> {
    let mut params = json!({"asset_id": asset_id, "settings": settings});
    if let Some(entry_id) = entry_id {
        params["entry_id"] = json!(entry_id);
    }
    call(owner, client, "export.plan", params).map(|(plan, _)| plan)
}

/// `export.write`'s parameters for one choice, under a new request id: `reference: true` only for a
/// choice that asks for the reference renderer's export, as an API client asks for it, and no
/// `reference` for any other.
pub(crate) fn export_params(choice: &ExportChoice) -> Value {
    let mut params = json!({
        "asset_id": choice.asset_id,
        "entry_id": choice.entry_id,
        "settings": choice.settings,
        "destination": choice.destination,
        "mutation": request(),
    });
    if choice.reference {
        params["reference"] = json!(true);
    }
    if let Some(acknowledgement) = &choice.acknowledge_stale {
        params["acknowledge_stale"] = acknowledgement.clone();
    }
    params
}

/// `export.write` for one choice ([`export_params`]), waiting for the source and asking again, under
/// one request id, when the owner answers `preparation-required`.
pub(crate) fn send_now(
    owner: &OwnerHandle,
    client: ClientId,
    choice: &ExportChoice,
) -> Result<Value, CallError> {
    let params = export_params(choice);
    let mut attempt = 0;
    loop {
        match call_detailed(owner, client, "export.write", params.clone()) {
            Err(error)
                if error.code == ErrorKind::PreparationRequired.code()
                    && attempt < PREPARATION_RETRIES =>
            {
                attempt += 1;
                let job = error
                    .job_id
                    .clone()
                    .unwrap_or_else(|| error.message.clone());
                wait_source_job(owner, client, &job)?;
            }
            other => return other,
        }
    }
}

/// `job.read` for one job.
#[cfg(test)]
pub(crate) fn read_now(
    owner: &OwnerHandle,
    client: ClientId,
    job_id: &str,
) -> Result<Value, String> {
    call(owner, client, JOB_READ, json!({"job_id": job_id})).map(|(read, _)| read)
}

/// Follow one authoritative job change at a time, without polling.
pub(crate) fn export_reads(reader: &Reader<String>) -> BoxStream<'static, Message> {
    let (owner, client, job_id) = (reader.owner.clone(), reader.client, reader.identity.clone());
    let visible = reader.presentation_visible;
    let state = std::sync::Arc::new(std::sync::Mutex::new((None, Watch::default())));
    job_reads::reads(move || {
        let (owner, job_id) = (owner.clone(), job_id.clone());
        // Keep token/watch between pulls; the future owns only this pass's copy.
        let (previous, mut observing) = std::mem::take(&mut *state.lock().expect("export reader"));
        let future = async move {
            let result = job_reads::wait(&owner, client, &job_id, previous).await;
            let (next, result) = match result {
                Ok((change, record)) => (Some(change), Ok(record)),
                Err(error) => (previous, Err(error)),
            };
            let verdict = observing.observe_filtered(
                &result,
                |record| !matches!(record["status"].as_str(), Some("queued" | "running")),
                |one, two| {
                    if visible {
                        one == two
                    } else {
                        job_reads::same_without_progress(one, two)
                    }
                },
            );
            (
                next,
                observing,
                Pass::of(verdict, || {
                    Message::Export(ExportMessage::Read { job_id, result })
                }),
            )
        };
        // The pass is awaited before another is made; shared state commits its token on completion.
        let state = state.clone();
        async move {
            let (next, watch, pass) = future.await;
            *state.lock().expect("export reader") = (next, watch);
            pass
        }
    })
}

/// One pass of an export's reader: read the job, and send the read only when it is news. A queued
/// or running record is news when it is the first or differs from the one sent before it; any
/// other status is the job's end, and a failed read ends the export too. Each of those is sent
/// once, and the reader reads no more.
#[cfg(test)]
pub(crate) fn export_pass(
    job_id: String,
    mut read: impl FnMut() -> Result<Value, String>,
) -> impl FnMut() -> Pass<Message> {
    let mut watch = Watch::default();
    move || {
        let result = read();
        let verdict = watch.observe(&result, |record| {
            !matches!(record["status"].as_str(), Some("queued" | "running"))
        });
        Pass::of(verdict, || {
            Message::Export(ExportMessage::Read {
                job_id: job_id.clone(),
                result,
            })
        })
    }
}

/// The running export is read by a reader that exists only while its job is queued or running
/// ([`Editor::export_reader_subscription`]).
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    editor
        .export_reader_subscription()
        .unwrap_or_else(Subscription::none)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_names_the_file_its_size_its_bytes_and_the_quality_used() {
        assert_eq!(
            exported_text("DSC_0042-web.jpg", 2048, 1638, 612_345, Some(80), None),
            "Exported DSC_0042-web.jpg \u{b7} 2048 \u{d7} 1638 \u{b7} 612 KB \u{b7} quality 80"
        );
        assert_eq!(
            exported_text("a.jpg", 2048, 1638, 498_000, Some(71), Some(500_000)),
            "Exported a.jpg \u{b7} 2048 \u{d7} 1638 \u{b7} 498 KB \u{b7} quality 71 to fit 500 KB"
        );
        assert_eq!(
            exported_text("a.tif", 6000, 4000, 144_000_000, None, None),
            "Exported a.tif \u{b7} 6000 \u{d7} 4000 \u{b7} 144.0 MB"
        );
        assert_eq!(size_text(31_600), "32 KB");
        assert_eq!(size_text(120), "1 KB");
        assert_eq!(size_text(1_000_000), "1.0 MB");
        assert_eq!(
            refused_text("a.jpg"),
            "Not exported: a.jpg already exists; Luxforge never replaces a file"
        );
        assert_eq!(phase_label(Some("optimising")), "Optimising");
        assert_eq!(phase_label(Some("limiting 2/8")), "Limiting 2/8");
        assert_eq!(phase_label(None), "Exporting");
    }

    #[test]
    fn an_export_asks_for_72_pixels_per_inch_per_physical_pixel_on_macos() {
        let expected = |density: u16| cfg!(target_os = "macos").then_some(density);
        assert_eq!(screen_pixels_per_inch(2.0), expected(144));
        assert_eq!(screen_pixels_per_inch(1.0), expected(72));
        assert_eq!(screen_pixels_per_inch(1.5), expected(108));
        assert_eq!(screen_pixels_per_inch(f32::NAN), None);
        assert_eq!(screen_pixels_per_inch(0.0), None);
    }

    #[test]
    fn paper_sharpening_without_a_density_sends_none_and_the_screen_the_windows() {
        let with = |medium: Option<luxforge_core::SharpenMedium>| ExportSettings {
            sharpen: medium.map(|medium| luxforge_core::Sharpen {
                medium,
                amount: luxforge_core::SharpenAmount::Standard,
            }),
            pixels_per_inch: None,
            ..ExportSettings::baseline(luxforge_core::MetadataLevel::None)
        };
        let screen = cfg!(target_os = "macos").then_some(144);
        for (medium, sent) in [
            (None, screen),
            (Some(luxforge_core::SharpenMedium::Screen), screen),
            (Some(luxforge_core::SharpenMedium::Matte), None),
            (Some(luxforge_core::SharpenMedium::Glossy), None),
        ] {
            assert_eq!(
                settings_from_window(with(medium), 2.0).pixels_per_inch,
                sent,
                "{medium:?}"
            );
        }
        // A density set is kept, whatever the medium.
        let mut set = with(Some(luxforge_core::SharpenMedium::Glossy));
        set.pixels_per_inch = Some(240);
        assert_eq!(settings_from_window(set, 2.0).pixels_per_inch, Some(240));
    }

    #[test]
    fn a_plan_record_names_only_the_suggested_file() {
        let suggested = json!({"suggested": "/photos/DSC_0042-web-2.jpg"});
        assert_eq!(
            plan_record(&suggested),
            json!({"suggested": "DSC_0042-web-2.jpg"})
        );
        assert!(carries(
            &json!({"notices": [{"code": "large-image"}]}),
            "large-image"
        ));
        assert!(!carries(&json!({"notices": []}), "large-image"));
    }
}
