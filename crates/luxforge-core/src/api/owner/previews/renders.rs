//! The preview lane on the owner: developed photographs' previews — `preview.read` of a
//! photograph, the render jobs of its grid and large tiers on their own worker, the camera preview
//! it shows until its first render, and following every commit.
//!
//! Everything here is SQL and bookkeeping on the owner thread, plus one `O(layers)` plan per render
//! ([`rendered::plan_render`]): the catalog's record of the photograph and its current entry read
//! from its rows, its rows of the index, one stat of the file a ready answer names. The render
//! worker (`crate::previews::RenderWorker`) reads, decodes, renders, encodes and writes.
//!
//! - **Renders.** A render is one photograph at one entry — the current one, or the one a request
//!   names — making every tier wanted of it (grid, large or both) from one preparation. Requests
//!   for the same (asset, entry) join it. Each tier's `preview-render` jobs, whose result is the
//!   tier's [`PreviewInfo`](crate::catalog_types::PreviewInfo), are the one the requests for it
//!   share by interest, as a file's tier's, and the one of a commit's re-render that wants it in
//!   the background ([`Tier`]), which belongs to no client, as a view's job does. A tier asked for
//!   while its render runs without it is rendered next. Renders wait in the lane's priority order
//!   (look-ahead, then visible newest first, then background) in a queue of at most
//!   [`RENDER_QUEUE_CAPACITY`], and run one at a time on the render worker; the one running is a
//!   "Rendering previews" row on the activity board with its first tier's job. A client's
//!   `job.cancel` of the requests' job, or its disconnect, releases its interest
//!   ([`Renders::released`]), and a client that never asked is refused `conflict` while one waits;
//!   when the last interested client leaves, the job ends `cancelled`. Any client's cancel of a
//!   commit's job stops it for everyone and drops the commit's want. A tier no job wants any more
//!   leaves the render, which is removed from the queue, or stopped at its next checkpoint, once
//!   no tier of it is wanted.
//! - **The camera preview.** Until a photograph has any render of a tier, a request for it also
//!   asks the extraction workers for its camera preview (`crate::previews::CameraSource`), at the
//!   request's priority: the fallback the next read answers. The render releases it.
//! - **Following commits.** Every change the owner records names the asset it changed
//!   ([`follow_changes`], from `Owner::record_announced`): a commit, an undo, a redo or a restore by
//!   any client. Currency is by key, so the next read of a photograph whose current entry moved
//!   answers a new render with the old tier as the fallback; and a photograph that had a rendered
//!   grid tier has it rendered again in the background at once, so views stay current. A newer
//!   commit supersedes such a re-render that has not started. The stale rows and files go when the
//!   new tiers are written.
//! - **Leaving the catalog.** A photograph that leaves has its renders forgotten
//!   ([`Renders::forget`]): a waiting one leaves the queue, the running one stops and writes no row
//!   after, and every tier's job ends `cancelled`, naming why.
use super::{
    CatalogMessage, ClientId, LEFT_THE_CATALOG, Owner, PreviewsMessage, answer, finish_cancelled,
    now_ms, want_camera,
};
use crate::{
    AssetId, EditorService, EntryId, Error, ErrorKind, JobId, SourceTag,
    activity::ActivitySpec,
    api::{Origin, owner::Call},
    catalog_types::{
        AssetRowId, PreviewAnswer, PreviewPriority, PreviewTier, ViewItem, VolumeId,
        jobs::PREVIEW_RENDER,
    },
    jobs::{CatalogOpened, JobControl, JobKind, Jobs, Output},
    previews::{
        self, CameraSource, Queue, RENDER_QUEUE_CAPACITY, RenderDone, RenderPost, RenderWork,
        RenderWorker, Store, rendered,
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

/// Why a render stops when no tier of it is wanted any more.
const UNWANTED: &str = "no request wants this render any more";
/// Why a commit's background re-render ends when a newer commit replaces it before it starts.
const SUPERSEDED: &str = "superseded by a newer commit";

/// The render jobs on the owner.
pub(super) struct Renders {
    /// Started with the first render.
    worker: Option<RenderWorker>,
    queue: Queue<u64>,
    /// Every waiting or running render, by the owner's number for it.
    tasks: HashMap<u64, RenderTask>,
    /// Which render each (asset, entry) is, so a request joins it.
    photos: HashMap<(AssetId, EntryId), u64>,
    /// Which render and tier each job is.
    jobs: HashMap<JobId, (u64, PreviewTier)>,
    /// The render on the worker.
    running: Option<u64>,
    next: u64,
    /// A photograph's preview was asked for since the owner started: from then on commits are
    /// followed. Until then no rendered tier this process could re-render is known to anyone.
    used: bool,
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
    /// Every render handed to the worker, in order, for the tests of its order.
    #[cfg(test)]
    dispatched: Vec<(AssetId, EntryId, Vec<PreviewTier>)>,
}

impl Default for Renders {
    fn default() -> Self {
        Self {
            worker: None,
            queue: Queue::with_capacity(RENDER_QUEUE_CAPACITY),
            tasks: HashMap::new(),
            photos: HashMap::new(),
            jobs: HashMap::new(),
            running: None,
            next: 0,
            used: false,
            #[cfg(test)]
            hold: None,
            #[cfg(test)]
            dispatched: Vec::new(),
        }
    }
}

/// One waiting or running render.
struct RenderTask {
    asset_id: AssetId,
    entry_id: EntryId,
    row: AssetRowId,
    /// The original's file name, for the activity board.
    name: String,
    /// The highest priority it was asked for.
    priority: PreviewPriority,
    /// The render's own cancel flag and render token, cancelled only when no tier is wanted.
    control: Arc<JobControl>,
    /// Each tier wanted, and what wants it.
    tiers: BTreeMap<PreviewTier, Tier>,
    /// While it runs, the tiers the render makes.
    making: Option<Vec<PreviewTier>>,
    /// The clients that asked and have not left every tier they asked for, woken when it writes
    /// or ends.
    waiters: BTreeSet<ClientId>,
    /// Its photograph left the catalog while it ran ([`Renders::forget`]): what it answers is
    /// nobody's.
    forgotten: bool,
}

impl RenderTask {
    /// A client waits for a tier of it; otherwise only a commit wants it, and a newer commit
    /// supersedes it.
    fn requested(&self) -> bool {
        self.tiers.values().any(|tier| tier.request.is_some())
    }

    /// Every job of its tiers, each with its control.
    fn jobs(&self) -> impl Iterator<Item = &(JobId, Arc<JobControl>)> {
        self.tiers.values().flat_map(Tier::jobs)
    }
}

/// One tier a render makes, and what wants it, each with its job and that job's control: the
/// requests for the tier, whose job they share by interest, and a commit's re-render, which wants
/// it in the background as a view wants a file's tier and whose job belongs to no client.
#[derive(Default)]
struct Tier {
    /// The job the requests for the tier share, while a client still waits for it.
    request: Option<(JobId, Arc<JobControl>)>,
    /// The job of a commit's re-render that wants the tier ([`follow`]): any client reads it, and
    /// its cancel stops it for everyone and drops the commit's want.
    background: Option<(JobId, Arc<JobControl>)>,
}

impl Tier {
    fn wanted(&self) -> bool {
        self.request.is_some() || self.background.is_some()
    }

    /// Its jobs: the requests' first, then the commit's.
    fn jobs(&self) -> impl Iterator<Item = &(JobId, Arc<JobControl>)> {
        self.request.iter().chain(&self.background)
    }

    /// Its jobs, taken.
    fn into_jobs(self) -> impl Iterator<Item = (JobId, Arc<JobControl>)> {
        self.request.into_iter().chain(self.background)
    }
}

impl Renders {
    /// Hold every render handed out from now on at `gate`, or stop holding them.
    #[cfg(test)]
    pub(super) fn hold(&mut self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.hold = gate;
    }

    #[cfg(test)]
    pub(super) fn dispatched(&self) -> Vec<(AssetId, EntryId, Vec<PreviewTier>)> {
        self.dispatched.clone()
    }

    #[cfg(test)]
    pub(super) fn set_capacity(&mut self, capacity: usize) {
        self.queue = Queue::with_capacity(capacity);
    }

    /// Commits are followed from now on.
    pub(super) fn used(&mut self) {
        self.used = true;
    }

    /// The job table stopped `job_id`, a render tier's: the requests' job, its last interested
    /// client gone, or a commit's background job, cancelled for everyone. It ends `cancelled` and no
    /// longer wants its tier; the tier is rendered still while the other wants it, and the render
    /// leaves the queue, or stops at its next checkpoint, once no tier of it is wanted. Whether it
    /// was one.
    pub(super) fn cancelled(&mut self, job_id: &JobId, jobs: &mut Jobs) -> bool {
        let Some((id, tier)) = self.jobs.remove(job_id) else {
            return false;
        };
        finish_cancelled(jobs, job_id);
        let Some(task) = self.tasks.get_mut(&id) else {
            return true;
        };
        if let Some(wanted) = task.tiers.get_mut(&tier) {
            for slot in [&mut wanted.request, &mut wanted.background] {
                if slot.as_ref().is_some_and(|(job, _)| job == job_id) {
                    *slot = None;
                }
            }
            if !wanted.wanted() {
                task.tiers.remove(&tier);
            }
        }
        if task.tiers.is_empty() {
            if self.running == Some(id) {
                task.control.cancel(UNWANTED);
            } else {
                self.remove(id);
            }
        }
        true
    }

    /// `client` left `job_id`, when it is a render's tier: it is woken for the render no more once
    /// it wants none of its tiers. Answers the photograph's row and the tier when the client wants
    /// no render of that tier of it any more, so the camera preview asked for in the tier's place
    /// wakes it no more either; `None` otherwise, and for any other job. One pass over the renders
    /// held, at most [`RENDER_QUEUE_CAPACITY`] and the running one.
    pub(super) fn released(
        &mut self,
        job_id: &JobId,
        client: ClientId,
        jobs: &Jobs,
    ) -> Option<(AssetRowId, PreviewTier)> {
        let (id, tier) = *self.jobs.get(job_id)?;
        let wants = |job: &JobId| jobs.wanted_by(job, client);
        let request = |tier: &Tier| tier.request.as_ref().is_some_and(|(job, _)| wants(job));
        let task = self.tasks.get_mut(&id)?;
        if !task.tiers.values().any(request) {
            task.waiters.remove(&client);
        }
        let row = task.row;
        let still = self
            .tasks
            .values()
            .any(|task| task.row == row && task.tiers.get(&tier).is_some_and(request));
        (!still).then_some((row, tier))
    }

    /// Take a waiting render out of the queue and forget it.
    fn remove(&mut self, id: u64) -> Option<RenderTask> {
        self.queue.remove(&id);
        let task = self.tasks.remove(&id)?;
        self.photos
            .remove(&(task.asset_id.clone(), task.entry_id.clone()));
        Some(task)
    }

    /// Forget the renders of `assets`, photographs that left the catalog: a waiting one leaves the
    /// queue, a commit's re-render among them; the running one is cancelled, so it stops at its
    /// next checkpoint and writes no row after, and is kept, forgotten, until the worker answers.
    /// Each tier's job ends `cancelled`, naming why. Answers the clients that waited on them, to
    /// wake.
    pub(super) fn forget(&mut self, assets: &HashSet<AssetId>, jobs: &mut Jobs) -> Vec<ClientId> {
        let ids: Vec<u64> = self
            .tasks
            .iter()
            .filter(|(_, task)| assets.contains(&task.asset_id))
            .map(|(id, _)| *id)
            .collect();
        let mut woken = Vec::new();
        for id in ids {
            let (tiers, waiters) = if self.running == Some(id) {
                let task = self.tasks.get_mut(&id).expect("the render is held");
                task.control.cancel(LEFT_THE_CATALOG);
                task.forgotten = true;
                let key = (task.asset_id.clone(), task.entry_id.clone());
                let taken = (
                    std::mem::take(&mut task.tiers),
                    std::mem::take(&mut task.waiters),
                );
                self.photos.remove(&key);
                taken
            } else {
                let task = self.remove(id).expect("the render is held");
                (task.tiers, task.waiters)
            };
            for (job_id, _) in tiers.into_values().flat_map(Tier::into_jobs) {
                self.jobs.remove(&job_id);
                jobs.finish(&job_id, Err(Error::cancelled(LEFT_THE_CATALOG)));
            }
            woken.extend(waiters);
        }
        woken
    }

    /// A client has gone: it is woken for nothing any more. Its interest in the tiers' jobs has
    /// left them already (`Jobs::disconnect`), and a tier it was the last to want has ended through
    /// [`Self::cancelled`].
    pub(super) fn disconnect(&mut self, client: ClientId) {
        for task in self.tasks.values_mut() {
            task.waiters.remove(&client);
        }
    }

    /// Stop as the owner stops: the running render at its next checkpoint, the worker once it is
    /// idle (its channel closes) and the discard of other generations at its next page.
    pub(super) fn shutdown(self) {
        if let Some(task) = self.running.and_then(|id| self.tasks.get(&id)) {
            task.control.cancel("the catalog owner stopped");
        }
        drop(self.worker);
    }
}

/// A photograph as the catalog records it now: its row, its original and its current entry, read
/// from its rows alone, never through the editor's caches. `validation` for an unknown asset.
pub(super) fn photo(service: &EditorService, asset_id: &AssetId) -> Result<CameraSource, Error> {
    let (_, photo) = service
        .connection
        .prepare_cached(&format!(
            "SELECT 0, {PHOTO_COLUMNS} FROM assets a JOIN asset_state s ON s.asset_id = a.id
             WHERE a.id = ?1"
        ))?
        .query_row([asset_id.as_str()], source)
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))??;
    Ok(photo)
}

/// The photographs at `rows`, in the order given, each as [`photo`] reads it; `None` for a row
/// the catalog does not hold. One query.
pub(super) fn photos_at(
    service: &EditorService,
    rows: &[AssetRowId],
) -> Result<Vec<Option<CameraSource>>, Error> {
    let mut found: Vec<Option<CameraSource>> = vec![None; rows.len()];
    if rows.is_empty() {
        return Ok(found);
    }
    let ids = serde_json::to_string(&rows.iter().map(|row| row.0).collect::<Vec<_>>())
        .map_err(|error| Error::internal(format!("asset rows: {error}")))?;
    // The listed rows drive the join, one lookup of each by its key, never a scan of the catalog.
    let mut statement = service.connection.prepare_cached(&format!(
        "SELECT j.key, {PHOTO_COLUMNS} FROM json_each(?1) j
             CROSS JOIN assets a ON a.row_id = j.value
             JOIN asset_state s ON s.asset_id = a.id"
    ))?;
    for listed in statement.query_map([ids], source)? {
        let (at, photo) = listed??;
        if let Some(slot) = usize::try_from(at).ok().and_then(|at| found.get_mut(at)) {
            *slot = Some(photo);
        }
    }
    Ok(found)
}

/// The columns [`source`] reads after the query's first, from the asset `a` and its state `s`.
const PHOTO_COLUMNS: &str = "a.row_id, a.id, a.locator, a.source_folder, a.file_name,
        a.volume_id, a.source_kind, a.file_identity, a.byte_len, s.current_entry_id";

/// One photograph from a row of `SELECT <position>, PHOTO_COLUMNS`: its position and itself.
fn source(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<(i64, CameraSource), Error>> {
    let at: i64 = row.get(0)?;
    let (row_id, id, locator, folder, name): (i64, String, String, String, String) = (
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    );
    let (volume, kind, identity, len, current): (String, String, String, i64, String) = (
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    );
    let photo = || -> Result<CameraSource, Error> {
        Ok(CameraSource {
            row: AssetRowId(row_id),
            asset_id: AssetId::parse(id)?,
            entry_id: EntryId::parse(current)?,
            locator: PathBuf::from(locator),
            folder: PathBuf::from(folder),
            name,
            volume_id: VolumeId::parse(volume)?,
            kind: match kind.as_str() {
                "raw" => SourceTag::Raw,
                _ => SourceTag::Jpeg,
            },
            file_identity: identity,
            byte_len: u64::try_from(len)
                .map_err(|_| Error::catalog("an asset records a negative length"))?,
        })
    };
    Ok(photo().map(|photo| (at, photo)))
}

/// `preview.read` of a photograph: its tier of the entry named, or of its current entry, when that
/// is rendered at this generation and its file is intact; otherwise the render that makes it, with
/// the best row cached meanwhile as the fallback — an earlier entry's render, or its camera
/// preview, which is asked for when the photograph has no row of the tier at all.
pub(super) fn read_photo(
    owner: &mut Owner,
    call: &Call<'_>,
    asset_id: &AssetId,
    entry_id: Option<EntryId>,
    tier: PreviewTier,
    priority: PreviewPriority,
) -> Result<Value, Error> {
    if tier == PreviewTier::Loupe {
        return Err(Error::validation(
            "a developed photograph has grid and large tiers; the loupe tier is a file's",
        ));
    }
    let source = photo(&owner.service, asset_id)?;
    let entry = entry_id.unwrap_or_else(|| source.entry_id.clone());
    owner.catalog.previews.renders.used();
    let (fallback, camera) = {
        let index = owner.service.index()?;
        let rows = previews::photo_rows(index.connection(), asset_id)?;
        if let Some(ready) = rows
            .iter()
            .find(|row| row.is_current(asset_id, &entry, tier) && previews::intact(&row.path))
        {
            if tier == PreviewTier::Large {
                previews::touch_photo(index.connection(), asset_id, &entry, now_ms())?;
            }
            return answer(&PreviewAnswer::Ready {
                preview: ready.info(asset_id),
            });
        }
        let fallback = previews::fallbacks(&rows, asset_id, &entry, tier)
            .into_iter()
            .find(|row| previews::intact(&row.path))
            .map(|row| row.info(asset_id));
        (fallback, !rows.iter().any(|row| row.tier == tier))
    };
    let job_id = want(owner, Some(call), &source, &entry, tier, priority)?;
    if camera {
        want_camera(owner, source, tier, priority, call.client);
    }
    dispatch(owner);
    answer(&PreviewAnswer::Queued { job_id, fallback })
}

/// Queue `tier` of `photo` at `entry`, or join the render that makes it, and answer the job that
/// waits for the tier: for a request (`call`), the job the requests share, opened by the first and
/// joined by every later one; for a commit's re-render (no `call`), its own background job, which
/// belongs to no client. A new render is planned once now, which refuses a stack no render could
/// make (an entry not of the photograph, an effect without its provider) before anything is queued.
/// `resource-limit` when a new render would pass the queue's bound.
fn want(
    owner: &mut Owner,
    call: Option<&Call<'_>>,
    photo: &CameraSource,
    entry: &EntryId,
    tier: PreviewTier,
    priority: PreviewPriority,
) -> Result<JobId, Error> {
    let key = (photo.asset_id.clone(), entry.clone());
    let known = owner.catalog.previews.renders.photos.get(&key).copied();
    let id = match known {
        Some(id) => id,
        None => {
            if !owner.catalog.previews.renders.queue.fits(1) {
                return Err(Error::resource_limit(format!(
                    "at most {} renders wait for the render worker",
                    owner.catalog.previews.renders.queue.capacity()
                )));
            }
            rendered::plan_render(&owner.service, &photo.asset_id, Some(entry), &[tier])?;
            ensure_worker(owner)?;
            let renders = &mut owner.catalog.previews.renders;
            renders.next += 1;
            let id = renders.next;
            renders.tasks.insert(
                id,
                RenderTask {
                    asset_id: photo.asset_id.clone(),
                    entry_id: entry.clone(),
                    row: photo.row,
                    name: photo.name.clone(),
                    priority,
                    control: JobControl::new(),
                    tiers: BTreeMap::new(),
                    making: None,
                    waiters: BTreeSet::new(),
                    forgotten: false,
                },
            );
            renders.photos.insert(key, id);
            id
        }
    };
    let renders = &mut owner.catalog.previews.renders;
    let task = renders.tasks.get_mut(&id).expect("the render is held");
    task.priority = task.priority.max(priority);
    if let Some(call) = call {
        task.waiters.insert(call.client);
    }
    let making = task
        .making
        .as_ref()
        .is_some_and(|making| making.contains(&tier));
    let wanted = task.tiers.entry(tier).or_default();
    let slot = match call {
        Some(_) => &mut wanted.request,
        None => &mut wanted.background,
    };
    let job_id = match slot.as_ref() {
        Some((job_id, _)) => {
            if let Some(call) = call {
                let joined = owner.jobs.join_catalog(job_id, call.client);
                debug_assert!(joined, "a tier's request job is live and shared");
            }
            job_id.clone()
        }
        None => {
            let job_id = JobId::new();
            let control = JobControl::new();
            let opened = CatalogOpened {
                job_id: job_id.clone(),
                kind: JobKind::PreviewRender,
                asset_id: Some(photo.asset_id.clone()),
                origin: call.map(|call| call.origin.clone()),
                control: control.clone(),
            };
            match call {
                Some(call) => owner.jobs.open_catalog_shared(opened, Some(call.client)),
                None => owner.jobs.open_catalog(opened),
            }
            if making {
                owner.jobs.start(&job_id);
            }
            *slot = Some((job_id.clone(), control));
            renders.jobs.insert(job_id.clone(), (id, tier));
            job_id
        }
    };
    if renders.running != Some(id) {
        // Joins the waiting render, raising it when this request asks for more.
        renders.queue.push(id, priority)?;
    }
    Ok(job_id)
}

/// Start the render worker with its own connection to the index, and the discard of other
/// generations with another, on the first render.
fn ensure_worker(owner: &mut Owner) -> Result<(), Error> {
    if owner.catalog.previews.renders.worker.is_some() {
        return Ok(());
    }
    let (store, discard) = {
        let index = owner.service.index()?;
        let dir = index.previews_dir();
        (
            Store::new(index.connect()?, dir.clone()),
            Store::new(index.connect()?, dir),
        )
    };
    let poster = owner.catalog.previews.poster.clone();
    let post: RenderPost = Arc::new(move |done| {
        poster.post(CatalogMessage::Previews(PreviewsMessage::Render(done)));
    });
    owner.catalog.previews.renders.worker = Some(RenderWorker::start(store, discard, post)?);
    Ok(())
}

/// The photograph's current entry now, from its state row.
fn current_entry(catalog: &Connection, asset_id: &AssetId) -> Result<EntryId, Error> {
    let current: String = catalog
        .prepare_cached("SELECT current_entry_id FROM asset_state WHERE asset_id = ?1")?
        .query_row([asset_id.as_str()], |row| row.get(0))
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))?;
    EntryId::parse(current)
}

/// Hand the highest-priority render to the render worker when it is idle: planned now with every
/// tier wanted of it, and published on the activity board as "Rendering previews", naming the file,
/// with its job. A commit's re-render whose entry is no longer current is superseded instead, and a
/// render that can no longer be planned fails its jobs.
pub(super) fn dispatch(owner: &mut Owner) {
    loop {
        let renders = &mut owner.catalog.previews.renders;
        if renders.running.is_some() || renders.worker.is_none() {
            return;
        }
        let Some((id, _)) = renders.queue.pop() else {
            return;
        };
        let Some(task) = renders.tasks.get(&id) else {
            continue;
        };
        let (asset_id, entry_id, requested) = (
            task.asset_id.clone(),
            task.entry_id.clone(),
            task.requested(),
        );
        let tiers: Vec<PreviewTier> = task.tiers.keys().copied().collect();
        let planned = current_entry(&owner.service.connection, &asset_id).and_then(|current| {
            if !requested && current != entry_id {
                return Err(Error::cancelled(SUPERSEDED));
            }
            let request =
                rendered::plan_render(&owner.service, &asset_id, Some(&entry_id), &tiers)?;
            Ok((request, current))
        });
        let (request, current) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                fail(owner, id, error);
                continue;
            }
        };
        let lane = &mut owner.catalog.previews;
        let renders = &mut lane.renders;
        let task = renders.tasks.get_mut(&id).expect("the render is held");
        for (job_id, _) in task.jobs() {
            owner.jobs.start(job_id);
        }
        if let Some((job_id, control)) = task.jobs().next() {
            control.begin_activity(lane.board.begin(ActivitySpec {
                kind: PREVIEW_RENDER.activity,
                label: PREVIEW_RENDER.label,
                detail: Some(task.name.clone()),
                asset_id: Some(asset_id.clone()),
                job_id: Some(job_id.to_string()),
            }));
        }
        task.making = Some(tiers.clone());
        let work = RenderWork {
            id,
            request,
            current,
            control: task.control.clone(),
            budget: lane.budget,
            #[cfg(test)]
            hold: renders.hold.clone(),
        };
        let Some(worker) = renders.worker.as_ref() else {
            return;
        };
        match worker.send(work) {
            Ok(()) => {
                renders.running = Some(id);
                #[cfg(test)]
                renders.dispatched.push((asset_id, entry_id, tiers));
                return;
            }
            Err(error) => {
                // The worker has gone: the render fails, and the next request starts another.
                renders.worker = None;
                fail(owner, id, error);
                return;
            }
        }
    }
}

/// A render that cannot run ends: every job of it records `error` — a `cancelled` one with its
/// reason — and whoever asked is woken.
fn fail(owner: &mut Owner, id: u64, error: Error) {
    let lane = &mut owner.catalog.previews;
    let Some(task) = lane.renders.remove(id) else {
        return;
    };
    for (job_id, _) in task.jobs() {
        lane.renders.jobs.remove(job_id);
        owner.jobs.finish(job_id, Err(error.clone()));
    }
    lane.wake(task.waiters.iter().copied());
}

/// The render worker finished a render: each tier's job records its preview, or the render's
/// error; a rendered tier releases the camera preview asked for in its place; whoever asked is
/// woken; a tier asked for while it ran, or a render stopped while something still wanted it,
/// runs again; and the next render is handed out.
pub(super) fn finished(owner: &mut Owner, done: RenderDone) {
    let RenderDone { id, result } = done;
    let lane = &mut owner.catalog.previews;
    if lane.renders.running != Some(id) {
        return;
    }
    lane.renders.running = None;
    let Some(mut task) = lane.renders.tasks.remove(&id) else {
        dispatch(owner);
        return;
    };
    let making = task.making.take().unwrap_or_default();
    let stopped = matches!(&result, Err(error) if error.kind == ErrorKind::Cancelled);
    let mut ended: Vec<(JobId, Result<Output, Error>)> = Vec::new();
    match &result {
        Ok(written) => {
            for info in written {
                let Some(tier) = task.tiers.remove(&info.tier) else {
                    continue;
                };
                for (job_id, _) in tier.into_jobs() {
                    let output = serde_json::to_value(info)
                        .map(Output::Value)
                        .map_err(|error| Error::internal(format!("preview result: {error}")));
                    ended.push((job_id, output));
                }
            }
        }
        // Stopped while a tier of it was still wanted — a cancel that raced a new request — it
        // runs again; otherwise every tier it was making records the error.
        Err(_) if stopped && !task.tiers.is_empty() => {}
        Err(error) => {
            for tier in &making {
                if let Some(tier) = task.tiers.remove(tier) {
                    ended.extend(
                        tier.into_jobs()
                            .map(|(job_id, _)| (job_id, Err(error.clone()))),
                    );
                }
            }
        }
    }
    let key = (task.asset_id.clone(), task.entry_id.clone());
    let (row, forgotten) = (task.row, task.forgotten);
    let waiters: Vec<ClientId> = task.waiters.iter().copied().collect();
    if !task.tiers.is_empty() {
        // What is still wanted runs again, with a flag no cancel has set.
        task.control = JobControl::new();
        match lane.renders.queue.push(id, task.priority) {
            Ok(_) => {
                lane.renders.tasks.insert(id, task);
            }
            Err(error) => {
                ended.extend(
                    task.tiers
                        .into_values()
                        .flat_map(Tier::into_jobs)
                        .map(|(job_id, _)| (job_id, Err(error.clone()))),
                );
                lane.renders.photos.remove(&key);
            }
        }
    } else {
        lane.renders.photos.remove(&key);
    }
    for (job_id, _) in &ended {
        lane.renders.jobs.remove(job_id);
    }
    // A forgotten photograph's row may already be another photograph's, whose camera preview is
    // still its own.
    if let (Ok(written), false) = (&result, forgotten) {
        for info in written {
            lane.release_camera((ViewItem::Photo(row), info.tier));
        }
    }
    lane.wake(waiters.into_iter());
    for (job_id, output) in ended {
        owner.jobs.finish(&job_id, output);
    }
    dispatch(owner);
}

/// The changes the owner just recorded: each photograph they name whose current entry moved away
/// from its rendered grid tier has that tier rendered again in the background, so views stay
/// current, superseding a re-render of an earlier commit that has not started. Nothing happens
/// until a photograph's preview has been asked for in this process, and an error — an asset that
/// is gone, an index that cannot be opened — leaves the next read to render it.
pub(in crate::api::owner) fn follow_changes(owner: &mut Owner, announced: &[Origin]) {
    if !owner.catalog.previews.renders.used {
        return;
    }
    let mut seen = BTreeSet::new();
    let mut queued = false;
    for asset_id in announced
        .iter()
        .filter_map(|origin| origin.asset_id.as_ref())
    {
        if seen.insert(asset_id) {
            queued |= follow(owner, asset_id).unwrap_or(false);
        }
    }
    if queued {
        dispatch(owner);
    }
}

/// Follow one photograph's change: whether a re-render was queued.
fn follow(owner: &mut Owner, asset_id: &AssetId) -> Result<bool, Error> {
    let photo = photo(&owner.service, asset_id)?;
    let rows = {
        let index = owner.service.index()?;
        previews::photo_rows(index.connection(), asset_id)?
    };
    let had_grid = rows
        .iter()
        .any(|row| row.tier == PreviewTier::Grid && row.rendered());
    if !had_grid
        || rows
            .iter()
            .any(|row| row.is_current(asset_id, &photo.entry_id, PreviewTier::Grid))
    {
        return Ok(false);
    }
    // An earlier commit's re-render that has not started is superseded by this one.
    let superseded: Vec<u64> = owner
        .catalog
        .previews
        .renders
        .tasks
        .iter()
        .filter(|(id, task)| {
            task.asset_id == *asset_id
                && task.entry_id != photo.entry_id
                && !task.requested()
                && owner.catalog.previews.renders.running != Some(**id)
        })
        .map(|(id, _)| *id)
        .collect();
    for id in superseded {
        fail(owner, id, Error::cancelled(SUPERSEDED));
    }
    want(
        owner,
        None,
        &photo,
        &photo.entry_id,
        PreviewTier::Grid,
        PreviewPriority::Background,
    )?;
    Ok(true)
}

#[cfg(test)]
#[path = "renders_tests.rs"]
mod preview_rendered_owner;
