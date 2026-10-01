//! **Lane A (files)** on the owner: the index lane's schedule and what it posts back, and the
//! handlers of `index.add-folder`, `index.remove-folder`, `index.folders`, `index.refresh`,
//! `card.list`, `volume.list` and `disk.folders` (`crate::catalog_types::api`). The index lane
//! itself is `crate::index::lane`: its threads walk, reconcile, read headers and write the index;
//! the owner only keeps its queue, opens and finishes its jobs, and records one event per batch it
//! committed.
//!
//! - **One job at a time.** `index.refresh` jobs (and forgetting a removed folder's rows) wait here,
//!   at most [`MAX_WAITING`], and the lane runs one at a time. A refresh of a source already
//!   waiting or running answers that job (`deduplicated`).
//! - **Started on first use.** The lane's threads start with its first work; a catalog that never
//!   indexes runs none.
//! - **Indexed folders follow the library.** Every committed library change that touched an indexed
//!   folder — `index.add-folder`, `index.remove-folder`, and the undo or redo of one — reaches
//!   [`indexed_folders_changed`]: a folder now indexed is listed, and watched once listed; a folder
//!   no longer indexed has its listing stopped, its watch stopped and its rows forgotten.
//! - **Kept current.** The lane watches the indexed folders and the volumes (`crate::index::lane`),
//!   from when the catalog opens with indexed folders ([`opened`]) or a client first asks about
//!   the disk. What it applies on its own is announced as work no request made: method
//!   [`UNREQUESTED`] with no request id, one event per batch. A volume mounted or taken out has
//!   the volumes surveyed again, and a card mounted is listed as an `index.refresh` job; so is
//!   every card already mounted as the catalog opens and starts the lane, once the first survey
//!   after its watcher has found it.
//!   The lane's own listings — rescans, and roots listed again — are `index-refresh` jobs no
//!   request started, which the lane announces ([`LaneEvent::Began`]) and the owner records like
//!   its own, so `job.read` and `job.cancel` answer for them.
//! - **One event as a listing ends.** Every `index.refresh` job, however it ends, records one event
//!   naming its request and the index revision it left, so a client learns it ended from the event
//!   log.
//! - **Never on the disk.** Nothing here touches a file system beyond the platform's mount table,
//!   which waits on none: what stats, canonicalizes or lists a path runs on the index lane's query
//!   thread, and the volumes, cards and offline folders are learned by its survey thread, while the
//!   call waits parked on the owner (`queries.rs`). The lane opens the index itself.
//!
//! One file per family of methods beside this one: `folders.rs` (`index.add-folder`,
//! `index.remove-folder`, `index.folders`, `index.refresh`), `volumes.rs` (`volume.list`,
//! `card.list`, `disk.folders`), and `queries.rs`, the calls parked on the lane's threads.
mod folders;
mod queries;
#[cfg(test)]
mod tests;
mod volumes;

pub(in crate::api) use folders::{
    index_add_folder, index_folders, index_refresh, index_remove_folder,
};
pub(super) use queries::{Deferred, defer};
pub(in crate::api) use volumes::{card_list, disk_folders, volume_list};

use super::{Call, ClientId, Owner, catalog::Poster};
use crate::{
    Error, JobId,
    activity::{ActivityBoard, ActivitySpec},
    api::{Origin, announce_once},
    catalog_types::{IndexReport, IndexSource, JobStarted, RootKind, jobs::INDEX_REFRESH},
    editor::folder_rows,
    index::{
        database,
        exclude::OwnDirs,
        lane::{Lane, LaneConfig, LaneEvent, Maker, Refresh, RootPlan, Work},
        volumes::MountSource,
        walk::WalkLimits,
    },
    jobs::{CatalogOpened, JobControl, JobKind, Output},
};
use queries::{Queries, Resume, SurveyPost, Surveys};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Work that may wait for the index lane behind the running one; past it `index.refresh` is
/// refused with `resource-limit`.
pub(crate) const MAX_WAITING: usize = 16;

/// The method a change no request made is announced under: what the index lane applies from the
/// platform's change notifications, and the listings it starts on its own (a card mounted, the
/// indexed folders as the catalog opens). Its request id is empty.
pub(crate) const UNREQUESTED: &str = "index-watch";

/// The origin of work no request asked for.
fn unrequested() -> Origin {
    Origin::new(UNREQUESTED, "")
}

/// Lane A's state on the owner.
pub(super) struct FilesLane {
    poster: Poster,
    board: Arc<ActivityBoard>,
    /// The lane's threads, once started.
    lane: Option<Lane>,
    waiting: VecDeque<Queued>,
    running: Option<Running>,
    /// Where the mount table comes from: the platform's, or a test's fixed one.
    mounts: MountSource,
    limits: WalkLimits,
    /// The query thread and the calls parked on it.
    queries: Queries,
    /// The survey thread, what it learned, and the calls waiting for its first survey.
    surveys: Surveys,
    /// Whether the lane follows each indexed folder's changes, or why not, as it last said.
    watching: HashMap<PathBuf, Result<(), String>>,
    /// Whether the lane's watcher reports volumes mounted and taken out, which keeps the survey
    /// current without a survey per call.
    notified: bool,
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
    /// The path of every header read the lane has taken in.
    #[cfg(test)]
    reads: Arc<std::sync::Mutex<Vec<PathBuf>>>,
}

/// Work waiting for the lane, with the request it was made under.
struct Queued {
    task: Task,
    origin: Origin,
}

/// What the lane is asked to do.
enum Task {
    Refresh {
        job_id: JobId,
        control: Arc<JobControl>,
        source: IndexSource,
        roots: Vec<RootPlan>,
        /// What the activity board says the job is indexing.
        detail: String,
    },
    Forget(PathBuf),
    /// Watch these indexed folders, as the catalog opens.
    Watch(Vec<RootPlan>),
}

/// The work the lane is running: which job, if it is one, with its control, and the request its
/// batches are announced under.
struct Running {
    job: Option<(JobId, IndexSource)>,
    control: Option<Arc<JobControl>>,
    origin: Origin,
}

/// What lane A's workers post back, and what a test hands the lane through the owner's channel.
pub(super) enum FilesMessage {
    Lane(LaneEvent),
    /// The query thread answered the running call's question.
    Answered(Resume),
    /// The survey thread learned something.
    Surveyed(Box<SurveyPost>),
    /// Stand in for the platform's mount table.
    #[cfg(test)]
    Mounts(MountSource),
    /// Hold every question the query thread takes from now on while the gate is shut.
    #[cfg(test)]
    HoldQueries(Arc<luxforge_testbase::Gate>),
    /// How many calls wait behind the query thread's running question.
    #[cfg(test)]
    QueriesWaiting(std::sync::mpsc::SyncSender<usize>),
    /// Hold every listing at each folder while the gate is shut, from the lane's next start.
    #[cfg(test)]
    Hold(Arc<luxforge_testbase::Gate>),
    /// Bound listings by these limits, from the lane's next start.
    #[cfg(test)]
    Limits(WalkLimits),
    /// Hand the lane an event as its watcher would, starting it first.
    #[cfg(test)]
    Inject(crate::index::lane::WatchEvent),
    /// How many header reads of files under this folder the lane has taken in: a test counts
    /// only its own, since the lane also lists any card the host mounts meanwhile.
    #[cfg(test)]
    HeaderReads(PathBuf, std::sync::mpsc::SyncSender<usize>),
}

impl FilesLane {
    pub(super) fn new(poster: Poster, board: Arc<ActivityBoard>) -> Self {
        Self {
            poster,
            board,
            lane: None,
            waiting: VecDeque::new(),
            running: None,
            mounts: MountSource::default(),
            limits: WalkLimits::default(),
            queries: Queries::default(),
            surveys: Surveys::default(),
            watching: HashMap::new(),
            notified: false,
            #[cfg(test)]
            hold: None,
            #[cfg(test)]
            reads: Arc::default(),
        }
    }

    /// A client left: its calls parked on the lane's threads are dropped. The lane's jobs belong
    /// to no client, so they run on.
    pub(super) fn disconnect(&mut self, client: ClientId) {
        self.forget_waiting(client);
    }

    /// Take a waiting job out of the queue: the request it was made under, if it waited.
    fn unqueue(&mut self, job_id: &JobId) -> Option<Origin> {
        let at = self.waiting.iter().position(
            |queued| matches!(&queued.task, Task::Refresh { job_id: id, .. } if id == job_id),
        )?;
        self.waiting.remove(at).map(|queued| queued.origin)
    }

    /// Stop the lane as the owner stops: the running listing is cancelled, stopping at its next
    /// checkpoint with what it committed, and the threads are joined, but for a query or survey
    /// thread still reading a volume, which is not waited for. The owner's channel is closed by
    /// then, so nothing the lane posts can block it.
    pub(super) fn shutdown(mut self) {
        self.stop_threads();
        if let Some(control) = self.running.and_then(|running| running.control) {
            control.cancel("the catalog closed");
        }
        if let Some(lane) = self.lane {
            lane.stop();
        }
    }

    /// The live job listing `source`, waiting or running.
    fn live_job(&self, source: &IndexSource) -> Option<JobId> {
        let running = self
            .running
            .as_ref()
            .and_then(|running| running.job.as_ref())
            .filter(|(_, running)| running == source)
            .map(|(job_id, _)| job_id.clone());
        running.or_else(|| {
            self.waiting.iter().find_map(|queued| match &queued.task {
                Task::Refresh {
                    job_id, source: s, ..
                } if s == source => Some(job_id.clone()),
                _ => None,
            })
        })
    }

    fn post(&self) -> crate::index::lane::Post {
        let poster = self.poster.clone();
        Arc::new(move |event| {
            poster.post(super::catalog::CatalogMessage::Files(FilesMessage::Lane(
                event,
            )));
        })
    }
}

/// Luxforge's own directories for the catalog `owner` serves, as named, touching no file system;
/// the lane's threads resolve them ([`OwnDirs::exclusions`]).
fn own_dirs(owner: &Owner) -> OwnDirs {
    let service = &owner.service;
    OwnDirs {
        index_dir: service.index_dir().to_path_buf(),
        catalog: service.connection.path().map(PathBuf::from),
    }
}

/// Queue a listing of `source`, whose roots are `roots`, as an `index.refresh` job under `origin`,
/// or answer the live job already listing it.
pub(super) fn start_refresh(
    owner: &mut Owner,
    source: IndexSource,
    roots: Vec<RootPlan>,
    detail: String,
    origin: &Origin,
) -> Result<JobStarted, Error> {
    if let Some(job_id) = owner.catalog.files.live_job(&source) {
        let status = owner
            .jobs
            .read(&job_id)
            .map_or(crate::JobStatus::Queued, |record| record.status);
        return Ok(JobStarted {
            job_id,
            status,
            deduplicated: true,
        });
    }
    if owner.catalog.files.waiting.len() >= MAX_WAITING {
        return Err(Error::resource_limit(format!(
            "{MAX_WAITING} listings are already waiting for the index lane"
        )));
    }
    let job_id = JobId::new();
    let control = JobControl::new();
    owner.jobs.open_catalog(CatalogOpened {
        job_id: job_id.clone(),
        kind: JobKind::IndexRefresh,
        asset_id: None,
        origin: Some(origin.clone()),
        control: control.clone(),
    });
    owner.catalog.files.waiting.push_back(Queued {
        task: Task::Refresh {
            job_id: job_id.clone(),
            control,
            source,
            roots,
            detail,
        },
        origin: origin.clone(),
    });
    dispatch(owner);
    let status = owner
        .jobs
        .read(&job_id)
        .map_or(crate::JobStatus::Queued, |record| record.status);
    Ok(JobStarted {
        job_id,
        status,
        deduplicated: false,
    })
}

/// `job.cancel` cancelled one of this lane's jobs in the job table: a waiting one leaves the queue
/// and its end is announced now; a running one stops at its next checkpoint through its control,
/// which the table set, and its end is announced when the lane reports it.
pub(super) fn cancelled(owner: &mut Owner, job_id: &JobId) {
    if let Some(origin) = owner.catalog.files.unqueue(job_id) {
        ended(owner, origin, job_id, None);
    }
}

/// An `index-refresh` job's report as its record keeps it.
fn output(result: Result<IndexReport, Error>) -> Result<Output, Error> {
    result.and_then(|report| {
        serde_json::to_value(report)
            .map(Output::Value)
            .map_err(|error| Error::internal(error.to_string()))
    })
}

/// Announce that an `index.refresh` job made under `origin` ended: one event naming its request, the
/// job and the index revision it left, `revision` as the lane read it, else the index's revision now.
fn ended(owner: &mut Owner, origin: Origin, job_id: &JobId, revision: Option<u64>) {
    let revision = revision.or_else(|| {
        queries::index(owner)
            .ok()
            .flatten()
            .and_then(|index| database::revision(index.connection()).ok())
    });
    let origin = origin.job(job_id.clone());
    let event = match revision {
        Some(revision) => origin.index(revision),
        None => origin,
    };
    announce_once(&mut owner.announced, &event);
}

/// Watch every indexed folder as the catalog opens, starting the lane: what changed while Luxforge
/// was closed is caught up (replayed on macOS, listed elsewhere), the volumes' notifications keep
/// what `volume.list` answers current, and the cards mounted already are listed once its watcher
/// runs, as cards mounted then would be ([`queries::lane_started`]). A catalog without indexed
/// folders starts nothing.
pub(super) fn opened(owner: &mut Owner) {
    #[cfg(test)]
    opening_mounts(owner);
    let Ok(folders) = indexed_folders(owner) else {
        return;
    };
    if folders.is_empty() {
        return;
    }
    let roots = folders
        .into_iter()
        .map(|folder| RootPlan {
            path: folder.path,
            kind: RootKind::Indexed,
            volume_id: Some(folder.volume_id),
        })
        .collect();
    owner.catalog.files.surveys.opening = true;
    owner.catalog.files.waiting.push_back(Queued {
        task: Task::Watch(roots),
        origin: unrequested(),
    });
    dispatch(owner);
}

/// Mount tables tests stand in with from a catalog's opening, by the catalog's path: the lane of a
/// catalog with indexed folders starts as it opens, before a test's message can reach the owner.
#[cfg(test)]
pub(super) static OPENING_MOUNTS: std::sync::Mutex<Vec<(PathBuf, MountSource)>> =
    std::sync::Mutex::new(Vec::new());

/// Stand in with the mount table a test gave for this catalog's opening, if it gave one.
#[cfg(test)]
fn opening_mounts(owner: &mut Owner) {
    let Some(catalog) = owner.service.connection.path().map(PathBuf::from) else {
        return;
    };
    let mounts = OPENING_MOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(path, _)| *path == catalog)
        .map(|(_, mounts)| mounts.clone());
    if let Some(mounts) = mounts {
        owner.catalog.files.mounts = mounts;
        owner.catalog.files.new_mount_source();
    }
}

/// Hand the lane its next piece of work when it is idle, starting it on first use.
fn dispatch(owner: &mut Owner) {
    while owner.catalog.files.running.is_none() {
        let Some(Queued { task, origin }) = owner.catalog.files.waiting.pop_front() else {
            return;
        };
        if let Err(error) = ensure_lane(owner) {
            if let Task::Refresh { job_id, .. } = task {
                owner.jobs.finish(&job_id, Err(error));
                ended(owner, origin, &job_id, None);
            }
            continue;
        }
        let files = &mut owner.catalog.files;
        let lane = files.lane.as_ref().expect("the lane was started above");
        let (work, job, control) = match task {
            Task::Refresh {
                job_id,
                control,
                source,
                roots,
                detail,
            } => {
                owner.jobs.start(&job_id);
                control.begin_activity(files.board.begin(ActivitySpec {
                    kind: INDEX_REFRESH.activity,
                    label: INDEX_REFRESH.label,
                    detail: Some(detail),
                    asset_id: None,
                    job_id: Some(job_id.to_string()),
                }));
                (
                    Work::Refresh(Refresh {
                        job_id: job_id.clone(),
                        control: control.clone(),
                        roots,
                        strict: !matches!(source, IndexSource::AllIndexed),
                    }),
                    Some((job_id, source)),
                    Some(control),
                )
            }
            Task::Forget(path) => (Work::Forget(path), None, None),
            Task::Watch(roots) => (Work::Watch(roots), None, None),
        };
        match lane.send(work) {
            Ok(()) => {
                files.running = Some(Running {
                    job,
                    control,
                    origin,
                });
            }
            Err(error) => {
                if let Some((job_id, _)) = job {
                    owner.jobs.finish(&job_id, Err(error));
                    ended(owner, origin, &job_id, None);
                }
            }
        }
    }
}

/// Start the lane's threads, once, with its watcher. The lane opens the index itself on its first
/// work (creating it, or recreating one it cannot use) and hands the owner a connection
/// ([`LaneEvent::Opened`]), so nothing here touches the disk.
fn ensure_lane(owner: &mut Owner) -> Result<(), Error> {
    if owner.catalog.files.lane.is_some() {
        return Ok(());
    }
    let own = own_dirs(owner);
    let (index_dir, catalog_id) = (
        owner.service.index_dir().to_path_buf(),
        owner.service.catalog_id().to_owned(),
    );
    let files = &mut owner.catalog.files;
    let lane = Lane::start(LaneConfig {
        index_dir,
        catalog_id,
        own,
        mounts: files.mounts.clone(),
        limits: files.limits,
        post: files.post(),
        board: files.board.clone(),
        #[cfg(test)]
        hold: files.hold.clone(),
        #[cfg(test)]
        reads: files.reads.clone(),
    })?;
    files.lane = Some(lane);
    Ok(())
}

/// A committed library change changed whether `folders` are indexed (`indexed_folders` holds the
/// answer now), under `origin`: a folder now indexed is listed, and watched once its listing ends;
/// a folder no longer indexed has its listing and its watch stopped and its rows forgotten, unless
/// another root lists them.
pub(super) fn indexed_folders_changed(owner: &mut Owner, origin: &Origin, folders: &[PathBuf]) {
    for path in folders {
        let source = IndexSource::IndexedFolder { path: path.clone() };
        match crate::editor::library_rows::indexed_folder(&owner.service.connection, path) {
            Ok(Some(folder)) => {
                // A full queue leaves the folder unlisted until its next refresh.
                let _ = start_refresh(
                    owner,
                    source,
                    vec![RootPlan {
                        path: folder.path.clone(),
                        kind: RootKind::Indexed,
                        volume_id: Some(folder.volume_id),
                    }],
                    folder.path.display().to_string(),
                    origin,
                );
            }
            Ok(None) => {
                if let Some(job_id) = owner.catalog.files.live_job(&source) {
                    owner
                        .jobs
                        .cancel(&job_id, "the folder is no longer indexed");
                    cancelled(owner, &job_id);
                }
                owner.catalog.files.watching.remove(path);
                owner.catalog.files.waiting.push_back(Queued {
                    task: Task::Forget(path.clone()),
                    origin: origin.clone(),
                });
                dispatch(owner);
            }
            Err(_) => {}
        }
    }
}

/// One message from the lane (or a test).
pub(super) fn handle(owner: &mut Owner, message: FilesMessage) {
    match message {
        FilesMessage::Lane(LaneEvent::Committed { revision, by }) => {
            // A batch of the owner's work is announced under the request that asked for it; one
            // the watcher's notifications made, or a listing of the lane's own, as work no request
            // made. A listing's batch names its job.
            let running = owner.catalog.files.running.as_ref();
            let origin = match by {
                Maker::Job(job) => running
                    .filter(|running| running.job.as_ref().map(|(id, _)| id) == Some(&job))
                    .map_or_else(unrequested, |running| running.origin.clone())
                    .job(job),
                Maker::Owner => running.map_or_else(unrequested, |running| running.origin.clone()),
                Maker::Watcher => unrequested(),
            };
            announce_once(&mut owner.announced, &origin.index(revision));
            owner.record_announced();
        }
        FilesMessage::Lane(LaneEvent::Began { job_id, control }) => {
            // Recorded running before the lane shows it on the board, which it does only after
            // posting this, so whoever saw it there finds it here.
            owner.jobs.open_catalog(CatalogOpened {
                job_id: job_id.clone(),
                kind: JobKind::IndexRefresh,
                asset_id: None,
                origin: Some(unrequested()),
                control,
            });
            owner.jobs.start(&job_id);
        }
        FilesMessage::Lane(LaneEvent::Listed {
            job_id,
            result,
            revision,
        }) => {
            owner.jobs.finish(&job_id, output(result));
            ended(owner, unrequested(), &job_id, revision);
            owner.record_announced();
        }
        FilesMessage::Lane(LaneEvent::Refreshed {
            job_id,
            result,
            revision,
        }) => {
            owner.jobs.finish(&job_id, output(result));
            let origin = owner
                .catalog
                .files
                .running
                .take()
                .map_or_else(unrequested, |running| running.origin);
            ended(owner, origin, &job_id, revision);
            owner.record_announced();
            dispatch(owner);
        }
        FilesMessage::Lane(LaneEvent::Forgotten | LaneEvent::Watched) => {
            owner.catalog.files.running = None;
            dispatch(owner);
        }
        FilesMessage::Lane(LaneEvent::Opened(index)) => owner.service.adopt_index(index),
        FilesMessage::Lane(LaneEvent::Started { watcher }) => {
            owner.catalog.files.notified = watcher.is_ok();
            queries::lane_started(owner);
        }
        FilesMessage::Lane(LaneEvent::Watching { path, watching }) => {
            owner.catalog.files.watching.insert(path, watching);
        }
        FilesMessage::Lane(LaneEvent::Volume(event)) => queries::volume(owner, event),
        FilesMessage::Answered(resume) => queries::answered(owner, resume),
        FilesMessage::Surveyed(post) => queries::surveyed(owner, *post),
        #[cfg(test)]
        FilesMessage::Mounts(mounts) => {
            owner.catalog.files.mounts = mounts;
            owner.catalog.files.new_mount_source();
        }
        #[cfg(test)]
        FilesMessage::HoldQueries(gate) => owner.catalog.files.queries.hold = Some(gate),
        #[cfg(test)]
        FilesMessage::QueriesWaiting(reply) => {
            let _ = reply.send(owner.catalog.files.queries_waiting());
        }
        #[cfg(test)]
        FilesMessage::Hold(gate) => owner.catalog.files.hold = Some(gate),
        #[cfg(test)]
        FilesMessage::Limits(limits) => owner.catalog.files.limits = limits,
        #[cfg(test)]
        FilesMessage::HeaderReads(under, reply) => {
            let reads = owner
                .catalog
                .files
                .reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = reply.send(reads.iter().filter(|path| path.starts_with(&under)).count());
        }
        #[cfg(test)]
        FilesMessage::Inject(event) => {
            if ensure_lane(owner).is_ok()
                && let Some(lane) = &owner.catalog.files.lane
            {
                lane.inject(event);
            }
        }
    }
}

/// Whether the lane follows the changes of the indexed folder at `path`, and why not when it does
/// not, for `index.folders`.
fn watching(owner: &Owner, path: &Path) -> (bool, Option<String>) {
    match owner.catalog.files.watching.get(path) {
        Some(Ok(())) => (true, None),
        Some(Err(reason)) => (false, Some(reason.clone())),
        None => (false, Some("not watched yet".into())),
    }
}

/// Whether `path` is absolute, which a path a client names must be: checked on the owner, before
/// anything reads the disk.
fn absolute(path: &Path) -> Result<(), Error> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(Error::validation(format!(
            "{} is not an absolute path",
            path.display()
        )))
    }
}

/// The indexed folders, in path order.
fn indexed_folders(owner: &Owner) -> Result<Vec<crate::catalog_types::IndexedFolder>, Error> {
    folder_rows::indexed_folders(&owner.service.connection)
}
