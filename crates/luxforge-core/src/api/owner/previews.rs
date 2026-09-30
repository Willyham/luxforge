//! **Lane B (previews)** on the owner: the preview lane's queue and workers (`crate::previews`),
//! who wants each task, the jobs clients read, each client's view job and its progress, the
//! failures the lane remembers, waking clients whose previews were written, and the handler of
//! `preview.read` (`crate::catalog_types::api`).
//!
//! Everything here is SQL and bookkeeping on the owner thread: a request reads the file's index
//! row and its preview rows in one query, stats the one cached file it answers with, and queues a
//! task; the workers read, decode, encode and write (performance rule 5).
//!
//! - **Tasks and jobs.** A task is one file's tier. A client's `preview.read` that finds no valid
//!   tier queues the task, or joins it, and answers its job: one catalog job per task, shared by
//!   every client that asks, whose result is the
//!   [`PreviewInfo`](crate::catalog_types::PreviewInfo). `job.cancel` of it removes the task from
//!   the queue, or stops it while it runs, unless a view still wants it. These jobs are too short
//!   for rows of their own on the activity board.
//! - **View jobs.** [`want_view`] queues, in the background, the grid tiers a client's view lacks,
//!   as one catalog job per client ("Reading previews", `n of N`), which replaces the client's
//!   previous one and whose cancel drops the tasks it alone wanted.
//! - **Waking.** A client that asked ([`OwnerHandle::watch_previews`]) is woken, on the owner
//!   thread, when a task its request or its view waits on writes a preview or ends, and reads
//!   `preview.read` again for the cells it waits on: nothing polls.
//! - **Failures.** A file with no usable preview is remembered, per tier and signature, in memory
//!   only: its grid reports `unavailable` and `preview.read` answers the failure until the file
//!   changes or Luxforge restarts.
use super::{
    Call, ClientId, EventWake, Owner, OwnerHandle, OwnerMessage,
    catalog::{CatalogMessage, Poster},
};
use crate::{
    Error, ErrorKind, JobId,
    activity::{ActivityBoard, ActivitySpec},
    catalog_types::{
        FileId, PreviewAnswer, PreviewItem, PreviewPriority, PreviewState, PreviewTier,
        SHARED_PREVIEW_BUDGET_BYTES, api::PreviewRead, jobs::PREVIEW_EXTRACT,
    },
    jobs::{CatalogOpened, JobControl, JobKind, Jobs, Output},
    previews::{self, Failures, Outcome, Post, Queue, Store, Task, TaskKey, WorkerEvent, Workers},
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
            running: false,
        }
    }

    fn wanted(&self) -> bool {
        self.job.is_some() || !self.views.is_empty()
    }

    /// Every client to wake when it writes a preview or ends.
    fn clients(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.waiters.iter().chain(&self.views).copied()
    }
}

/// One client's view job: the grid tiers its view lacked when it was evaluated.
struct View {
    job_id: JobId,
    control: Arc<JobControl>,
    total: usize,
    failed: usize,
    /// The tasks it still waits on.
    pending: HashSet<TaskKey>,
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
    /// Hold every task handed out from now on at this gate, or stop holding them.
    #[cfg(test)]
    Hold(Option<Arc<luxforge_testbase::Gate>>),
    /// Hold every grid task handed out from now on after its thumbnail stage, or stop.
    #[cfg(test)]
    HoldStages(Option<Arc<luxforge_testbase::Gate>>),
    /// Set the loupe and large tiers' budget.
    #[cfg(test)]
    Budget(u64),
    /// Answer the tasks handed out so far, in order.
    #[cfg(test)]
    Dispatched(std::sync::mpsc::SyncSender<Vec<TaskKey>>),
    /// Call [`want_view`] for this client, as lane D's `browse.view` will.
    #[cfg(test)]
    WantView {
        client: ClientId,
        files: Vec<FileId>,
        reply: std::sync::mpsc::SyncSender<Result<Option<JobId>, Error>>,
    },
    /// Answer [`PreviewsLane::grid_states`] for these files.
    #[cfg(test)]
    GridStates {
        files: Vec<FileId>,
        reply: std::sync::mpsc::SyncSender<Result<Vec<PreviewState>, Error>>,
    },
}

impl OwnerHandle {
    /// Wake `client` whenever a preview it waits on is written or its task ends: a tier its
    /// `preview.read` queued, or a grid tier its view job reads. `wake` runs on the owner thread
    /// and must only post a signal; the client then reads `preview.read` again for the cells it
    /// waits on. A later call replaces the waker, and disconnecting the client drops it.
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
            #[cfg(test)]
            hold: None,
            #[cfg(test)]
            stage_hold: None,
            #[cfg(test)]
            dispatched: Vec::new(),
        }
    }

    /// A client has gone: its waker goes, and its view job ends, dropping the tasks only it
    /// wanted. The jobs its requests made belong to no client and stay.
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
    }

    /// `job.cancel` cancelled one of this lane's jobs in the job table: a view job drops the tasks
    /// only it wanted; a request's job leaves its task, which is removed from the queue, or
    /// stopped while it runs, when nothing else wants it.
    pub(super) fn cancelled(&mut self, job_id: &JobId, jobs: &mut Jobs) {
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

    /// Stop every task as the owner stops: each running one at its next checkpoint, and each
    /// worker as soon as it is idle, when its channel closes. The workers are not joined: one may
    /// be posting into the owner's channel, which the owner no longer reads.
    pub(super) fn shutdown(self) {
        for wanted in self.tasks.values() {
            wanted.control.cancel("the catalog owner stopped");
        }
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
                        .get(&(*file, PreviewTier::Grid), &signature)
                        .is_some() =>
                {
                    PreviewState::Unavailable
                }
                (state, _) => state,
            })
            .collect())
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
/// meanwhile. A developed photograph's tiers are not built yet.
pub(in crate::api) fn preview_read(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PreviewRead,
) -> Result<Value, Error> {
    let file = match params.item {
        PreviewItem::File { file_id } => file_id,
        PreviewItem::Photo { .. } => {
            return Err(Error::unsupported_input(
                "rendered previews of developed photographs are not built yet",
            ));
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
    let priority = params.priority.unwrap_or_default();
    let key = (file, tier);
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
        if let Some(error) = owner.catalog.previews.failures.get(&key, &tiers.signature) {
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

/// Queue in the background the grid tiers `client`'s view lacks among `files` — those without a
/// valid complete grid tier, found in one query, less those the lane knows have no usable
/// preview — as one view job that replaces the client's previous one. Answers the job, or none
/// when every grid tier is there. `resource-limit` when the queue cannot take them, and then
/// nothing is queued. Lane D's `browse.view` calls it when it evaluates a view over files.
#[allow(dead_code, reason = "lane D's browse.view calls it as it lands")]
pub(super) fn want_view(
    owner: &mut Owner,
    client: ClientId,
    files: &[FileId],
) -> Result<Option<JobId>, Error> {
    if let Some(view) = owner.catalog.previews.views.get(&client) {
        let job_id = view.job_id.clone();
        owner.jobs.cancel(&job_id, REPLACED);
        owner.catalog.previews.end_view(client, &mut owner.jobs);
    }
    let wanted = {
        let index = owner.service.index()?;
        previews::grids_wanted(index.connection(), files)?
    };
    let lane = &owner.catalog.previews;
    let keys: Vec<TaskKey> = wanted
        .into_iter()
        .filter(|(file, signature)| {
            lane.failures
                .get(&(*file, PreviewTier::Grid), signature)
                .is_none()
        })
        .map(|(file, _)| (file, PreviewTier::Grid))
        .collect();
    if keys.is_empty() {
        return Ok(None);
    }
    let new = keys
        .iter()
        .filter(|key| !lane.tasks.contains_key(key))
        .count();
    if !lane.queue.fits(new) {
        return Err(Error::resource_limit(format!(
            "the preview queue cannot take the {new} grid previews this view lacks"
        )));
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
    for key in &keys {
        let wanted = lane
            .want(*key, PreviewPriority::Background)
            .expect("the queue was checked to fit them");
        wanted.views.insert(client);
        running |= wanted.running;
    }
    if running {
        owner.jobs.start(&job_id);
    }
    let view = View {
        job_id: job_id.clone(),
        control,
        total: keys.len(),
        failed: 0,
        pending: keys.into_iter().collect(),
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
        #[cfg(test)]
        PreviewsMessage::Hold(hold) => owner.catalog.previews.hold = hold,
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
            files,
            reply,
        } => {
            let _ = reply.send(want_view(owner, client, &files));
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
/// remember a file with no usable preview, wake whoever waits, and hand out the next task.
fn finished(owner: &mut Owner, worker: usize, key: TaskKey, outcome: Outcome) {
    let lane = &mut owner.catalog.previews;
    if let Some(workers) = lane.workers.as_mut() {
        workers.finished(worker);
    }
    let Outcome {
        result,
        signature,
        permanent,
    } = outcome;
    match (&result, signature) {
        (Ok(_), _) => lane.failures.forget(&key),
        (Err(error), Some(signature)) if permanent => {
            lane.failures.remember(key, signature, error.clone());
        }
        _ => {}
    }
    let Some(mut wanted) = lane.tasks.remove(&key) else {
        dispatch(owner);
        return;
    };
    let stopped = matches!(&result, Err(error) if error.kind == ErrorKind::Cancelled);
    if stopped && wanted.wanted() {
        // Stopped while something still wanted it (a cancel that raced a new request): it runs
        // again with a fresh flag.
        wanted.control = JobControl::new();
        wanted.running = false;
        if lane.queue.push(key, wanted.priority).is_ok() {
            lane.tasks.insert(key, wanted);
            dispatch(owner);
            return;
        }
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
    for client in &wanted.views {
        let Some(view) = lane.views.get_mut(client) else {
            continue;
        };
        if !view.pending.remove(&key) {
            continue;
        }
        if result.is_err() {
            view.failed += 1;
        }
        view.progress();
        if view.pending.is_empty() {
            let view = lane.views.remove(client).expect("the view is held");
            owner.jobs.finish(
                &view.job_id,
                Ok(Output::Value(json!({
                    "files": view.total,
                    "read": view.total - view.failed,
                    "failed": view.failed,
                }))),
            );
        }
    }
    dispatch(owner);
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

    /// Set the loupe and large tiers' byte budget.
    pub(crate) fn preview_budget(&self, bytes: u64) {
        self.previews(PreviewsMessage::Budget(bytes));
    }

    /// The preview tasks handed to workers so far, in order.
    pub(crate) fn previews_dispatched(&self) -> Vec<TaskKey> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::Dispatched(reply));
        answer.recv().expect("the owner answered")
    }

    /// [`want_view`] for `client`, as lane D's `browse.view` will call it.
    pub(crate) fn want_view(
        &self,
        client: ClientId,
        files: Vec<FileId>,
    ) -> Result<Option<JobId>, Error> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.previews(PreviewsMessage::WantView {
            client,
            files,
            reply,
        });
        answer.recv().expect("the owner answered")
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
