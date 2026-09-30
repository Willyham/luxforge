//! **Lane B (previews)** on the owner: the 100% region jobs of `preview.region`.
//!
//! A request is planned here from SQL alone — a file's index row, or the catalog's record of a
//! developed photograph's original — and becomes one `preview-region` catalog job, whose result is
//! the [`RegionAnswer`](crate::catalog_types::RegionAnswer). The jobs run on the one region worker
//! (`crate::previews::RegionWorker`), started on the first request and apart from the extraction
//! workers, one at a time in the order they arrived.
//!
//! - **Latest wins per client.** A client's next region cancels its previous one, waiting or
//!   running: the pointer moved. The job table records it `cancelled`, and a running one stops at
//!   its next checkpoint, a wait for the development included.
//! - **A small bounded queue.** Regions of different clients wait in one FIFO of at most
//!   [`REGION_QUEUE_CAPACITY`], one per client since each client's next region replaces its
//!   last; one past it is `resource-limit`.
//! - **Answers.** A client's answer file is valid until its next region: the previous one is
//!   removed when the next is written, and the client's answer when it disconnects (one unlink on
//!   the owner each). An answer written for a job cancelled meanwhile is removed at once.
//! - **Progress.** While a region runs it is a "Checking focus" row on the activity board with its
//!   job, so the Performance section shows it and can cancel it.
use super::{CatalogMessage, ClientId, Owner, PreviewsMessage, finish_cancelled};
use crate::{
    AssetId, Error, JobId, JobStatus,
    activity::ActivitySpec,
    api::{owner::Call, transport::MAX_CLIENTS},
    catalog_types::{JobStarted, PreviewItem, api::PreviewRegion, jobs::PREVIEW_REGION},
    jobs::{CANCELLED, CatalogOpened, JobControl, JobKind, Jobs, Output},
    previews::{
        RegionDone, RegionPost, RegionSource, RegionWork, RegionWorker, answer_path, region,
        remove_answer,
    },
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

/// The most region jobs waiting for the worker: one per client, since a client's next region
/// cancels its last, so the live-client limit and the desktop.
pub(in crate::api) const REGION_QUEUE_CAPACITY: usize = MAX_CLIENTS + 1;

/// Why a region job ends when its client asks for the next one.
const SUPERSEDED: &str = "superseded by the client's next region";
/// Why a region job ends when its client disconnects.
const DISCONNECTED: &str = "the client disconnected";

/// The region jobs on the owner.
#[derive(Default)]
pub(super) struct Regions {
    /// Started with the first region.
    worker: Option<RegionWorker>,
    /// The job on the worker.
    running: Option<Running>,
    /// Jobs waiting for the worker, oldest first.
    waiting: VecDeque<(ClientId, RegionWork)>,
    /// Each client's live region job, waiting or running, which its next region cancels.
    latest: HashMap<ClientId, JobId>,
    /// Each client's answer file, valid until its next answer is written or it disconnects.
    answers: HashMap<ClientId, PathBuf>,
    /// Numbers the answers, so a new answer never overwrites a path a client may be reading.
    sequence: u64,
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// The job on the worker.
struct Running {
    job_id: JobId,
    client: ClientId,
    control: Arc<JobControl>,
    /// Its client has disconnected, so what it writes is removed as soon as it is written.
    gone: bool,
}

impl Regions {
    /// Hold every region job handed out from now on at `gate`, or stop holding them.
    #[cfg(test)]
    pub(super) fn hold(&mut self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.hold = gate;
    }

    /// End `client`'s live region job, if it has one, for `reason`: a waiting one leaves the queue,
    /// and a running one is recorded `cancelled` now and stops at its next checkpoint, a wait for
    /// the development woken to see it.
    fn end_latest(&mut self, client: ClientId, jobs: &mut Jobs, reason: &str) {
        let Some(job_id) = self.latest.remove(&client) else {
            return;
        };
        if let Some(at) = self
            .waiting
            .iter()
            .position(|(_, work)| work.job_id == job_id)
        {
            self.waiting.remove(at);
            // Still queued in the table, which records it cancelled at once.
            jobs.cancel(&job_id, reason);
        } else if self
            .running
            .as_ref()
            .is_some_and(|running| running.job_id == job_id)
        {
            jobs.cancel(&job_id, reason);
            finish_cancelled(jobs, &job_id);
            region::wake_development_waiters();
        }
    }

    /// `job.cancel` cancelled `job_id` in the job table: when it is a client's live region job it
    /// leaves the queue, or ends now and stops at its next checkpoint. Whether it was one.
    pub(super) fn cancelled(&mut self, job_id: &JobId, jobs: &mut Jobs) -> bool {
        let Some(client) = self
            .latest
            .iter()
            .find_map(|(client, latest)| (latest == job_id).then_some(*client))
        else {
            return false;
        };
        // The table has already recorded the cancel and its reason; this ends the lane's side.
        self.end_latest(client, jobs, CANCELLED);
        true
    }

    /// A client has gone: its region job ends, its answer is removed, and what its running job
    /// writes is removed as soon as it is written.
    pub(super) fn disconnect(&mut self, client: ClientId, jobs: &mut Jobs) {
        self.end_latest(client, jobs, DISCONNECTED);
        if let Some(running) = self.running.as_mut()
            && running.client == client
        {
            running.gone = true;
        }
        if let Some(path) = self.answers.remove(&client) {
            remove_answer(&path);
        }
    }

    /// Stop as the owner stops: the running job at its next checkpoint, the worker once it is idle
    /// (its channel closes), and every answer removed. The worker is not joined.
    pub(super) fn shutdown(self) {
        if let Some(running) = &self.running {
            running.control.cancel("the catalog owner stopped");
        }
        drop(self.worker);
        for path in self.answers.values() {
            remove_answer(path);
        }
    }
}

/// `preview.region`: plan the region, cancel the client's previous one, and queue a job on the
/// region worker; answers the job.
pub(in crate::api) fn preview_region(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PreviewRegion,
) -> Result<Value, Error> {
    let client = call.client;
    region::check_rect(params.rect)?;
    if params
        .frame
        .is_some_and(|frame| frame.width == 0 || frame.height == 0)
    {
        return Err(Error::validation("a region's frame has no pixels"));
    }
    let (source, asset_id) = plan(owner, &params.item)?;
    let previews = owner.service.index()?.previews_dir();
    let regions = &owner.catalog.previews.regions;
    // The client's previous region, while it waits, makes room for this one.
    let own = regions
        .latest
        .get(&client)
        .is_some_and(|job| regions.waiting.iter().any(|(_, work)| work.job_id == *job));
    if regions.waiting.len() - usize::from(own) >= REGION_QUEUE_CAPACITY {
        return Err(Error::resource_limit(format!(
            "at most {REGION_QUEUE_CAPACITY} regions wait for the region worker"
        )));
    }
    ensure_worker(owner, &previews)?;
    let lane = &mut owner.catalog.previews;
    lane.regions.end_latest(client, &mut owner.jobs, SUPERSEDED);
    lane.regions.sequence += 1;
    let job_id = JobId::new();
    let control = JobControl::new();
    owner.jobs.open_catalog(CatalogOpened {
        job_id: job_id.clone(),
        kind: JobKind::PreviewRegion,
        asset_id,
        origin: Some(call.origin.clone()),
        control: control.clone(),
    });
    let work = RegionWork {
        job_id: job_id.clone(),
        item: params.item,
        source,
        rect: params.rect,
        frame: params.frame,
        path: answer_path(&previews, client.0, lane.regions.sequence),
        control,
        #[cfg(test)]
        hold: lane.regions.hold.clone(),
    };
    lane.regions.waiting.push_back((client, work));
    lane.regions.latest.insert(client, job_id.clone());
    lane.developing.insert(client);
    dispatch(owner);
    let status = owner
        .jobs
        .read(&job_id)
        .map_or(JobStatus::Queued, |record| record.status);
    serde_json::to_value(JobStarted {
        job_id,
        status,
        deduplicated: false,
    })
    .map_err(|error| Error::internal(format!("region answer: {error}")))
}

/// Where `item`'s region comes from, from SQL alone: a file's index row, or a developed
/// photograph's original as the catalog records it, with the photograph's asset.
fn plan(owner: &Owner, item: &PreviewItem) -> Result<(RegionSource, Option<AssetId>), Error> {
    match item {
        PreviewItem::File { file_id } => {
            let index = owner.service.index()?;
            let record = crate::index::file(index.connection(), *file_id)?.ok_or_else(|| {
                Error::validation(format!("the index holds no file {}", file_id.0))
            })?;
            Ok((
                RegionSource::File {
                    path: record.path,
                    kind: record.kind,
                    signature: record.signature,
                },
                None,
            ))
        }
        PreviewItem::Photo { asset_id, entry_id } => {
            let asset = owner.service.photo_original(asset_id, entry_id.as_ref())?;
            Ok((
                RegionSource::Original(Box::new(asset)),
                Some(asset_id.clone()),
            ))
        }
    }
}

/// Start the region worker on the first region.
fn ensure_worker(owner: &mut Owner, previews: &Path) -> Result<(), Error> {
    let lane = &mut owner.catalog.previews;
    if lane.regions.worker.is_some() {
        return Ok(());
    }
    let poster = lane.poster.clone();
    let post: RegionPost = Arc::new(move |done| {
        poster.post(CatalogMessage::Previews(PreviewsMessage::Region(done)));
    });
    lane.regions.worker = Some(RegionWorker::start(previews, post)?);
    Ok(())
}

/// Hand the oldest waiting region to the worker when it is idle, publishing it on the activity
/// board as "Checking focus", naming the file, with its job.
fn dispatch(owner: &mut Owner) {
    let lane = &mut owner.catalog.previews;
    let regions = &mut lane.regions;
    if regions.running.is_some() {
        return;
    }
    let Some(worker) = regions.worker.as_ref() else {
        return;
    };
    let Some((client, work)) = regions.waiting.pop_front() else {
        return;
    };
    let (path, asset_id) = match &work.source {
        RegionSource::File { path, .. } => (path, None),
        RegionSource::Original(asset) => (&asset.locator, Some(asset.id.clone())),
    };
    let detail = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    owner.jobs.start(&work.job_id);
    work.control.begin_activity(lane.board.begin(ActivitySpec {
        kind: PREVIEW_REGION.activity,
        label: PREVIEW_REGION.label,
        detail,
        asset_id,
        job_id: Some(work.job_id.to_string()),
    }));
    let running = Running {
        job_id: work.job_id.clone(),
        client,
        control: work.control.clone(),
        gone: false,
    };
    match worker.send(work) {
        Ok(()) => regions.running = Some(running),
        Err(error) => {
            // The worker has gone: the job fails, and the next region starts another.
            regions.worker = None;
            regions.latest.retain(|_, latest| *latest != running.job_id);
            owner.jobs.finish(&running.job_id, Err(error));
        }
    }
}

/// The worker finished a job: the job records its result, and its answer becomes the client's,
/// replacing the one before, unless the job was cancelled meanwhile or its client has gone, when
/// the answer is removed; then the next region is handed out.
pub(super) fn finished(owner: &mut Owner, done: RegionDone) {
    let RegionDone { job_id, result } = done;
    let regions = &mut owner.catalog.previews.regions;
    let Some(running) = regions.running.take_if(|running| running.job_id == job_id) else {
        return;
    };
    if regions.latest.get(&running.client) == Some(&job_id) {
        regions.latest.remove(&running.client);
    }
    let output = result.clone().and_then(|answer| {
        serde_json::to_value(&answer)
            .map(Output::Value)
            .map_err(|error| Error::internal(format!("region result: {error}")))
    });
    let answered = owner
        .jobs
        .finish(&job_id, output)
        .is_some_and(|finished| finished.record.status == JobStatus::Ready);
    let regions = &mut owner.catalog.previews.regions;
    if let Ok(answer) = result {
        if answered && !running.gone {
            if let Some(previous) = regions.answers.insert(running.client, answer.path.clone())
                && previous != answer.path
            {
                remove_answer(&previous);
            }
        } else {
            remove_answer(&answer.path);
        }
    }
    dispatch(owner);
}

#[cfg(test)]
#[path = "regions_tests.rs"]
mod preview_region_owner;
