//! **Lane C (catalog)** on the owner: the develop lane's handle (its worker, started on the first
//! Develop), the source search and batch jobs, what they post back (a committed batch, a finished
//! file, progress), and the handlers of `pick.*`, `folder.*`, `asset.move`, `asset.send-back`,
//! `asset.remove`, `asset.restore`, `collection.*`, `library.*`, `catalog.empty-removed`,
//! `source.check`, `source.missing`, `source.find`, `source.locate`, `source.relink`, `batch.*` and
//! `catalog.info` (`crate::catalog_types::api`). The library itself is `crate::library`.
//!
//! One file per family of methods beside this one: `picks.rs` (`pick.*`), `journal.rs`
//! (`library.*`) and `info.rs` (`catalog.info`). Every method that changes the library records it
//! through [`change`], which runs the journal in one catalog transaction and announces the change
//! it recorded as one event.
#![allow(dead_code, reason = "catalog contracts: lane C fills this as it lands")]

mod info;
mod journal;
#[cfg(test)]
mod journal_tests;
mod picks;

// Catalog folders and collections (TASK-012): `folder.*`, `asset.move`, `collection.*`.
#[cfg(test)]
mod catalog_folder_tests;
pub(in crate::api) mod organize;

// Availability and Locate (TASK-016): `source.check`, `source.locate`.
#[cfg(test)]
mod locate_tests;
pub(in crate::api) mod sources;

pub(in crate::api) use info::catalog_info;
pub(in crate::api) use journal::{library_inspect, library_journal, library_redo, library_undo};
pub(in crate::api) use picks::{pick_list, pick_set};

use super::{Call, ClientId, Owner, catalog::Poster};
use crate::{
    Error, JobId,
    activity::ActivityBoard,
    api::announce_once,
    catalog_types::{LibraryAnswer, ViewItem},
    library::journal::{self as library_journal, Outcome, Request},
};
use rusqlite::Transaction;
use std::sync::Arc;

// The lane's worker (TASK-016).
use super::catalog::CatalogMessage;
use crate::{
    AssetId, JobStatus,
    activity::ActivitySpec,
    api::Origin,
    catalog_types::{JobStarted, jobs::CatalogJob},
    jobs::{CatalogOpened, JobControl, Jobs, LANE_QUEUE, Output},
    library::{locate::Phase, worker},
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    panic::{self, AssertUnwindSafe},
    sync::mpsc::SyncSender,
};

/// What the lane's worker runs for one job, off the owner: everything the job needs, moved to the
/// worker, given the job's control (its cancel flag and the activity it publishes to) and where it
/// may be held. It answers the owner's half.
pub(super) type Work = Box<dyn FnOnce(&JobControl, &dyn Fn(Phase)) -> Commit + Send>;

/// The owner's half of a job: it records what the worker found and answers the job's result, which
/// `job.read` reads.
pub(super) type Commit = Box<dyn FnOnce(&mut Owner) -> Result<Value, Error> + Send>;

/// What the worker calls as a job reaches each [`Phase`]; only a test sets one, to hold a job there.
pub(super) type Hold = Arc<dyn Fn(Phase) + Send + Sync>;

/// A job for the lane: its identity, its kind, what the activity board says of it, the request that
/// started it and its work.
pub(super) struct Task {
    pub job_id: JobId,
    pub job: &'static CatalogJob,
    pub asset_id: Option<AssetId>,
    /// The board's second line, such as the file being verified.
    pub detail: Option<String>,
    pub origin: Origin,
    pub work: Work,
}

/// A job waiting for the worker.
struct Queued {
    job_id: JobId,
    control: Arc<JobControl>,
    activity: ActivitySpec,
    work: Work,
}

/// A job handed to the worker.
struct Dispatch {
    job_id: JobId,
    control: Arc<JobControl>,
    work: Work,
    hold: Option<Hold>,
}

/// Lane C's state on the owner: one worker thread, started on the lane's first job, that runs one
/// job at a time while at most [`LANE_QUEUE`] wait, and blocks on its channel while idle. What it
/// finds comes back as [`LibraryMessage::Done`], which the owner commits.
pub(super) struct LibraryLane {
    poster: Poster,
    board: Arc<ActivityBoard>,
    waiting: VecDeque<Queued>,
    running: Option<(JobId, Arc<JobControl>)>,
    /// The worker's one-slot channel; it is sent to only while the worker is idle.
    worker: Option<SyncSender<Dispatch>>,
    hold: Option<Hold>,
}

/// What lane C's worker posts back.
pub(super) enum LibraryMessage {
    /// The worker finished a job's work: the owner runs its commit and records the result.
    Done { job_id: JobId, commit: Commit },
    /// Hold every job dispatched from now on at each phase it reaches, or stop holding them.
    #[cfg(test)]
    Hold(Option<Hold>),
    /// Run this on the owner, between messages, as a test arranges the catalog.
    #[cfg(test)]
    Run(Box<dyn FnOnce(&mut Owner) + Send>),
}

impl LibraryLane {
    pub(super) fn new(poster: Poster, board: Arc<ActivityBoard>) -> Self {
        Self {
            poster,
            board,
            waiting: VecDeque::new(),
            running: None,
            worker: None,
            hold: None,
        }
    }

    /// Open `task` as a `queued` catalog job and start it once the worker is idle, answering the
    /// start as the method does. Refused with `resource-limit`, opening nothing, when
    /// [`LANE_QUEUE`] jobs already wait behind a running one.
    pub(super) fn queue(&mut self, jobs: &mut Jobs, task: Task) -> Result<JobStarted, Error> {
        if self.running.is_some() && self.waiting.len() >= LANE_QUEUE {
            return Err(Error::resource_limit(format!(
                "{LANE_QUEUE} jobs already wait for the library lane"
            )));
        }
        let Task {
            job_id,
            job,
            asset_id,
            detail,
            origin,
            work,
        } = task;
        let control = JobControl::new();
        jobs.open_catalog(CatalogOpened {
            job_id: job_id.clone(),
            kind: job.kind,
            asset_id: asset_id.clone(),
            origin: Some(origin),
            control: control.clone(),
        });
        self.waiting.push_back(Queued {
            job_id: job_id.clone(),
            control,
            activity: ActivitySpec {
                kind: job.activity,
                label: job.label,
                detail,
                asset_id,
                job_id: Some(job_id.to_string()),
            },
            work,
        });
        self.dispatch(jobs);
        let status = jobs
            .read(&job_id)
            .map_or(JobStatus::Queued, |record| record.status);
        Ok(JobStarted {
            job_id,
            status,
            deduplicated: false,
        })
    }

    /// Record a job whose answer is already known — a retry's, which its first attempt recorded —
    /// as finished at once, so the retry is answered as the first attempt was: a job to read.
    pub(super) fn answered(
        jobs: &mut Jobs,
        job: &'static CatalogJob,
        asset_id: Option<AssetId>,
        origin: Origin,
        answer: Value,
    ) -> JobStarted {
        let job_id = JobId::new();
        jobs.open_catalog(CatalogOpened {
            job_id: job_id.clone(),
            kind: job.kind,
            asset_id,
            origin: Some(origin),
            control: JobControl::new(),
        });
        jobs.finish(&job_id, Ok(Output::Value(answer)));
        JobStarted {
            job_id,
            status: JobStatus::Ready,
            deduplicated: true,
        }
    }

    /// Hand the next waiting job to the worker when it is idle, starting its thread on first use.
    fn dispatch(&mut self, jobs: &mut Jobs) {
        while self.running.is_none() {
            let Some(queued) = self.waiting.pop_front() else {
                return;
            };
            let sender = match self.worker() {
                Ok(sender) => sender,
                Err(error) => {
                    jobs.finish(&queued.job_id, Err(error));
                    continue;
                }
            };
            jobs.start(&queued.job_id);
            queued
                .control
                .begin_activity(self.board.begin(queued.activity));
            let dispatch = Dispatch {
                job_id: queued.job_id.clone(),
                control: queued.control.clone(),
                work: queued.work,
                hold: self.hold.clone(),
            };
            // The worker is idle, so its one-slot channel is empty and this never blocks.
            if sender.send(dispatch).is_err() {
                self.worker = None;
                jobs.finish(
                    &queued.job_id,
                    Err(Error::internal("the library lane stopped")),
                );
                continue;
            }
            self.running = Some((queued.job_id, queued.control));
        }
    }

    /// The worker's channel, its thread started on first use.
    fn worker(&mut self) -> Result<SyncSender<Dispatch>, Error> {
        if let Some(sender) = &self.worker {
            return Ok(sender.clone());
        }
        let poster = self.poster.clone();
        let sender = worker::start("luxforge-library", move |dispatch| run(dispatch, &poster))?;
        self.worker = Some(sender.clone());
        Ok(sender)
    }

    /// Catalog jobs belong to no client, so a client leaving changes nothing here.
    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// `job.cancel` cancelled one of this lane's jobs in the job table: a waiting one, which the
    /// table has already finished, leaves the queue; a running one stops at its next checkpoint,
    /// and a result that arrives after the cancel is not committed.
    pub(super) fn cancelled(&mut self, job_id: &JobId) {
        self.waiting.retain(|queued| queued.job_id != *job_id);
    }

    /// Stop as the owner stops: the running job is cancelled and the worker's channel closed, so
    /// the thread ends at its job's next checkpoint. It is not waited for, since its last post may
    /// be waiting for room in the owner's channel, which the owner drops after the lanes stop.
    pub(super) fn shutdown(self) {
        if let Some((_, control)) = &self.running {
            control.cancel("the editor is closing");
        }
    }
}

/// One job on the worker ([`worker::start`]): run its work unless it was cancelled on the way, and
/// post the owner's half back. A job whose work panics fails with `internal` and the worker goes on.
fn run(dispatch: Dispatch, poster: &Poster) {
    let Dispatch {
        job_id,
        control,
        work,
        hold,
    } = dispatch;
    let pause = |phase: Phase| {
        if let Some(hold) = &hold {
            hold(phase);
        }
    };
    let commit: Commit = if control.is_cancelled() {
        let error = control.cancelled_error();
        Box::new(move |_| Err(error))
    } else {
        panic::catch_unwind(AssertUnwindSafe(|| work(&control, &pause)))
            .unwrap_or_else(|_| Box::new(|_| Err(Error::internal("the job stopped unexpectedly"))))
    };
    poster.post(CatalogMessage::Library(LibraryMessage::Done {
        job_id,
        commit,
    }));
}

/// Handle what the worker posted: commit a finished job's result unless it was cancelled first,
/// record it in the job table, announce what it changed, and start the next job.
pub(super) fn handle(owner: &mut Owner, message: LibraryMessage) {
    match message {
        LibraryMessage::Done { job_id, commit } => {
            let lane = &mut owner.catalog.library;
            let control = match &lane.running {
                Some((running, _)) if *running == job_id => {
                    lane.running.take().map(|(_, control)| control)
                }
                _ => None,
            };
            // A cancel the owner took before the result arrived wins: nothing is committed.
            let result = match control {
                Some(control) if control.is_cancelled() => Err(control.cancelled_error()),
                _ => panic::catch_unwind(AssertUnwindSafe(|| commit(owner))).unwrap_or_else(|_| {
                    Err(Error::internal("the job's result could not be recorded"))
                }),
            };
            owner.jobs.finish(&job_id, result.map(Output::Value));
            owner.record_announced();
            owner.catalog.library.dispatch(&mut owner.jobs);
        }
        #[cfg(test)]
        LibraryMessage::Hold(hold) => owner.catalog.library.hold = hold,
        #[cfg(test)]
        LibraryMessage::Run(run) => run(owner),
    }
}

/// Record one library change and announce it: `change` runs the journal in one catalog
/// transaction ([`EditorService::library_write`](crate::EditorService::library_write)), and a
/// change it recorded now is one event naming its sequence, however many items it covered, and is
/// handed to the lanes that follow its items ([`CatalogLanes::library_changed`]). A retry the
/// journal answered announces nothing and hands over nothing, as its first attempt did.
///
/// [`CatalogLanes::library_changed`]: super::catalog::CatalogLanes::library_changed
///
/// Every lane's library change goes through here, lane A's indexed folders included: its
/// `index.add-folder` passes `|tx| { upsert_volume(tx, &volume)?; journal::apply(tx, request,
/// vec![(LibraryItem::IndexedFolder { path }, Desired::Value(Some(folder)))], label) }`, and
/// `index.remove-folder` the same with `Desired::Value(None)`; undo and redo rewrite the
/// `indexed_folders` row from the journal like any other item.
pub(super) fn change(
    owner: &mut Owner,
    origin: &crate::api::Origin,
    change: impl FnOnce(&Transaction<'_>) -> Result<Outcome, Error>,
) -> Result<LibraryAnswer, Error> {
    let outcome = owner.service.library_write(change)?;
    if let Some(sequence) = outcome.announced() {
        announce_once(&mut owner.announced, &origin.clone().library(sequence));
        owner.catalog.library_changed(outcome.items());
    }
    Ok(outcome.answer())
}

/// The answer a request's first attempt recorded, when it recorded a change: a retry is answered
/// before its targets are resolved again, so it neither reads the disk nor fails on a file that has
/// moved since.
pub(super) fn retried(owner: &Owner, request: Request<'_>) -> Result<Option<LibraryAnswer>, Error> {
    Ok(library_journal::find(&owner.service.connection, request)?
        .map(|change| Outcome::deduplicated(change).answer()))
}

/// The items selected in the calling client's view.
fn selected(owner: &Owner, client: ClientId) -> Result<Vec<ViewItem>, Error> {
    owner.catalog.views.selected(client)
}
