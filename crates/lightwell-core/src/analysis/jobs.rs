//! Analysis job identity, the owner-held result store and the bounded analysis worker.
//!
//! The reducer in the parent module is a pure function over a rendered raster. This module is what
//! turns it into a service: an identity that says exactly which evaluated image a report belongs
//! to, a bounded store the catalog owner keeps, and one worker with a single active job and a
//! single replaceable pending job, as the integration contract's "Analysis jobs and identity"
//! requires (`docs/design/basic-and-histogram.md`).
//!
//! Nothing here runs on the catalog owner thread except bookkeeping: building a job costs a state
//! read, a cached source verification and an `O(layers)` plan and compile. The render and the
//! reduction happen on the worker, which sends its report back through the owner's own channel, so
//! no timer and no polling loop is involved.

use super::{DOMAIN, Report, deserialize_domain, reduce_raster};
use crate::{
    AssetId, ClientId, DraftStamp, EntryId, Error, ErrorKind, HistoryEntry, JobId, JobStatus,
    ModuleRegistry, PreviewSource, Recipe, SnapshotId,
    activity::{ActivityBoard, ActivitySpec, Outcome},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Arc,
};

/// How many completed reports the owner keeps. The shared API and jobs contract caps live reports
/// at the eight live clients; the oldest is evicted first.
pub const MAX_READY_REPORTS: usize = 8;

/// How many job records the owner keeps at all, including the finished ones a client may still
/// read as `superseded` or `cancelled`. Finished records are evicted oldest first; a pending job is
/// never evicted, because a worker still refers to it.
pub const MAX_JOB_RECORDS: usize = 32;

/// Which evaluated image one report describes. Two requests with equal identities describe byte for
/// byte the same rendered output, so they share one job and one cached report; a request whose
/// identity differs in any field is different work.
///
/// `width` and `height` are the **output** stage of the effective recipe. They are `0` only for a
/// job that failed before any output stage existed — an unavailable provider, or a payload the
/// registry refuses to compile. Such a job carries its error and never a report, so zero dimensions
/// can never be read as an empty but valid histogram.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisIdentity {
    pub asset_id: AssetId,
    /// The SHA-256 of the original file the catalog verified this decode against.
    pub source_fingerprint: String,
    /// The history entry the analysed stack belongs to. A draft is evaluated over the current
    /// entry, so a drafted identity names that entry and its `draft` stamp.
    pub entry_id: EntryId,
    pub snapshot_id: SnapshotId,
    /// SHA-256 (hex) of the effective recipe's canonical `serde_json` serialization. `serde_json`
    /// is pinned without `preserve_order`, so object keys serialize in sorted order and the same
    /// recipe always hashes the same way.
    pub recipe_hash: String,
    /// Present when the analysed stack is a client draft's effective recipe rather than a stored
    /// one, with the revision the draft held when the job was planned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftStamp>,
    pub width: u32,
    pub height: u32,
    /// Always `"srgb-8bit-output"`: the rendered SDR sRGB output of the composition, never an
    /// inference about RAW or sensor clipping.
    pub domain: AnalysisDomain,
}

/// The one analysis output domain, as a type. It encodes as the string `"srgb-8bit-output"` and
/// refuses any other value, and it owns no borrow, so an identity decodes out of an owned JSON
/// value — a `&'static str` field would tie the whole struct's `Deserialize` to `'de: 'static`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AnalysisDomain;

impl AnalysisDomain {
    pub const fn as_str(self) -> &'static str {
        DOMAIN
    }
}

impl std::fmt::Display for AnalysisDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(DOMAIN)
    }
}

impl Serialize for AnalysisDomain {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(DOMAIN)
    }
}

impl<'de> Deserialize<'de> for AnalysisDomain {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_domain(deserializer).map(|_| Self)
    }
}

impl AnalysisIdentity {
    /// The identity of the analysis of one evaluated stack. Pure and `O(layers)`: it serializes and
    /// hashes the recipe and reads nothing else. `stage` is the output stage the caller learned
    /// from [`render::extents`]; `None` records that the stack has no output stage at all.
    pub fn of(
        asset_id: &AssetId,
        source_fingerprint: &str,
        entry: &HistoryEntry,
        recipe: &Recipe,
        draft: Option<DraftStamp>,
        stage: Option<(u32, u32)>,
    ) -> Result<Self, Error> {
        let canonical = serde_json::to_vec(recipe).map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("a recipe could not be serialized for hashing: {error}"),
            )
        })?;
        let (width, height) = stage.unwrap_or((0, 0));
        Ok(Self {
            asset_id: asset_id.clone(),
            source_fingerprint: source_fingerprint.to_owned(),
            entry_id: entry.id.clone(),
            snapshot_id: entry.snapshot.id.clone(),
            recipe_hash: format!("{:x}", Sha256::digest(&canonical)),
            draft,
            width,
            height,
            domain: AnalysisDomain,
        })
    }

    /// Whether this identity describes an evaluable output stage at all.
    pub fn has_output_stage(&self) -> bool {
        self.width != 0 && self.height != 0
    }
}

/// What one job needs to run: the immutable source buffer, the shared registry and the effective
/// recipe, bound with the verified bytes of the artifacts it references, which the job holds until
/// the worker is done with them. The worker holds no catalog handle and no session.
pub struct AnalysisJob {
    pub job_id: JobId,
    pub identity: AnalysisIdentity,
    pub source: PreviewSource,
    pub registry: Arc<ModuleRegistry>,
    pub recipe: Recipe,
}

impl std::fmt::Debug for AnalysisJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisJob")
            .field("job_id", &self.job_id)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// One worker thread with one active job and one replaceable pending job, modelled on
/// [`crate::PreviewQueue`]. Unlike the preview queue nothing polls it: the worker posts its result
/// back into the catalog owner's own message channel through the `deliver` callback the owner
/// installed, and the owner starts the pending job when it handles that message.
pub struct AnalysisQueue {
    deliver: Arc<dyn Fn(JobId, Result<Report, Error>) + Send + Sync>,
    active: Option<JobId>,
    pending: Option<AnalysisJob>,
    activity: Option<Arc<ActivityBoard>>,
}

impl AnalysisQueue {
    /// `deliver` posts a finished job back to the owner loop. It is called from the worker thread.
    pub fn new(deliver: Arc<dyn Fn(JobId, Result<Report, Error>) + Send + Sync>) -> Self {
        Self {
            deliver,
            active: None,
            pending: None,
            activity: None,
        }
    }

    /// Publish every job this queue runs to `board` as an `analysis.histogram` activity, from the
    /// moment its worker starts to the moment it has a result. A queue without a board publishes
    /// nothing.
    pub fn set_activity(&mut self, board: Arc<ActivityBoard>) {
        self.activity = Some(board);
    }

    /// Hand a job to the worker, or into the one pending slot. Returns the job id that was
    /// displaced from that slot, which the caller marks `superseded`.
    pub fn submit(&mut self, job: AnalysisJob) -> Option<JobId> {
        if self.active.is_some() {
            let displaced = self.pending.replace(job);
            displaced.map(|job| job.job_id)
        } else {
            self.start(job);
            None
        }
    }

    /// The active job reported back: release the worker and start the pending job, if any.
    pub fn finished(&mut self, job_id: &JobId) {
        if self.active.as_ref() == Some(job_id) {
            self.active = None;
            if let Some(job) = self.pending.take() {
                self.start(job);
            }
        }
    }

    /// Drop the pending job when it is this one, because nobody is interested any more. Returns
    /// whether it was dropped.
    pub fn drop_pending(&mut self, job_id: &JobId) -> bool {
        if self.pending.as_ref().map(|job| &job.job_id) == Some(job_id) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    pub fn is_active(&self, job_id: &JobId) -> bool {
        self.active.as_ref() == Some(job_id)
    }

    pub fn is_pending(&self, job_id: &JobId) -> bool {
        self.pending.as_ref().map(|job| &job.job_id) == Some(job_id)
    }

    fn start(&mut self, job: AnalysisJob) {
        self.active = Some(job.job_id.clone());
        let deliver = self.deliver.clone();
        let board = self.activity.clone();
        std::thread::spawn(move || {
            let AnalysisJob {
                job_id,
                identity,
                source,
                registry,
                recipe,
            } = job;
            let activity = board.map(|board| {
                board.begin(ActivitySpec {
                    kind: "analysis.histogram",
                    label: "Measuring histogram",
                    detail: None,
                    asset_id: Some(identity.asset_id.clone()),
                    job_id: Some(job_id.to_string()),
                })
            });
            let result = source
                .render(&registry, identity.snapshot_id.clone(), &recipe)
                .and_then(|raster| {
                    let report = reduce_raster(&raster);
                    // No per-result raster is retained: the frame is released here, before the
                    // bounded report travels back to the owner.
                    drop(raster);
                    report
                });
            // The render has compiled the stack; the artifacts it was bound with go with it.
            drop(recipe);
            // The activity ends before the result is posted, so a client that reads the job as
            // finished never still finds it listed as running.
            if let Some(activity) = activity {
                activity.finish(Outcome::of(&result));
            }
            deliver(job_id, result);
        });
    }
}

impl std::fmt::Debug for AnalysisQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisQueue")
            .field("active", &self.active)
            .field("pending", &self.pending.as_ref().map(|job| &job.job_id))
            .finish()
    }
}

#[derive(Debug)]
enum JobState {
    Pending,
    /// Boxed: a `Report` is about 6 KiB, and the job table must not carry that inline per record.
    Ready(Box<Report>),
    Failed(Error),
    Superseded,
    Cancelled,
}

impl JobState {
    /// The shared [`JobStatus`] this record reads as. `Pending` alone cannot say whether the
    /// worker holds the job or it still waits in the queue's one replaceable slot, so it asks
    /// `queue`, the same distinction [`AnalysisQueue::is_active`] and [`AnalysisQueue::is_pending`]
    /// already draw.
    fn status(&self, job_id: &JobId, queue: &AnalysisQueue) -> JobStatus {
        match self {
            Self::Pending if queue.is_active(job_id) => JobStatus::Running,
            Self::Pending => JobStatus::Queued,
            Self::Ready(_) => JobStatus::Ready,
            Self::Failed(_) => JobStatus::Failed,
            Self::Superseded => JobStatus::Superseded,
            Self::Cancelled => JobStatus::Cancelled,
        }
    }
    fn is_finished(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Debug)]
struct JobRecord {
    identity: AnalysisIdentity,
    /// Every client that has ever requested this job. It decides who may read or cancel it, so a
    /// client that cancelled still reads the `cancelled` outcome it asked for.
    requesters: BTreeSet<ClientId>,
    /// The clients that still want the work. When this empties, the job is cancelled.
    interested: BTreeSet<ClientId>,
    state: JobState,
}

/// What one client reads back about a job.
#[derive(Debug)]
pub struct AnalysisRead<'a> {
    pub status: JobStatus,
    pub identity: &'a AnalysisIdentity,
    /// Only ever `Some` for [`JobStatus::Ready`].
    pub report: Option<&'a Report>,
    /// Only ever `Some` for [`JobStatus::Failed`].
    pub error: Option<&'a Error>,
}

/// What the owner must do after a client released its interest in a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Other clients still want it, or it had already finished: leave the work alone.
    Kept,
    /// Nobody is interested any more: drop it from the queue, or discard its result on arrival.
    Cancelled,
}

/// The catalog owner's bounded analysis bookkeeping: which identities have jobs, who is interested
/// in each, and the small ring of completed reports. It holds no pixels beyond the reports and
/// never touches a source or a frame.
#[derive(Debug, Default)]
pub struct AnalysisStore {
    jobs: HashMap<JobId, JobRecord>,
    /// Only pending, ready and failed jobs are indexed here, so a superseded or cancelled identity
    /// can be requested again and gets fresh work.
    by_identity: HashMap<AnalysisIdentity, JobId>,
    /// Job ids oldest first, which is the eviction order for both bounds.
    order: VecDeque<JobId>,
}

impl AnalysisStore {
    /// Join or open the job for this identity. Returns its id and whether the caller must schedule
    /// new work for it.
    pub fn request(&mut self, identity: AnalysisIdentity, client: ClientId) -> (JobId, bool) {
        if let Some(job_id) = self.by_identity.get(&identity).cloned()
            && let Some(record) = self.jobs.get_mut(&job_id)
        {
            record.requesters.insert(client);
            // A finished job needs no worker, so joining it never revives interest in work.
            if !record.state.is_finished() {
                record.interested.insert(client);
            }
            return (job_id, false);
        }
        let job_id = JobId::new();
        self.insert(
            job_id.clone(),
            JobRecord {
                identity,
                requesters: BTreeSet::from([client]),
                interested: BTreeSet::from([client]),
                state: JobState::Pending,
            },
        );
        (job_id, true)
    }

    /// Record a report the desktop's preview worker already produced for this identity, so the next
    /// `analysis.request` for it is a cache hit and no second render happens.
    pub fn submit(&mut self, identity: AnalysisIdentity, report: Report) {
        if let Some(job_id) = self.by_identity.get(&identity).cloned()
            && let Some(record) = self.jobs.get_mut(&job_id)
        {
            // A job that is already running keeps its own outcome; a stored report is not replaced.
            if record.state.is_finished() {
                return;
            }
            record.state = JobState::Ready(Box::new(report));
            record.interested.clear();
            self.trim();
            return;
        }
        self.insert(
            JobId::new(),
            JobRecord {
                identity,
                requesters: BTreeSet::new(),
                interested: BTreeSet::new(),
                state: JobState::Ready(Box::new(report)),
            },
        );
    }

    /// Store the worker's outcome. A job nobody is waiting for any more keeps its `cancelled`
    /// outcome and the result is discarded.
    pub fn complete(&mut self, job_id: &JobId, result: Result<Report, Error>) {
        let Some(record) = self.jobs.get_mut(job_id) else {
            return;
        };
        if record.state.is_finished() {
            return;
        }
        record.state = match result {
            Ok(report) => JobState::Ready(Box::new(report)),
            Err(error) => JobState::Failed(error),
        };
        record.interested.clear();
        self.trim();
    }

    /// Record a job that failed before it could be scheduled at all: the effective recipe resolved
    /// but has no output stage the host can evaluate.
    pub fn fail(&mut self, job_id: &JobId, error: Error) {
        if let Some(record) = self.jobs.get_mut(job_id)
            && !record.state.is_finished()
        {
            record.state = JobState::Failed(error);
            record.interested.clear();
        }
        self.trim();
    }

    /// A newer request took the single pending slot from this one.
    pub fn supersede(&mut self, job_id: &JobId) {
        self.finish(job_id, JobState::Superseded);
    }

    /// Nobody is interested any more.
    pub fn cancel(&mut self, job_id: &JobId) {
        self.finish(job_id, JobState::Cancelled);
    }

    fn finish(&mut self, job_id: &JobId, state: JobState) {
        let identity = match self.jobs.get_mut(job_id) {
            Some(record) if !record.state.is_finished() => {
                record.state = state;
                record.interested.clear();
                Some(record.identity.clone())
            }
            _ => return,
        };
        // A superseded or cancelled identity is re-requestable: it must not answer a later request
        // from the index.
        if let Some(identity) = identity
            && self.by_identity.get(&identity) == Some(job_id)
        {
            self.by_identity.remove(&identity);
        }
        self.trim();
    }

    /// Whether the worker's result for this job should still be stored.
    pub fn awaits(&self, job_id: &JobId) -> bool {
        self.jobs
            .get(job_id)
            .is_some_and(|record| !record.state.is_finished())
    }

    /// Drop one client's interest. `Err` when this client never requested the job, which is how a
    /// foreign or unknown job id is refused.
    pub fn release(&mut self, job_id: &JobId, client: ClientId) -> Result<Release, Error> {
        let record = self
            .jobs
            .get_mut(job_id)
            .filter(|record| record.requesters.contains(&client))
            .ok_or_else(|| unknown_job(job_id))?;
        record.interested.remove(&client);
        Ok(
            if record.interested.is_empty() && !record.state.is_finished() {
                Release::Cancelled
            } else {
                Release::Kept
            },
        )
    }

    /// Release every interest this client holds and forget it as a requester. Returns the jobs that
    /// lost their last interested client, which the owner then cancels.
    pub fn disconnect(&mut self, client: ClientId) -> Vec<JobId> {
        let mut orphaned = Vec::new();
        for (job_id, record) in &mut self.jobs {
            let held = record.requesters.remove(&client);
            record.interested.remove(&client);
            if held && record.interested.is_empty() && !record.state.is_finished() {
                orphaned.push(job_id.clone());
            }
        }
        orphaned
    }

    /// One client's view of one job. A job this client never requested is simply not its own.
    /// `queue` resolves a `Pending` record to `running` or `queued`, since the record alone cannot
    /// say which.
    pub fn read(
        &self,
        job_id: &JobId,
        client: ClientId,
        queue: &AnalysisQueue,
    ) -> Result<AnalysisRead<'_>, Error> {
        let record = self
            .jobs
            .get(job_id)
            .filter(|record| record.requesters.contains(&client))
            .ok_or_else(|| unknown_job(job_id))?;
        Ok(AnalysisRead {
            status: record.state.status(job_id, queue),
            identity: &record.identity,
            report: match &record.state {
                JobState::Ready(report) => Some(report.as_ref()),
                _ => None,
            },
            error: match &record.state {
                JobState::Failed(error) => Some(error),
                _ => None,
            },
        })
    }

    /// The job's state without a client check, for the owner's own responses.
    pub fn state_of(&self, job_id: &JobId, queue: &AnalysisQueue) -> Option<AnalysisRead<'_>> {
        let record = self.jobs.get(job_id)?;
        Some(AnalysisRead {
            status: record.state.status(job_id, queue),
            identity: &record.identity,
            report: match &record.state {
                JobState::Ready(report) => Some(report.as_ref()),
                _ => None,
            },
            error: match &record.state {
                JobState::Failed(error) => Some(error),
                _ => None,
            },
        })
    }

    /// How many completed reports are held. Bounded by [`MAX_READY_REPORTS`].
    pub fn reports(&self) -> usize {
        self.jobs
            .values()
            .filter(|record| matches!(record.state, JobState::Ready(_)))
            .count()
    }

    /// How many job records are held. Bounded by [`MAX_JOB_RECORDS`].
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    fn insert(&mut self, job_id: JobId, record: JobRecord) {
        self.by_identity
            .insert(record.identity.clone(), job_id.clone());
        self.order.push_back(job_id.clone());
        self.jobs.insert(job_id, record);
        self.trim();
    }

    /// Enforce both bounds: at most [`MAX_READY_REPORTS`] completed reports and at most
    /// [`MAX_JOB_RECORDS`] records, evicting the oldest finished record first. A pending job is
    /// never evicted, because a worker still refers to it.
    fn trim(&mut self) {
        while self.reports() > MAX_READY_REPORTS {
            let Some(oldest) = self.oldest(|state| matches!(state, JobState::Ready(_))) else {
                break;
            };
            self.remove(&oldest);
        }
        while self.jobs.len() > MAX_JOB_RECORDS {
            let Some(oldest) = self.oldest(JobState::is_finished) else {
                break;
            };
            self.remove(&oldest);
        }
    }

    fn oldest(&self, matching: impl Fn(&JobState) -> bool) -> Option<JobId> {
        self.order
            .iter()
            .find(|job_id| {
                self.jobs
                    .get(*job_id)
                    .is_some_and(|record| matching(&record.state))
            })
            .cloned()
    }

    fn remove(&mut self, job_id: &JobId) {
        self.order.retain(|held| held != job_id);
        if let Some(record) = self.jobs.remove(job_id)
            && self.by_identity.get(&record.identity) == Some(job_id)
        {
            self.by_identity.remove(&record.identity);
        }
    }
}

fn unknown_job(job_id: &JobId) -> Error {
    Error::new(
        ErrorKind::Validation,
        format!("unknown analysis job {job_id} for this client"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetId, Layer, Snapshot};
    use serde_json::json;

    fn entry(asset: &AssetId) -> HistoryEntry {
        let snapshot = Snapshot::original(asset.clone());
        HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 0,
            action_id: "original".into(),
            label: "Original".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        }
    }

    fn identity(asset: &AssetId, entry: &HistoryEntry, recipe: &Recipe) -> AnalysisIdentity {
        AnalysisIdentity::of(asset, "sha256:test", entry, recipe, None, Some((4, 3))).unwrap()
    }

    fn report(pixels: u64) -> Report {
        let mut report = super::super::reduce(&[0, 0, 0, 255], 1, 1).unwrap();
        report.r0 = pixels;
        report
    }

    /// A queue with no worker behind it, for tests that drive `AnalysisStore` directly and never
    /// read a job while it is genuinely `Pending` — only `JobState::status` ever consults it.
    fn queue() -> AnalysisQueue {
        AnalysisQueue::new(Arc::new(|_, _| {}))
    }

    #[test]
    fn an_identity_hashes_the_recipe_and_marks_a_stack_without_an_output_stage() {
        let asset = AssetId::new();
        let entry = entry(&asset);
        let empty = entry.snapshot.recipe.clone();
        let edited = entry
            .snapshot
            .clone()
            .append(Layer::pixel(0, 0, [1, 2, 3]))
            .unwrap()
            .recipe;
        let a = identity(&asset, &entry, &empty);
        let b = identity(&asset, &entry, &empty);
        let c = identity(&asset, &entry, &edited);
        assert_eq!(a, b, "the same recipe hashes the same way");
        assert_eq!(a.recipe_hash.len(), 64, "SHA-256 as hex");
        assert_ne!(a.recipe_hash, c.recipe_hash);
        assert!(a.has_output_stage());
        assert_eq!(a.domain.as_str(), DOMAIN);
        let unavailable =
            AnalysisIdentity::of(&asset, "sha256:test", &entry, &empty, None, None).unwrap();
        assert!(!unavailable.has_output_stage());
        assert_ne!(unavailable, a, "no output stage is its own identity");
        // Round trips through JSON, including the refusal of a foreign domain.
        let encoded = serde_json::to_value(&a).unwrap();
        assert_eq!(
            serde_json::from_value::<AnalysisIdentity>(encoded.clone()).unwrap(),
            a
        );
        let mut foreign = encoded;
        foreign["domain"] = json!("raw-linear");
        assert!(serde_json::from_value::<AnalysisIdentity>(foreign).is_err());
    }

    #[test]
    fn identical_identities_share_one_job_and_releases_only_cancel_the_last_interest() {
        let asset = AssetId::new();
        let entry = entry(&asset);
        let recipe = entry.snapshot.recipe.clone();
        let identity = identity(&asset, &entry, &recipe);
        let mut store = AnalysisStore::default();
        let one = ClientId::testing(1);
        let two = ClientId::testing(2);
        let (job, fresh) = store.request(identity.clone(), one);
        assert!(fresh, "the first request schedules work");
        let (same, again) = store.request(identity.clone(), two);
        assert_eq!(job, same, "an identical identity joins the same job");
        assert!(!again, "and schedules nothing");
        assert_eq!(store.release(&job, one).unwrap(), Release::Kept);
        assert_eq!(store.release(&job, two).unwrap(), Release::Cancelled);
        store.cancel(&job);
        // Both clients still read the outcome they asked for; nobody else can.
        assert_eq!(
            store.read(&job, one, &queue()).unwrap().status,
            JobStatus::Cancelled
        );
        assert!(store.read(&job, ClientId::testing(3), &queue()).is_err());
        // A cancelled identity is re-requestable and gets a fresh job.
        let (fresh_job, scheduled) = store.request(identity, one);
        assert_ne!(fresh_job, job);
        assert!(scheduled);
    }

    #[test]
    fn the_report_ring_and_the_job_table_stay_bounded() {
        let asset = AssetId::new();
        let mut entries = Vec::new();
        let client = ClientId::testing(1);
        let mut store = AnalysisStore::default();
        let mut jobs = Vec::new();
        for index in 0..MAX_READY_REPORTS + 4 {
            let entry = entry(&asset);
            let recipe = entry.snapshot.recipe.clone();
            let identity = AnalysisIdentity::of(
                &asset,
                "sha256:test",
                &entry,
                &recipe,
                None,
                Some((index as u32 + 1, 1)),
            )
            .unwrap();
            let (job, _) = store.request(identity, client);
            store.complete(&job, Ok(report(index as u64)));
            jobs.push(job);
            entries.push(entry);
        }
        assert_eq!(store.reports(), MAX_READY_REPORTS, "oldest reports evicted");
        assert_eq!(store.len(), MAX_READY_REPORTS);
        for evicted in &jobs[..4] {
            assert!(store.read(evicted, client, &queue()).is_err(), "evicted");
        }
        assert_eq!(
            store
                .read(jobs.last().unwrap(), client, &queue())
                .unwrap()
                .status,
            JobStatus::Ready
        );
        // Finished non-report records are bounded by the job table cap.
        for index in 0..MAX_JOB_RECORDS * 2 {
            let entry = entry(&asset);
            let recipe = entry.snapshot.recipe.clone();
            let identity = AnalysisIdentity::of(
                &asset,
                "sha256:other",
                &entry,
                &recipe,
                None,
                Some((1, index as u32 + 1)),
            )
            .unwrap();
            let (job, _) = store.request(identity, client);
            store.supersede(&job);
        }
        assert!(store.len() <= MAX_JOB_RECORDS, "{}", store.len());
        assert!(store.reports() <= MAX_READY_REPORTS);
    }

    #[test]
    fn a_disconnect_releases_every_interest_that_client_held() {
        let asset = AssetId::new();
        let entry = entry(&asset);
        let recipe = entry.snapshot.recipe.clone();
        let identity = identity(&asset, &entry, &recipe);
        let mut store = AnalysisStore::default();
        let one = ClientId::testing(1);
        let two = ClientId::testing(2);
        let (job, _) = store.request(identity, one);
        store.request(
            AnalysisIdentity::of(&asset, "sha256:test", &entry, &recipe, None, Some((9, 9)))
                .unwrap(),
            two,
        );
        assert!(
            store.disconnect(two).len() == 1,
            "two's own job is orphaned"
        );
        assert!(
            store.disconnect(one).contains(&job),
            "one's job is orphaned too"
        );
        assert!(
            store.read(&job, one, &queue()).is_err(),
            "a gone client owns nothing"
        );
    }
}
