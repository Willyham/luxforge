//! **Lane B (previews)** on the owner: the preview lane's queue and workers (`crate::previews`),
//! who wants each task, the jobs clients read, each client's view job and its progress, the
//! failures and deferrals the lane remembers, waking clients whose previews were written, the
//! handler of `preview.read` (`crate::catalog_types::api`), in `previews/regions.rs` the region
//! jobs of `preview.region` on their own worker, and in `previews/renders.rs` developed
//! photographs' previews: their render jobs on the render worker, the camera preview each shows
//! until its first render, and following every commit.
//!
//! Everything here is SQL and bookkeeping on the owner thread: a request reads the file's index
//! row and its preview rows in one query, stats the one cached file it answers with, and queues a
//! task; the workers read, decode, encode and write (performance rule 5).
//!
//! - **Tasks and jobs.** A task is one file's tier, or the camera preview of a developed
//!   photograph's tier (`crate::previews::CameraSource`). A client's `preview.read` that finds no
//!   valid tier of a file queues the task, or joins it, and answers its job: one catalog job per
//!   task, shared by every client that asks, whose result is the
//!   [`PreviewInfo`](crate::catalog_types::PreviewInfo). `job.cancel` of it removes the task from
//!   the queue, or stops it while it runs, unless a view still wants it. These jobs are too short
//!   for rows of their own on the activity board.
//! - **View jobs.** [`want_view_items`] queues, in the background, the grid tiers a client's view
//!   lacks, as one catalog job per client ("Reading previews", `n of N`), which replaces the
//!   client's previous one and whose cancel drops the tasks it alone wanted. For a developed
//!   photograph with no grid row at all that is its camera preview only, never a render: renders
//!   run for what is visible or looked ahead to.
//! - **Waking.** A client that asked ([`OwnerHandle::watch_previews`]) is woken, on the owner
//!   thread, when something its own request waits on is written or ends — a task its
//!   `preview.read` was answered `queued` for (each stage of a file's grid tier; a photograph's
//!   camera preview and its render), or its `preview.region` — and reads `preview.read` again
//!   for the cells it waits on: nothing polls. A view job's progress wakes nobody: a client reads
//!   it from the activity board, as any job's.
//! - **Failures.** A file that cannot give a tier — no usable preview Luxforge can develop, a
//!   corrupt file, one past a limit — is remembered, per tier and signature, in memory only: its
//!   grid reports `unavailable` and `preview.read` answers the failure until the file changes or
//!   Luxforge restarts.
//! - **The development fallback.** A RAW with no usable preview (the Canon EOS R5 Mark II's and
//!   R8's H.265-only files) is developed neutrally for a visible or look-ahead task, one RAW at a
//!   time in the process, and its tier labelled `developed`. A background task — a view job's —
//!   never develops: it ends deferred, counted done in its view job's progress and not failed, is
//!   remembered per tier and signature so views do not read it again, and its grid stays
//!   `pending` until a visible request develops it. A background `preview.read` of it answers
//!   `not-ready`.
//! - **The kept development.** The one development the process keeps (`previews::region`) is
//!   released when a client's view is replaced, when the last client that asked for a region or
//!   was served a developed tier disconnects, and when the lane stops; every cancel of running
//!   work wakes the callers waiting for the development, so a cancelled one returns at once.
use super::{
    Call, ClientId, EventWake, Owner, OwnerHandle, OwnerMessage,
    catalog::{CatalogMessage, Poster},
};
use crate::api::{Origin, announce_once};
use crate::{
    EditorService, Error, ErrorKind, JobId,
    activity::{ActivityBoard, ActivitySpec},
    catalog_types::{
        AssetRowId, FileId, PreviewAnswer, PreviewItem, PreviewOrigin, PreviewPriority,
        PreviewState, PreviewTier, SHARED_PREVIEW_BUDGET_BYTES, ViewItem, api::PreviewRead,
        jobs::PREVIEW_EXTRACT,
    },
    jobs::{CatalogOpened, JobControl, JobKind, Jobs, Output},
    previews::{
        self, CameraSource, Failures, Outcome, Post, Queue, RegionDone, RenderDone, Store, Task,
        TaskKey, WorkerEvent, Workers, region,
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// The reason a view job ends when a newer view replaces it.
const REPLACED: &str = "replaced by a newer view";
/// The reason a view job ends when its client disconnects.
const DISCONNECTED: &str = "the client disconnected";
/// The reason a task stops when nobody wants it any more.
const UNWANTED: &str = "no request or view wants this preview any more";

mod regions;
mod renders;
pub(in crate::api) use regions::preview_region;
pub(in crate::api::owner) use renders::follow_changes;

/// Lane B's state on the owner.
pub(super) struct PreviewsLane {
    poster: Poster,
    board: Arc<ActivityBoard>,
    /// The bytes the loupe and large tiers may take together.
    budget: u64,
    queue: Queue,
    /// Every queued or running task and who wants it.
    tasks: HashMap<TaskKey, Wanted>,
    /// Which task each client-requested job is.
    jobs: HashMap<JobId, TaskKey>,
    /// Started with the first task.
    workers: Option<Workers>,
    views: HashMap<ClientId, View>,
    wakers: HashMap<ClientId, EventWake>,
    failures: Failures,
    /// The RAWs a background task found with no usable preview and did not develop, per tier and
    /// signature, each with the `not-ready` error a background request answers.
    deferred: Failures,
    /// The clients that asked for a region or were served a developed tier: when the last of them
    /// disconnects, the kept development is released.
    developing: BTreeSet<ClientId>,
    regions: regions::Regions,
    renders: renders::Renders,
    #[cfg(test)]
    develop: Option<previews::DevelopHook>,
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
    #[cfg(test)]
    stage_hold: Option<Arc<luxforge_testbase::Gate>>,
    /// Every task handed to a worker, in order, for the tests of the lane's order.
    #[cfg(test)]
    dispatched: Vec<TaskKey>,
}

/// One queued or running task and who wants it.
struct Wanted {
    /// The run's own cancel flag and render token, cancelled only when nobody wants the task.
    control: Arc<JobControl>,
    /// The highest priority it was asked for.
    priority: PreviewPriority,
    /// The job clients' requests share, when a client asked for this tier itself.
    job: Option<JobId>,
    /// The clients whose view jobs want it.
    views: BTreeSet<ClientId>,
    /// The clients that asked for it, woken when it writes a preview or ends.
    waiters: BTreeSet<ClientId>,
    /// A photograph's render waits on it: its camera preview is the fallback until the render.
    for_render: bool,
    /// For a photograph's camera preview, its original as the catalog records it.
    camera: Option<Arc<CameraSource>>,
    running: bool,
}

impl Wanted {
    fn new(priority: PreviewPriority) -> Self {
        Self {
            control: JobControl::new(),
            priority,
            job: None,
            views: BTreeSet::new(),
            waiters: BTreeSet::new(),
            for_render: false,
            camera: None,
            running: false,
        }
    }

    fn wanted(&self) -> bool {
        self.job.is_some() || !self.views.is_empty() || self.for_render
    }

    /// Every client to wake when it writes a preview or ends: those whose own requests wait on
    /// it, never a view's.
    fn clients(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.waiters.iter().copied()
    }
}

/// One client's view job: the grid tiers its view lacked when it was evaluated.
struct View {
    job_id: JobId,
    control: Arc<JobControl>,
    total: usize,
    failed: usize,
    /// Tiers of RAWs with no usable preview, left for a visible request to develop: done, not
    /// failed.
    deferred: usize,
    /// The tasks it still waits on.
    pending: HashSet<TaskKey>,
    /// Whether one of its tasks wrote a file's complete grid tier, and with it the tier's
    /// brightness fingerprint: a view grouped before then may hold a metadata-less bracket it
    /// called a burst, so the view's end advances the index's revision ([`fingerprints_written`]).
    fingerprinted: bool,
}

impl View {
    fn progress(&self) {
        let done = self.total - self.pending.len();
        self.control.set_progress(
            Some(done as f64 / self.total as f64),
            &format!("{done} of {}", self.total),
        );
    }
}

/// What lane B's workers post back, and what the lane's own handle sends it.
pub(super) enum PreviewsMessage {
    /// Wake this client when a preview it waits on is written ([`OwnerHandle::watch_previews`]).
    Watch {
        client: ClientId,
        wake: EventWake,
    },
    Worker(WorkerEvent),
    /// The region worker finished a job.
    Region(RegionDone),
    /// The render worker finished a render.
    Render(RenderDone),
    /// Hold every task handed out from now on at this gate, or stop holding them.
    #[cfg(test)]
    Hold(Option<Arc<luxforge_testbase::Gate>>),
    /// Hold every region job handed out from now on at this gate, or stop holding them.
    #[cfg(test)]
    HoldRegions(Option<Arc<luxforge_testbase::Gate>>),
    /// Hold every render handed out from now on at this gate, or stop holding them.
    #[cfg(test)]
    HoldRenders(Option<Arc<luxforge_testbase::Gate>>),
    /// Answer the renders handed out so far, in order.
    #[cfg(test)]
    RendersDispatched(
        std::sync::mpsc::SyncSender<Vec<(crate::AssetId, crate::EntryId, Vec<PreviewTier>)>>,
    ),
    /// Bound the render queue at this many renders.
    #[cfg(test)]
    RenderCapacity(usize),
    /// Develop a RAW with no usable preview through this hook from now on, or through the
    /// production development again.
    #[cfg(test)]
    Develop(Option<previews::DevelopHook>),
    /// Hold every grid task handed out from now on after its thumbnail stage, or stop.
    #[cfg(test)]
    HoldStages(Option<Arc<luxforge_testbase::Gate>>),
    /// Set the loupe and large tiers' budget.
    #[cfg(test)]
    Budget(u64),
    /// Answer the tasks handed out so far, in order.
    #[cfg(test)]
    Dispatched(std::sync::mpsc::SyncSender<Vec<TaskKey>>),
    /// Call [`want_view_items`] for this client, as lane D's `browse.view` will.
    #[cfg(test)]
    WantView {
        client: ClientId,
        items: Vec<ViewItem>,
        reply: std::sync::mpsc::SyncSender<Result<Option<JobId>, Error>>,
    },
    /// Answer [`PreviewsLane::grid_states`] for these files.
    #[cfg(test)]
    GridStates {
        files: Vec<FileId>,
        reply: std::sync::mpsc::SyncSender<Result<Vec<PreviewState>, Error>>,
    },
    /// Answer [`PreviewsLane::view_grid_states`] for these items.
    #[cfg(test)]
    ViewGridStates {
        items: Vec<ViewItem>,
        reply: std::sync::mpsc::SyncSender<Result<Vec<PreviewState>, Error>>,
    },
}

impl OwnerHandle {
    /// Wake `client` whenever something its own request waits on is written or ends: a tier its
    /// `preview.read` was answered `queued` for, once per stage it waits on (a file's grid tier's
    /// thumbnail and its embedded preview; a photograph's camera preview and its render), or its
    /// `preview.region`. A view job's progress wakes nobody; the activity board carries it. `wake`
    /// runs on the owner thread and must only post a signal; the client then reads `preview.read`
    /// again for the cells it waits on. A later call replaces the waker, and disconnecting the
    /// client drops it.
    pub fn watch_previews(&self, client: ClientId, wake: EventWake) {
        let _ = self
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Previews(
                PreviewsMessage::Watch { client, wake },
            )));
    }
}

impl PreviewsLane {
    pub(super) fn new(poster: Poster, board: Arc<ActivityBoard>) -> Self {
        Self {
            poster,
            board,
            budget: SHARED_PREVIEW_BUDGET_BYTES,
            queue: Queue::default(),
            tasks: HashMap::new(),
            jobs: HashMap::new(),
            workers: None,
            views: HashMap::new(),
            wakers: HashMap::new(),
            failures: Failures::default(),
            deferred: Failures::default(),
            developing: BTreeSet::new(),
            regions: regions::Regions::default(),
            renders: renders::Renders::default(),
            #[cfg(test)]
            develop: None,
            #[cfg(test)]
            hold: None,
            #[cfg(test)]
            stage_hold: None,
            #[cfg(test)]
            dispatched: Vec::new(),
        }
    }

    /// A client has gone: its waker goes, its view job ends, dropping the tasks only it wanted,
    /// its region job is cancelled and its region answer removed, and the kept development is
    /// released when it was the last client that used it. The jobs its preview requests made
    /// belong to no client and stay.
    pub(super) fn disconnect(&mut self, client: ClientId, jobs: &mut Jobs) {
        self.wakers.remove(&client);
        for wanted in self.tasks.values_mut() {
            wanted.waiters.remove(&client);
        }
        if let Some(view) = self.views.get(&client) {
            let job_id = view.job_id.clone();
            jobs.cancel(&job_id, DISCONNECTED);
            self.end_view(client, jobs);
        }
        self.regions.disconnect(client, jobs);
        self.renders.disconnect(client);
        if self.developing.remove(&client) && self.developing.is_empty() {
            region::release_development();
        }
    }

    /// `job.cancel` cancelled one of this lane's jobs in the job table: a view job drops the tasks
    /// only it wanted; a request's job leaves its task, which is removed from the queue, or
    /// stopped while it runs, when nothing else wants it.
    pub(super) fn cancelled(&mut self, job_id: &JobId, jobs: &mut Jobs) {
        if self.regions.cancelled(job_id, jobs) || self.renders.cancelled(job_id, jobs) {
            return;
        }
        if let Some(client) = self
            .views
            .iter()
            .find_map(|(client, view)| (view.job_id == *job_id).then_some(*client))
        {
            self.end_view(client, jobs);
            return;
        }
        let Some(key) = self.jobs.remove(job_id) else {
            return;
        };
        // The table asked a running job to stop; it ends now, whatever becomes of the task.
        finish_cancelled(jobs, job_id);
        if let Some(wanted) = self.tasks.get_mut(&key) {
            wanted.job = None;
            wanted.waiters.clear();
        }
        self.drop_unwanted(key);
    }

    /// Stop every task and region as the owner stops: each running one at its next checkpoint,
    /// a waiter for the development at once, and each worker as soon as it is idle, when its
    /// channel closes. The kept development and the region answers go. The workers are not
    /// joined: one may be posting into the owner's channel, which the owner no longer reads.
    pub(super) fn shutdown(self) {
        for wanted in self.tasks.values() {
            wanted.control.cancel("the catalog owner stopped");
        }
        self.regions.shutdown();
        self.renders.shutdown();
        region::wake_development_waiters();
        region::release_development();
        drop(self.workers);
    }

    /// Each file's grid state, in the order given, in one query, reporting `unavailable` for a
    /// file the lane found no usable preview in and holds no thumbnail stage of. For lane D's
    /// `browse.rows`.
    #[allow(dead_code, reason = "lane D's browse.rows calls it as it lands")]
    pub(super) fn grid_states(
        &self,
        connection: &Connection,
        files: &[FileId],
    ) -> Result<Vec<PreviewState>, Error> {
        let rows = previews::grid_rows(connection, files)?;
        Ok(files
            .iter()
            .zip(rows)
            .map(|(file, row)| match (row.state, row.signature) {
                (PreviewState::Pending, Some(signature))
                    if self
                        .failures
                        .get(&(ViewItem::File(*file), PreviewTier::Grid), &signature)
                        .is_some() =>
                {
                    PreviewState::Unavailable
                }
                (state, _) => state,
            })
            .collect())
    }

    /// Each item's grid state, in the order given, for lane D's `browse.rows`: a file's as
    /// [`Self::grid_states`] answers it, and a developed photograph's against its current entry —
    /// `ready` for its rendered grid tier at this generation, `thumbnail` for a camera preview only
    /// or a render of another entry or generation, `pending` for nothing. Three queries at most:
    /// the files', the photographs' current entries in the catalog, and their grid rows.
    #[allow(dead_code, reason = "lane D's browse.rows calls it as it lands")]
    pub(super) fn view_grid_states(
        &self,
        service: &EditorService,
        items: &[ViewItem],
    ) -> Result<Vec<PreviewState>, Error> {
        let files: Vec<FileId> = items
            .iter()
            .filter_map(|item| match item {
                ViewItem::File(file) => Some(*file),
                ViewItem::Photo(_) => None,
            })
            .collect();
        let rows: Vec<AssetRowId> = items
            .iter()
            .filter_map(|item| match item {
                ViewItem::Photo(row) => Some(*row),
                ViewItem::File(_) => None,
            })
            .collect();
        let photos = renders::photos_at(service, &rows)?;
        let current: Vec<_> = photos
            .iter()
            .flatten()
            .map(|photo| (photo.asset_id.clone(), photo.entry_id.clone()))
            .collect();
        let index = service.index()?;
        let mut files = self.grid_states(index.connection(), &files)?.into_iter();
        let mut grids = previews::photo_grid_rows(index.connection(), &current)?.into_iter();
        let mut photos = photos.into_iter();
        Ok(items
            .iter()
            .map(|item| match item {
                ViewItem::File(_) => files.next().unwrap_or(PreviewState::Pending),
                ViewItem::Photo(_) => match photos.next().flatten() {
                    Some(_) => grids
                        .next()
                        .map_or(PreviewState::Pending, |grid| grid.state()),
                    None => PreviewState::Pending,
                },
            })
            .collect())
    }

    /// A photograph's render wrote `key`'s tier: its camera preview is no longer wanted in its
    /// place, and is dropped unless a view still wants it.
    fn release_camera(&mut self, key: TaskKey) {
        if let Some(wanted) = self.tasks.get_mut(&key) {
            wanted.for_render = false;
        }
        self.drop_unwanted(key);
    }

    /// Wake `clients`, each once.
    fn wake(&self, clients: impl Iterator<Item = ClientId>) {
        let mut woken = BTreeSet::new();
        for client in clients {
            if woken.insert(client)
                && let Some(wake) = self.wakers.get(&client)
            {
                wake();
            }
        }
    }

    /// Queue `key` at `priority`, or join it where it waits or runs.
    fn want(&mut self, key: TaskKey, priority: PreviewPriority) -> Result<&mut Wanted, Error> {
        let running = self.tasks.get(&key).is_some_and(|wanted| wanted.running);
        if !running {
            self.queue.push(key, priority)?;
        }
        let wanted = self
            .tasks
            .entry(key)
            .or_insert_with(|| Wanted::new(priority));
        wanted.priority = wanted.priority.max(priority);
        Ok(wanted)
    }

    /// Forget `key` when nothing wants it any more: out of the queue while it waits, stopped at its
    /// next checkpoint while it runs.
    fn drop_unwanted(&mut self, key: TaskKey) {
        let Some(wanted) = self.tasks.get(&key) else {
            return;
        };
        if wanted.wanted() {
            return;
        }
        if wanted.running {
            wanted.control.cancel(UNWANTED);
            region::wake_development_waiters();
        } else {
            self.queue.remove(&key);
            self.tasks.remove(&key);
        }
    }

    /// End `client`'s view job: its record, when the table still holds it live, and its interest
    /// in every task it still waits on. Its running tasks run on and are cached; its waiting ones
    /// are dropped unless something else wants them.
    fn end_view(&mut self, client: ClientId, jobs: &mut Jobs) {
        let Some(view) = self.views.remove(&client) else {
            return;
        };
        finish_cancelled(jobs, &view.job_id);
        for key in view.pending {
            let Some(wanted) = self.tasks.get_mut(&key) else {
                continue;
            };
            wanted.views.remove(&client);
            if !wanted.running {
                self.drop_unwanted(key);
            }
        }
    }
}

/// End a job the table still holds live as cancelled with the reason its cancel gave, or the
/// lane's own. A job already finished is left as it is.
fn finish_cancelled(jobs: &mut Jobs, job_id: &JobId) {
    jobs.finish(job_id, Err(Error::cancelled(crate::jobs::CANCELLED)));
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().min(i64::MAX as u128) as i64)
}

fn answer(answer: &PreviewAnswer) -> Result<Value, Error> {
    serde_json::to_value(answer)
        .map_err(|error| Error::internal(format!("preview answer: {error}")))
}

/// `preview.read`: a file's cached tier, or the job making it with the best preview cached
/// meanwhile; a developed photograph's rendered tier, or the render making it
/// ([`renders::read_photo`]).
pub(in crate::api) fn preview_read(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PreviewRead,
) -> Result<Value, Error> {
    let priority = params.priority.unwrap_or_default();
    let file = match params.item {
        PreviewItem::File { file_id } => file_id,
        PreviewItem::Photo { asset_id, entry_id } => {
            return renders::read_photo(owner, call, &asset_id, entry_id, params.tier, priority);
        }
    };
    let tier = match params.tier {
        PreviewTier::Large => {
            return Err(Error::validation(
                "a file has grid and loupe tiers; the large tier is a developed photograph's",
            ));
        }
        tier => tier,
    };
    let key = (ViewItem::File(file), tier);
    let fallback = {
        let index = owner.service.index()?;
        let tiers = previews::file_tiers(index.connection(), file)?
            .ok_or_else(|| Error::validation(format!("the index holds no file {}", file.0)))?;
        if let Some(ready) = tiers
            .tier(tier)
            .filter(|preview| preview.complete() && previews::intact(&preview.path))
        {
            if tier == PreviewTier::Loupe {
                previews::touch(index.connection(), file, now_ms())?;
            }
            return answer(&PreviewAnswer::Ready {
                preview: ready.info(),
            });
        }
        // The grid tier, whole or its thumbnail stage, while the loupe tier is read; the
        // thumbnail stage while the grid's embedded stage is.
        let fallback = tiers
            .grid
            .as_ref()
            .filter(|grid| previews::intact(&grid.path))
            .map(|grid| grid.info());
        let lane = &owner.catalog.previews;
        let remembered = lane.failures.get(&key, &tiers.signature).or_else(|| {
            // A background request never develops; a visible or look-ahead one does.
            (priority == PreviewPriority::Background)
                .then(|| lane.deferred.get(&key, &tiers.signature))
                .flatten()
        });
        if let Some(error) = remembered {
            return match fallback {
                // The thumbnail stage is the grid tier this file has.
                Some(preview) if tier == PreviewTier::Grid => {
                    answer(&PreviewAnswer::Ready { preview })
                }
                _ => Err(error.clone()),
            };
        }
        fallback
    };
    ensure_workers(owner)?;
    let lane = &mut owner.catalog.previews;
    let wanted = lane.want(key, priority)?;
    wanted.waiters.insert(call.client);
    let running = wanted.running;
    let (job_id, opened) = match &wanted.job {
        Some(job_id) => (job_id.clone(), false),
        None => {
            let job_id = JobId::new();
            wanted.job = Some(job_id.clone());
            (job_id, true)
        }
    };
    if opened {
        owner.jobs.open_catalog(CatalogOpened {
            job_id: job_id.clone(),
            kind: JobKind::PreviewExtract,
            asset_id: None,
            origin: Some(call.origin.clone()),
            control: JobControl::new(),
        });
        if running {
            owner.jobs.start(&job_id);
        }
        lane.jobs.insert(job_id.clone(), key);
    }
    dispatch(owner);
    answer(&PreviewAnswer::Queued { job_id, fallback })
}

/// [`want_view_items`] for a view over files.
#[allow(dead_code, reason = "lane D's browse.view calls it as it lands")]
pub(super) fn want_view(
    owner: &mut Owner,
    client: ClientId,
    files: &[FileId],
) -> Result<Option<JobId>, Error> {
    let items: Vec<ViewItem> = files.iter().copied().map(ViewItem::File).collect();
    want_view_items(owner, client, &items)
}

/// Queue in the background what `client`'s view lacks among `items`, as one view job that
/// replaces the client's previous one, and release the kept development, whose frame belonged to
/// the view replaced:
///
/// - for a file, its grid tier when it has no valid complete one (one query), less the files the
///   lane knows it cannot read or deferred for a development;
/// - for a developed photograph with no grid row at all — no render of any entry and no camera
///   preview (two queries: the catalog's record, the index's rows) — its camera preview only,
///   which is cheap, and never a render: renders run for visible and look-ahead requests, never
///   for thousands of photographs in the background.
///
/// Answers the job, or none when nothing is lacking. `resource-limit` when the queue cannot take
/// the files' tiers, and then nothing is queued; photographs' camera previews take the room left,
/// in the view's order, since a visible read of any other asks for its own. Lane D's `browse.view`
/// calls it when it evaluates a view.
pub(super) fn want_view_items(
    owner: &mut Owner,
    client: ClientId,
    items: &[ViewItem],
) -> Result<Option<JobId>, Error> {
    if let Some(view) = owner.catalog.previews.views.get(&client) {
        let job_id = view.job_id.clone();
        owner.jobs.cancel(&job_id, REPLACED);
        owner.catalog.previews.end_view(client, &mut owner.jobs);
    }
    region::release_development();
    let mut files = Vec::new();
    let mut rows = Vec::new();
    for item in items {
        match item {
            ViewItem::File(file) => files.push(*file),
            ViewItem::Photo(row) => rows.push(*row),
        }
    }
    let wanted = {
        let index = owner.service.index()?;
        previews::grids_wanted(index.connection(), &files)?
    };
    let mut cameras: Vec<(TaskKey, CameraSource)> = Vec::new();
    if !rows.is_empty() {
        owner.catalog.previews.renders.used();
        let photos: Vec<CameraSource> = renders::photos_at(&owner.service, &rows)?
            .into_iter()
            .flatten()
            .collect();
        let current: Vec<_> = photos
            .iter()
            .map(|photo| (photo.asset_id.clone(), photo.entry_id.clone()))
            .collect();
        let grids = {
            let index = owner.service.index()?;
            previews::photo_grid_rows(index.connection(), &current)?
        };
        let lane = &owner.catalog.previews;
        let mut seen = HashSet::new();
        for (photo, grid) in photos.into_iter().zip(grids) {
            let key = (ViewItem::Photo(photo.row), PreviewTier::Grid);
            let signature = previews::recorded_signature(&photo);
            if !grid.any
                && seen.insert(key)
                && lane.failures.get(&key, &signature).is_none()
                && lane.deferred.get(&key, &signature).is_none()
            {
                cameras.push((key, photo));
            }
        }
    }
    let lane = &owner.catalog.previews;
    let keys: Vec<TaskKey> = wanted
        .into_iter()
        .map(|(file, signature)| ((ViewItem::File(file), PreviewTier::Grid), signature))
        .filter(|(key, signature)| {
            lane.failures.get(key, signature).is_none()
                && lane.deferred.get(key, signature).is_none()
        })
        .map(|(key, _)| key)
        .collect();
    let new = keys
        .iter()
        .filter(|key| !lane.tasks.contains_key(key))
        .count();
    if !lane.queue.fits(new) {
        return Err(Error::resource_limit(format!(
            "the preview queue cannot take the {new} grid previews this view lacks"
        )));
    }
    let mut room = lane.queue.room() - new;
    cameras.retain(|(key, _)| {
        if lane.tasks.contains_key(key) {
            return true;
        }
        let fits = room > 0;
        room = room.saturating_sub(1);
        fits
    });
    if keys.is_empty() && cameras.is_empty() {
        return Ok(None);
    }
    ensure_workers(owner)?;
    let lane = &mut owner.catalog.previews;
    let job_id = JobId::new();
    let control = JobControl::new();
    owner.jobs.open_catalog(CatalogOpened {
        job_id: job_id.clone(),
        kind: JobKind::PreviewExtract,
        asset_id: None,
        origin: None,
        control: control.clone(),
    });
    control.begin_activity(lane.board.begin(ActivitySpec {
        kind: PREVIEW_EXTRACT.activity,
        label: PREVIEW_EXTRACT.label,
        detail: None,
        asset_id: None,
        job_id: Some(job_id.to_string()),
    }));
    let mut running = false;
    let mut pending = HashSet::with_capacity(keys.len() + cameras.len());
    let photos = cameras.into_iter().map(|(key, photo)| (key, Some(photo)));
    for (key, camera) in keys.into_iter().map(|key| (key, None)).chain(photos) {
        let wanted = lane
            .want(key, PreviewPriority::Background)
            .expect("the queue was checked to fit them");
        wanted.views.insert(client);
        if let Some(camera) = camera {
            wanted.camera.get_or_insert_with(|| Arc::new(camera));
        }
        running |= wanted.running;
        pending.insert(key);
    }
    if running {
        owner.jobs.start(&job_id);
    }
    let view = View {
        job_id: job_id.clone(),
        control,
        total: pending.len(),
        failed: 0,
        deferred: 0,
        pending,
        fingerprinted: false,
    };
    view.progress();
    lane.views.insert(client, view);
    dispatch(owner);
    Ok(Some(job_id))
}

/// Start the workers with their own connections to the index, on the first task.
fn ensure_workers(owner: &mut Owner) -> Result<(), Error> {
    if owner.catalog.previews.workers.is_some() {
        return Ok(());
    }
    let stores = {
        let index = owner.service.index()?;
        let dir = index.previews_dir();
        (0..previews::PREVIEW_WORKERS)
            .map(|_| Ok(Store::new(index.connect()?, dir.clone())))
            .collect::<Result<Vec<_>, Error>>()?
    };
    let poster = owner.catalog.previews.poster.clone();
    let post: Post = Arc::new(move |event| {
        poster.post(CatalogMessage::Previews(PreviewsMessage::Worker(event)));
    });
    owner.catalog.previews.workers = Some(Workers::start(stores, post)?);
    Ok(())
}

/// Ask the extraction workers for `photo`'s camera preview of `tier` at `priority`, the fallback
/// `client`'s read waits on until the photograph's render: not when the lane remembers it has none
/// or cannot read it, and not when the queue is full — the render still comes.
fn want_camera(
    owner: &mut Owner,
    photo: CameraSource,
    tier: PreviewTier,
    priority: PreviewPriority,
    client: ClientId,
) {
    let key = (ViewItem::Photo(photo.row), tier);
    let signature = previews::recorded_signature(&photo);
    let lane = &owner.catalog.previews;
    if lane.failures.get(&key, &signature).is_some()
        || lane.deferred.get(&key, &signature).is_some()
    {
        return;
    }
    if ensure_workers(owner).is_err() {
        return;
    }
    let Ok(wanted) = owner.catalog.previews.want(key, priority) else {
        return;
    };
    wanted.for_render = true;
    wanted.waiters.insert(client);
    wanted.camera.get_or_insert_with(|| Arc::new(photo));
    dispatch(owner);
}

/// Hand the highest-priority tasks to the idle workers.
fn dispatch(owner: &mut Owner) {
    let lane = &mut owner.catalog.previews;
    let Some(workers) = lane.workers.as_mut() else {
        return;
    };
    while workers.has_idle() {
        let Some((key, _)) = lane.queue.pop() else {
            break;
        };
        let Some(wanted) = lane.tasks.get_mut(&key) else {
            continue;
        };
        wanted.running = true;
        if let Some(job_id) = &wanted.job {
            owner.jobs.start(job_id);
        }
        for client in &wanted.views {
            if let Some(view) = lane.views.get(client) {
                owner.jobs.start(&view.job_id);
            }
        }
        #[cfg(test)]
        lane.dispatched.push(key);
        let task = Task {
            key,
            control: wanted.control.clone(),
            budget: lane.budget,
            develops: wanted.priority >= PreviewPriority::Visible,
            camera: wanted.camera.clone(),
            #[cfg(test)]
            develop: lane.develop.clone(),
            #[cfg(test)]
            hold: lane.hold.clone(),
            #[cfg(test)]
            stage_hold: lane.stage_hold.clone(),
        };
        if workers.dispatch(task).is_err() {
            // That worker has gone; the task waits for another.
            wanted.running = false;
            let _ = lane.queue.push(key, wanted.priority);
        }
    }
}

pub(super) fn handle(owner: &mut Owner, message: PreviewsMessage) {
    match message {
        PreviewsMessage::Watch { client, wake } => {
            owner.catalog.previews.wakers.insert(client, wake);
        }
        PreviewsMessage::Worker(WorkerEvent::Stage { key }) => {
            let lane = &owner.catalog.previews;
            if let Some(wanted) = lane.tasks.get(&key) {
                lane.wake(wanted.clients());
            }
        }
        PreviewsMessage::Worker(WorkerEvent::Finished {
            worker,
            key,
            outcome,
        }) => finished(owner, worker, key, outcome),
        PreviewsMessage::Region(done) => regions::finished(owner, done),
        PreviewsMessage::Render(done) => renders::finished(owner, done),
        #[cfg(test)]
        PreviewsMessage::Hold(hold) => owner.catalog.previews.hold = hold,
        #[cfg(test)]
        PreviewsMessage::HoldRegions(hold) => owner.catalog.previews.regions.hold(hold),
        #[cfg(test)]
        PreviewsMessage::HoldRenders(hold) => owner.catalog.previews.renders.hold(hold),
        #[cfg(test)]
        PreviewsMessage::RendersDispatched(reply) => {
            let _ = reply.send(owner.catalog.previews.renders.dispatched());
        }
        #[cfg(test)]
        PreviewsMessage::RenderCapacity(capacity) => {
            owner.catalog.previews.renders.set_capacity(capacity);
        }
        #[cfg(test)]
        PreviewsMessage::Develop(develop) => owner.catalog.previews.develop = develop,
        #[cfg(test)]
        PreviewsMessage::HoldStages(hold) => owner.catalog.previews.stage_hold = hold,
        #[cfg(test)]
        PreviewsMessage::Budget(budget) => owner.catalog.previews.budget = budget,
        #[cfg(test)]
        PreviewsMessage::Dispatched(reply) => {
            let _ = reply.send(owner.catalog.previews.dispatched.clone());
        }
        #[cfg(test)]
        PreviewsMessage::WantView {
            client,
            items,
            reply,
        } => {
            let _ = reply.send(want_view_items(owner, client, &items));
        }
        #[cfg(test)]
        PreviewsMessage::ViewGridStates { items, reply } => {
            let states = owner
                .catalog
                .previews
                .view_grid_states(&owner.service, &items);
            let _ = reply.send(states);
        }
        #[cfg(test)]
        PreviewsMessage::GridStates { files, reply } => {
            let states = owner.service.index().and_then(|index| {
                owner
                    .catalog
                    .previews
                    .grid_states(index.connection(), &files)
            });
            let _ = reply.send(states);
        }
    }
}

/// A worker finished a task: record its outcome for the job and the views that wait on it,
/// remember a file that cannot give the tier or a deferred development, wake whoever waits, and
/// hand out the next task.
fn finished(owner: &mut Owner, worker: usize, key: TaskKey, outcome: Outcome) {
    let lane = &mut owner.catalog.previews;
    if let Some(workers) = lane.workers.as_mut() {
        workers.finished(worker);
    }
    let Outcome {
        result,
        signature,
        permanent,
        deferred,
    } = outcome;
    match (&result, signature) {
        (Ok(_), _) => {
            lane.failures.forget(&key);
            lane.deferred.forget(&key);
        }
        (Err(error), Some(signature)) if permanent => {
            lane.failures.remember(key, signature, error.clone());
        }
        (Err(error), Some(signature)) if deferred => {
            lane.deferred.remember(key, signature, error.clone());
        }
        _ => {}
    }
    let Some(mut wanted) = lane.tasks.remove(&key) else {
        dispatch(owner);
        return;
    };
    let stopped = matches!(&result, Err(error) if error.kind == ErrorKind::Cancelled);
    // Stopped while something still wanted it (a cancel that raced a new request), or deferred
    // while a visible or look-ahead request joined it: it runs again, with a fresh flag when it
    // was stopped, and develops if it needs to.
    let raised = deferred && wanted.job.is_some() && wanted.priority >= PreviewPriority::Visible;
    if (stopped && wanted.wanted()) || raised {
        if stopped {
            wanted.control = JobControl::new();
        }
        wanted.running = false;
        if lane.queue.push(key, wanted.priority).is_ok() {
            lane.tasks.insert(key, wanted);
            dispatch(owner);
            return;
        }
    }
    if matches!(&result, Ok(info) if info.origin == PreviewOrigin::Developed) {
        lane.developing.extend(wanted.waiters.iter().copied());
    }
    lane.wake(wanted.clients());
    if let Some(job_id) = &wanted.job {
        lane.jobs.remove(job_id);
        owner.jobs.finish(
            job_id,
            result.clone().and_then(|info| {
                serde_json::to_value(&info)
                    .map(Output::Value)
                    .map_err(|error| Error::internal(format!("preview result: {error}")))
            }),
        );
    }
    // A file's complete grid tier writes its fingerprint beside it.
    let fingerprint = matches!(
        (&key, &result),
        ((ViewItem::File(_), PreviewTier::Grid), Ok(info))
            if matches!(info.origin, PreviewOrigin::Embedded | PreviewOrigin::Developed)
    );
    let mut advance = false;
    for client in &wanted.views {
        let Some(view) = lane.views.get_mut(client) else {
            continue;
        };
        if !view.pending.remove(&key) {
            continue;
        }
        view.fingerprinted |= fingerprint;
        if deferred {
            view.deferred += 1;
        } else if result.is_err() {
            view.failed += 1;
        }
        view.progress();
        if view.pending.is_empty() {
            let view = lane.views.remove(client).expect("the view is held");
            advance |= view.fingerprinted;
            owner.jobs.finish(
                &view.job_id,
                Ok(Output::Value(json!({
                    "files": view.total,
                    "read": view.total - view.failed - view.deferred,
                    "deferred": view.deferred,
                    "failed": view.failed,
                }))),
            );
        }
    }
    if advance {
        fingerprints_written(owner);
    }
    dispatch(owner);
}

/// A view job that wrote grid fingerprints has ended: advance the index's revision once and record
/// one index event naming it, as a batch of the index lane does, so a browse view grouped before
/// the fingerprints existed is stale and groups its metadata-less runs again with them. Only a
/// view job's end does this, never each tier or a replaced view, so a view that regroups on the
/// event and asks for its missing tiers again cannot keep itself busy: the view job it starts
/// holds only the tiers still missing, and ends with one more revision only when it wrote some.
fn fingerprints_written(owner: &mut Owner) {
    let revision = owner.service.index().and_then(|mut index| {
        let tx = index.connection_mut().transaction()?;
        let revision = crate::index::database::advance_revision(&tx)?;
        tx.commit()?;
        Ok(revision)
    });
    // The index is a cache: a revision that could not be written leaves views as they are until
    // the next change, never an error for a finished job.
    if let Ok(revision) = revision {
        announce_once(
            &mut owner.announced,
            &Origin::new(PREVIEW_EXTRACT.job_kind, "").index(revision),
        );
        owner.record_announced();
    }
}

#[cfg(test)]
impl OwnerHandle {
    fn previews(&self, message: PreviewsMessage) {
        self.sender
            .send(OwnerMessage::Catalog(CatalogMessage::Previews(message)))
            .expect("the owner is running");
    }

    /// Hold every preview task handed out from now on at `gate`, or stop holding them.
    pub(crate) fn hold_previews(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.previews(PreviewsMessage::Hold(gate));
    }

    /// Hold every grid task handed out from now on at `gate` after its thumbnail stage, or stop.
    pub(crate) fn hold_preview_stages(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.previews(PreviewsMessage::HoldStages(gate));
    }

    /// Hold every region job handed out from now on at `gate`, or stop holding them.
    pub(crate) fn hold_regions(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.previews(PreviewsMessage::HoldRegions(gate));
    }

    /// Develop a RAW with no usable preview through `develop` from now on, or through the
    /// production development again.
    pub(crate) fn develop_previews_with(&self, develop: Option<previews::DevelopHook>) {
        self.previews(PreviewsMessage::Develop(develop));
    }

    /// Set the loupe and large tiers' byte budget.
    pub(crate) fn preview_budget(&self, bytes: u64) {
        self.previews(PreviewsMessage::Budget(bytes));
    }

    /// Every preview task handed to the extraction workers so far, in order.
    fn preview_tasks_dispatched(&self) -> Vec<TaskKey> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::Dispatched(reply));
        answer.recv().expect("the owner answered")
    }

    /// The files' preview tasks handed to workers so far, in order.
    pub(crate) fn previews_dispatched(&self) -> Vec<(FileId, PreviewTier)> {
        self.preview_tasks_dispatched()
            .into_iter()
            .filter_map(|(item, tier)| match item {
                ViewItem::File(file) => Some((file, tier)),
                ViewItem::Photo(_) => None,
            })
            .collect()
    }

    /// The photographs' camera previews handed to the extraction workers so far, in order.
    pub(crate) fn cameras_dispatched(&self) -> Vec<(AssetRowId, PreviewTier)> {
        self.preview_tasks_dispatched()
            .into_iter()
            .filter_map(|(item, tier)| match item {
                ViewItem::Photo(row) => Some((row, tier)),
                ViewItem::File(_) => None,
            })
            .collect()
    }

    /// Hold every render handed out from now on at `gate`, or stop holding them.
    pub(crate) fn hold_renders(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.previews(PreviewsMessage::HoldRenders(gate));
    }

    /// The renders handed to the render worker so far, in order: each photograph, entry and the
    /// tiers it made.
    pub(crate) fn renders_dispatched(
        &self,
    ) -> Vec<(crate::AssetId, crate::EntryId, Vec<PreviewTier>)> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::RendersDispatched(reply));
        answer.recv().expect("the owner answered")
    }

    /// Bound the render queue at `capacity` renders.
    pub(crate) fn render_capacity(&self, capacity: usize) {
        self.previews(PreviewsMessage::RenderCapacity(capacity));
    }

    /// [`want_view`] for `client`, as lane D's `browse.view` will call it.
    pub(crate) fn want_view(
        &self,
        client: ClientId,
        files: Vec<FileId>,
    ) -> Result<Option<JobId>, Error> {
        self.want_view_items(client, files.into_iter().map(ViewItem::File).collect())
    }

    /// [`want_view_items`] for `client`, as lane D's `browse.view` will call it.
    pub(crate) fn want_view_items(
        &self,
        client: ClientId,
        items: Vec<ViewItem>,
    ) -> Result<Option<JobId>, Error> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::WantView {
            client,
            items,
            reply,
        });
        answer.recv().expect("the owner answered")
    }

    /// The owner's grid states for `items`.
    pub(crate) fn view_grid_states(&self, items: Vec<ViewItem>) -> Vec<PreviewState> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::ViewGridStates { items, reply });
        answer
            .recv()
            .expect("the owner answered")
            .expect("the grid states")
    }

    /// The owner's grid states for `files`.
    pub(crate) fn preview_grid_states(&self, files: Vec<FileId>) -> Vec<PreviewState> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::GridStates { files, reply });
        answer
            .recv()
            .expect("the owner answered")
            .expect("the grid states")
    }
}

#[cfg(test)]
#[path = "previews_tests.rs"]
mod preview_cache_owner;
