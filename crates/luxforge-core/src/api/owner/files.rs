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
//!   [`indexed_folders_changed`]: a folder now indexed is listed, and a folder no longer indexed has
//!   its listing stopped and its rows forgotten. The watchers of TASK-005 start and stop there too.
//!
//! One file per family of methods beside this one: `folders.rs` (`index.add-folder`,
//! `index.remove-folder`, `index.folders`, `index.refresh`) and `volumes.rs` (`volume.list`,
//! `card.list`, `disk.folders`).
mod folders;
#[cfg(test)]
mod tests;
mod volumes;

pub(in crate::api) use folders::{
    index_add_folder, index_folders, index_refresh, index_remove_folder,
};
pub(in crate::api) use volumes::{card_list, disk_folders, volume_list};

use super::{Call, ClientId, Owner, catalog::Poster};
use crate::{
    Error, JobId,
    activity::{ActivityBoard, ActivitySpec},
    api::{Origin, announce_once},
    catalog_types::{IndexSource, JobStarted, RootKind, jobs::INDEX_REFRESH},
    editor::{default_artifact_root, folder_rows, now_ms},
    index::{
        exclude::Exclusions,
        lane::{Lane, LaneConfig, LaneEvent, Refresh, RootPlan, Work},
        volumes::MountSource,
        walk::WalkLimits,
    },
    jobs::{CatalogOpened, JobControl, JobKind, Output},
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Work that may wait for the index lane behind the running one; past it `index.refresh` is
/// refused with `resource-limit`.
pub(crate) const MAX_WAITING: usize = 16;

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
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
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
    /// Stand in for the platform's mount table.
    #[cfg(test)]
    Mounts(MountSource),
    /// Hold every listing at each folder while the gate is shut, from the lane's next start.
    #[cfg(test)]
    Hold(Arc<luxforge_testbase::Gate>),
    /// Bound listings by these limits, from the lane's next start.
    #[cfg(test)]
    Limits(WalkLimits),
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
            #[cfg(test)]
            hold: None,
        }
    }

    /// A client left: the lane's jobs belong to no client, so nothing changes.
    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// `job.cancel` cancelled one of this lane's jobs in the job table: a waiting one leaves the
    /// queue; a running one stops at its next checkpoint through its control, which the table set.
    pub(super) fn cancelled(&mut self, job_id: &JobId) {
        self.waiting.retain(
            |queued| !matches!(&queued.task, Task::Refresh { job_id: id, .. } if id == job_id),
        );
    }

    /// Stop the lane as the owner stops: the running listing is cancelled, stopping at its next
    /// checkpoint with what it committed, and the threads are joined. The owner's channel is closed
    /// by then, so nothing the lane posts can block it.
    pub(super) fn shutdown(self) {
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

/// Luxforge's own directories for the catalog `owner` serves: its index and artifact
/// directories, canonical where they exist.
fn exclusions(owner: &Owner) -> Exclusions {
    let service = &owner.service;
    let mut own = vec![service.index_dir().to_path_buf()];
    if let Some(catalog) = service.connection.path() {
        own.push(default_artifact_root(Path::new(catalog)));
    }
    Exclusions::new(
        own.into_iter()
            .map(|dir| dir.canonicalize().unwrap_or(dir))
            .collect(),
    )
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

/// Hand the lane its next piece of work when it is idle, starting it on first use.
fn dispatch(owner: &mut Owner) {
    while owner.catalog.files.running.is_none() {
        let Some(Queued { task, origin }) = owner.catalog.files.waiting.pop_front() else {
            return;
        };
        if let Err(error) = ensure_lane(owner) {
            if let Task::Refresh { job_id, .. } = task {
                owner.jobs.finish(&job_id, Err(error));
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
                }
            }
        }
    }
}

/// Start the lane's threads, once: the owner opens the index first (creating it, or recreating one
/// it cannot use), then the lane opens its own connection to it.
fn ensure_lane(owner: &mut Owner) -> Result<(), Error> {
    if owner.catalog.files.lane.is_some() {
        return Ok(());
    }
    let index_dir = owner.service.index()?.dir().to_path_buf();
    let exclusions = exclusions(owner);
    let files = &mut owner.catalog.files;
    let lane = Lane::start(LaneConfig {
        index_dir,
        exclusions,
        mounts: files.mounts.clone(),
        limits: files.limits,
        post: files.post(),
        #[cfg(test)]
        hold: files.hold.clone(),
    })?;
    files.lane = Some(lane);
    Ok(())
}

/// A committed library change changed whether `folders` are indexed (`indexed_folders` holds the
/// answer now), under `origin`: a folder now indexed is listed; a folder no longer indexed has its
/// listing stopped and its rows forgotten, unless another root lists them. TASK-005's watchers
/// start and stop here too.
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
                    owner.catalog.files.cancelled(&job_id);
                }
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
        FilesMessage::Lane(LaneEvent::Committed { revision }) => {
            let origin = owner.catalog.files.running.as_ref().map_or_else(
                || Origin::new(INDEX_REFRESH.job_kind, ""),
                |running| running.origin.clone(),
            );
            announce_once(&mut owner.announced, &origin.index(revision));
            owner.record_announced();
        }
        FilesMessage::Lane(LaneEvent::Refreshed { job_id, result }) => {
            owner.jobs.finish(
                &job_id,
                result.and_then(|report| {
                    serde_json::to_value(report)
                        .map(Output::Value)
                        .map_err(|error| Error::internal(error.to_string()))
                }),
            );
            owner.catalog.files.running = None;
            dispatch(owner);
        }
        FilesMessage::Lane(LaneEvent::Forgotten) => {
            owner.catalog.files.running = None;
            dispatch(owner);
        }
        #[cfg(test)]
        FilesMessage::Mounts(mounts) => owner.catalog.files.mounts = mounts,
        #[cfg(test)]
        FilesMessage::Hold(gate) => owner.catalog.files.hold = Some(gate),
        #[cfg(test)]
        FilesMessage::Limits(limits) => owner.catalog.files.limits = limits,
    }
}

/// The mount table now, as this owner reads it.
fn mount_table(owner: &Owner) -> crate::index::volumes::MountTable {
    owner.catalog.files.mounts.read(now_ms())
}

/// The indexed folders, in path order.
fn indexed_folders(owner: &Owner) -> Result<Vec<crate::catalog_types::IndexedFolder>, Error> {
    folder_rows::indexed_folders(&owner.service.connection)
}
