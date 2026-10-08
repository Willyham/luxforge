//! Developing picks and Develop's development set ([catalog design](../../../../docs/design/catalog.md#developing-picks)).
//! Its model is `state/develop.rs`.
//!
//! - **Develop N.** The title bar's Develop N or `Cmd+Return`, over a view of files with picks,
//!   reads `pick.plan` of the picks in view and `folder.list` in one owner task, and opens the
//!   confirmation under the button: each event's catalog folder — a new one named after it, its
//!   name selected for typing, or an existing one — and what happens to picks on a card. Escape
//!   closes it and nothing is sent. Develop (Return) sends one `pick.develop {into, mutation,
//!   use_copies?, confirm_removable?}`, the request an agent sends for the same choice, of the picks
//!   in view. Its job progresses on the activity board; its end is heard through the owner's own
//!   answer that the job ended ([`OwnerHandle::wait_source`], which answers for the library lane's
//!   jobs too), never a timer, and its record read with `job.read`. Then Develop opens on the first
//!   photograph it developed, with the photographs it answered as the development set. A failed or
//!   cancelled Develop says so; what it committed stays in the catalog, as the core keeps it.
//! - **Opening a catalog photograph.** A double-click opens Develop on the photograph with the
//!   view's photographs as the set, read into it a window of `browse.rows` at a time.
//! - **Moving through the set.** `←`, `→`, the filmstrip's buttons and a press on its cell replace
//!   the one document through the open path every open shares ([`tasks::open_photograph`]:
//!   `source.prepare`, then `job.adopt`), refused with the one start refusal's reason while a draft
//!   is open. The photograph's large preview (`preview.read {kind: photo, tier: large}`), decoded
//!   ahead off the update loop for the active photograph and its neighbours in the set within a
//!   byte budget (the loupe's own frames cache), is handed to the photo surface in the update of
//!   the key, so it is drawn in the frame after it, labelled in the status bar's render slot. The
//!   controls enable once the original is prepared, and its render replaces the preview as every
//!   frame replaces the one before. Nothing prepares a neighbour.
//! - **Identity.** A cached preview is of one photograph at one entry, and only the photograph
//!   being switched to has its preview drawn: an answer or a decode for another arriving after a
//!   move is kept for that photograph and never drawn for this one, and a preview of another entry
//!   than the one the open then shows is withdrawn.
//! - **The filmstrip** under the canvas draws a window of the set, each cell's grid preview read
//!   and decoded by a cache of the Select grid's own kind; `Cmd+Option+F` collapses it.
use crate::app::{
    Before, Editor,
    gesture::Starting,
    loupe_frames::{self, LoupeFrames, LoupeFramesMessage, Slot, Want},
    message::{Message, develop::DevelopMessage},
    select_previews::{self, Item, SelectPreviewMessage, SelectPreviews, Wanted},
    tasks::{self, call, owner_task, owner_work, request},
    waker::Signal,
};
use crate::layout;
use crate::state::{
    self,
    develop::{
        Confirmation, DevelopSet, Developing, SetPhoto, ShownPreview, develop_params,
        developed_photos, developed_sentence,
    },
    select::RowsRequest,
};
use iced::{Subscription, Task, widget::image::Handle};
use luxforge_core::{
    AssetId, ClientId, JobId, OwnerHandle,
    catalog_types::{
        CatalogFolders, DevelopPlan, DevelopReport, PreviewItem, PreviewState, RowItem, ViewRows,
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// The large previews Develop holds decoded, in RGBA8 bytes: the photograph on screen and its
/// neighbours, each at most the large tier's 2048 px square, 16 MiB.
pub(crate) const FRAMES_BUDGET_BYTES: usize = 96 << 20;
/// How many photographs either side of the active one have their large preview decoded ahead.
pub(crate) const AHEAD: usize = 2;
/// The filmstrip's decoded grid previews: a strip's window and a window either side, at a cell's
/// 144 × 104 pixels, several times over.
pub(crate) const STRIP_BUDGET_BYTES: usize = 32 << 20;
/// The most photographs a view's development set holds, around the photograph it opens on: the
/// design's catalog of 100,000 photographs, 16 bytes of identity and a name each.
pub(crate) const MAX_SET: u32 = 100_000;
/// The rows one `browse.rows` of a set's read asks for: the method's most.
const SET_WINDOW: u32 = 1000;

/// The last Develop this desktop sent, for evidence: its request, what `pick.develop` answered
/// and the job's record when it ended.
#[derive(Clone, Debug, Default)]
pub(crate) struct DevelopRecord {
    pub(crate) request: Option<Value>,
    pub(crate) answer: Option<Value>,
    pub(crate) record: Option<Value>,
}

/// A move to another photograph of the set, until its open has answered.
#[derive(Clone, Debug)]
pub(crate) struct Switch {
    pub(crate) asset: AssetId,
    pub(crate) generation: u64,
}

/// The newest move's timing, for evidence: the frames from its key until its preview was drawn.
#[derive(Clone, Debug, Default)]
pub(crate) struct SwitchTiming {
    pub(crate) asset: Option<AssetId>,
    /// The photo surface's drawn frames when the key was pressed, which the frames until the
    /// preview was drawn are counted from.
    pub(crate) key_frames: u64,
    /// The photo surface version the preview was handed over as, in the key's own update.
    pub(crate) preview_version: Option<u64>,
    /// The surface's drawn frames since the key when it was first seen drawing the preview.
    pub(crate) presented_after: Option<u64>,
}

/// This seam's state in the editor: the model's state, what is in flight and its two caches.
pub(crate) struct Develop {
    pub(crate) state: state::develop::DevelopState,
    /// The newest confirmation's number: a `pick.plan` answered for another is dropped.
    plan_serial: u64,
    /// The newest set's number.
    set_serial: u64,
    pub(crate) last: DevelopRecord,
    /// The move in flight.
    pub(crate) switch: Option<Switch>,
    pub(crate) timing: SwitchTiming,
    /// The large previews decoded ahead: the loupe's frames cache, woken through this seam.
    pub(crate) frames: LoupeFrames,
    /// The filmstrip's grid previews: the Select grid's cache, woken through this seam.
    pub(crate) strip: SelectPreviews,
    /// Both caches were emptied when Select was shown, so they are not emptied again.
    released: bool,
    /// The large previews' read batches, which a test runs against its owner in place of the tasks.
    #[cfg(test)]
    pub(crate) reads: Vec<loupe_frames::ReadBatch>,
}

impl Default for Develop {
    fn default() -> Self {
        Self {
            state: Default::default(),
            plan_serial: 0,
            set_serial: 0,
            last: DevelopRecord::default(),
            switch: None,
            timing: SwitchTiming::default(),
            frames: LoupeFrames::waking(FRAMES_BUDGET_BYTES, post),
            strip: SelectPreviews::waking(STRIP_BUDGET_BYTES, post),
            released: true,
            #[cfg(test)]
            reads: Vec::new(),
        }
    }
}

impl std::fmt::Debug for Develop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Develop")
            .field("state", &self.state)
            .field("switch", &self.switch)
            .finish_non_exhaustive()
    }
}

/// The owner woke this client for a preview it waits on; taken on the update loop.
static OWNER_WOKE: AtomicBool = AtomicBool::new(false);

/// This seam's signal: a decode of either cache landed, or the owner woke this client.
fn signal() -> &'static Signal<Message> {
    static SIGNAL: OnceLock<Signal<Message>> = OnceLock::new();
    SIGNAL.get_or_init(|| Signal::new(|| Message::Develop(DevelopMessage::Woken)))
}

/// The owner's previews wake for this client, passed on by the Select grid, which registers the
/// one wake a client has: called on the owner thread, it only raises the flag and posts the
/// signal.
pub(crate) fn owner_woke() {
    OWNER_WOKE.store(true, Ordering::Release);
    signal().post();
}

/// A decode landed: called on a cache's worker, it only posts the signal.
fn post() {
    signal().post();
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

// -- The owner calls, each the body of one owner task, so a test runs exactly what a task would. --

/// `pick.plan` of the picks in the caller's view, and `folder.list` for the folders Or add to an
/// existing folder offers.
pub(crate) fn plan_now(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<Box<(DevelopPlan, CatalogFolders)>, String> {
    let (plan, _) = call(owner, client, "pick.plan", json!({}))?;
    let (folders, _) = call(owner, client, "folder.list", json!({}))?;
    Ok(Box::new((parse(plan)?, parse(folders)?)))
}

/// `pick.develop` with `params`: the job that develops the picks.
pub(crate) fn develop_now(
    owner: &OwnerHandle,
    client: ClientId,
    params: Value,
) -> Result<String, String> {
    let (started, _) = call(owner, client, "pick.develop", params)?;
    started["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("pick.develop answered no job: {started}"))
}

/// The record of the Develop's job once it has ended: each turn reads `job.read` and, while the
/// job is queued or running, blocks until the owner says it ended — the owner's own answer, which
/// costs nothing while the job runs. The thread blocks: run it inside an owner task.
pub(crate) fn ended_now(owner: &OwnerHandle, client: ClientId, job: &str) -> Result<Value, String> {
    let id = JobId::parse(job).map_err(|error| error.to_string())?;
    loop {
        let (record, _) = call(
            owner,
            client,
            luxforge_core::jobs::JOB_READ,
            json!({ "job_id": job }),
        )?;
        match record["status"].as_str() {
            Some("queued" | "running") => owner
                .wait_source(client, Some(&id))
                .map_err(|error| error.to_string())?,
            _ => return Ok(record),
        }
    }
}

/// The photographs of the caller's view at `revision`, around `position`: at most [`MAX_SET`],
/// read a window of [`SET_WINDOW`] rows at a time, never all at once. Answers them and where
/// `position` is among them.
pub(crate) fn set_now(
    owner: &OwnerHandle,
    client: ClientId,
    revision: u64,
    count: u32,
    position: u32,
) -> Result<(Vec<SetPhoto>, usize), String> {
    let start = position
        .saturating_sub(MAX_SET / 2)
        .min(count.saturating_sub(MAX_SET));
    let end = start.saturating_add(MAX_SET).min(count);
    let mut photos = Vec::with_capacity((end - start) as usize);
    let mut at = start;
    while at < end {
        let request = RowsRequest {
            revision,
            from: at,
            count: SET_WINDOW.min(end - at),
        };
        let (rows, _) = call(
            owner,
            client,
            "browse.rows",
            state::select::rows_params(&request),
        )?;
        let rows: ViewRows = parse(rows)?;
        if rows.rows.is_empty() {
            break;
        }
        for row in rows.rows {
            if let RowItem::Photo { asset_id } = row.item {
                photos.push(SetPhoto {
                    asset_id,
                    kind: Some(row.kind),
                    name: row.file_name,
                });
            }
            at = row.position + 1;
        }
    }
    let active = (position - start) as usize;
    Ok((photos, active))
}

pub(crate) fn folder_set_now(
    owner: &OwnerHandle,
    query: &luxforge_core::catalog_types::ViewQuery,
    active: Option<&AssetId>,
) -> Result<(Vec<SetPhoto>, usize), String> {
    let client = owner.register();
    let result = (|| {
        let summary = super::select::evaluate_now(owner, client, query)?.0;
        let (photos, _) = set_now(owner, client, summary.revision, summary.count, 0)?;
        let index = active
            .and_then(|asset| photos.iter().position(|photo| &photo.asset_id == asset))
            .unwrap_or(0);
        Ok((photos, index))
    })();
    owner.disconnect(client);
    result
}

/// A photograph as the large previews cache names it: its current entry, which `preview.read`
/// answers with the entry its preview is of.
fn item(asset: &AssetId) -> PreviewItem {
    PreviewItem::Photo {
        asset_id: asset.clone(),
        entry_id: None,
    }
}

impl Editor {
    /// One message of developing picks or the development set.
    pub(crate) fn develop_update(&mut self, message: DevelopMessage) -> Task<Message> {
        match message {
            DevelopMessage::Open => return self.develop_open(),
            DevelopMessage::OpenAt(position) => return self.open_view_set(position),
            DevelopMessage::Planned { serial, result } => return self.planned(serial, result),
            DevelopMessage::Name { event, text } => {
                if let Some(choice) = self
                    .develop
                    .state
                    .confirm
                    .as_mut()
                    .and_then(|confirm| confirm.events.get_mut(event))
                {
                    choice.name = text;
                    choice.existing = None;
                }
            }
            DevelopMessage::Menu(event) => {
                if let Some(confirm) = &mut self.develop.state.confirm {
                    confirm.menu = event;
                }
            }
            DevelopMessage::Existing { event, folder } => {
                if let Some(confirm) = &mut self.develop.state.confirm {
                    if let Some(choice) = confirm.events.get_mut(event) {
                        choice.existing = Some(folder);
                    }
                    confirm.menu = None;
                }
            }
            DevelopMessage::Copies(used) => {
                if let Some(confirm) = &mut self.develop.state.confirm {
                    confirm.use_copies = used;
                }
            }
            DevelopMessage::Confirm => return self.develop_confirm(),
            DevelopMessage::Cancel => {
                if self.develop.state.confirm.take().is_some() || self.develop.state.planning {
                    self.develop.state.planning = false;
                    self.develop.plan_serial += 1;
                    self.status.text = "Nothing developed".into();
                }
            }
            DevelopMessage::Started(result) => return self.develop_started(result),
            DevelopMessage::Ended { job, result } => return self.develop_ended(job, result),
            DevelopMessage::SetRead { serial, result } => self.set_read(serial, result),
            DevelopMessage::FolderSetRead { serial, result } => {
                if serial != self.develop.set_serial {
                    return Task::none();
                }
                self.develop.state.folder_loading = false;
                match result {
                    Ok((photos, index)) if !photos.is_empty() => {
                        self.develop.state.set = Some(DevelopSet::new(serial, photos, index));
                        self.develop.state.collapsed = false;
                        self.status.text = "Showing Develop".into();
                        let shown = self.show_develop_workspace();
                        return Task::batch([shown, self.switch_to(index)]);
                    }
                    Ok(_) => {
                        self.status.text =
                            "This catalog folder has no photographs to develop".into()
                    }
                    Err(error) => {
                        self.status.text = format!("Could not load the development set: {error}")
                    }
                }
            }
            DevelopMessage::Step(delta) => {
                let Some(index) = self
                    .develop
                    .state
                    .set
                    .as_ref()
                    .and_then(|set| set.step(delta))
                else {
                    return Task::none();
                };
                return self.switch_to(index);
            }
            DevelopMessage::Select {
                index,
                command,
                shift,
            } => {
                if let Some(set) = &mut self.develop.state.set {
                    set.select(index, command, shift);
                }
            }
            DevelopMessage::SelectAll => {
                self.view_state.copy_settings.cell_menu = None;
                if let Some(set) = &mut self.develop.state.set {
                    if set.reading {
                        self.status.text = "Wait for the development set to finish loading".into();
                    } else {
                        set.selected = (0..set.photos.len()).collect();
                    }
                }
            }
            DevelopMessage::SelectOnly => {
                self.view_state.copy_settings.cell_menu = None;
                if let Some(set) = &mut self.develop.state.set {
                    set.selected.clear();
                }
            }
            DevelopMessage::CellMenu { index, at } => {
                self.view_state.copy_settings.cell_menu = Some((index, (at.x, at.y)));
            }
            DevelopMessage::Show(index) => return self.switch_to(index),
            DevelopMessage::Collapse => {
                if self.develop.state.set.is_some() {
                    self.develop.state.collapsed = !self.develop.state.collapsed;
                }
            }
            DevelopMessage::Frames(LoupeFramesMessage::Read(answers)) => {
                let batch = self.develop.frames.answered(answers);
                return self.develop_frames_task(batch);
            }
            DevelopMessage::Strip(message) => {
                let task = self
                    .develop
                    .strip
                    .update(&self.owner, self.client, message)
                    .map(strip_message);
                return task;
            }
            DevelopMessage::Woken => {
                let owner = OWNER_WOKE.swap(false, Ordering::AcqRel);
                if owner {
                    self.develop.strip.owner_woke();
                }
                let frames = self.develop.frames.woken(owner);
                let strip = self.develop.strip.woken();
                return Task::batch([self.develop_frames_task(frames), self.strip_task(strip)]);
            }
        }
        Task::none()
    }

    // -- Develop N and its confirmation --------------------------------------------------------

    /// Develop N or `Cmd+Return`: read the plan of the picks in view for the confirmation.
    fn develop_open(&mut self) -> Task<Message> {
        if !self.select_shown() {
            return Task::none();
        }
        if self.develop.state.developing.is_some() {
            self.status.text = "A Develop is already running".into();
            return Task::none();
        }
        if self.select.state.over_catalog() {
            self.status.text =
                "Only picks are developed: a photograph in the catalog is developed already".into();
            return Task::none();
        }
        let picks = self
            .select
            .state
            .summary
            .as_ref()
            .map_or(0, |summary| summary.picked);
        if picks == 0 {
            self.status.text = "Pick the photographs to develop first".into();
            return Task::none();
        }
        self.select.state.menu = None;
        self.develop.state.confirm = None;
        self.plan_task()
    }

    /// Ask for the plan of the picks in view.
    fn plan_task(&mut self) -> Task<Message> {
        self.develop.plan_serial += 1;
        self.develop.state.planning = true;
        let serial = self.develop.plan_serial;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || plan_now(&owner, client),
            move |result| Message::Develop(DevelopMessage::Planned { serial, result }),
        )
    }

    /// `pick.plan` answered: the confirmation opens on it, the first new folder's name selected for
    /// typing, so Return accepts it and typing names it.
    fn planned(
        &mut self,
        serial: u64,
        result: Result<Box<(DevelopPlan, CatalogFolders)>, String>,
    ) -> Task<Message> {
        if serial != self.develop.plan_serial || !self.develop.state.planning {
            return Task::none();
        }
        let (plan, folders) = match result {
            Ok(answer) => *answer,
            Err(error) => {
                self.develop.state.planning = false;
                self.status.text = format!("Could not plan the Develop: {error}");
                return Task::none();
            }
        };
        self.develop.state.planning = false;
        if plan.count == 0 {
            self.status.text = "No picks in view to develop".into();
            return Task::none();
        }
        let from = state::select::title(&self.select.state).name;
        let confirmation = Confirmation::new(plan, folders, from);
        let first = confirmation
            .events
            .iter()
            .position(|choice| choice.existing.is_none());
        self.develop.state.confirm = Some(confirmation);
        match first {
            Some(event) => {
                let field = iced::widget::Id::from(crate::view::develop::name_field(event));
                Task::batch([
                    iced::widget::operation::focus(field.clone()),
                    iced::widget::operation::select_all(field),
                ])
            }
            None => Task::none(),
        }
    }

    /// Develop, or Return: `pick.develop` of the picks in view, as the confirmation stands.
    fn develop_confirm(&mut self) -> Task<Message> {
        let Some(confirmation) = &self.develop.state.confirm else {
            return Task::none();
        };
        if let Some(reason) = confirmation.refusal() {
            self.status.text = reason;
            return Task::none();
        }
        let params = develop_params(confirmation, &request());
        let total = confirmation.plan.count;
        self.develop.last = DevelopRecord {
            request: Some(params.clone()),
            ..DevelopRecord::default()
        };
        self.develop.state.confirm = None;
        self.develop.state.developing = Some(Developing { job: None, total });
        self.status.text = format!("Developing {}\u{2026}", state::select::thousands(total));
        self.develop_task(params)
    }

    /// Send `pick.develop` with `params`.
    fn develop_task(&self, params: Value) -> Task<Message> {
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || develop_now(&owner, client, params),
            |result| Message::Develop(DevelopMessage::Started(result)),
        )
    }

    /// `pick.develop` answered with its job: wait for its end. Refused, nothing was developed.
    fn develop_started(&mut self, result: Result<String, String>) -> Task<Message> {
        self.develop.last.answer = Some(match &result {
            Ok(job) => json!({ "job_id": job }),
            Err(error) => json!({ "error": error }),
        });
        let job = match result {
            Ok(job) => job,
            Err(error) => {
                self.develop.state.developing = None;
                self.status.text = format!("Could not develop: {error}");
                return Task::none();
            }
        };
        if let Some(developing) = &mut self.develop.state.developing {
            developing.job = Some(job.clone());
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let result = ended_now(&owner, client, &job);
                (job, result)
            },
            |(job, result)| Message::Develop(DevelopMessage::Ended { job, result }),
        )
    }

    /// The Develop's job ended. Ready, Develop opens on the first photograph it developed with
    /// them as the set; failed or cancelled, the status bar says so, and what it committed stays.
    /// Either way the view's picks changed, so Select reads its view again when it is next shown.
    fn develop_ended(&mut self, job: String, result: Result<Value, String>) -> Task<Message> {
        if self
            .develop
            .state
            .developing
            .as_ref()
            .and_then(|developing| developing.job.as_deref())
            != Some(job.as_str())
        {
            return Task::none();
        }
        self.develop.state.developing = None;
        self.select_woken();
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                self.status.text = format!("Could not follow the Develop: {error}");
                return Task::none();
            }
        };
        self.develop.last.record = Some(record.clone());
        match record["status"].as_str() {
            Some("ready") => {}
            Some("cancelled") => {
                self.status.text =
                    "Develop cancelled \u{b7} what it committed stays in the catalog".into();
                return Task::none();
            }
            _ => {
                let reason = record["error"]["message"]
                    .as_str()
                    .unwrap_or("the job failed");
                self.status.text = format!(
                    "Develop failed: {reason} \u{b7} what it committed stays in the catalog"
                );
                return Task::none();
            }
        }
        let report: DevelopReport = match parse(record["result"].clone()) {
            Ok(report) => report,
            Err(error) => {
                self.status.text = format!("The Develop answered unexpectedly: {error}");
                return Task::none();
            }
        };
        // This desktop's own change: the view read again is not called a change made elsewhere.
        if let Some(change) = report.changes.last() {
            self.select.own_change = Some(change.0);
        }
        let sentence = developed_sentence(&report);
        let photos = developed_photos(&report);
        self.status.text = sentence;
        if photos.is_empty() {
            return Task::none();
        }
        self.develop.set_serial += 1;
        self.develop.state.folder_query = None;
        self.develop.state.set = Some(DevelopSet::new(self.develop.set_serial, photos, 0));
        self.develop.state.collapsed = false;
        let shown = self.show_develop_workspace();
        let opened = self.switch_to(0);
        Task::batch([shown, opened])
    }

    /// Catalog views select or clear the Develop folder; import browsing preserves it.
    /// Changing that selection fences an outstanding set read for the previous folder.
    pub(crate) fn remember_develop_folder(
        &mut self,
        query: &luxforge_core::catalog_types::ViewQuery,
    ) {
        let folder = match &query.source {
            luxforge_core::catalog_types::ViewSource::CatalogFolder { .. } => Some(query.clone()),
            source if source.over_files() => return,
            _ => None,
        };
        if self.develop.state.folder_query != folder {
            self.develop.set_serial += 1;
            self.develop.state.folder_loading = false;
            self.develop.state.folder_query = folder;
        }
    }

    /// Enter Develop using the last catalog folder, or the previously loaded set.
    pub(crate) fn enter_develop_set(&mut self) -> Task<Message> {
        if self.develop.state.folder_loading {
            return Task::none();
        }
        if let Some(query) = self.develop.state.folder_query.clone() {
            self.develop.set_serial += 1;
            let serial = self.develop.set_serial;
            self.develop.state.folder_loading = true;
            self.status.text = "Reading the catalog folder for Develop…".into();
            let (owner, active) = (
                self.owner.clone(),
                self.document
                    .state
                    .as_ref()
                    .map(|state| state.asset.id.clone()),
            );
            return owner_task(
                move || folder_set_now(&owner, &query, active.as_ref()),
                move |result| Message::Develop(DevelopMessage::FolderSetRead { serial, result }),
            );
        }
        if self
            .develop
            .state
            .set
            .as_ref()
            .is_some_and(|set| !set.photos.is_empty())
        {
            let index = self.develop.state.set.as_ref().unwrap().active;
            let shown = self.show_develop_workspace();
            return Task::batch([shown, self.switch_to(index)]);
        }
        if let Some(state) = &self.document.state {
            self.develop.set_serial += 1;
            self.develop.state.set = Some(DevelopSet::new(
                self.develop.set_serial,
                vec![SetPhoto {
                    asset_id: state.asset.id.clone(),
                    kind: Some(state.asset.source.tag()),
                    name: state
                        .asset
                        .locator
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                }],
                0,
            ));
            return self.show_develop_workspace();
        }
        self.status.text = "Pick a catalog folder or choose images to develop from the grid".into();
        Task::none()
    }

    // -- The development set --------------------------------------------------------------------

    /// A double-click on the photograph at `position` of a catalog view: Develop opens on
    /// it with the view's photographs as the set, read into it a window of rows at a time.
    fn open_view_set(&mut self, position: u32) -> Task<Message> {
        let Some(summary) = &self.select.state.summary else {
            return Task::none();
        };
        self.develop.state.folder_query = matches!(
            summary.query.source,
            luxforge_core::catalog_types::ViewSource::CatalogFolder { .. }
        )
        .then(|| summary.query.clone());
        let (revision, count) = (summary.revision, summary.count);
        let Some(row) = self.select.state.rows.read(position) else {
            self.status.text = "Reading the photograph\u{2026}".into();
            return Task::none();
        };
        let RowItem::Photo { asset_id } = &row.item else {
            return Task::none();
        };
        let photo = SetPhoto {
            asset_id: asset_id.clone(),
            kind: Some(row.kind),
            name: row.file_name.clone(),
        };
        self.develop.set_serial += 1;
        let serial = self.develop.set_serial;
        let mut set = DevelopSet::new(serial, vec![photo], 0);
        set.reading = count > 1;
        self.develop.state.set = Some(set);
        self.develop.state.collapsed = false;
        // The grid's wake is the one a client has; Develop is passed it.
        self.select.previews.watch(&self.owner, self.client);
        let shown = self.show_develop_workspace();
        let opened = self.switch_to(0);
        let read = if count > 1 {
            let (owner, client) = (self.owner.clone(), self.client);
            owner_task(
                move || set_now(&owner, client, revision, count, position),
                move |result| Message::Develop(DevelopMessage::SetRead { serial, result }),
            )
        } else {
            Task::none()
        };
        Task::batch([shown, opened, read])
    }

    /// A view's photographs were read into the set: the photograph Develop shows keeps its place.
    fn set_read(&mut self, serial: u64, result: Result<(Vec<SetPhoto>, usize), String>) {
        let Some(set) = self
            .develop
            .state
            .set
            .as_mut()
            .filter(|set| set.serial == serial)
        else {
            return;
        };
        set.reading = false;
        match result {
            Ok((photos, index)) => {
                let active = set.photo(set.active).map(|photo| photo.asset_id.clone());
                let index = match photos.get(index) {
                    Some(photo) if Some(&photo.asset_id) == active.as_ref() => index,
                    _ => active
                        .and_then(|active| photos.iter().position(|photo| photo.asset_id == active))
                        .unwrap_or(0),
                };
                if !photos.is_empty() {
                    set.photos = photos;
                    set.active = index;
                    set.selected.clear();
                    set.first = index;
                    set.reveal(self.develop.state.capacity);
                }
            }
            Err(error) => self.status.text = format!("The set was not read: {error}"),
        }
    }

    /// Move to the set's photograph at `index`: the one document is replaced through the open path,
    /// its cached large preview drawn at once when it is decoded. Refused while a draft is open.
    pub(crate) fn switch_to(&mut self, index: usize) -> Task<Message> {
        self.view_state.copy_settings.cell_menu = None;
        let Some(photo) = self
            .develop
            .state
            .set
            .as_ref()
            .and_then(|set| set.photo(index))
            .cloned()
        else {
            return Task::none();
        };
        let open = self
            .document
            .state
            .as_ref()
            .is_some_and(|state| state.asset.id == photo.asset_id);
        let moving = self
            .develop
            .switch
            .as_ref()
            .is_some_and(|switch| switch.asset == photo.asset_id);
        if (open && self.develop.switch.is_none()) || moving {
            if let Some(set) = &mut self.develop.state.set {
                set.active = index;
                set.selected.clear();
                set.reveal(self.develop.state.capacity);
            }
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Photograph) {
            self.status.text = reason;
            return Task::none();
        }
        // A request of this desktop's in flight other than an open would read back into the
        // document this move replaces.
        if self.busy && self.develop.switch.is_none() && !self.activity.pending {
            self.status.text = state::IN_FLIGHT.into();
            return Task::none();
        }
        if let Some(set) = &mut self.develop.state.set {
            set.active = index;
            set.selected.clear();
            set.reveal(self.develop.state.capacity);
        }
        // The open path's own bookkeeping, as opening a file does: one open request, the frames in
        // flight for the photograph on screen stopped.
        self.begin_request();
        let generation = self.activity.requested;
        self.open_generation.store(generation, Ordering::Release);
        self.presentation.preview_generation = self.cancel_preview_queue();
        // The photograph on screen is closed: nothing reads back into it, and no panel or control
        // describes it while the next one prepares, nor does a Before/After slider's
        // After frame; the adopted session ends its comparison.
        self.document = Default::default();
        self.presentation.compare_after = None;
        self.controls.editing = None;
        self.controls.dragging = None;
        self.busy = true;
        self.develop.state.switching = Some(photo.name.clone());
        self.develop.state.preview = None;
        let key_frames =
            luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE).drawn_frames;
        self.develop.switch = Some(Switch {
            asset: photo.asset_id.clone(),
            generation,
        });
        self.develop.timing = SwitchTiming {
            asset: Some(photo.asset_id.clone()),
            key_frames,
            preview_version: None,
            presented_after: None,
        };
        self.status.text = format!("Opening {}\u{2026}", photo.name);
        self.event(
            "develop_switch",
            || json!({"asset_id": photo.asset_id, "index": index, "generation": generation}),
        );
        if !self.present_cached() {
            // Nothing is decoded for it yet: the photograph on screen is taken off rather than left
            // standing for the one being opened.
            self.presentation.withdraw();
        }
        // Its frame is drawn as any open's is: at the display bounds, its GPU picture at rest
        // planned at the view.
        let proxy = self.drawn();
        tasks::photograph_task(
            self.owner.clone(),
            self.client,
            photo.asset_id,
            generation,
            self.open_generation.clone(),
            proxy,
        )
    }

    /// Hand the decoded large preview of the photograph being switched to to the photo surface, in
    /// place of what is on screen, when one is held and none is drawn yet: only that photograph's.
    fn present_cached(&mut self) -> bool {
        let Some(switch) = &self.develop.switch else {
            return false;
        };
        if self.develop.state.preview.is_some() {
            return false;
        }
        let asset = switch.asset.clone();
        let Some(held) = self.develop.frames.photo(&Slot::frame(item(&asset))) else {
            return false;
        };
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = held.handle
        else {
            return false;
        };
        let picture = held.picture.clone();
        let entry = held.entry.cloned();
        let pixels = Arc::new(pixels.clone());
        let size = (*width, *height);
        let Some(version) = self.presentation.show_cached(pixels, size) else {
            return false;
        };
        let name = self.develop.state.switching.clone().unwrap_or_default();
        let preview = ShownPreview {
            asset: asset.clone(),
            entry,
            name,
            origin: picture.origin,
            approximate: picture.approximate,
            size,
            version,
            key: picture.key.clone(),
        };
        self.status.text = preview.status_text();
        self.event(
            "develop_preview_presented",
            || json!({"asset_id": asset, "entry_id": preview.entry, "key": preview.key, "version": version, "approximate": preview.approximate}),
        );
        self.develop.state.preview = Some(preview);
        self.develop.timing.preview_version = Some(version);
        true
    }

    /// What Develop decodes ahead: the active photograph, on screen, then its neighbours in the
    /// set, nearest first and the direction of travel before the other, at the canvas's pixels.
    fn frames_wanted(&self) -> Vec<Want> {
        let Some(set) = &self.develop.state.set else {
            return Vec::new();
        };
        let [left, top, right, bottom] = layout::canvas_logical(
            self.view_state.window,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.filmstrip_shown(),
        );
        let scale = self.view_state.scale_factor;
        let pixels = (
            ((right - left) * scale).ceil().max(1.0) as u32,
            ((bottom - top) * scale).ceil().max(1.0) as u32,
        );
        let mut wants = Vec::with_capacity(1 + 2 * AHEAD);
        let mut push = |index: usize, shown: bool| {
            if let Some(photo) = set.photo(index) {
                wants.push(Want {
                    slot: Slot::frame(item(&photo.asset_id)),
                    pixels,
                    shown,
                });
            }
        };
        push(set.active, true);
        for distance in 1..=AHEAD {
            push(set.active + distance, false);
            if let Some(before) = set.active.checked_sub(distance) {
                push(before, false);
            }
        }
        wants
    }

    /// The filmstrip's cells on screen and a window either side, at the pixels a cell's image needs.
    fn strip_wanted(&self) -> Wanted {
        let Some(set) = self
            .develop
            .state
            .set
            .as_ref()
            .filter(|_| !self.develop.state.collapsed)
        else {
            return Wanted::default();
        };
        let capacity = self.develop.state.capacity.max(1);
        let end = (set.first + capacity).min(set.photos.len());
        let at = |index: usize| {
            set.photo(index)
                .map(|photo| (Item::Photo(photo.asset_id.clone()), PreviewState::Pending))
        };
        let visible = (set.first..end).filter_map(at).collect();
        let mut margin = Vec::with_capacity(2 * capacity);
        for distance in 0..capacity {
            margin.extend(at(end + distance));
            if let Some(before) = set.first.checked_sub(distance + 1) {
                margin.extend(at(before));
            }
        }
        let scale = self.view_state.scale_factor;
        let side = (luxforge_ui::theme::FILMSTRIP_IMAGE_WIDTH * scale)
            .ceil()
            .max(1.0) as u32;
        Wanted {
            revision: set.serial,
            visible,
            margin,
            side,
        }
    }

    fn develop_frames_task(&mut self, batch: Option<loupe_frames::ReadBatch>) -> Task<Message> {
        let Some(batch) = batch else {
            return Task::none();
        };
        #[cfg(test)]
        self.develop.reads.push(batch.clone());
        let (owner, client) = (self.owner.clone(), self.client);
        owner_work(move || loupe_frames::read(&owner, client, batch)).map(|answers| {
            Message::Develop(DevelopMessage::Frames(LoupeFramesMessage::Read(answers)))
        })
    }

    fn strip_task(&self, batch: Option<select_previews::ReadBatch>) -> Task<Message> {
        let Some(batch) = batch else {
            return Task::none();
        };
        let (owner, client) = (self.owner.clone(), self.client);
        owner_work(move || select_previews::read(&owner, client, batch))
            .map(|answers| strip_message(SelectPreviewMessage::Read(answers)))
    }

    /// A file is opened as today, alone: the set, a move in flight and its preview are forgotten.
    pub(crate) fn develop_opened_alone(&mut self) {
        self.develop.state.set = None;
        self.develop.state.switching = None;
        self.develop.state.preview = None;
        self.develop.switch = None;
    }

    /// Develop's layout has the filmstrip under its canvas: Develop has a set and the strip is not
    /// collapsed. It is Develop's layout whichever workspace is on screen, so a frame rendered for
    /// Develop while Select is shown is sized for the canvas it is drawn in.
    pub(crate) fn filmstrip_shown(&self) -> bool {
        self.develop.state.strip_shown()
    }

    /// The development set's photographs moved to since the key, followed through: the move ends
    /// once its open answered; the preview goes once another frame replaced it, and at once when it
    /// is of another photograph or entry than the one the open shows.
    fn follow_switch(&mut self) {
        let answered = self.develop.switch.as_ref().is_some_and(|switch| {
            self.open_generation.load(Ordering::Acquire) != switch.generation || !self.busy
        });
        if answered {
            self.develop.switch = None;
        }
        if self.document.state.is_some() || self.develop.switch.is_none() {
            self.develop.state.switching = None;
        }
        let Some(preview) = &self.develop.state.preview else {
            return;
        };
        // Replaced on the surface by a render, or taken off. A warm open's GPU picture stands over
        // the cached preview, which stays the surface's base: once the GPU presents the open
        // photograph's content, that picture, not the preview, is the photograph on screen.
        let gpu_presented = self.document.state.is_some()
            && self.presentation.gpu_presented == Some(self.presentation.presented_content);
        if self.presentation.presenter.photo_version() != preview.version
            || !self.presentation.has_picture()
            || gpu_presented
        {
            self.develop.state.preview = None;
            return;
        }
        let shown = self.document.state.as_ref().map(|state| {
            (
                state.asset.id.clone(),
                self.displayed_entry()
                    .unwrap_or_else(|| state.current_entry.id.clone()),
            )
        });
        let wrong = match &shown {
            Some((asset, entry)) => {
                *asset != preview.asset || preview.entry.as_ref().is_some_and(|held| held != entry)
            }
            None => self
                .develop
                .switch
                .as_ref()
                .is_some_and(|switch| switch.asset != preview.asset),
        };
        if wrong {
            let (asset, entry) = (preview.asset.clone(), preview.entry.clone());
            self.presentation.withdraw();
            self.develop.state.preview = None;
            self.event(
                "develop_preview_withdrawn",
                || json!({"asset_id": asset, "entry_id": entry}),
            );
        }
    }

    /// The frames from the key until the preview was drawn, once the surface has drawn it: the
    /// surface records which of its frames first drew each photograph it was handed, so the count
    /// does not depend on when it is read, as long as the preview is still the photograph drawn.
    pub(crate) fn follow_timing(&mut self) {
        let timing = &mut self.develop.timing;
        let Some(version) = timing.preview_version else {
            return;
        };
        if timing.presented_after.is_some() {
            return;
        }
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if gpu.drawn_full_version == Some(version) {
            timing.presented_after = Some(
                gpu.drawn_full_version_frame
                    .saturating_sub(timing.key_frames),
            );
        }
    }

    /// Nothing developing picks or the set asked for is in flight: no plan, Develop, set read or
    /// move, and, with a photograph open in Develop, its newest requested frame on screen. What an
    /// evidence step settles on; a capture then waits for the photograph's actual GPU draw.
    pub(crate) fn develop_quiet(&self) -> bool {
        if self.develop.state.folder_loading {
            return false;
        }
        let state = &self.develop.state;
        !state.planning
            && state.developing.is_none()
            && self.develop.switch.is_none()
            && !state.set.as_ref().is_some_and(|set| set.reading)
            && !self.busy
            && !self.activity.pending
            && (self.select_shown()
                || self.document.state.is_none()
                || (!self.presentation.queue.is_busy()
                    && self.presentation.presented_generation
                        == self.presentation.preview_generation))
    }

    /// The large previews Develop decodes ahead are decoded: the active photograph's and its
    /// neighbours', each its rendered tier rather than a stand-in, or with nothing more to wait for.
    pub(crate) fn develop_ahead_ready(&self) -> bool {
        let wants = self.frames_wanted();
        let (ahead, ready) = self.develop.frames.ahead();
        !wants.is_empty() && self.develop.frames.settled(&wants) && ahead == ready
    }

    /// What developing picks and the set show, for correlated evidence.
    pub(crate) fn develop_summary(&self) -> Value {
        let state = &self.develop.state;
        let set = state.set.as_ref().map(|set| {
            json!({
                "serial": set.serial,
                "count": set.photos.len(),
                "active": set.active,
                "first": set.first,
                "reading": set.reading,
                "selected": set.selected_assets(),
                "assets": set.photos.iter().map(|photo| &photo.asset_id).collect::<Vec<_>>(),
                "names": set.photos.iter().map(|photo| &photo.name).collect::<Vec<_>>(),
            })
        });
        let model = &self.workspace.develop;
        let confirm = state.confirm.as_ref().map(|confirmation| {
            json!({
                "plan": confirmation.plan,
                "events": confirmation.events.iter().map(|choice| json!({
                    "name": choice.name,
                    "existing": choice.existing,
                })).collect::<Vec<_>>(),
                "use_copies": confirmation.use_copies,
                "menu": confirmation.menu,
                "params": develop_params(confirmation, &luxforge_core::MutationRequest {
                    request_id: "shown".into(),
                    actor: state::ACTOR.into(),
                }),
                "model": model.confirm.as_ref().map(|confirm| json!({
                    "title": confirm.title,
                    "from": confirm.from,
                    "heading": confirm.heading,
                    "events": confirm.events.iter().map(|row| json!({
                        "label": row.label,
                        "field": row.field,
                        "existing": row.existing,
                        "tag": row.tag,
                        "menu": row.menu.as_ref().map(|menu| menu.iter().map(|(_, label)| label).collect::<Vec<_>>()),
                    })).collect::<Vec<_>>(),
                    "notes": confirm.notes,
                    "copies": confirm.copies,
                    "refusal": confirm.refusal,
                })),
            })
        });
        let strip = model.strip.as_ref().map(|strip| {
            json!({
                "first": strip.first,
                "total": strip.total,
                "active": strip.active,
                "cells": strip.cells,
                "caption": strip.caption,
                "held": strip.cells.iter().filter(|asset| self.develop.strip.held(&Item::Photo((*asset).clone())).is_some()).count(),
            })
        });
        let document = self.document.state.as_ref().map(|document| {
            json!({
                "asset_id": document.asset.id,
                "entry_id": document.current_entry.id,
                "file": document.asset.locator.file_name().map(|name| name.to_string_lossy().into_owned()),
                "source": document.asset.source.tag().label(),
            })
        });
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        json!({
            "planning": state.planning,
            "confirm": confirm,
            "developing": state.developing.as_ref().map(|developing| json!({"job": developing.job, "total": developing.total})),
            "busy": model.busy,
            "last": {
                "request": self.develop.last.request,
                "answer": self.develop.last.answer,
                "record": self.develop.last.record,
            },
            "set": set,
            "collapsed": state.collapsed,
            "filmstrip_shown": self.filmstrip_shown(),
            "strip": strip,
            "capacity": state.capacity,
            "switching": state.switching,
            "switch": self.develop.switch.as_ref().map(|switch| json!({"asset_id": switch.asset, "generation": switch.generation})),
            "preview": state.preview.as_ref().map(|preview| json!({
                "asset_id": preview.asset,
                "entry_id": preview.entry,
                "name": preview.name,
                "origin": preview.origin.as_str(),
                "approximate": preview.approximate,
                "size": [preview.size.0, preview.size.1],
                "version": preview.version,
                "key": preview.key,
                "drawn": gpu.drawn_full_version == Some(preview.version),
            })),
            "render_label": model.render,
            "timing": {
                "asset_id": self.develop.timing.asset,
                "key_frames": self.develop.timing.key_frames,
                "preview_version": self.develop.timing.preview_version,
                "presented_after": self.develop.timing.presented_after,
            },
            "document": document,
            "frames": self.develop.frames.summary(),
            "strip_previews": self.develop.strip.summary(),
            "ahead_ready": self.develop_ahead_ready(),
            "quiet": self.develop_quiet(),
        })
    }
}

fn strip_message(message: SelectPreviewMessage) -> Message {
    Message::Develop(DevelopMessage::Strip(message))
}

/// After every message: the strip's capacity for the window; while Develop shows a set, its large
/// previews and its strip's grid previews read and decoded; a move followed through; and both
/// caches emptied while Select is shown. An evidence run also hears when a develop step settles.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let (width, _) = layout::photo_surface(
        editor.view_state.window,
        editor.session.workspace.state_panel,
        editor.session.workspace.tools_panel,
        false,
    );
    editor.develop.state.capacity = luxforge_ui::filmstrip_capacity(width);
    if let Some(set) = &mut editor.develop.state.set {
        set.reveal(editor.develop.state.capacity);
    }
    let mut tasks = Vec::new();
    if editor.select_shown() || editor.develop.state.set.is_none() {
        if !editor.develop.released {
            editor.develop.released = true;
            let cancel = editor.develop.frames.release();
            tasks.push(editor.develop_frames_task(cancel));
            editor.develop.strip.release();
        }
    } else {
        editor.develop.released = false;
        let wants = editor.frames_wanted();
        let batch = editor.develop.frames.want(wants);
        tasks.push(editor.develop_frames_task(batch));
        let wanted = editor.strip_wanted();
        let batch = editor.develop.strip.plan_for(wanted);
        tasks.push(editor.strip_task(batch));
        // A preview decoded while its photograph prepares is drawn as soon as it lands.
        editor.present_cached();
    }
    editor.follow_switch();
    editor.follow_timing();
    if editor.evidence.is_some() {
        editor.develop_evidence_after();
    }
    Task::batch(tasks)
}

/// This seam's signal while Develop shows a set; a signal posted meanwhile waits for it.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if !editor.select_shown() && editor.develop.state.set.is_some() {
        Subscription::run(|| signal().stream())
    } else {
        Subscription::none()
    }
}

/// The filmstrip's cell images, looked up by photograph: each held handle, borrowed so it keeps its
/// id and uploads once.
#[derive(Clone, Copy)]
pub(crate) struct StripImages<'a> {
    previews: &'a SelectPreviews,
}

impl<'a> StripImages<'a> {
    pub(crate) fn image(&self, asset: &AssetId) -> Option<&'a Handle> {
        let previews: &'a SelectPreviews = self.previews;
        previews.held(&Item::Photo(asset.clone()))
    }
}

impl Develop {
    /// What the filmstrip's view borrows.
    pub(crate) fn strip_images(&self) -> StripImages<'_> {
        StripImages {
            previews: &self.strip,
        }
    }
}

#[cfg(test)]
#[path = "develop_tests.rs"]
mod filmstrip_tests;
