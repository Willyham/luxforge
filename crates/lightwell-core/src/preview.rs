use crate::{
    Cancel, EntryId, Error, ErrorKind, HistoryEntry, LinearImage, LinearSettings, ModuleRegistry,
    ProxyBounds, ProxyCache, ProxyKey, Raster, Recipe, SourceImage,
    analysis::{AnalysisIdentity, Report},
    render, render_cancellable, render_linear, render_linear_cancellable,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistorySelection {
    Current,
    Entry(EntryId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Zoom {
    Fit,
    Percent { value: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewState {
    pub zoom: Zoom,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

impl ViewState {
    pub fn set_zoom(&mut self, zoom: Zoom) -> Result<(), crate::Error> {
        if let Zoom::Percent { value } = zoom
            && (!value.is_finite() || !(10.0..=1600.0).contains(&value))
        {
            return Err(crate::Error::new(
                crate::ErrorKind::Validation,
                "zoom percent must be finite and within 10..=1600",
            ));
        }
        self.zoom = zoom;
        Ok(())
    }
    pub fn pan_to(&mut self, x: f32, y: f32) -> Result<(), crate::Error> {
        if !x.is_finite() || !y.is_finite() {
            return Err(crate::Error::new(
                crate::ErrorKind::Validation,
                "pan coordinates must be finite",
            ));
        }
        self.pan_x = x;
        self.pan_y = y;
        Ok(())
    }
    pub fn source_detail_required(&self) -> bool {
        matches!(self.zoom, Zoom::Percent { value } if value >= 100.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSession {
    pub selection: HistorySelection,
    pub view: ViewState,
    pub generation: u64,
}

impl Default for PreviewSession {
    fn default() -> Self {
        Self {
            selection: HistorySelection::Current,
            view: ViewState::default(),
            generation: 0,
        }
    }
}

impl PreviewSession {
    pub fn select(&mut self, selection: HistorySelection) -> u64 {
        self.selection = selection;
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
    pub fn return_current(&mut self) -> u64 {
        self.select(HistorySelection::Current)
    }
    pub fn can_edit(&self) -> bool {
        self.selection == HistorySelection::Current
    }
}

#[derive(Clone, Debug)]
pub enum PreviewSource {
    Jpeg(SourceImage),
    Raw {
        image: LinearImage,
        settings: LinearSettings,
    },
}

impl PreviewSource {
    pub fn orientation(&self) -> u8 {
        match self {
            Self::Jpeg(image) => image.orientation,
            Self::Raw { image, .. } => image.view().1,
        }
    }

    /// The source fingerprint every frame and every report is stamped with.
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::Jpeg(image) => &image.fingerprint,
            Self::Raw { image, .. } => image.fingerprint(),
        }
    }

    /// The content-stage dimensions a recipe is compiled against.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Jpeg(image) => (image.width, image.height),
            Self::Raw { image, .. } => (image.width(), image.height()),
        }
    }

    /// Render this stack, through the path the source interpretation asks for.
    pub fn render(
        &self,
        registry: &ModuleRegistry,
        snapshot_id: crate::SnapshotId,
        recipe: &Recipe,
    ) -> Result<Raster, Error> {
        match self {
            Self::Jpeg(image) => render(registry, image, snapshot_id, recipe),
            Self::Raw { image, settings } => {
                render_linear(registry, image, snapshot_id, recipe, *settings)
            }
        }
    }

    /// [`Self::render`] under a [`Cancel`] token, through the same two paths: the exact phase of a
    /// preview job, which a newer job stops within one row or chunk.
    pub fn render_cancellable(
        &self,
        registry: &ModuleRegistry,
        snapshot_id: crate::SnapshotId,
        recipe: &Recipe,
        cancel: &Cancel,
    ) -> Result<Raster, Error> {
        match self {
            Self::Jpeg(image) => render_cancellable(registry, image, snapshot_id, recipe, cancel),
            Self::Raw { image, settings } => {
                render_linear_cancellable(registry, image, snapshot_id, recipe, *settings, cancel)
            }
        }
    }

    /// One output pixel of this stack without rasterizing a frame, through the same two paths.
    pub fn sample(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        x: u32,
        y: u32,
    ) -> Result<crate::Sample, Error> {
        match self {
            Self::Jpeg(image) => crate::sample(registry, image, recipe, x, y),
            Self::Raw { image, settings } => {
                crate::sample_linear(registry, image, recipe, *settings, x, y)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreviewJob {
    pub source: PreviewSource,
    /// The entry this preview shows. Its identity and snapshot correlate the frame with history;
    /// what is rendered is [`PreviewJob::recipe`], which differs from the entry's own stack while a
    /// draft is open.
    pub entry: HistoryEntry,
    /// The providers the worker evaluates this stack with; shared, never rebuilt per job.
    pub registry: Arc<ModuleRegistry>,
    /// The stack to render: the entry's own recipe, or an open draft's effective recipe.
    pub recipe: Recipe,
    /// `Some(n)` renders only the first `n` layers of that recipe, which is how the desktop shows
    /// the input stage of the layer it is drafting. `None` renders the whole stack.
    pub layer_count: Option<usize>,
    /// The draft revision this recipe was planned from, for correlating a frame with the settings
    /// that produced it. `None` when no draft was involved.
    pub draft_revision: Option<u64>,
    /// Which evaluated image this frame is, computed exactly as an analysis job's identity is. It
    /// describes the whole planned stack, so a truncated job's identity is the stack it was planned
    /// from, not the prefix it renders — which is why a truncated job is never analysed.
    pub identity: AnalysisIdentity,
    /// Reduce the rendered raster into a [`Report`] and return it with the frame, so the displayed
    /// target needs no second render. Refused together with [`PreviewJob::layer_count`].
    pub analyse: bool,
    /// The physical pixels the display can show this frame in. `Some` asks for a proxy phase before
    /// the exact one; `None` is the exact path alone, as a percentage zoom at or above 100% takes.
    /// A proxy is only ever an offer: an ineligible stack, a scale of one or any failure building
    /// or rendering the proxy declines it in [`PreviewResult::proxy_declined`] and the exact phase
    /// runs unchanged.
    pub proxy: Option<ProxyBounds>,
}

/// Which of a job's two phases produced a result.
///
/// A job that asked for a proxy and got one sends [`PreviewPhase::Proxy`] first — the display-size
/// frame the desktop presents — and [`PreviewPhase::Exact`] second, under the same generation. Every
/// other job sends [`PreviewPhase::Exact`] alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewPhase {
    Proxy,
    Exact,
}

#[derive(Debug)]
pub struct PreviewResult {
    pub generation: u64,
    pub entry_id: EntryId,
    /// The identity of the job that produced this frame, so the desktop can submit the report under
    /// the identity a later `analysis.request` will look up.
    pub identity: AnalysisIdentity,
    /// The draft revision the rendered recipe was planned from, carried through from the job so a
    /// displayed frame correlates with the gesture settings that produced it.
    pub draft_revision: Option<u64>,
    pub result: Result<Raster, Error>,
    /// The exact reduction of the raster in `result`, when the job asked for it. `None` means the
    /// job did not ask, the render failed, or this is the proxy phase — a proxy raster is never
    /// reduced. It never means an empty histogram.
    pub report: Option<Report>,
    /// Which phase produced this frame.
    pub phase: PreviewPhase,
    /// The proxy source dimensions this frame was rendered against. `Some` only on a
    /// [`PreviewPhase::Proxy`] result; the exact phase renders the prepared source itself.
    pub proxy_dimensions: Option<(u32, u32)>,
    /// Why a job that asked for a proxy phase has none: the ineligible layer, a scale of one, or
    /// the failure that building or rendering the proxy returned. `None` on the proxy phase, and on
    /// an exact phase whose job either asked for no proxy or got one.
    pub proxy_declined: Option<String>,
    /// Whether this proxy frame's source was built by the worker rather than taken from the queue's
    /// cache. Always `false` on the exact phase.
    pub proxy_built: bool,
}

/// What the worker sends back: one result, and the proxy source it built for it, which the queue
/// inserts into its cache on the thread that owns it.
struct WorkerMessage {
    result: PreviewResult,
    built: Option<(ProxyKey, PreviewSource)>,
}

/// What the proxy phase of one job should do, decided on the thread that asked rather than on the
/// worker, because the cache lives in the queue and no worker can borrow it.
enum ProxyStep {
    /// The job asked for no proxy phase.
    Skipped,
    /// The job asked, and this is why it has none.
    Declined(String),
    /// Render this plan, against the cached source when the key already held one.
    Planned {
        key: ProxyKey,
        cached: Option<PreviewSource>,
    },
}

struct Active {
    generation: u64,
    receiver: Receiver<WorkerMessage>,
    /// Stops the proxy phase. Set by [`PreviewQueue::cancel`] only: a newer request never stops a
    /// proxy render, because that frame is newer than anything on screen and will be delivered.
    proxy_cancel: Cancel,
    /// Stops the exact phase. Set by [`PreviewQueue::cancel`] and by every superseding
    /// [`PreviewQueue::request`], so a full-resolution render nobody is waiting for stops competing
    /// for the shared Rayon pool with the job that replaced it.
    exact_cancel: Cancel,
}

/// The order results are delivered in: within one generation the proxy frame precedes the exact
/// one, and a generation never goes backwards.
fn rank(phase: PreviewPhase) -> u8 {
    match phase {
        PreviewPhase::Proxy => 0,
        PreviewPhase::Exact => 1,
    }
}

/// One active job and one replaceable pending job, results tagged with a generation.
///
/// # What is delivered
///
/// A completed result is delivered whenever it is newer than what the display already has, not
/// only when it belongs to the newest request. Under a sustained drag a render almost always
/// finishes after a newer job has been requested, so dropping every superseded result presents no
/// frames at all. The rule is therefore a floor and a monotone order:
///
/// - [`Self::cancel`] raises the floor to the generation it returns, so everything in flight at
///   that moment is stale. It is the only thing that invalidates an in-flight result, which is what
///   an asset or selection change needs.
/// - `poll` delivers a result whose generation is above the floor and whose `(generation, phase)`
///   is strictly after the last delivered one, so an older frame never follows a newer one on
///   screen and a job's exact phase still follows its own proxy phase.
/// - An exact phase that answered [`ErrorKind::Cancelled`] carries no frame; it is counted in
///   [`Self::cancelled_exact`] and never delivered.
///
/// Newest-wins survives where it belongs: a newer request replaces the pending job, so at most one
/// job waits and the newest value is the one that runs next.
#[derive(Default)]
pub struct PreviewQueue {
    generation: u64,
    active: Option<Active>,
    pending: Option<(u64, PreviewJob)>,
    /// One proxy source, keyed by source identity and plan. Bounded by construction: a new plan
    /// replaces the old entry rather than accumulating beside it.
    cache: ProxyCache,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    cancelled_exact: u64,
    /// Results at or below this generation are stale, whatever they carry.
    floor: u64,
    last_delivered: u64,
    last_delivered_rank: u8,
}

impl PreviewQueue {
    pub fn request(&mut self, job: PreviewJob) -> u64 {
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        // The superseded job's exact phase is stopped: nothing is waiting for that frame and it
        // would compete with the render that replaced it. Its proxy phase runs on, because that
        // frame is still newer than what is on screen and stopping it is what starves a drag.
        if let Some(active) = &self.active {
            active.exact_cancel.cancel();
        }
        if self.active.is_some() {
            self.pending = Some((generation, job));
        } else {
            self.start(generation, job);
        }
        generation
    }

    /// Abandon the preview: drop the pending job, stop both phases of the active one and raise the
    /// delivery floor, so no frame planned before this call reaches the display.
    pub fn cancel(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.pending = None;
        if let Some(active) = &self.active {
            active.proxy_cancel.cancel();
            active.exact_cancel.cancel();
        }
        self.floor = self.generation;
        self.generation
    }

    /// Call this after every result is sent, so nothing has to wake on a timer to find out. It runs
    /// on the worker thread, never on the catalog owner thread, and it must do nothing but post a
    /// message.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// How many exact phases have answered [`ErrorKind::Cancelled`] because a newer request
    /// superseded them. Superseded frames are dropped, so this is the only account of them.
    pub fn cancelled_exact(&self) -> u64 {
        self.cancelled_exact
    }

    /// The generation of the last result [`Self::poll`] delivered, so a caller can correlate what
    /// is on screen with the request that produced it. `0` before anything is delivered.
    pub fn last_delivered(&self) -> u64 {
        self.last_delivered
    }

    /// Whether this job has a proxy phase, and against which source.
    ///
    /// Cost is `O(layers)`: `proxy_eligible` reads stages and `proxy_plan` compiles the recipe, and
    /// neither reads a pixel. Building a proxy is frame work and stays on the worker below.
    fn plan_proxy(&self, job: &PreviewJob) -> ProxyStep {
        let Some(bounds) = job.proxy else {
            return ProxyStep::Skipped;
        };
        if job.layer_count.is_some() {
            // A truncated job renders a layer prefix, and the plan describes the whole stack's
            // output stage, so the prefix has no proxy phase at all.
            return ProxyStep::Declined(
                "a truncated preview renders a layer prefix, which has no proxy phase".into(),
            );
        }
        if let Err(error) = job.registry.proxy_eligible(&job.recipe) {
            return ProxyStep::Declined(error.detail);
        }
        match job.source.proxy_plan(&job.registry, &job.recipe, bounds) {
            Ok(Some(plan)) => {
                let key = ProxyKey {
                    identity: job.source.identity(),
                    plan,
                };
                let cached = self.cache.get(&key).cloned();
                ProxyStep::Planned { key, cached }
            }
            Ok(None) => ProxyStep::Declined(
                "the proxy scale is 1: the stage already fits the display bounds".into(),
            ),
            Err(error) => ProxyStep::Declined(error.detail),
        }
    }

    fn start(&mut self, generation: u64, job: PreviewJob) {
        let step = self.plan_proxy(&job);
        let proxy_cancel = Cancel::new();
        let exact_cancel = Cancel::new();
        let tokens = (proxy_cancel.clone(), exact_cancel.clone());
        let waker = self.waker.clone();
        // Two results per job at most, so the worker never blocks on the desktop draining the
        // proxy frame before it can answer with the exact one.
        let (sender, receiver) = sync_channel(2);
        std::thread::spawn(move || run(job, generation, step, tokens, sender, waker));
        self.active = Some(Active {
            generation,
            receiver,
            proxy_cancel,
            exact_cancel,
        });
    }

    pub fn is_busy(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }

    /// At most one result per call. A job's second result of the same generation stays queued for
    /// the next call; a stale result is dropped here rather than returned, and draining it is what
    /// lets a pending job start.
    ///
    /// The active job is held until its final — exact — result arrives or its channel disconnects,
    /// which is also what [`Self::is_busy`] reports. What counts as stale is on [`PreviewQueue`].
    pub fn poll(&mut self) -> Option<PreviewResult> {
        loop {
            let (generation, received) = match self.active.as_ref() {
                Some(active) => (active.generation, active.receiver.try_recv()),
                None => return None,
            };
            let message = match received {
                Ok(message) => message,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.finish_active();
                    return None;
                }
            };
            // The proxy this job built belongs to the queue, whether or not its frame is still
            // wanted: the next job at the same bounds is a hit either way.
            if let Some((key, source)) = message.built {
                self.cache.insert(key, source);
            }
            let result = message.result;
            let mut cancelled = false;
            if result.phase == PreviewPhase::Exact {
                cancelled = result
                    .result
                    .as_ref()
                    .is_err_and(|error| error.kind == ErrorKind::Cancelled);
                if cancelled {
                    self.cancelled_exact = self.cancelled_exact.saturating_add(1);
                }
                self.finish_active();
            }
            let rank = rank(result.phase);
            let newer = (generation, rank) > (self.last_delivered, self.last_delivered_rank);
            // A cancelled exact phase carries no frame at all: it is counted, never delivered.
            if !cancelled && generation > self.floor && newer {
                self.last_delivered = generation;
                self.last_delivered_rank = rank;
                return Some(result);
            }
        }
    }

    /// The active job has nothing further to send: release it and start whatever waited.
    fn finish_active(&mut self) {
        self.active = None;
        if let Some((generation, job)) = self.pending.take() {
            self.start(generation, job);
        }
    }
}

/// One preview job, on its own thread: the proxy phase when the job has one, then the exact phase.
///
/// The two tokens are `(proxy, exact)`. Both phases read a token per row or chunk; only the exact
/// one is stopped by a superseding request, so a drag keeps presenting proxy frames while the
/// full-resolution renders behind them are abandoned.
fn run(
    job: PreviewJob,
    generation: u64,
    step: ProxyStep,
    (proxy_cancel, exact_cancel): (Cancel, Cancel),
    sender: SyncSender<WorkerMessage>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let wake = || {
        if let Some(waker) = &waker {
            waker();
        }
    };
    let entry_id = job.entry.id.clone();
    let draft_revision = job.draft_revision;
    let snapshot_id = job.entry.snapshot.id.clone();
    // A truncated job copies the layer prefix only; the whole stack is rendered in place.
    let prefix = job.layer_count.map(|count| Recipe {
        format: job.recipe.format,
        layers: job.recipe.layers.iter().take(count).cloned().collect(),
    });
    let recipe = prefix.as_ref().unwrap_or(&job.recipe);

    // Nothing in the proxy phase is fatal. A plan, a build or a render that fails — including a
    // cancel — records its reason on the exact result and the exact phase runs as it always does,
    // so a job never loses its frame because the shortcut did not work out. Nothing is logged.
    let declined = match step {
        ProxyStep::Skipped => None,
        ProxyStep::Declined(reason) => Some(reason),
        ProxyStep::Planned { key, cached } => {
            let built = match cached {
                Some(source) => Ok((source, false)),
                None => job.source.proxy(key.plan).map(|source| (source, true)),
            };
            match built {
                Err(error) => Some(error.detail),
                Ok((source, fresh)) => {
                    let dimensions = (key.plan.width, key.plan.height);
                    match source.render_cancellable(
                        &job.registry,
                        snapshot_id.clone(),
                        &job.recipe,
                        &proxy_cancel,
                    ) {
                        Err(error) => Some(error.detail),
                        Ok(raster) => {
                            let message = WorkerMessage {
                                result: PreviewResult {
                                    generation,
                                    entry_id: entry_id.clone(),
                                    identity: job.identity.clone(),
                                    draft_revision,
                                    result: Ok(raster),
                                    // A proxy raster is never reduced: every number the histogram
                                    // and the clipping counters report is the exact phase's.
                                    report: None,
                                    phase: PreviewPhase::Proxy,
                                    proxy_dimensions: Some(dimensions),
                                    proxy_declined: None,
                                    proxy_built: fresh,
                                },
                                built: fresh.then_some((key, source)),
                            };
                            if sender.send(message).is_err() {
                                return;
                            }
                            wake();
                            None
                        }
                    }
                }
            }
        }
    };

    let rendered = job
        .source
        .render_cancellable(&job.registry, snapshot_id, recipe, &exact_cancel);
    // The histogram is reduced from the frame this worker just produced, in place and without a
    // second render or a copy. A failed reduction leaves no report rather than reporting zeroes; a
    // cancelled one means the job was superseded mid-reduce, and the frame it describes is as stale
    // as the reduction, so the phase answers cancelled rather than a frame nothing will adopt.
    let (result, report) = match rendered {
        Ok(raster) if job.analyse => {
            match crate::analysis::reduce_raster_cancellable(&raster, &exact_cancel) {
                Ok(report) => (Ok(raster), Some(report)),
                Err(error) if error.kind == ErrorKind::Cancelled => (Err(error), None),
                Err(_) => (Ok(raster), None),
            }
        }
        rendered => (rendered, None),
    };
    let sent = sender.send(WorkerMessage {
        result: PreviewResult {
            generation,
            entry_id,
            identity: job.identity,
            draft_revision,
            result,
            report,
            phase: PreviewPhase::Exact,
            proxy_dimensions: None,
            proxy_declined: declined,
            proxy_built: false,
        },
        built: None,
    });
    if sent.is_ok() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, BASIC_EFFECT, BoxRect, CropStage, EFFECT_FORMAT, Layer, LayerId, Orientation,
        PIXEL_EFFECT, RECIPE_FORMAT, Snapshot, SnapshotId, Transform,
    };
    use serde_json::json;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    /// Deadlines are generous on purpose: these tests assert order and content, never speed, and
    /// they run unoptimized in the workspace check.
    const DEADLINE: Duration = Duration::from_secs(120);

    fn entry(color: u8) -> PreviewJob {
        job(color, false)
    }

    fn job(color: u8, analyse: bool) -> PreviewJob {
        let asset = AssetId::new();
        let original = Snapshot::original(asset.clone());
        let snapshot = original.append(Layer::pixel(0, 0, [color, 0, 0])).unwrap();
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset,
            sequence: u64::from(color),
            action_id: "set-pixel".into(),
            label: "Pixel 0, 0".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        };
        let recipe = entry.snapshot.recipe.clone();
        let identity = AnalysisIdentity::of(
            &entry.asset_id.clone(),
            "test",
            &entry,
            &recipe,
            None,
            Some((1, 1)),
        )
        .unwrap();
        PreviewJob {
            source: PreviewSource::Jpeg(SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "test".into(),
                orientation: 1,
            }),
            registry: Arc::new(ModuleRegistry::builtin()),
            recipe,
            layer_count: None,
            draft_revision: None,
            identity,
            analyse,
            proxy: None,
            entry,
        }
    }

    /// The pending slot is still newest-wins: three rapid requests run at most two jobs, the second
    /// is replaced by the third, and the third is what the display ends on. The first job may or
    /// may not have finished before it was superseded; if it did, its frame is delivered, because a
    /// frame newer than what is on screen is never thrown away — that is what starves a drag.
    /// Deliveries are strictly increasing either way.
    #[test]
    fn newest_preview_wins_with_one_active_and_one_pending() {
        let mut queue = PreviewQueue::default();
        queue.request(entry(1));
        queue.request(entry(2));
        let wanted = queue.request(entry(3));
        let deadline = Instant::now() + DEADLINE;
        let mut delivered: Vec<u64> = Vec::new();
        loop {
            if let Some(result) = queue.poll() {
                assert!(
                    delivered
                        .last()
                        .is_none_or(|last| *last < result.generation),
                    "deliveries must strictly increase: {delivered:?} then {}",
                    result.generation
                );
                assert_eq!(result.generation, queue.last_delivered());
                delivered.push(result.generation);
                if result.generation == wanted {
                    assert_eq!(result.result.unwrap().pixel(0, 0), Some([3, 0, 0, 255]));
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the newest preview never came");
            std::thread::yield_now();
        }
        assert_eq!(delivered.last(), Some(&wanted));
        assert!(
            !delivered.contains(&2),
            "the second request was replaced in the pending slot and never ran: {delivered:?}"
        );
    }

    /// A job that finished before a newer request superseded it still has the newest frame anybody
    /// has seen, so it is delivered rather than dropped.
    #[test]
    fn a_superseded_job_that_already_finished_is_still_delivered() {
        let sends = Arc::new(AtomicU64::new(0));
        let counter = sends.clone();
        let mut queue = PreviewQueue::default();
        queue.set_waker(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let first = queue.request(entry(1));
        // The waker says the frame is in the channel, so the request below supersedes a job that
        // has already answered and cancels nothing.
        wait_until(&sends, 1, "the first job never answered");
        let second = queue.request(entry(2));
        let delivered = drain_until(&mut queue, second, PreviewPhase::Exact);
        assert_eq!(
            delivered,
            vec![(first, PreviewPhase::Exact), (second, PreviewPhase::Exact)],
            "a superseded but completed frame is delivered before the newer one"
        );
        assert_eq!(queue.cancelled_exact(), 0, "nothing was cancelled");
    }

    /// `cancel` is the only thing that invalidates an in-flight result: a frame planned before it
    /// never reaches the display, even when it is the only frame there is.
    #[test]
    fn a_result_older_than_the_cancel_floor_is_dropped() {
        let sends = Arc::new(AtomicU64::new(0));
        let counter = sends.clone();
        let mut queue = PreviewQueue::default();
        queue.set_waker(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        queue.request(entry(1));
        // The frame is finished and waiting in the channel: only the floor can drop it now.
        wait_until(&sends, 1, "the job never answered");
        queue.cancel();
        let deadline = Instant::now() + DEADLINE;
        while queue.is_busy() {
            assert!(queue.poll().is_none(), "a frame from before the cancel");
            assert!(Instant::now() < deadline, "the cancelled job never drained");
            std::thread::yield_now();
        }
        assert_eq!(queue.last_delivered(), 0, "nothing was ever delivered");
    }

    /// The preview worker reduces the frame it just rendered, so a displayed target needs no second
    /// render. The report must equal the reduction of that very raster, byte for byte.
    #[test]
    fn an_analysing_preview_returns_the_exact_reduction_of_the_frame_it_rendered() {
        let mut queue = PreviewQueue::default();
        let wanted = queue.request(job(9, true));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = queue.poll() {
                assert_eq!(result.generation, wanted);
                let raster = result.result.expect("a frame");
                assert_eq!(
                    result.report.expect("the job asked for a report"),
                    crate::analysis::reduce_raster(&raster).unwrap()
                );
                assert!(result.identity.has_output_stage());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the analysing preview never came"
            );
            std::thread::yield_now();
        }
        // A job that does not ask carries no report: `None` is "not asked", never empty counts.
        let wanted = queue.request(job(9, false));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = queue.poll() {
                assert_eq!(result.generation, wanted);
                assert!(result.report.is_none());
                break;
            }
            assert!(Instant::now() < deadline, "the plain preview never came");
            std::thread::yield_now();
        }
    }

    // ---------------------------------------------------------------------------------------
    // The proxy phase
    // ---------------------------------------------------------------------------------------

    /// A programmatically filled source, so a photo-shaped case costs an allocation and a fill and
    /// reads no file. The fingerprint is fixed, so two sources of the same size share a proxy cache
    /// identity exactly as two jobs over one prepared source do.
    fn synthetic(width: u32, height: u32) -> PreviewSource {
        let mut rgba = vec![0_u8; width as usize * height as usize * 4];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[index as u8, (index >> 5) as u8, (index >> 11) as u8, 255]);
        }
        PreviewSource::Jpeg(SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:preview-proxy-fixture".into(),
            orientation: 1,
        })
    }

    /// The stack the proxy design calls eligible: the one orientation layer, one colour-stage Basic
    /// layer and a 7 degree straightening crop fitted onto the turned stage.
    fn eligible_layers(width: u32, height: u32) -> Vec<Layer> {
        let stage = CropStage {
            width: height,
            height: width,
            angle: 7.0,
        };
        // The whole rotated box fitted about the centre: what a crop-fit commits. It touches the
        // rotated stage exactly, and re-rounding it at the proxy size is what the crop module's
        // covered rectangle exists for, so this stack proves the proxy phase on a real fitted crop.
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: 0.0,
            y: 0.0,
            width: box_width,
            height: box_height,
        });
        vec![
            Layer::orientation(Orientation::of(Transform::RotateRight)),
            Layer {
                id: LayerId::new(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 0.5, "contrast": 20.0}),
            },
            Layer::crop(fitted.normalized(&stage)),
        ]
    }

    fn bounds(width: u32, height: u32) -> ProxyBounds {
        ProxyBounds { width, height }
    }

    /// A job over a synthetic source with an explicit stack, built without the catalog: the queue
    /// is what is under test, not how a job is planned.
    fn stacked(
        width: u32,
        height: u32,
        layers: Vec<Layer>,
        proxy: Option<ProxyBounds>,
    ) -> PreviewJob {
        let asset = AssetId::new();
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers,
        };
        let snapshot = Snapshot {
            id: SnapshotId::new(),
            asset_id: asset.clone(),
            recipe: recipe.clone(),
        };
        let source = synthetic(width, height);
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 1,
            action_id: "test".into(),
            label: "Test".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        };
        let identity = AnalysisIdentity::of(
            &asset,
            source.fingerprint(),
            &entry,
            &recipe,
            None,
            Some((width, height)),
        )
        .unwrap();
        PreviewJob {
            source,
            registry: Arc::new(ModuleRegistry::builtin()),
            recipe,
            layer_count: None,
            draft_revision: None,
            identity,
            analyse: false,
            proxy,
            entry,
        }
    }

    fn wait_until(counter: &Arc<AtomicU64>, wanted: u64, what: &str) {
        let deadline = Instant::now() + DEADLINE;
        while counter.load(Ordering::Relaxed) < wanted {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::yield_now();
        }
    }

    /// Every result the active job has left to send, in order. The queue releases the active slot
    /// when the exact phase lands, so an idle queue with no pending job is the end of the job.
    fn drain_all(queue: &mut PreviewQueue) -> Vec<PreviewResult> {
        let deadline = Instant::now() + DEADLINE;
        let mut results = Vec::new();
        loop {
            if let Some(result) = queue.poll() {
                results.push(result);
            }
            if !queue.is_busy() {
                return results;
            }
            assert!(
                Instant::now() < deadline,
                "the preview worker never finished"
            );
            std::thread::yield_now();
        }
    }

    /// Poll until this generation's phase is delivered, collecting what came before it.
    fn drain_until(
        queue: &mut PreviewQueue,
        generation: u64,
        phase: PreviewPhase,
    ) -> Vec<(u64, PreviewPhase)> {
        let deadline = Instant::now() + DEADLINE;
        let mut delivered = Vec::new();
        loop {
            if let Some(result) = queue.poll() {
                delivered.push((result.generation, result.phase));
                if (result.generation, result.phase) == (generation, phase) {
                    return delivered;
                }
            }
            assert!(
                Instant::now() < deadline,
                "generation {generation} {phase:?} never arrived: {delivered:?}"
            );
            std::thread::yield_now();
        }
    }

    /// A job with display bounds smaller than its stage produces two frames under one generation:
    /// the proxy first, then the exact one. Each is byte for byte the render this test computes
    /// independently — the proxy against the exact downscale of the source, the exact one against
    /// the prepared source itself.
    #[test]
    fn a_job_with_bounds_yields_the_proxy_phase_then_the_exact_phase() {
        let display = bounds(40, 40);
        let job = stacked(64, 48, eligible_layers(64, 48), Some(display));
        let registry = job.registry.clone();
        let source = job.source.clone();
        let recipe = job.recipe.clone();
        let snapshot = job.entry.snapshot.id.clone();
        let plan = source
            .proxy_plan(&registry, &recipe, display)
            .expect("a plan")
            .expect("a proxy is worthwhile");

        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
        let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();

        assert_eq!(proxy.generation, generation);
        assert_eq!(exact.generation, generation);
        assert_eq!(proxy.phase, PreviewPhase::Proxy);
        assert_eq!(exact.phase, PreviewPhase::Exact);
        assert_eq!(proxy.proxy_dimensions, Some((plan.width, plan.height)));
        assert_eq!(exact.proxy_dimensions, None);
        assert_eq!(exact.proxy_declined, None, "the proxy phase ran");
        assert!(proxy.proxy_built, "nothing was cached before this job");
        // A proxy raster is never reduced, whatever the job asked for.
        assert!(proxy.report.is_none());

        let reference = source
            .proxy(plan)
            .expect("the exact downscale")
            .render(&registry, snapshot.clone(), &recipe)
            .expect("the recipe renders at proxy size");
        let frame = proxy.result.expect("a proxy frame");
        assert_eq!(
            (frame.width, frame.height),
            (reference.width, reference.height)
        );
        assert_eq!(
            frame.rgba.as_ref(),
            reference.rgba.as_ref(),
            "the proxy frame is the exact recipe over the exact downscale"
        );
        assert!(
            frame.width <= display.width && frame.height <= display.height,
            "the proxy frame fits the display bounds"
        );

        let proxy_size = (frame.width, frame.height);
        let reference = source
            .render(&registry, snapshot, &recipe)
            .expect("the exact render");
        let frame = exact.result.expect("an exact frame");
        assert_eq!(
            (frame.width, frame.height),
            (reference.width, reference.height)
        );
        assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref());
        assert!(
            frame.width > proxy_size.0 && frame.height > proxy_size.1,
            "the exact phase renders the prepared source, not the proxy: {:?} against {proxy_size:?}",
            (frame.width, frame.height)
        );
    }

    /// A newer request stops the exact phase of the job it replaced within a chunk, and that phase
    /// answers with no frame at all. The proxy phase of the older job is polled first, so the
    /// cancel lands inside the exact render rather than before it.
    #[test]
    fn a_newer_request_cancels_the_exact_phase_of_the_job_it_replaced() {
        let display = bounds(200, 200);
        let mut queue = PreviewQueue::default();
        let older = queue.request(stacked(
            1200,
            900,
            eligible_layers(1200, 900),
            Some(display),
        ));
        let first = drain_until(&mut queue, older, PreviewPhase::Proxy);
        assert_eq!(first, vec![(older, PreviewPhase::Proxy)]);

        let newer = queue.request(stacked(
            1200,
            900,
            eligible_layers(1200, 900),
            Some(display),
        ));
        let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
        assert_eq!(
            delivered.last(),
            Some(&(newer, PreviewPhase::Exact)),
            "the newer generation is what the display ends on"
        );
        assert!(
            delivered
                .windows(2)
                .all(|pair| pair[0].0 <= pair[1].0 && pair[0] != pair[1]),
            "deliveries never go backwards: {delivered:?}"
        );
        // The exact phase of the older job either finished before the cancel reached it — which is
        // vanishingly unlikely on a frame this size but is not forbidden — or it was cancelled and
        // counted. A cancelled phase is never delivered: it carries no frame.
        if !delivered.contains(&(older, PreviewPhase::Exact)) {
            assert!(
                queue.cancelled_exact() >= 1,
                "the superseded exact phase was neither delivered nor counted"
            );
        }
    }

    /// The three ways a job that offered bounds has no proxy phase, and the one way a job never
    /// offered them. Each yields exactly one exact frame, and each says why.
    #[test]
    fn a_job_without_a_proxy_phase_yields_one_exact_result_and_says_why() {
        let mut queue = PreviewQueue::default();

        let only = |queue: &mut PreviewQueue, job: PreviewJob| -> PreviewResult {
            queue.request(job);
            let mut results = drain_all(queue);
            assert_eq!(results.len(), 1, "one exact frame and nothing else");
            let result = results.pop().unwrap();
            assert_eq!(result.phase, PreviewPhase::Exact);
            assert!(result.result.is_ok(), "the exact path runs unchanged");
            result
        };

        // No bounds at all: the exact path, and nothing to decline.
        let result = only(&mut queue, stacked(64, 48, eligible_layers(64, 48), None));
        assert_eq!(result.proxy_declined, None);

        // An ineligible stack: the layer that made it so is named.
        let pixel = vec![Layer::pixel(0, 0, [9, 9, 9])];
        let result = only(&mut queue, stacked(64, 48, pixel, Some(bounds(8, 8))));
        let reason = result.proxy_declined.expect("a reason");
        assert!(reason.contains(PIXEL_EFFECT), "{reason}");
        assert!(reason.contains("layer 0"), "{reason}");

        // Bounds the stage already fits: there is no proxy smaller than the source to build.
        let large = stacked(64, 48, eligible_layers(64, 48), Some(bounds(4000, 4000)));
        let result = only(&mut queue, large);
        let reason = result.proxy_declined.expect("a reason");
        assert!(reason.contains("scale is 1"), "{reason}");

        // A truncated job renders a layer prefix, which has no proxy phase. It is never analysed
        // either: the owner refuses to plan a job that is both truncated and analysing, because the
        // prefix is not the stack the job's identity describes.
        let mut truncated = stacked(64, 48, eligible_layers(64, 48), Some(bounds(8, 8)));
        truncated.layer_count = Some(1);
        let result = only(&mut queue, truncated);
        let reason = result.proxy_declined.expect("a reason");
        assert!(reason.contains("truncated"), "{reason}");
        assert!(result.report.is_none(), "a truncated job carries no report");
    }

    /// The proxy source is built once and held by the queue: the next job at the same identity and
    /// the same bounds renders against the source already in hand.
    #[test]
    fn two_jobs_at_the_same_bounds_build_the_proxy_once() {
        let display = bounds(32, 32);
        let mut queue = PreviewQueue::default();

        queue.request(stacked(64, 48, eligible_layers(64, 48), Some(display)));
        let first = drain_all(&mut queue);
        assert!(first[0].proxy_built, "the first job builds the proxy");

        queue.request(stacked(64, 48, eligible_layers(64, 48), Some(display)));
        let second = drain_all(&mut queue);
        assert_eq!(second[0].phase, PreviewPhase::Proxy);
        assert!(
            !second[0].proxy_built,
            "the second job reuses the cached one"
        );
        assert_eq!(second[0].proxy_dimensions, first[0].proxy_dimensions);
        // Same source, same plan, same picture: the cached proxy is the built one.
        let cached = second[0].result.as_ref().expect("a proxy frame");
        let built = first[0].result.as_ref().expect("a proxy frame");
        assert_eq!(cached.rgba.as_ref(), built.rgba.as_ref());

        // Different bounds are a different plan and a miss, which is what a window resize is.
        queue.request(stacked(
            64,
            48,
            eligible_layers(64, 48),
            Some(bounds(24, 24)),
        ));
        let resized = drain_all(&mut queue);
        assert!(
            resized[0].proxy_built,
            "a resized window rebuilds the proxy"
        );
    }

    /// The waker is what replaces the preview poll timer: one call per result sent, on the worker
    /// thread, and never on the owner thread.
    #[test]
    fn the_waker_is_called_once_per_result() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();
        let owner = std::thread::current().id();
        let mut queue = PreviewQueue::default();
        queue.set_waker(Arc::new(move || {
            assert_ne!(
                std::thread::current().id(),
                owner,
                "the waker runs on the preview worker, never on the thread that asked"
            );
            counter.fetch_add(1, Ordering::Relaxed);
        }));

        queue.request(stacked(
            64,
            48,
            eligible_layers(64, 48),
            Some(bounds(32, 32)),
        ));
        assert_eq!(drain_all(&mut queue).len(), 2);
        wait_until(&calls, 2, "the two-phase job woke twice");

        queue.request(stacked(64, 48, eligible_layers(64, 48), None));
        assert_eq!(drain_all(&mut queue).len(), 1);
        wait_until(&calls, 3, "the exact-only job woke once");
        // A generous wait proves no extra call follows the last result.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn history_selection_and_view_are_read_only_validated_session_state() {
        let mut session = PreviewSession::default();
        let entry = EntryId::new();
        session.select(HistorySelection::Entry(entry.clone()));
        assert!(!session.can_edit());
        session
            .view
            .set_zoom(Zoom::Percent { value: 100.0 })
            .unwrap();
        assert!(session.view.source_detail_required());
        assert!(
            session
                .view
                .set_zoom(Zoom::Percent { value: f32::NAN })
                .is_err()
        );
        session.return_current();
        assert!(session.can_edit());
        assert_eq!(session.selection, HistorySelection::Current);
    }
}
