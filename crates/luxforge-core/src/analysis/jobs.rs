//! Analysis job identity and the bounded analysis worker.
//!
//! The reducer in the parent module is a pure function over a rendered raster. This module is what
//! turns it into a service: an identity that says exactly which evaluated image a report belongs
//! to, and one worker with a single active job and a single replaceable pending job, as the
//! integration contract's "Analysis jobs and identity" requires
//! (`docs/design/basic-and-histogram.md`). The jobs' records, who wants them and the kept reports
//! live in the catalog owner's one job table (`crate::jobs`).
//!
//! Nothing here runs on the catalog owner thread except bookkeeping: building a job costs a state
//! read, a cached source verification and an `O(layers)` plan and compile. The render and the
//! reduction happen on the worker, which wakes the owner through its own channel, so no timer and
//! no polling loop is involved.

use super::{DOMAIN, Report, deserialize_domain, reduce};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, DraftStamp, EntryId, Error, Evaluation, HistoryEntry, JobId, Recipe, SnapshotId,
    activity::{ActivityBoard, ActivitySpec, Outcome},
    latest::{Latest, Running, WAITING_RESULTS},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, sync::Arc};

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
    /// from compiling it; `None` records that the stack has no output stage at all.
    pub(crate) fn of(
        asset_id: &AssetId,
        source_fingerprint: &str,
        entry: &HistoryEntry,
        recipe: &Recipe,
        draft: Option<DraftStamp>,
        stage: Option<(u32, u32)>,
    ) -> Result<Self, Error> {
        let canonical = serde_json::to_vec(recipe).map_err(|error| {
            Error::internal(format!(
                "a recipe could not be serialized for hashing: {error}"
            ))
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
    #[cfg(test)]
    pub(crate) fn has_output_stage(&self) -> bool {
        self.width != 0 && self.height != 0
    }
}

/// What one job needs to run: the evaluation the catalog owner planned — the immutable source
/// buffer, the shared registry and the effective recipe, bound with the verified bytes of the
/// artifacts it references, and its one compilation — which the job holds until the worker is done
/// with it. The worker holds no catalog handle and no session, and compiles nothing.
pub(crate) struct AnalysisJob {
    pub job_id: JobId,
    pub identity: AnalysisIdentity,
    pub evaluation: Evaluation,
}

impl std::fmt::Debug for AnalysisJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisJob")
            .field("job_id", &self.job_id)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// One analysis job as the worker receives it: the job, and the activity board it is published on.
struct AnalysisTask {
    job: AnalysisJob,
    board: Option<Arc<ActivityBoard>>,
    started: Option<Arc<dyn Fn(JobId) + Send + Sync>>,
}

/// How one analysis job ended, as [`AnalysisQueue::poll`] delivers it. A job withdrawn while it
/// ran ends [`ErrorKind::Cancelled`].
#[derive(Debug)]
pub(crate) struct AnalysisOutcome {
    pub job_id: JobId,
    pub result: Result<Report, Error>,
}

/// How many job ids [`AnalysisQueue`] remembers the generation of. The worker holds at most one
/// running job, one pending job and [`WAITING_RESULTS`] undelivered outcomes, and a job dropped from
/// the pending slot is forgotten at once, so the running and pending jobs — the only ones a lookup is
/// for — are always among this many newest.
const HELD_JOBS: usize = WAITING_RESULTS + 2;

/// The analysis worker: one persistent [`Latest`] worker with one active job and one replaceable
/// pending job, globally. Nothing polls it on a timer: the worker calls the waker the catalog owner
/// installed, which posts into the owner's own message channel, and the owner takes the outcomes
/// with [`Self::poll`] when it handles that message. The worker starts the pending job itself when
/// the active one ends.
///
/// A newer request does not interrupt the running job, because a client may still want it. Only
/// [`Self::withdraw`] does: the render and the reduction read the job's abandoned token at chunk
/// granularity, so a withdrawn analysis stops within a chunk and ends cancelled.
pub(crate) struct AnalysisQueue {
    worker: Latest<AnalysisTask, AnalysisOutcome>,
    started: Option<Arc<dyn Fn(JobId) + Send + Sync>>,
    activity: Option<Arc<ActivityBoard>>,
    /// The generation each recent job was requested under, oldest first, bounded by
    /// [`HELD_JOBS`], so a job id can be withdrawn and its slot read.
    generations: VecDeque<(JobId, u64)>,
}

impl AnalysisQueue {
    /// `waker` tells the owner loop an outcome is ready. It is called from the worker thread.
    pub(crate) fn new(waker: Arc<dyn Fn() + Send + Sync>) -> Self {
        let worker = Latest::new("luxforge-analysis", analyse);
        worker.set_waker(waker);
        Self {
            worker,
            started: None,
            activity: None,
            generations: VecDeque::new(),
        }
    }

    pub(crate) fn set_started(&mut self, started: Arc<dyn Fn(JobId) + Send + Sync>) {
        self.started = Some(started);
    }

    /// Publish every job submitted from now on to `board` as an `analysis.histogram` activity, from
    /// the moment the worker starts it to the moment it has an outcome. A queue without a board
    /// publishes nothing.
    pub(crate) fn set_activity(&mut self, board: Arc<ActivityBoard>) {
        self.activity = Some(board);
    }

    /// Hand a job to the worker, or into the one pending slot. Returns the job id that was
    /// displaced from that slot, which the caller marks `superseded`.
    pub(crate) fn submit(&mut self, job: AnalysisJob) -> Option<JobId> {
        let job_id = job.job_id.clone();
        let requested = self.worker.request(AnalysisTask {
            job,
            board: self.activity.clone(),
            started: self.started.clone(),
        });
        let displaced = requested.replaced.map(|(generation, task)| {
            self.generations.retain(|(_, other)| *other != generation);
            task.job.job_id
        });
        if self.generations.len() == HELD_JOBS {
            self.generations.pop_front();
        }
        self.generations.push_back((job_id, requested.generation));
        displaced
    }

    /// Nobody is interested in this job any more: drop it when it waits in the pending slot, or
    /// abandon it when it runs, which stops its render within a chunk and delivers it cancelled.
    /// Returns whether the worker still held it.
    pub(crate) fn withdraw(&mut self, job_id: &JobId) -> bool {
        let Some(generation) = self.generation_of(job_id) else {
            return false;
        };
        let pending = self.worker.pending_generation() == Some(generation);
        let withdrawn = self.worker.withdraw(generation);
        // A job dropped from the slot never delivers, so nothing will prune it; a running one
        // is pruned when its cancelled outcome is taken.
        if pending {
            self.generations.retain(|(_, other)| *other != generation);
        }
        withdrawn
    }

    /// Whether this job waits in the pending slot. A job submitted and not yet finished that does
    /// not wait there is the running one.
    pub(crate) fn is_pending(&self, job_id: &JobId) -> bool {
        self.generation_of(job_id)
            .is_some_and(|generation| self.worker.pending_generation() == Some(generation))
    }

    /// The oldest outcome the worker has delivered and the owner has not taken yet.
    pub(crate) fn poll(&mut self) -> Option<AnalysisOutcome> {
        let (generation, outcome) = self.worker.poll()?;
        // Outcomes arrive in generation order, so a job at or below this one that has not
        // delivered never will: it was displaced, or withdrawn before it started.
        self.generations.retain(|(_, other)| *other > generation);
        Some(outcome)
    }

    fn generation_of(&self, job_id: &JobId) -> Option<u64> {
        self.generations
            .iter()
            .find(|(held, _)| held == job_id)
            .map(|(_, generation)| *generation)
    }
}

impl std::fmt::Debug for AnalysisQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisQueue")
            .field("active", &self.worker.active_generation())
            .field("pending", &self.worker.pending_generation())
            .field("generations", &self.generations)
            .finish()
    }
}

/// One analysis job, on the analysis worker: render the effective recipe and reduce it, both under
/// the job's abandoned token.
fn analyse(
    task: AnalysisTask,
    running: &Running<'_, AnalysisTask, AnalysisOutcome>,
) -> Option<AnalysisOutcome> {
    let AnalysisTask {
        job,
        board,
        started,
    } = task;
    if let Some(started) = started {
        started(job.job_id.clone());
    }
    let AnalysisJob {
        job_id,
        identity,
        evaluation,
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
    let cancel = running.abandoned();
    let result = evaluation
        .exact(cancel)
        .and_then(|render| render.frame(identity.snapshot_id.clone()))
        .and_then(|raster| {
            let report = reduce(&raster.rgba, raster.width, raster.height, cancel);
            // No per-result raster is retained: the frame is released here, before the bounded
            // report travels back to the owner.
            drop(raster);
            report
        });
    // The frame is rendered; the artifacts the stack was bound with go with its evaluation.
    drop(evaluation);
    // The activity ends before the outcome is handed over, so a client that reads the job as
    // finished never still finds it listed as running.
    if let Some(activity) = activity {
        activity.finish(Outcome::of(&result));
    }
    Some(AnalysisOutcome { job_id, result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetId, Layer, Snapshot};
    use serde_json::json;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

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

    /// A 64 x 48 analysis of one held colour layer, whose render reaches `gate` once per row.
    fn held_job(gate: &Arc<luxforge_testbase::Gate>) -> AnalysisJob {
        let asset = AssetId::new();
        let entry = entry(&asset);
        let recipe = Recipe {
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::modules::HELD_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: json!({}),
                mask: None,
                artifacts: Vec::new(),
            }],
            ..entry.snapshot.recipe.clone()
        };
        let mut registry = crate::ModuleRegistry::builtin();
        registry
            .register(crate::modules::HeldModule::shared(gate.clone()))
            .expect("a valid holding module");
        let (width, height) = (64, 48);
        let evaluation = Evaluation::new(
            Arc::new(registry),
            crate::RenderContext::new(),
            crate::PreviewSource::Jpeg(crate::SourceImage {
                width,
                height,
                rgba: [40, 90, 160, 255].repeat((width * height) as usize).into(),
                fingerprint: "sha256:test".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            entry,
            recipe,
            None,
        );
        AnalysisJob {
            job_id: JobId::new(),
            identity: evaluation.identity().unwrap(),
            evaluation,
        }
    }

    /// The outcome of the one job a fresh queue ran, once the worker has woken the owner for it.
    fn outcome_after_wake(queue: &mut AnalysisQueue, wakes: &AtomicU64) -> AnalysisOutcome {
        luxforge_testbase::wait_until("the worker to wake the owner", || {
            wakes.load(Ordering::SeqCst) != 0
        });
        queue.poll().expect("the outcome the waker announced")
    }

    /// A withdrawn analysis stops within a chunk of its render instead of running to the end.
    ///
    /// The render is held at its gate inside the first chunk of its colour pass when the job is
    /// withdrawn, so the withdrawal lands mid-render. The gate counts every row the render
    /// evaluates: the withdrawn job ends `cancelled` through its token having evaluated fewer rows
    /// than the frame has, where the same job left alone evaluates every row and reports.
    #[test]
    fn a_withdrawn_analysis_stops_within_a_chunk_and_ends_cancelled() {
        let rows = 48;
        let board = crate::ActivityBoard::with_recent_threshold(Duration::ZERO);
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let wakes = Arc::new(AtomicU64::new(0));
        let counter = wakes.clone();
        let mut queue = AnalysisQueue::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        queue.set_activity(board.clone());
        gate.shut();
        let job = held_job(&gate);
        let job_id = job.job_id.clone();
        assert_eq!(queue.submit(job), None, "nothing was pending");
        assert!(!queue.is_pending(&job_id), "the worker took it");
        gate.wait_reached(1, "the render");
        assert!(queue.withdraw(&job_id), "the worker still held it");
        gate.open();
        let outcome = outcome_after_wake(&mut queue, &wakes);
        assert_eq!(outcome.job_id, job_id);
        let error = outcome
            .result
            .expect_err("a withdrawn analysis carries no report");
        assert_eq!(error.kind, ErrorKind::Cancelled);
        let evaluated = gate.reached();
        assert!(
            evaluated < rows,
            "the render stopped at the chunk after the withdrawal, {evaluated} of {rows} rows in"
        );
        let recent = board.snapshot().recent;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].entry.kind, "analysis.histogram");
        assert_eq!(recent[0].outcome, Outcome::Cancelled);

        // The same job left alone evaluates every row and reports.
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let wakes = Arc::new(AtomicU64::new(0));
        let counter = wakes.clone();
        let mut queue = AnalysisQueue::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let job = held_job(&gate);
        let job_id = job.job_id.clone();
        let _ = queue.submit(job);
        let outcome = outcome_after_wake(&mut queue, &wakes);
        assert_eq!(outcome.job_id, job_id);
        let report = outcome.result.expect("an analysis left alone reports");
        assert_eq!(report.r.iter().sum::<u64>(), rows * 64);
        assert_eq!(gate.reached(), rows, "every row was evaluated");
    }
}
