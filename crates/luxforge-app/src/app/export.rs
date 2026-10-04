//! Exporting the displayed entry as a JPEG ([export design](../../../../docs/design/export.md#desktop)).
//!
//! The desktop holds no export logic. One chain of owner requests runs off the update loop:
//! `export.plan` for the displayed entry's suggested name (the fixed After entry during slider
//! comparison), the native save dialog in the original's folder, and `export.jpeg` for that entry
//! with the chosen destination, asked again once the source is prepared when the owner answers
//! `preparation-required`. The job then runs on the core's export
//! lane, and the desktop reads it with `job.read` until it ends, through a reader that exists only
//! while this window's export is queued or running (performance rule 8) and sends the desktop a
//! message only when the job's record changes or the job ends ([`job_reads`]). The status bar says
//! what happened; the Performance section lists the running job from `activity.list` like any
//! other.
//!
//! An evidence run bypasses only the dialog: its `export` step names the file, written into the
//! run's evidence directory, and the rest of the chain is the same.
use crate::app::{
    Editor,
    gesture::Starting,
    job_reads::{self, Pass, Reader, Watch},
    message::{Message, export::ExportMessage},
    outcome::Outcome,
    tasks::{CallError, call, call_detailed, owner_task, owner_work, request, wait_source_job},
};
use crate::state::MenuTarget;
use iced::{Subscription, Task, futures::stream::BoxStream};
use luxforge_core::{AssetId, ClientId, EntryId, ErrorKind, OwnerHandle, jobs::JOB_READ};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// How often the running export is read. The core pushes no client anything, so a client that
/// wants to know when its export ended reads it. The reader exists only while this window's export
/// is queued or running; an idle desktop has none. An export of a photograph takes a few hundred
/// milliseconds to seconds, so a read every 100 ms ends the status line within a tenth of a second
/// of the file appearing, at one owner lookup per read, and matches the capability job poll. An
/// export reports no progress, so nearly every read answers what the one before it did, and none
/// of those reaches the update loop.
pub(crate) const EXPORT_POLL: Duration = Duration::from_millis(100);

/// How many times `export.jpeg` is asked again after its source was prepared.
const PREPARATION_RETRIES: usize = 4;

/// What the plan answered and where the export goes: everything `export.jpeg` needs.
#[derive(Clone, Debug)]
pub(crate) struct ExportChoice {
    pub(crate) asset_id: AssetId,
    pub(crate) entry_id: EntryId,
    pub(crate) destination: PathBuf,
    pub(crate) keep_metadata: bool,
    /// `export.plan`'s answer, reported when the choice is taken up.
    pub(crate) plan: Value,
}

/// The one export this window runs, from its plan to its last read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Exporting {
    pub(crate) run: Option<ExportRun>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExportRun {
    pub(crate) keep_metadata: bool,
    /// The destination's file name, once the dialog has chosen it.
    pub(crate) file_name: Option<String>,
    /// The core's job, once `export.jpeg` has queued it.
    pub(crate) job_id: Option<String>,
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
}

impl Editor {
    pub(super) fn export_update(&mut self, message: ExportMessage) -> Task<Message> {
        match message {
            ExportMessage::Start { keep_metadata } => {
                // The dialog is the only way a person names a destination, and an evidence run
                // never opens one: its `export` step names the file instead.
                if self.evidence.is_some() {
                    self.close_export_menu();
                    return Task::none();
                }
                self.export_start(keep_metadata, None)
            }
            ExportMessage::Chosen(result) => self.export_chosen(result),
            ExportMessage::Queued(result) => self.export_queued(result),
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

    /// Start exporting the displayed saved entry, or the comparison's fixed After entry.
    /// `destination` bypasses the save dialog, for an evidence step; without it the dialog chooses.
    pub(crate) fn export_start(
        &mut self,
        keep_metadata: bool,
        destination: Option<PathBuf>,
    ) -> Task<Message> {
        self.close_export_menu();
        if let Some(refusal) = self.export_refusal() {
            self.status.text = refusal;
            return Task::none();
        }
        let Some(entry) = self.export_entry() else {
            self.status.text = "No history entry is displayed".into();
            return Task::none();
        };
        let state = self.document.state.as_ref().expect("checked above");
        let asset = state.asset.id.clone();
        let original = state.asset.locator.clone();
        if destination.is_none() {
            self.view_state.picker_open = true;
        }
        self.export.run = Some(ExportRun {
            keep_metadata,
            file_name: None,
            job_id: None,
        });
        self.event(
            "export_started",
            || json!({"asset_id":asset,"entry_id":entry,"keep_metadata":keep_metadata,"dialog":destination.is_none()}),
        );
        plan_task(
            self.owner.clone(),
            self.client,
            (asset, entry),
            keep_metadata,
            original,
            destination,
        )
    }

    fn export_chosen(
        &mut self,
        result: Result<Option<Box<ExportChoice>>, String>,
    ) -> Task<Message> {
        self.view_state.picker_open = false;
        match result {
            Ok(Some(choice)) => {
                let file_name = file_name(&choice.destination);
                self.status.text = format!("Exporting {file_name}\u{2026}");
                if let Some(run) = &mut self.export.run {
                    run.file_name = Some(file_name);
                    self.outcome(Outcome::ExportPlanned(&choice.plan));
                }
                send_task(self.owner.clone(), self.client, *choice)
            }
            Ok(None) => {
                self.export_finished("Export cancelled".into(), None, None);
                Task::none()
            }
            Err(error) => {
                self.export_finished(format!("Export failed: {error}"), Some(&error), None);
                Task::none()
            }
        }
    }

    fn export_queued(&mut self, result: Result<Value, CallError>) -> Task<Message> {
        let file = self
            .export
            .run
            .as_ref()
            .and_then(|run| run.file_name.clone())
            .unwrap_or_default();
        match result {
            Ok(answer) => {
                let Some(job_id) = answer["job_id"].as_str().map(str::to_owned) else {
                    let error = "export.jpeg answered no job".to_owned();
                    self.export_finished(format!("Export failed: {error}"), Some(&error), None);
                    return Task::none();
                };
                if let Some(run) = &mut self.export.run {
                    run.job_id = Some(job_id);
                    self.outcome(Outcome::ExportQueued(&answer));
                }
                // The reader starts with the next subscription rebuild and reads at once, so a
                // small photograph that is already written is found without waiting a tick.
                Task::none()
            }
            Err(error) => {
                let reason = error.to_string();
                let status = if error.code == ErrorKind::Conflict.code() {
                    refused_text(&file)
                } else {
                    format!("Export failed: {}", error.message)
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
            // Still going: the reader sends a record again only when it changes, and nothing the
            // desktop shows follows an export's record before it ends.
            Some("queued" | "running") => {}
            Some("ready") => {
                let result = &record["result"];
                let written = result["path"]
                    .as_str()
                    .map(|path| file_name(Path::new(path)))
                    .unwrap_or(file);
                let status = exported_text(
                    &written,
                    result["width"].as_u64().unwrap_or(0),
                    result["height"].as_u64().unwrap_or(0),
                    result["bytes"].as_u64().unwrap_or(0),
                );
                self.export_finished(status, None, Some(record));
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

    /// The export ended, one way or another: say so, forget it, and report it with the job's last
    /// record. `failure` marks an export that was refused or failed.
    fn export_finished(&mut self, status: String, failure: Option<&str>, record: Option<Value>) {
        self.status.text = status;
        self.export.run = None;
        self.event(
            "export_finished",
            || json!({"status":self.status.text,"record":record}),
        );
        self.outcome(Outcome::ExportEnded {
            record: record.as_ref(),
            failure,
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
                },
                export_reads,
            )
        })
    }

    /// The export as a captured frame records it: the menu, whether one can start, and the one
    /// running. No path is recorded.
    pub(super) fn export_summary(&self) -> Value {
        json!({
            "menu_open": self.workspace.title.export_menu_open,
            "can_export": self.workspace.title.can_export,
            "running": self.export.run.as_ref().map(|run| json!({
                "keep_metadata": run.keep_metadata,
                "file_name": run.file_name,
                "job_id": run.job_id,
            })),
        })
    }
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

/// The folder and file name the save dialog opens on: the plan's suggestion, or `<stem>-edited.jpg`
/// in the original's folder when the plan suggests none.
fn dialog_start(plan: &Value, original: &Path) -> (PathBuf, String) {
    let folder = original.parent().map(Path::to_path_buf).unwrap_or_default();
    match plan["suggested"].as_str().map(Path::new) {
        Some(suggested) => (
            suggested.parent().map(Path::to_path_buf).unwrap_or(folder),
            file_name(suggested),
        ),
        None => {
            let stem = original
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "export".into());
            (folder, format!("{stem}-edited.jpg"))
        }
    }
}

/// `Exported DSC_0042-edited.jpg · 6000 × 4000 · 8.4 MB`.
pub(crate) fn exported_text(file: &str, width: u64, height: u64, bytes: u64) -> String {
    format!(
        "Exported {file} \u{b7} {width} \u{d7} {height} \u{b7} {}",
        size_text(bytes)
    )
}

/// A file's size in decimal units: megabytes to a tenth from 1 MB, whole kilobytes below.
fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", (bytes as f64 / 1000.0).round().max(1.0) as u64)
    }
}

/// What the status bar says when the destination already exists.
pub(crate) fn refused_text(file: &str) -> String {
    format!("Not exported: {file} already exists; Luxforge never replaces a file")
}

/// `export.plan` for the entry, then the save dialog unless `destination` names the file.
fn plan_task(
    owner: OwnerHandle,
    client: ClientId,
    (asset_id, entry_id): (AssetId, EntryId),
    keep_metadata: bool,
    original: PathBuf,
    destination: Option<PathBuf>,
) -> Task<Message> {
    let target = (asset_id.clone(), entry_id.clone());
    owner_work(move || plan_now(&owner, client, &asset_id, &entry_id)).then(move |plan| {
        let (asset_id, entry_id) = target.clone();
        let original = original.clone();
        let destination = destination.clone();
        Task::perform(
            async move {
                let plan = plan?;
                let destination = match destination {
                    Some(destination) => destination,
                    None => {
                        let (folder, name) = dialog_start(&plan, &original);
                        let Some(file) = rfd::AsyncFileDialog::new()
                            .set_directory(&folder)
                            .set_file_name(&name)
                            .add_filter("JPEG", &["jpg", "jpeg"])
                            .save_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        file.path().to_path_buf()
                    }
                };
                Ok(Some(Box::new(ExportChoice {
                    asset_id,
                    entry_id,
                    destination,
                    keep_metadata,
                    plan,
                })))
            },
            |result| Message::Export(ExportMessage::Chosen(result)),
        )
    })
}

/// `export.jpeg` for the chosen destination. One press is one request: asking again after the
/// source was prepared keeps its request id, so the core answers a retry that did land from its
/// stored answer and writes no second file.
fn send_task(owner: OwnerHandle, client: ClientId, choice: ExportChoice) -> Task<Message> {
    owner_task(
        move || send_now(&owner, client, &choice),
        |result| Message::Export(ExportMessage::Queued(result)),
    )
}

/// `export.plan` for one entry, blocking this thread until the owner answers.
pub(crate) fn plan_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: &AssetId,
    entry_id: &EntryId,
) -> Result<Value, String> {
    call(
        owner,
        client,
        "export.plan",
        json!({"asset_id": asset_id, "entry_id": entry_id}),
    )
    .map(|(plan, _)| plan)
}

/// `export.jpeg` for one choice, waiting for the source and asking again, under one request id,
/// when the owner answers `preparation-required`.
pub(crate) fn send_now(
    owner: &OwnerHandle,
    client: ClientId,
    choice: &ExportChoice,
) -> Result<Value, CallError> {
    let params = json!({
        "asset_id": choice.asset_id,
        "entry_id": choice.entry_id,
        "destination": choice.destination,
        "keep_metadata": choice.keep_metadata,
        "mutation": request(),
    });
    let mut attempt = 0;
    loop {
        match call_detailed(owner, client, "export.jpeg", params.clone()) {
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
pub(crate) fn read_now(
    owner: &OwnerHandle,
    client: ClientId,
    job_id: &str,
) -> Result<Value, String> {
    call(owner, client, JOB_READ, json!({"job_id": job_id})).map(|(read, _)| read)
}

/// The reader of one export job, as the subscription starts it: a `job.read` through the owner at
/// once and every [`EXPORT_POLL`] after, inside the subscription's stream ([`job_reads`]).
pub(crate) fn export_reads(reader: &Reader<String>) -> BoxStream<'static, Message> {
    let (owner, client, job_id) = (reader.owner.clone(), reader.client, reader.identity.clone());
    job_reads::reads(
        EXPORT_POLL,
        export_pass(job_id.clone(), move || read_now(&owner, client, &job_id)),
    )
}

/// One pass of an export's reader: read the job, and send the read only when it is news. A queued
/// or running record is news when it is the first or differs from the one sent before it; any
/// other status is the job's end, and a failed read ends the export too. Each of those is sent
/// once, and the reader reads no more.
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
    fn the_status_line_names_the_file_its_size_and_its_bytes() {
        assert_eq!(
            exported_text("DSC_0042-edited.jpg", 6000, 4000, 8_412_345),
            "Exported DSC_0042-edited.jpg \u{b7} 6000 \u{d7} 4000 \u{b7} 8.4 MB"
        );
        assert_eq!(size_text(31_600), "32 KB");
        assert_eq!(size_text(120), "1 KB");
        assert_eq!(size_text(1_000_000), "1.0 MB");
        assert_eq!(
            refused_text("a.jpg"),
            "Not exported: a.jpg already exists; Luxforge never replaces a file"
        );
    }

    #[test]
    fn the_dialog_opens_on_the_suggestion_or_the_edited_name_beside_the_original() {
        let original = Path::new("/photos/DSC_0042.NEF");
        let suggested = json!({"suggested": "/photos/DSC_0042-edited-2.jpg"});
        assert_eq!(
            dialog_start(&suggested, original),
            (PathBuf::from("/photos"), "DSC_0042-edited-2.jpg".to_owned())
        );
        assert_eq!(
            dialog_start(&json!({"suggested": null}), original),
            (PathBuf::from("/photos"), "DSC_0042-edited.jpg".to_owned())
        );
        assert_eq!(
            plan_record(&suggested),
            json!({"suggested": "DSC_0042-edited-2.jpg"})
        );
    }
}
