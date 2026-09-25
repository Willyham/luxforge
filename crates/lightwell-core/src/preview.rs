use crate::{
    Cancel, Component, ComponentId, ComponentMode, EntryId, Error, ErrorKind, HistoryEntry,
    LinearImage, LinearSettings, Mask, MaskId, ModuleRegistry, ProxyApproximation, ProxyBounds,
    ProxyCache, ProxyKey, Raster, Recipe, Render, RenderContext, RenderOptions, RenderSource,
    SourceImage,
    activity::{ActivityBoard, ActivitySpec, Outcome},
    analysis::{AnalysisIdentity, MAX_OVERLAY_CELLS, MaskOverlay, MaskPixels, Report},
    latest::{Latest, Running},
    mask::CompiledMask,
    modules::Stage,
    render,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

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

    /// Whether this source evaluates a RAW white balance its developed planes do not hold, through
    /// a [`crate::WhiteBalanceApproximation`]. Only the preview of an open draft is planned that
    /// way. Every frame rendered from such a source is approximate, at the proxy scale and at full
    /// size alike, and none is ever reduced into a report.
    pub fn approximate_white_balance(&self) -> bool {
        matches!(self, Self::Raw { settings, .. } if settings.white_balance.is_some())
    }

    /// The content-stage dimensions a recipe is compiled against.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Jpeg(image) => (image.width, image.height),
            Self::Raw { image, .. } => (image.width(), image.height()),
        }
    }

    /// The pixels a render of this source reads, borrowed.
    pub fn input(&self) -> RenderSource<'_> {
        self.into()
    }
}

impl<'a> From<&'a PreviewSource> for RenderSource<'a> {
    fn from(source: &'a PreviewSource) -> Self {
        match source {
            PreviewSource::Jpeg(image) => Self::Byte(image),
            PreviewSource::Raw { image, settings } => Self::Linear {
                image,
                settings: *settings,
            },
        }
    }
}

/// What a preview job asks the worker for beside the frame: the coverage grid of one mask, over the
/// frame that job renders, on a `cells_w × cells_h` display grid.
///
/// **Why a component *identity* and not an index.** Hovering a row of the component list shows that
/// row's own contribution, so one component's grid has to be obtainable on its own. The cheapest
/// honest way to ask for it is one more field on this request, because it costs nothing anywhere
/// else: the host derives a one-component mask and compiles it through the same [`CompiledMask`]
/// the whole mask goes through, so the row's overlay and the mask's overlay cannot disagree about
/// that component's field, and compiling one component is strictly cheaper than compiling all of
/// them. It is a [`ComponentId`] rather than a position because a position is not an identity: the
/// component list is reorderable, a hover and the frame that answers it are a request apart, and an
/// index that silently slid onto the neighbouring row would draw the wrong field with no way to
/// tell. A component the mask does not hold is refused by name instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskOverlayRequest {
    /// The mask to describe. It must be one this job's recipe holds.
    pub mask: MaskId,
    /// One component of that mask, on its own, or `None` for the whole composed mask.
    pub component: Option<ComponentId>,
    pub cells_w: u32,
    pub cells_h: u32,
}

/// The mask a component's own row describes: that one component alone.
///
/// Its mode is `add` because there is nothing before it to subtract from or intersect with, the
/// whole-mask amount and inversion are left out because they are the mask's modifiers and not the
/// row's, and the component's *own* inversion is kept because that is a control on the row. `None`
/// when the mask does not hold that component.
fn one_component(mask: &Mask, component: &ComponentId) -> Option<Mask> {
    let found = mask.components.iter().find(|held| &held.id == component)?;
    let alone = Component {
        mode: ComponentMode::Add,
        ..found.clone()
    };
    Some(Mask {
        id: mask.id.clone(),
        name: mask.name.clone(),
        amount: Mask::FULL_AMOUNT,
        invert: false,
        next_ordinal: BTreeMap::new(),
        components: vec![alone],
    })
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
    /// The budgets and the estimate store this job's renders share with every other evaluation
    /// its planner runs; shared, never rebuilt per job.
    pub context: RenderContext,
    /// The stack to render: the entry's own recipe, or an open draft's effective recipe, bound
    /// with the verified bytes of every artifact it lists, so the worker compiles it whatever the
    /// owner's cache evicts meanwhile.
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
    ///
    /// Ignored when the source approximates its white balance
    /// ([`PreviewSource::approximate_white_balance`]): an approximate frame is never reduced into a
    /// report, whatever the job asked, so every histogram and clipping count comes from an exact
    /// render.
    pub analyse: bool,
    /// The physical pixels the display can show this frame in. `Some` asks for a proxy phase before
    /// the exact one; `None` is the exact path alone, as a percentage zoom at or above 100% takes.
    /// A proxy is only ever an offer: an ineligible stack, a scale of one or any failure building
    /// or rendering the proxy declines it in [`PreviewResult::proxy_declined`] and the exact phase
    /// runs unchanged.
    pub proxy: Option<ProxyBounds>,
    /// Fill one mask's coverage grid beside the frame and return it with it, exactly as
    /// [`PreviewJob::analyse`] returns a [`Report`]. Set through
    /// [`PreviewJob::with_mask_overlay`], which is what validates it against this job's own stack.
    pub mask_overlay: Option<MaskOverlayRequest>,
}

impl PreviewJob {
    /// Ask this job's exact phase for one mask's coverage grid, validated against the stack this
    /// job renders.
    ///
    /// Validation happens here and not on the worker because the answer depends on the stack, and
    /// the stack is in hand: a mask or a component this recipe does not hold is a named
    /// `validation` refusal now rather than a silently absent overlay later. It costs
    /// `O(masks + components)` and reads no pixel, so the thread that plans a job may call it
    /// ([performance rule 5](../../docs/engineering/performance-rules.md#rules)).
    pub fn with_mask_overlay(mut self, request: MaskOverlayRequest) -> Result<Self, Error> {
        let mask = self
            .recipe
            .masks
            .iter()
            .find(|mask| mask.id == request.mask)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    format!(
                        "mask {} is not in the stack this preview renders",
                        request.mask
                    ),
                )
            })?;
        if let Some(component) = &request.component
            && !mask.components.iter().any(|held| &held.id == component)
        {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("mask {} holds no component {component}", mask.name),
            ));
        }
        if request.cells_w == 0 || request.cells_h == 0 {
            return Err(Error::new(
                ErrorKind::Validation,
                "a mask overlay needs a non-empty cell grid",
            ));
        }
        if request.cells_w > MAX_OVERLAY_CELLS || request.cells_h > MAX_OVERLAY_CELLS {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "a mask overlay of {}x{} cells exceeds the {MAX_OVERLAY_CELLS} cells a side the display overlay allows",
                    request.cells_w, request.cells_h
                ),
            ));
        }
        self.mask_overlay = Some(request);
        Ok(self)
    }
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
    /// job did not ask, the render failed, this is the proxy phase — a proxy raster is never
    /// reduced — or the frame approximates its white balance
    /// ([`Self::approximate_white_balance`]), which is never reduced either. It never means an
    /// empty histogram.
    pub report: Option<Report>,
    /// The coverage grid of the mask the job named, over the frame in `result` and under the same
    /// generation. `None` means the job did not ask, the render failed, this is the proxy phase, or
    /// the mask had nothing to describe. It never means a mask whose coverage happens to be zero
    /// everywhere: that is a grid of zeros, and this is its absence.
    pub mask_overlay: Option<MaskOverlay>,
    /// Why the grid the job asked for is not in `mask_overlay`, in the host's own words.
    ///
    /// A client that asked for an overlay and waits for its texture has to be able to stop waiting:
    /// the grid is refused for reasons that belong to the mask rather than to the frame — a mask
    /// whose coverage depends on the pixel it reads has no grid at all
    /// ([proposal P16](../../docs/design/range-study.md#proposals)) — and an absence with no reason
    /// beside it is indistinguishable from a grid still on its way. `None` means the job asked for
    /// no overlay, this is the proxy phase, the render itself failed, or a newer request is coming
    /// with its own grid; in the last case the wait is correct and this must stay empty.
    pub mask_overlay_absent: Option<String>,
    /// Which phase produced this frame.
    pub phase: PreviewPhase,
    /// The proxy source dimensions this frame was rendered against. `Some` only on a
    /// [`PreviewPhase::Proxy`] result; the exact phase renders the prepared source itself.
    pub proxy_dimensions: Option<(u32, u32)>,
    /// Why a job that asked for a proxy phase has none: the ineligible layer, a scale of one, or
    /// the failure that building or rendering the proxy returned. `None` on the proxy phase, and on
    /// an exact phase whose job either asked for no proxy or got one.
    pub proxy_declined: Option<String>,
    /// Whether this proxy frame's source was built for this job rather than taken from the
    /// worker's cache. Always `false` on the exact phase.
    pub proxy_built: bool,
    /// Whether this proxy frame is an approximation of the exact render at display size, and why:
    /// a spatial-stage layer whose neighbourhoods scale with the stage, a mask drawing a feature
    /// narrower than two proxy pixels, or both. Always the default — approximate in no way — on the
    /// exact phase, which is the frame every number comes from.
    pub proxy_approximation: ProxyApproximation,
    /// Whether this frame approximates a RAW white balance the developed planes do not hold — a
    /// drafted temperature or tint, previewed during its gesture before the release redevelops the
    /// mosaic ([`PreviewSource::approximate_white_balance`]). Set on **both** phases of such a job:
    /// the matrix is linear and the proxy's box filter is linear, so it applies to the proxy
    /// exactly as it does to the full frame, and neither phase is the exact picture. Such a job
    /// never carries a [`Self::report`], even when it asked for one.
    pub approximate_white_balance: bool,
    /// Milliseconds of wall-clock time the preview worker spent producing this phase's result, and
    /// nothing else.
    ///
    /// - [`PreviewPhase::Proxy`]: compiling the job's stack, planning the proxy from it, building
    ///   its source when this job built it ([`Self::proxy_built`]), and compiling and rendering the
    ///   recipe against it. A cache hit costs the compiles, the plan and the render.
    /// - [`PreviewPhase::Exact`]: rendering the prepared source, plus reducing the frame into
    ///   [`Self::report`] and filling [`Self::mask_overlay`]'s coverage grid when the job asked for
    ///   them. The job compiles its stack once for both phases, and that compile is counted here
    ///   only when no proxy frame came before. A proxy phase that was attempted and declined is not
    ///   counted here; it produced no frame.
    ///
    /// It excludes everything outside the worker's own work on this phase: the wait in the queue's
    /// pending slot, preparing or redeveloping the source on the source worker, the other phase of
    /// the same job, and handing the result to the display. It is measured on a failed or cancelled
    /// phase too, up to the moment it stopped. So it answers "how long did this picture take to
    /// render", not "how long after the request did it appear".
    pub render_ms: f64,
    /// Present only when the caller explicitly opted into phase diagnostics. Time from the queue
    /// request to the preview worker starting this job, which is the time it waited behind the
    /// active one. It never changes the default queue path.
    pub queue_wait_ms: Option<f64>,
}

impl PreviewResult {
    /// Whether this frame is approximate at all. The one word a client reads; the reason beside it
    /// says which of the two made it so.
    pub fn proxy_approximate(&self) -> bool {
        self.proxy_approximation.is_approximate()
    }

    /// Whether this is an exact phase that a newer request or [`PreviewQueue::cancel`] stopped: it
    /// carries no frame, only the fact that this generation has ended. A proxy phase is never
    /// delivered cancelled; a failed proxy is recorded on the exact result instead.
    pub fn cancelled(&self) -> bool {
        self.result
            .as_ref()
            .is_err_and(|error| error.kind == ErrorKind::Cancelled)
    }
}

/// One preview job as the worker receives it: the job, the activity board it is published on, and
/// the moment it was requested when the caller opted into phase timing.
struct PreviewTask {
    job: PreviewJob,
    board: Option<Arc<ActivityBoard>>,
    requested_at: Option<Instant>,
}

/// What one [`PreviewQueue::request_replacing`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Queued {
    /// The generation the new job, and both of its results, are tagged with.
    pub generation: u64,
    /// The job this request took the pending slot from. It never started and never will, so it
    /// delivers nothing at all: this answer is the only moment its end is known.
    pub replaced: Option<u64>,
}

/// What the proxy phase of one job should do. Decided on the worker, which owns the proxy cache,
/// at the start of the job.
enum ProxyStep {
    /// The job asked for no proxy phase.
    Skipped,
    /// The job asked, and this is why it has none.
    Declined(String),
    /// Render against the proxy source this key names, from the cache or built on a miss.
    Planned(ProxyKey),
}

/// The preview worker: one persistent [`Latest`] worker that renders each job's proxy phase and
/// then its exact phase, with one active job and one replaceable pending job, results tagged with a
/// generation. It owns the one cached proxy source, so planning a job's proxy phase, building its
/// source and caching it all happen on the worker.
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
/// - [`Self::poll`] delivers results in the order the worker produced them, which is
///   `(generation, phase)` order: jobs run one at a time and a pending job is always newer than the
///   active one, so an older frame never follows a newer one on screen and a job's exact phase
///   follows its own proxy phase.
/// - An exact phase that answered [`ErrorKind::Cancelled`] carries no frame, and is delivered all
///   the same, under the same rules, as that outcome ([`PreviewResult::cancelled`]). So every job
///   that starts delivers exactly one exact-phase outcome above the floor — a frame, a failure or
///   cancelled — and a caller waiting for one generation learns when it has ended.
///
/// Newest-wins survives where it belongs: a newer request replaces the pending job, so at most one
/// job waits and the newest value is the one that runs next. A replaced job never starts and has
/// nothing to deliver; [`Self::request_replacing`] names it.
///
/// # The two phases and the two tokens
///
/// The proxy phase reads the job's **abandoned** token and the exact phase its **superseded** one
/// ([`crate::latest`]). A newer request supersedes the active job, which stops its exact phase —
/// nothing is waiting for that full-resolution frame, and it would compete for the Rayon pool with
/// the render that replaced it — but leaves its proxy phase running, because that frame is still
/// newer than what is on screen and stopping it is what starves a drag. [`Self::cancel`] abandons
/// the job, which stops both.
///
/// # When the next job starts
///
/// The worker takes the pending job itself as soon as the active one has handed over its exact
/// phase, so the next proxy render never waits for the consumer to poll.
pub struct PreviewQueue {
    worker: Latest<PreviewTask, PreviewResult>,
    /// Where each job is published as a `preview.render` activity; `None` publishes nothing.
    activity: Option<Arc<ActivityBoard>>,
}

impl Default for PreviewQueue {
    fn default() -> Self {
        // One proxy source, keyed by source identity and plan, held by the worker alone. Bounded by
        // construction: a new plan replaces the old entry rather than accumulating beside it.
        let mut cache = ProxyCache::default();
        Self {
            worker: Latest::new("lightwell-preview", move |task, running| {
                run(&mut cache, task, running)
            }),
            activity: None,
        }
    }
}

impl PreviewQueue {
    /// Queue `job` as the newest request and return its generation. The active job's exact phase
    /// is stopped; its proxy phase runs on.
    pub fn request(&mut self, job: PreviewJob) -> u64 {
        self.request_replacing(job).generation
    }

    /// [`Self::request`], also naming the job it replaced in the pending slot. That job never
    /// starts and delivers nothing at all, so this answer is the only way to learn that it ended.
    /// It is given in the same step as the request: asking [`Self::pending_generation`] first
    /// would race the worker, which takes the pending job by itself when the active one ends.
    pub fn request_replacing(&mut self, job: PreviewJob) -> Queued {
        self.request_inner(job, None)
    }

    /// [`Self::request_replacing`] with opt-in timing from this call until the worker starts the
    /// job, reported as [`PreviewResult::queue_wait_ms`]. The other request methods do not read the
    /// clock.
    pub fn request_timed(&mut self, job: PreviewJob) -> (Queued, Instant) {
        let requested_at = Instant::now();
        (self.request_inner(job, Some(requested_at)), requested_at)
    }

    fn request_inner(&mut self, job: PreviewJob, requested_at: Option<Instant>) -> Queued {
        let requested = self.worker.request(PreviewTask {
            job,
            board: self.activity.clone(),
            requested_at,
        });
        Queued {
            generation: requested.generation,
            replaced: requested.replaced.map(|(generation, _)| generation),
        }
    }

    /// Abandon the preview: drop the pending job, stop both phases of the active one and raise the
    /// delivery floor, so no frame planned before this call reaches the display.
    pub fn cancel(&mut self) -> u64 {
        self.worker.cancel()
    }

    /// Call this after every result is handed over, so nothing has to wake on a timer to find out.
    /// It runs on the worker thread, never on the catalog owner thread, and it must do nothing but
    /// post a message.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.worker.set_waker(waker);
    }

    /// Publish every job requested from now on to `board` as a `preview.render` activity, from the
    /// moment the worker starts it to the end of its exact phase, with its phase as it moves from
    /// `proxy` to `exact`. The entry ends before the exact result is handed over, so by the time
    /// [`Self::poll`] delivers it the activity has already ended. A queue without a board
    /// publishes nothing.
    pub fn set_activity(&mut self, board: Arc<ActivityBoard>) {
        self.activity = Some(board);
    }

    /// The generation of the job waiting in the pending slot. It starts by itself when the active
    /// job has handed over its exact phase, so a caller that is about to request learns what that
    /// request replaced from [`Self::request_replacing`] instead.
    pub fn pending_generation(&self) -> Option<u64> {
        self.worker.pending_generation()
    }

    /// The generation of the last result [`Self::poll`] delivered, so a caller can correlate its
    /// frames and outcomes with the request that produced them. `0` before anything is delivered.
    pub fn last_delivered(&self) -> u64 {
        self.worker.last_delivered()
    }

    /// Whether a job is active or pending, or a result waits for [`Self::poll`].
    pub fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }

    /// Whether a result waits for [`Self::poll`].
    pub fn ready(&self) -> bool {
        self.worker.ready()
    }

    /// The oldest result waiting, or nothing. What counts as stale is on [`PreviewQueue`]; a stale
    /// result is never handed over at all.
    pub fn poll(&mut self) -> Option<PreviewResult> {
        self.worker.poll().map(|(_, result)| result)
    }
}

/// Whether this job has a proxy phase, and against which source.
///
/// Cost is `O(layers)`: `proxy_eligible` reads stages and the plan reads the output stage of the
/// job's exact compilation, which it does not repeat. Neither reads a pixel. It runs on the preview
/// worker, as does building the proxy itself.
fn plan_proxy(job: &PreviewJob, exact: &Result<Render<'_>, Error>) -> ProxyStep {
    let Some(bounds) = job.proxy else {
        return ProxyStep::Skipped;
    };
    if job.layer_count.is_some() {
        // A truncated job renders a layer prefix, and the plan describes the whole stack's output
        // stage, so the prefix has no proxy phase at all.
        return ProxyStep::Declined(
            "a truncated preview renders a layer prefix, which has no proxy phase".into(),
        );
    }
    if let Err(error) = job.registry.proxy_eligible(&job.recipe) {
        return ProxyStep::Declined(error.detail);
    }
    match exact.as_ref().map(|exact| exact.proxy_plan(bounds)) {
        Ok(Some(plan)) => ProxyStep::Planned(ProxyKey {
            identity: job.source.identity(),
            plan,
        }),
        Ok(None) => ProxyStep::Declined(
            "the proxy scale is 1: the stage already fits the display bounds".into(),
        ),
        Err(error) => ProxyStep::Declined(error.detail.clone()),
    }
}

/// One preview job, on the preview worker: the proxy phase when the job has one, handed over as
/// soon as it is rendered, then the exact phase, returned as the job's last result.
///
/// The proxy phase reads the job's `abandoned` token and the exact phase its `superseded` one, so
/// a drag keeps presenting proxy frames while the full-resolution renders behind them are
/// abandoned.
fn run(
    cache: &mut ProxyCache,
    task: PreviewTask,
    running: &Running<'_, PreviewTask, PreviewResult>,
) -> Option<PreviewResult> {
    let PreviewTask {
        job,
        board,
        requested_at,
    } = task;
    let queue_wait_ms = requested_at.map(|requested| requested.elapsed().as_secs_f64() * 1000.0);
    let generation = running.generation();
    let (proxy_cancel, exact_cancel) = (running.abandoned(), running.superseded());
    // One activity spans both phases. A job abandoned mid-way, its results stale before its exact
    // phase could be handed over, drops the guard, which records it as cancelled.
    let activity = board.map(|board| {
        board.begin(ActivitySpec {
            kind: "preview.render",
            label: "Rendering preview",
            detail: None,
            asset_id: Some(job.entry.asset_id.clone()),
            job_id: None,
        })
    });
    let entry_id = job.entry.id.clone();
    let draft_revision = job.draft_revision;
    let snapshot_id = job.entry.snapshot.id.clone();
    // Both phases of a job share its source, so both are approximate or neither is. An approximate
    // frame is never reduced, which is the rule on `PreviewJob::analyse`.
    let approximate_white_balance = job.source.approximate_white_balance();
    let analyse = job.analyse && !approximate_white_balance;
    // A truncated job copies the layer prefix only; the whole stack is rendered in place.
    let prefix = job.layer_count.map(|count| Recipe {
        format: job.recipe.format,
        layers: job.recipe.layers.iter().take(count).cloned().collect(),
        // The mask table belongs to the recipe, not to the prefix: a truncated stack keeps it so a
        // masked layer inside the prefix still finds the mask it names.
        masks: job.recipe.masks.clone(),
        strokes: job.recipe.strokes.clone(),
        artifacts: job.recipe.artifacts.clone(),
    });
    let recipe = prefix.as_ref().unwrap_or(&job.recipe);

    // The job's one compilation at the exact stage. The proxy plan reads its output stage, the
    // exact phase renders it and the coverage grid composes its geometry tail, so none of them
    // compiles the stack again. It is charged to the first phase that hands over a frame.
    let compile_started = Instant::now();
    let exact = render(
        &job.registry,
        job.source.input(),
        recipe,
        RenderOptions::exact(exact_cancel),
        &job.context,
    );
    let mut compile_ms = Some(milliseconds_since(compile_started));

    // Nothing in the proxy phase is fatal. A plan, a build or a render that fails — including a
    // cancel — records its reason on the exact result and the exact phase runs as it always does,
    // so a job never loses its frame because the shortcut did not work out. Nothing is logged.
    // The proxy phase's own clock: the plan, the build when this job builds, then the render.
    let started = Instant::now();
    let declined = match plan_proxy(&job, &exact) {
        ProxyStep::Skipped => None,
        ProxyStep::Declined(reason) => Some(reason),
        ProxyStep::Planned(key) => {
            if let Some(activity) = &activity {
                activity.phase("proxy");
            }
            // The cache holds pixels; the settings a RAW development layer asks for come from this
            // job's recipe, so a drafted exposure renders against the cached planes.
            let built = match cache.get(&key) {
                Some(cached) => Ok((cached.with_settings_of(&job.source), false)),
                None => job.source.proxy(key.plan).map(|source| (source, true)),
            };
            match built {
                Err(error) => Some(error.detail),
                Ok((source, fresh)) => {
                    let dimensions = (key.plan.width, key.plan.height);
                    // The proxy stage's one compilation: the frame and the reason it is
                    // approximate both come from it, so what is reported and what is drawn cannot
                    // disagree.
                    let rendered = render(
                        &job.registry,
                        source.input(),
                        &job.recipe,
                        RenderOptions::proxy(proxy_cancel),
                        &job.context,
                    )
                    .and_then(|proxy| {
                        Ok((proxy.frame(snapshot_id.clone())?, proxy.approximation()))
                    });
                    // The proxy this job built belongs to the worker whether or not its frame is
                    // still wanted: the next job at the same bounds is a hit either way.
                    if fresh {
                        cache.insert(key, source);
                    }
                    match rendered {
                        Err(error) => Some(error.detail),
                        Ok((raster, proxy_approximation)) => {
                            let proxy = PreviewResult {
                                generation,
                                entry_id: entry_id.clone(),
                                identity: job.identity.clone(),
                                draft_revision,
                                result: Ok(raster),
                                // A proxy raster is never reduced: every number the histogram and
                                // the clipping counters report is the exact phase's. The mask
                                // overlay rides with the same frame for the same reason — the
                                // proxy phase is what a drag presents, and the histogram, the
                                // overlays and the 100% view follow the exact one (performance
                                // rule 11).
                                report: None,
                                mask_overlay: None,
                                mask_overlay_absent: None,
                                phase: PreviewPhase::Proxy,
                                proxy_dimensions: Some(dimensions),
                                proxy_declined: None,
                                proxy_built: fresh,
                                // Read from the compilation at exactly the dimensions this frame
                                // was rendered against, because whether a mask draws a feature the
                                // proxy's pixel grid can resolve is a fact about that grid.
                                proxy_approximation,
                                approximate_white_balance,
                                render_ms: compile_ms.take().unwrap_or(0.0)
                                    + milliseconds_since(started),
                                queue_wait_ms,
                            };
                            // A proxy nobody will ever see — the queue was cancelled or dropped —
                            // means the exact phase is not wanted either.
                            if !running.send(proxy) {
                                return None;
                            }
                            None
                        }
                    }
                }
            }
        }
    };

    if let Some(activity) = &activity {
        activity.phase("exact");
    }
    // The exact phase's own clock starts here, after the proxy phase has handed over its frame, so
    // the two phases' times never overlap and neither includes the other.
    let started = Instant::now();
    let rendered = exact
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|exact| exact.frame(snapshot_id));
    // The histogram is reduced from the frame this worker just produced, in place and without a
    // second render or a copy. A failed reduction leaves no report rather than reporting zeroes; a
    // cancelled one means the job was superseded mid-reduce, and the frame it describes is as stale
    // as the reduction, so the phase answers cancelled rather than a frame nothing will adopt.
    let (result, report) = match rendered {
        Ok(raster) if analyse => {
            match crate::analysis::reduce_raster_cancellable(&raster, exact_cancel) {
                Ok(report) => (Ok(raster), Some(report)),
                Err(error) if error.kind == ErrorKind::Cancelled => (Err(error), None),
                Err(_) => (Ok(raster), None),
            }
        }
        rendered => (rendered, None),
    };
    // The coverage grid is filled beside the frame it describes, from the very stack that produced
    // it, so the two travel together under one generation. It reads no pixel of that frame and
    // allocates one byte per display cell; a mask that reads pixels reads them from the input of its
    // own first bound layer instead, one point query per cell.
    let (mask_overlay, mask_overlay_absent) = match (&result, &exact, &job.mask_overlay) {
        (Ok(_), Ok(exact), Some(request)) => mask_overlay_for(
            &job.registry,
            exact,
            recipe,
            request,
            exact_cancel,
            &job.context,
        ),
        _ => (None, None),
    };
    let render_ms = compile_ms.unwrap_or(0.0) + milliseconds_since(started);
    // A superseded or abandoned exact phase answers `Cancelled`, so its activity ends cancelled.
    if let Some(activity) = activity {
        activity.finish(Outcome::of(&result));
    }
    Some(PreviewResult {
        generation,
        entry_id,
        identity: job.identity,
        draft_revision,
        result,
        report,
        mask_overlay,
        mask_overlay_absent,
        phase: PreviewPhase::Exact,
        proxy_dimensions: None,
        proxy_declined: declined,
        proxy_built: false,
        proxy_approximation: ProxyApproximation::default(),
        approximate_white_balance,
        render_ms,
        queue_wait_ms,
    })
}

/// One mask's coverage grid over the frame `recipe` just produced against `source`.
///
/// `recipe` is the stack that was rendered — a truncated job's prefix, when it had one — because
/// the grid describes the frame it arrives with and a prefix has its own geometry tail. The mask
/// table travels with a prefix, so a mask is still found there.
///
/// Every reason there is no grid is a reason there is none to draw, never a silently empty one, and
/// the reason travels with the frame in the second half of the pair — the host's own words, for a
/// client that asked for an overlay and would otherwise wait for a texture nothing will fill. The
/// mask or component the request named was validated against this stack when the job was planned,
/// and the stack rendered, so compiling it cannot fail here for a reason the frame did not already
/// fail for. Two absences carry **no** reason on purpose: a mask with nothing to describe, which
/// [`crate::analysis::coverage_grid`] decides in closed form and which a grid of zeros would
/// misreport, and a cancel, where a newer request is already on its way with its own grid and
/// waiting for it is correct.
fn mask_overlay_for(
    registry: &ModuleRegistry,
    frame: &Render<'_>,
    recipe: &Recipe,
    request: &MaskOverlayRequest,
    cancel: &Cancel,
    context: &RenderContext,
) -> (Option<MaskOverlay>, Option<String>) {
    let refused = |error: Error| match error.kind {
        ErrorKind::Cancelled => (None, None),
        _ => (None, Some(error.detail)),
    };
    let Some(held) = recipe.masks.iter().find(|mask| mask.id == request.mask) else {
        return (
            None,
            Some(format!(
                "mask {} is not in the stack this frame was rendered from",
                request.mask
            )),
        );
    };
    let derived;
    let mask = match &request.component {
        None => held,
        Some(component) => match one_component(held, component) {
            Some(one) => {
                derived = one;
                &derived
            }
            None => {
                return (
                    None,
                    Some(format!("mask {} holds no component {component}", held.name)),
                );
            }
        },
    };
    // `O(layers)`: it composes the geometry tail of the stack already compiled for this frame and
    // reads no pixel.
    let transform = match frame.transform() {
        Ok(transform) => transform,
        Err(error) => return refused(error),
    };
    let stage = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    let compiled = match CompiledMask::new(mask, stage, &recipe.strokes) {
        Ok(compiled) => compiled,
        Err(error) => return refused(error),
    };
    // A value-based component is answered on the pixel the masked operation receives, which is the
    // input of the mask's **first bound layer** — the rule `mask::commands::input_layer_index`
    // states once for everything that reads a pixel through a mask, and which the colour-constrained
    // brush's seed and `mask.sample-input` already read, so the overlay and the seed cannot disagree
    // about which pixel a mask reads. The prefix is compiled once and asked once per cell.
    let input;
    let unavailable;
    let pixels = if !compiled.reads_pixels() {
        // Position-only: no operation is needed and none is looked for, so a geometric grid costs
        // exactly what it did before a value-based component existed.
        MaskPixels::Unavailable("this mask reads no pixel")
    } else {
        match crate::mask::commands::input_layer_index(recipe, &request.mask).and_then(|layer| {
            crate::render::layer_input(registry, frame.source(), recipe, layer, context)
        }) {
            // Two different stages would be two different coverage fields, and `coverage_grid`
            // refuses that mismatch for the frame; it is refused here for the operation, in the same
            // voice, rather than read at coordinates of another stage.
            Ok(prefix) if prefix.stage() != stage => {
                unavailable = format!(
                    "the masked operation receives a {}x{} stage and this mask is compiled against \
                     {}x{}",
                    prefix.stage().width,
                    prefix.stage().height,
                    stage.width,
                    stage.height
                );
                MaskPixels::Unavailable(&unavailable)
            }
            Ok(prefix) => {
                input = prefix;
                MaskPixels::Input(&input)
            }
            // No layer is bound to this mask, or its prefix holds a spatial layer, or it does not
            // compile: in every case there is no operation whose input this grid can read, and the
            // refusal's own sentence says which and what to do about it.
            Err(error) => {
                unavailable = error.detail;
                MaskPixels::Unavailable(&unavailable)
            }
        }
    };
    let coverage = match crate::analysis::coverage_grid(
        &compiled,
        &transform,
        request.cells_w,
        request.cells_h,
        pixels,
        cancel,
    ) {
        Ok(Some(coverage)) => coverage,
        Ok(None) => return (None, None),
        Err(error) => return refused(error),
    };
    (
        Some(MaskOverlay {
            mask: request.mask.clone(),
            component: request.component.clone(),
            cells_w: request.cells_w,
            cells_h: request.cells_h,
            coverage,
        }),
        None,
    )
}

/// Wall-clock milliseconds since `started`, as [`PreviewResult::render_ms`] reports them.
fn milliseconds_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, BASIC_EFFECT, BoxRect, CropStage, EFFECT_FORMAT, Layer, LayerId, Orientation,
        PIXEL_EFFECT, RECIPE_FORMAT, Snapshot, SnapshotId, Transform,
    };
    use crate::{Component, ComponentMode};
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
            context: crate::render::testing::context().clone(),
            recipe,
            layer_count: None,
            draft_revision: None,
            identity,
            analyse,
            proxy: None,
            mask_overlay: None,
            entry,
        }
    }

    /// [`entry`] with one held colour layer after its pixel layer, so its render waits at `gate`
    /// while the gate is shut and otherwise renders the same picture.
    fn held_entry(gate: &Arc<crate::modules::RenderGate>, color: u8) -> PreviewJob {
        let mut job = entry(color);
        job.recipe.layers.push(Layer {
            id: LayerId::new(),
            effect_id: crate::modules::HELD_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            artifacts: Vec::new(),
            mask: None,
        });
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(crate::modules::HeldModule::shared(gate.clone()))
            .expect("a valid holding module");
        job.registry = Arc::new(registry);
        job
    }

    /// The pending slot is still newest-wins: three rapid requests run at most two jobs, the second
    /// is replaced by the third, and the third is what the display ends on. The first job is held
    /// at its gate inside the one chunk of its one-pixel render, so it is still running when the
    /// others are requested, and past the last point that render reads its token: it finishes
    /// although superseded, and its frame is delivered, because a frame newer than what is on
    /// screen is never thrown away — that is what starves a drag. Each job that started delivers
    /// one exact outcome, in increasing order, and the replaced one, which the request that
    /// replaced it names, delivers nothing.
    #[test]
    fn newest_preview_wins_with_one_active_and_one_pending() {
        let gate = crate::modules::RenderGate::open_gate();
        let mut queue = PreviewQueue::default();
        gate.shut();
        let first = queue.request(held_entry(&gate, 1));
        assert_eq!(queue.pending_generation(), None, "the first job started");
        // Inside the render, past its first check: a job superseded before it begins rendering
        // stops at once and would let the next one start.
        gate_until(&gate, 1, "the first render never reached its gate");
        let replaced = queue.request(held_entry(&gate, 2));
        assert_eq!(queue.pending_generation(), Some(replaced));
        let Queued {
            generation: wanted,
            replaced: displaced,
        } = queue.request_replacing(held_entry(&gate, 3));
        assert_eq!(
            displaced,
            Some(replaced),
            "the request names what it replaced"
        );
        assert_eq!(
            queue.pending_generation(),
            Some(wanted),
            "the third request replaced the second"
        );
        gate.open();
        let deadline = Instant::now() + DEADLINE;
        let mut delivered: Vec<(u64, bool)> = Vec::new();
        loop {
            if let Some(result) = queue.poll() {
                assert!(
                    delivered
                        .last()
                        .is_none_or(|(last, _)| *last < result.generation),
                    "deliveries must strictly increase: {delivered:?} then {}",
                    result.generation
                );
                assert_eq!(result.generation, queue.last_delivered());
                assert_eq!(
                    result.phase,
                    PreviewPhase::Exact,
                    "no job had a proxy phase"
                );
                delivered.push((result.generation, result.cancelled()));
                if result.generation == wanted {
                    assert_eq!(result.result.unwrap().pixel(0, 0), Some([3, 0, 0, 255]));
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the newest preview never came");
            std::thread::yield_now();
        }
        assert_eq!(
            delivered,
            vec![(first, false), (wanted, false)],
            "the first job's frame, then the third's; the second was replaced in the pending slot \
             and never ran"
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
            vec![
                (first, PreviewPhase::Exact, false),
                (second, PreviewPhase::Exact, false)
            ],
            "a superseded but completed frame is delivered before the newer one, and nothing was \
             cancelled"
        );
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

        // An exact phase that `cancel` stopped mid-render answers cancelled, and that outcome is
        // at the floor too: the caller that raised it already knows the generation has ended.
        queue.request(stacked(1200, 900, eligible_layers(1200, 900), None));
        queue.cancel();
        let deadline = Instant::now() + DEADLINE;
        while queue.is_busy() {
            assert!(queue.poll().is_none(), "an outcome from before the cancel");
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
                mask: None,
                artifacts: Vec::new(),
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
        stacked_with_masks(width, height, layers, Vec::new(), proxy)
    }

    /// [`stacked`] over a stack that carries a mask table, which is where a masked layer's `mask`
    /// reference is resolved.
    fn stacked_with_masks(
        width: u32,
        height: u32,
        layers: Vec<Layer>,
        masks: Vec<Mask>,
        proxy: Option<ProxyBounds>,
    ) -> PreviewJob {
        let asset = AssetId::new();
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
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
            context: crate::render::testing::context().clone(),
            recipe,
            layer_count: None,
            draft_revision: None,
            identity,
            analyse: false,
            proxy,
            mask_overlay: None,
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

    /// Poll until this generation's phase is delivered, collecting what came before it: each
    /// delivery's generation, phase and whether it was cancelled.
    fn drain_until(
        queue: &mut PreviewQueue,
        generation: u64,
        phase: PreviewPhase,
    ) -> Vec<(u64, PreviewPhase, bool)> {
        let deadline = Instant::now() + DEADLINE;
        let mut delivered = Vec::new();
        loop {
            if let Some(result) = queue.poll() {
                delivered.push((result.generation, result.phase, result.cancelled()));
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
        let requested = Instant::now();
        let generation = queue.request(job);
        let results = drain_all(&mut queue);
        let lifetime_ms = requested.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
        let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();

        // Each phase reports its own worker time: finite, and inside the job's own lifetime. The
        // two clocks run one after the other on the worker, so together they fit inside it too —
        // neither phase counts the other, and neither counts anything before the request.
        for (phase, ms) in [("proxy", proxy.render_ms), ("exact", exact.render_ms)] {
            assert!(
                ms.is_finite() && ms >= 0.0 && ms <= lifetime_ms,
                "the {phase} phase reports {ms} ms of a {lifetime_ms} ms job"
            );
        }
        assert!(
            proxy.render_ms + exact.render_ms <= lifetime_ms,
            "the phases overlap: {} + {} ms of a {lifetime_ms} ms job",
            proxy.render_ms,
            exact.render_ms
        );

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

    /// A job compiles its stack once at each stage it renders at: once at the exact stage, whose
    /// compilation plans the proxy, renders the exact frame and gives the coverage grid its
    /// geometry, and once at the proxy stage, whose compilation renders the proxy frame and says
    /// whether it is approximate. A job without a proxy phase compiles once.
    #[test]
    fn a_preview_job_compiles_its_stack_once_per_stage_it_renders_at() {
        let mask = gradient_mask(0.5);
        let request = MaskOverlayRequest {
            mask: mask.id.clone(),
            component: None,
            cells_w: 8,
            cells_h: 6,
        };
        let mut queue = PreviewQueue::default();
        for (proxy, phases, compiles) in [(Some(bounds(40, 40)), 2, 2), (None, 1, 1)] {
            let mut job =
                stacked_with_masks(64, 48, masked_basic(&mask), vec![mask.clone()], proxy)
                    .with_mask_overlay(request.clone())
                    .expect("the stack holds the mask");
            let context = RenderContext::new();
            job.context = context.clone();
            queue.request(job);
            let results = drain_all(&mut queue);
            assert_eq!(results.len(), phases, "{proxy:?}");
            let exact = results.last().expect("an exact phase");
            assert!(
                exact.result.is_ok() && exact.mask_overlay.is_some(),
                "{proxy:?}"
            );
            assert_eq!(context.compiles(), compiles, "{proxy:?}");
        }
    }

    /// A mask whose narrowest feature spans `length x stage.height` pixels. The gradient runs down
    /// the frame, so its ramp is `length` mask-space units — the one number the thin-feature rule
    /// reads.
    fn gradient_mask(length: f64) -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.5, "y0": 0.5 - length / 2.0, "x1": 0.5, "y1": 0.5 + length / 2.0}),
        ));
        mask
    }

    /// One Basic layer bound to `mask`, which is the masked colour stack every assertion below
    /// renders. No geometry, so the stage a proxy is fitted into is the source itself.
    fn masked_basic(mask: &Mask) -> Vec<Layer> {
        vec![Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": 0.8, "contrast": 25.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        }]
    }

    /// The same stack without its mask reference, for the comparisons that have to show the mask
    /// is doing something.
    fn without_masks(recipe: &Recipe) -> Recipe {
        Recipe {
            format: recipe.format,
            masks: Vec::new(),
            layers: recipe
                .layers
                .iter()
                .map(|layer| Layer {
                    mask: None,
                    ..layer.clone()
                })
                .collect(),
            ..Recipe::default()
        }
    }

    /// The delivered proxy contract over a **masked** stack: the recipe is proxy eligible, the job
    /// yields both phases, and the proxy frame is byte for byte the exact recipe rendered against
    /// the exact downscale of the source.
    ///
    /// This is the assertion
    /// [`a_job_with_bounds_yields_the_proxy_phase_then_the_exact_phase`] makes, over a stack whose
    /// colour layer is modulated by a mask. It holds because a mask's geometry is stored
    /// normalized: the mask compiled against the proxy stage is the same field at a smaller scale,
    /// so nothing about the equation changed, and the only sampling question — whether the proxy's
    /// pixel grid resolves the mask's narrowest feature — is answered yes here, at nine proxy
    /// pixels of ramp.
    #[test]
    fn a_masked_recipe_is_proxy_eligible_and_its_proxy_frame_is_the_exact_recipe_at_proxy_size() {
        let display = bounds(40, 40);
        let mask = gradient_mask(0.3);
        let job = stacked_with_masks(
            64,
            48,
            masked_basic(&mask),
            vec![mask.clone()],
            Some(display),
        );
        let registry = job.registry.clone();
        let source = job.source.clone();
        let recipe = job.recipe.clone();
        let snapshot = job.entry.snapshot.id.clone();
        registry
            .proxy_eligible(&recipe)
            .expect("a masked colour stack is proxy eligible");
        let plan = source
            .proxy_plan(&registry, &recipe, display)
            .expect("a plan")
            .expect("a proxy is worthwhile");
        assert!(
            0.3 * f64::from(plan.height) >= 2.0,
            "this mask's ramp must be at least two proxy pixels for the equality to be claimed"
        );

        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
        let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
        assert_eq!(
            (proxy.generation, proxy.phase),
            (generation, PreviewPhase::Proxy)
        );
        assert_eq!(
            (exact.generation, exact.phase),
            (generation, PreviewPhase::Exact)
        );
        assert_eq!(exact.proxy_declined, None, "the proxy phase ran");
        assert_eq!(proxy.proxy_dimensions, Some((plan.width, plan.height)));
        assert!(
            !proxy.proxy_approximate(),
            "a mask the proxy grid resolves is not an approximation: {:?}",
            proxy.proxy_approximation
        );
        assert_eq!(proxy.proxy_approximation.reason(), None);

        let reference = source
            .proxy(plan)
            .expect("the exact downscale")
            .render(&registry, snapshot.clone(), &recipe)
            .expect("the masked recipe renders at proxy size");
        let frame = proxy.result.expect("a proxy frame");
        assert_eq!(
            (frame.width, frame.height),
            (reference.width, reference.height)
        );
        assert_eq!(
            frame.rgba.as_ref(),
            reference.rgba.as_ref(),
            "the masked proxy frame is the exact recipe over the exact downscale"
        );
        // The mask did something: the same units applied everywhere are a different picture, so the
        // equality above is not the equality of two unmasked renders.
        let global = source
            .proxy(plan)
            .expect("the exact downscale")
            .render(&registry, snapshot.clone(), &without_masks(&recipe))
            .expect("the unmasked recipe renders at proxy size");
        assert_ne!(
            frame.rgba.as_ref(),
            global.rgba.as_ref(),
            "the mask must modulate the frame, or this proves nothing about masks"
        );

        let reference = source
            .render(&registry, snapshot, &recipe)
            .expect("the exact render");
        let frame = exact.result.expect("an exact frame");
        assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref());
    }

    #[test]
    fn queue_timing_is_opt_in_and_survives_the_worker_result() {
        let mut ordinary = PreviewQueue::default();
        ordinary.request(stacked(
            64,
            48,
            eligible_layers(64, 48),
            Some(bounds(16, 16)),
        ));
        let ordinary_results = drain_all(&mut ordinary);
        assert!(
            ordinary_results
                .iter()
                .all(|result| result.queue_wait_ms.is_none())
        );

        let mut measured = PreviewQueue::default();
        let (queued, requested_at) = measured.request_timed(stacked(
            64,
            48,
            eligible_layers(64, 48),
            Some(bounds(16, 16)),
        ));
        assert_eq!(queued.generation, 1);
        assert!(requested_at <= Instant::now());
        let measured_results = drain_all(&mut measured);
        assert!(
            measured_results
                .iter()
                .all(|result| result.queue_wait_ms.is_some_and(|ms| ms >= 0.0))
        );
    }

    /// The thin-feature rule, on both sides of its threshold, over the same stack and the same
    /// bounds: only the mask's ramp changes.
    ///
    /// Above two proxy pixels the frame is the exact recipe at proxy size and reports no
    /// approximation. Below it the **mask field** is evaluated with a 2 x 2 supersample per pixel —
    /// the effect is not — so the frame is no longer the point-sampled render, and it says so with
    /// the word the spatial layer already uses.
    #[test]
    fn a_mask_thinner_than_two_proxy_pixels_is_supersampled_and_reported_approximate() {
        let display = bounds(40, 40);
        // A 40x30 proxy of a 64x48 source: 0.3 x 30 = 9 px of ramp resolves, 0.05 x 30 = 1.5 px
        // does not — and 0.05 x 48 = 2.4 px still resolves at full resolution, so the rule is about
        // the grid the frame is sampled on and not about the mask alone.
        let resolvable = gradient_mask(0.3);
        let thin = gradient_mask(0.05);

        let frame_of = |mask: &Mask| -> PreviewResult {
            let job = stacked_with_masks(
                64,
                48,
                masked_basic(mask),
                vec![mask.clone()],
                Some(display),
            );
            let mut queue = PreviewQueue::default();
            queue.request(job);
            let mut results = drain_all(&mut queue);
            assert_eq!(results.len(), 2);
            results.remove(0)
        };

        let coarse = frame_of(&resolvable);
        assert!(!coarse.proxy_approximate());
        assert!(!coarse.proxy_approximation.mask);

        let fine = frame_of(&thin);
        assert!(
            fine.proxy_approximate(),
            "a ramp of 1.5 proxy pixels is below the threshold"
        );
        assert!(fine.proxy_approximation.mask);
        assert!(
            !fine.proxy_approximation.spatial,
            "there is no spatial layer in this stack"
        );
        let reason = fine.proxy_approximation.reason().expect("a reason");
        assert!(
            reason.contains("narrower than two proxy pixels"),
            "{reason}"
        );
        assert!(reason.contains("supersample"), "{reason}");

        // The supersample changes the picture it is applied to, which is why it is reported: the
        // point-sampled render of the same stack at the same size is a different frame.
        let source = synthetic(64, 48);
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: masked_basic(&thin),
            masks: vec![thin.clone()],
            ..Recipe::default()
        };
        let plan = source
            .proxy_plan(&registry, &recipe, display)
            .unwrap()
            .unwrap();
        let point_sampled = source
            .proxy(plan)
            .unwrap()
            .render(&registry, SnapshotId::new(), &recipe)
            .unwrap();
        assert_ne!(
            fine.result.expect("a proxy frame").rgba.as_ref(),
            point_sampled.rgba.as_ref(),
            "the thin mask was supersampled, so its frame differs from the point-sampled one"
        );
    }

    /// A mask with no components draws no feature at all, so `min_feature_px` answers
    /// `f32::INFINITY` and the comparison against two pixels reads it correctly: the supersample
    /// path is not tripped and the frame reports no approximation.
    #[test]
    fn a_mask_with_no_components_reports_no_approximation() {
        let display = bounds(40, 40);
        let empty = Mask::new("Mask 1");
        let job = stacked_with_masks(
            64,
            48,
            masked_basic(&empty),
            vec![empty.clone()],
            Some(display),
        );
        let registry = job.registry.clone();
        let recipe = job.recipe.clone();
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), 2);
        let proxy = &results[0];
        assert_eq!(proxy.phase, PreviewPhase::Proxy);
        assert!(!proxy.proxy_approximate());
        assert!(!proxy.proxy_approximation.mask);
        assert_eq!(registry.proxy_approximation(&recipe, 40, 30).reason(), None);
    }

    /// Both reasons at once: a spatial layer and a thin mask in one stack. The frame is approximate
    /// for two separate reasons and names both, so a person can tell which is which.
    #[test]
    fn a_spatial_layer_and_a_thin_mask_are_reported_separately() {
        let registry = ModuleRegistry::builtin();
        let thin = gradient_mask(0.05);
        let spatial_and_mask = Recipe {
            format: RECIPE_FORMAT,
            layers: masked_basic(&thin)
                .into_iter()
                .chain([Layer {
                    id: LayerId::new(),
                    effect_id: crate::PRESENCE_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"texture": 40.0}),
                    mask: None,
                    artifacts: Vec::new(),
                }])
                .collect(),
            masks: vec![thin.clone()],
            ..Recipe::default()
        };
        let both = registry.proxy_approximation(&spatial_and_mask, 40, 30);
        assert!(both.spatial && both.mask);
        let reason = both.reason().expect("a reason");
        assert!(reason.contains("neighbourhoods"), "{reason}");
        assert!(
            reason.contains("narrower than two proxy pixels"),
            "{reason}"
        );

        // The spatial layer alone still says only what it is.
        let only = registry.proxy_approximation(&without_masks(&spatial_and_mask), 40, 30);
        assert!(only.spatial && !only.mask);
        let reason = only.reason().expect("a reason");
        assert!(
            !reason.contains("narrower than two proxy pixels"),
            "{reason}"
        );
    }

    /// A newer request stops the exact phase of the job it replaced within a chunk, and that phase
    /// answers with no frame at all, delivered under its own generation before anything of the
    /// newer job. The proxy phase of the older job is polled first, so the cancel lands inside the
    /// exact render rather than before it.
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
        assert_eq!(first, vec![(older, PreviewPhase::Proxy, false)]);

        let newer = queue.request(stacked(
            1200,
            900,
            eligible_layers(1200, 900),
            Some(display),
        ));
        let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
        let order: Vec<(u64, PreviewPhase)> = delivered
            .iter()
            .map(|(generation, phase, _)| (*generation, *phase))
            .collect();
        assert_eq!(
            order,
            vec![
                (older, PreviewPhase::Exact),
                (newer, PreviewPhase::Proxy),
                (newer, PreviewPhase::Exact)
            ],
            "the older job's one exact outcome, then the newer job's two phases"
        );
        // The exact phase of the older job either finished before the cancel reached it — which is
        // vanishingly unlikely on a frame this size but is not forbidden — or it was cancelled. The
        // newer job's phases are frames either way.
        assert!(
            delivered[1..].iter().all(|(_, _, cancelled)| !cancelled),
            "{delivered:?}"
        );
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

    // ---------------------------------------------------------------------------------------
    // The activity each job publishes
    // ---------------------------------------------------------------------------------------

    /// A job whose one colour layer waits on `gate` in every phase that renders it, so a test can
    /// hold the job in the phase it is about. The layer leaves its pixels as it found them.
    fn held(gate: &Arc<crate::modules::RenderGate>, proxy: Option<ProxyBounds>) -> PreviewJob {
        let layer = Layer {
            id: LayerId::new(),
            effect_id: crate::modules::HELD_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            artifacts: Vec::new(),
            mask: None,
        };
        let mut job = stacked(64, 48, vec![layer], proxy);
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(crate::modules::HeldModule::shared(gate.clone()))
            .expect("a valid holding module");
        job.registry = Arc::new(registry);
        job
    }

    /// Read the board until `wanted` holds. The job under test is held at a gate, so what it waits
    /// for is the worker reaching that gate, never a race with how fast the machine renders.
    fn board_until(
        board: &ActivityBoard,
        wanted: impl Fn(&crate::ActivitySnapshot) -> bool,
        what: &str,
    ) -> crate::ActivitySnapshot {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let snapshot = board.snapshot();
            if wanted(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "{what}: {snapshot:?}");
            std::thread::yield_now();
        }
    }

    /// A job with a proxy phase is listed in `proxy` while that phase runs and ends in `exact`, and
    /// its entry has already ended when the queue releases the job. The board keeps every finished
    /// entry here, because its recent threshold is zero.
    #[test]
    fn a_jobs_activity_moves_from_proxy_to_exact_and_ends_when_the_queue_releases_it() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let gate = crate::modules::RenderGate::open_gate();
        let mut queue = PreviewQueue::default();
        queue.set_activity(board.clone());
        let job = held(&gate, Some(bounds(16, 16)));
        let asset = job.entry.asset_id.clone();

        gate.shut();
        queue.request(job);
        let running = board_until(
            &board,
            |snapshot| {
                snapshot
                    .active
                    .first()
                    .is_some_and(|active| active.entry.phase == Some("proxy"))
            },
            "the proxy phase never reached its gate",
        );
        assert_eq!(running.active.len(), 1);
        let entry = &running.active[0].entry;
        assert_eq!(
            (entry.kind, entry.label),
            ("preview.render", "Rendering preview")
        );
        assert_eq!(entry.asset_id.as_ref(), Some(&asset));
        assert_eq!((&entry.detail, &entry.job_id), (&None, &None));
        assert!(running.recent.is_empty());

        gate.open();
        let results = drain_all(&mut queue);
        assert_eq!(
            results
                .iter()
                .map(|result| result.phase)
                .collect::<Vec<_>>(),
            [PreviewPhase::Proxy, PreviewPhase::Exact]
        );
        // The queue released the job the moment its exact result arrived, and the entry had
        // already ended by then: nothing here waits for the worker again.
        let ended = board.snapshot();
        assert!(ended.active.is_empty(), "{ended:?}");
        assert_eq!(ended.recent.len(), 1);
        let recent = &ended.recent[0];
        assert_eq!(recent.entry.kind, "preview.render");
        assert_eq!(
            recent.entry.phase,
            Some("exact"),
            "the job moved on to its exact phase"
        );
        assert_eq!(recent.outcome, crate::activity::Outcome::Completed);
    }

    /// A newer request stops the exact phase of the job it replaces, and that job's activity ends
    /// cancelled while the newer one completes. The older job is held at its gate inside the first
    /// 16-row chunk of its colour pass, so the stop reaches the check before its second chunk.
    #[test]
    fn a_superseded_jobs_activity_ends_cancelled() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let gate = crate::modules::RenderGate::open_gate();
        let mut queue = PreviewQueue::default();
        queue.set_activity(board.clone());

        gate.shut();
        let first = queue.request(held(&gate, None));
        let running = board_until(
            &board,
            |snapshot| {
                snapshot
                    .active
                    .first()
                    .is_some_and(|active| active.entry.phase == Some("exact"))
            },
            "the exact phase never reached its gate",
        );
        let older = running.active[0].entry.id;
        let newer = queue.request(held(&gate, None));
        gate.open();
        let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
        assert_eq!(
            delivered,
            vec![
                (first, PreviewPhase::Exact, true),
                (newer, PreviewPhase::Exact, false)
            ],
            "the older exact phase stopped, and its cancelled outcome came first"
        );

        let ended = board.snapshot();
        assert!(ended.active.is_empty(), "{ended:?}");
        let outcomes: Vec<(u64, crate::activity::Outcome)> = ended
            .recent
            .iter()
            .map(|recent| (recent.entry.id, recent.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [
                (older + 1, crate::activity::Outcome::Completed),
                (older, crate::activity::Outcome::Cancelled),
            ],
            "newest first: the job that replaced it completed"
        );
    }

    /// Wait until the job held at `gate` has reached it at least `rows` times.
    fn gate_until(gate: &crate::modules::RenderGate, rows: u64, what: &str) {
        let deadline = Instant::now() + DEADLINE;
        while gate.reached() < rows {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::yield_now();
        }
    }

    /// The two-phase rule under the persistent worker: a newer request arrives while the older
    /// job's proxy render is held at its gate. The proxy phase is not interrupted — its frame is
    /// still newer than anything on screen — and is delivered as a frame; the exact phase behind it
    /// was superseded before it began, so it answers cancelled; and the newer job then runs both of
    /// its phases.
    #[test]
    fn a_superseded_jobs_exact_phase_is_cancelled_but_its_proxy_is_not() {
        let gate = crate::modules::RenderGate::open_gate();
        let mut queue = PreviewQueue::default();
        gate.shut();
        let older = queue.request(held(&gate, Some(bounds(16, 16))));
        gate_until(&gate, 1, "the proxy render never reached its gate");
        let newer = queue.request(held(&gate, Some(bounds(16, 16))));
        gate.open();
        let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
        assert_eq!(
            delivered,
            vec![
                (older, PreviewPhase::Proxy, false),
                (older, PreviewPhase::Exact, true),
                (newer, PreviewPhase::Proxy, false),
                (newer, PreviewPhase::Exact, false),
            ],
            "the superseded job's proxy frame, its cancelled exact phase, then the newer job"
        );
    }

    /// The worker takes the pending job itself when the active one ends: nothing here polls, and
    /// the pending job still runs to the end while the first job's outcome waits undelivered.
    #[test]
    fn the_next_job_starts_without_a_poll() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let gate = crate::modules::RenderGate::open_gate();
        let mut queue = PreviewQueue::default();
        queue.set_activity(board.clone());
        gate.shut();
        let first = queue.request(held(&gate, None));
        let second = queue.request(held(&gate, None));
        assert_eq!(queue.pending_generation(), Some(second));
        gate.open();
        let ended = board_until(
            &board,
            |snapshot| snapshot.active.is_empty() && snapshot.recent.len() == 2,
            "the pending job never ran without a poll",
        );
        assert_eq!(
            ended
                .recent
                .iter()
                .map(|recent| recent.outcome)
                .collect::<Vec<_>>(),
            [
                crate::activity::Outcome::Completed,
                crate::activity::Outcome::Cancelled
            ],
            "newest first: the pending job completed, the first was superseded"
        );
        assert_eq!(queue.pending_generation(), None);
        assert_eq!(queue.last_delivered(), 0, "nothing was polled");
        assert!(queue.ready());
        let delivered = drain_until(&mut queue, second, PreviewPhase::Exact);
        assert_eq!(
            delivered,
            vec![
                (first, PreviewPhase::Exact, true),
                (second, PreviewPhase::Exact, false)
            ]
        );
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

    /// A RAW job over planes developed at one white balance, rendering them at `white_balance`, at
    /// display bounds that give it a proxy phase, asking for a report.
    fn raw_job(white_balance: Option<crate::WhiteBalanceApproximation>) -> PreviewJob {
        use crate::{LinearImage, LinearSettings};
        let (width, height) = (240, 160);
        let planes: Vec<f32> = (0..3 * width * height)
            .map(|index| 0.02 + ((index * 37) % 1009) as f32 / 1100.0)
            .collect();
        let image = LinearImage::with_fingerprint(width, height, planes, "sha256:raw-wb").unwrap();
        let mut job = job(1, true);
        // The stock test job carries a pixel-stage layer, which is not proxy-eligible.
        job.recipe.layers.clear();
        job.source = PreviewSource::Raw {
            image,
            settings: LinearSettings {
                exposure_ev: 0.25,
                white_balance,
            },
        };
        job.proxy = Some(ProxyBounds {
            width: 60,
            height: 60,
        });
        job
    }

    fn approximation() -> crate::WhiteBalanceApproximation {
        crate::WhiteBalanceApproximation::from_matrix([
            [1.35, 0.08, -0.04],
            [0.03, 0.98, 0.02],
            [-0.06, 0.04, 0.71],
        ])
        .unwrap()
    }

    /// A job whose source approximates its white balance says so on both of its phases and is
    /// never reduced into a report, although it asked for one. Otherwise it is an ordinary job:
    /// the proxy phase is the approximate recipe rendered against the exact downscale of the
    /// developed planes, byte for byte, and the exact phase the approximate recipe at full size.
    #[test]
    fn an_approximate_white_balance_is_labelled_on_both_phases_and_never_analysed() {
        let job = raw_job(Some(approximation()));
        assert!(job.analyse, "the job asked for a report");
        assert!(job.source.approximate_white_balance());
        let (registry, source, recipe) =
            (job.registry.clone(), job.source.clone(), job.recipe.clone());
        let snapshot = job.entry.snapshot.id.clone();
        let plan = source
            .proxy_plan(&registry, &recipe, job.proxy.unwrap())
            .unwrap()
            .expect("a proxy is worthwhile");

        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
        let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
        assert_eq!(
            (proxy.generation, exact.generation),
            (generation, generation)
        );
        assert_eq!(
            (proxy.phase, exact.phase),
            (PreviewPhase::Proxy, PreviewPhase::Exact)
        );
        assert!(proxy.approximate_white_balance && exact.approximate_white_balance);
        assert!(proxy.report.is_none());
        assert!(
            exact.report.is_none(),
            "an approximate frame is never reduced, whatever the job asked"
        );

        let reference = source
            .proxy(plan)
            .expect("the exact downscale")
            .render(&registry, snapshot.clone(), &recipe)
            .expect("the approximate recipe at proxy size");
        assert_eq!(
            proxy.result.expect("a proxy frame").rgba.as_ref(),
            reference.rgba.as_ref(),
            "the proxy frame is the approximate recipe over the exact downscale"
        );
        let reference = source
            .render(&registry, snapshot, &recipe)
            .expect("the approximate recipe at full size");
        assert_eq!(
            exact.result.expect("an exact frame").rgba.as_ref(),
            reference.rgba.as_ref()
        );
    }

    /// The proxy cache keys on the developed planes and takes the settings from the job, so a
    /// drafted white balance renders against the proxy the committed frame built — a cache hit —
    /// through its own matrix, and says so.
    #[test]
    fn a_drafted_white_balance_hits_the_proxy_the_exact_job_built() {
        let exact = raw_job(None);
        let mut drafted = raw_job(Some(approximation()));
        // The same developed planes: a drafted job reads the planes the committed one did.
        drafted.source = PreviewSource::Raw {
            image: match &exact.source {
                PreviewSource::Raw { image, .. } => image.clone(),
                PreviewSource::Jpeg(_) => unreachable!(),
            },
            settings: match &drafted.source {
                PreviewSource::Raw { settings, .. } => *settings,
                PreviewSource::Jpeg(_) => unreachable!(),
            },
        };
        let mut queue = PreviewQueue::default();
        queue.request(exact);
        let first = drain_all(&mut queue);
        assert!(first[0].proxy_built && !first[0].approximate_white_balance);
        queue.request(drafted);
        let second = drain_all(&mut queue);
        assert_eq!(second[0].phase, PreviewPhase::Proxy);
        assert!(!second[0].proxy_built, "the drafted job reuses the proxy");
        assert!(second[0].approximate_white_balance);
        assert_ne!(
            second[0].result.as_ref().unwrap().rgba,
            first[0].result.as_ref().unwrap().rgba,
            "the cached pixels render through the drafted matrix, not the cached settings"
        );
    }

    /// The same job over planes that hold its white balance is exact: unlabelled, and reduced.
    #[test]
    fn an_exact_raw_job_is_unlabelled_and_analysed() {
        let job = raw_job(None);
        assert!(!job.source.approximate_white_balance());
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|result| !result.approximate_white_balance)
        );
        assert!(
            results[0].report.is_none(),
            "a proxy frame is never reduced"
        );
        assert!(results[1].report.is_some(), "the exact frame is");
    }

    /// A cached RAW proxy is pixels, not settings: a second job over the same developed planes
    /// with another exposure is a cache hit that renders at its own exposure.
    #[test]
    fn a_cached_raw_proxy_renders_at_the_exposure_of_the_job_that_hits_it() {
        use crate::{LinearImage, LinearSettings};
        let width = 2000;
        let height = 1200;
        let planes: Vec<f32> = (0..3 * width * height)
            .map(|index| 0.1 + (index % 997) as f32 / 4000.0)
            .collect();
        let image = LinearImage::with_fingerprint(width, height, planes, "sha256:raw").unwrap();
        let bounds = ProxyBounds {
            width: 400,
            height: 300,
        };
        let job_at = |ev: f64, analyse: bool| {
            let mut job = job(1, analyse);
            // The stock test job carries a pixel-stage layer, which is not proxy-eligible; the
            // development settings alone are the stack under test.
            job.recipe.layers.clear();
            job.source = PreviewSource::Raw {
                image: image.clone(),
                settings: LinearSettings {
                    exposure_ev: ev,
                    white_balance: None,
                },
            };
            job.proxy = Some(bounds);
            job
        };
        let mut queue = PreviewQueue::default();
        let first = queue.request(job_at(0.0, false));
        let mut dark = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while dark.is_none() {
            if let Some(result) = queue.poll()
                && result.generation == first
                && result.phase == PreviewPhase::Proxy
            {
                assert!(result.proxy_built, "the first job builds the proxy");
                dark = Some(result.result.expect("a proxy frame"));
            }
            assert!(Instant::now() < deadline, "the first proxy never came");
            std::thread::yield_now();
        }
        while queue.is_busy() {
            let _ = queue.poll();
            std::thread::yield_now();
        }
        let second = queue.request(job_at(1.0, false));
        let mut bright = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while bright.is_none() {
            if let Some(result) = queue.poll()
                && result.generation == second
                && result.phase == PreviewPhase::Proxy
            {
                assert!(
                    !result.proxy_built,
                    "the same planes and bounds are a cache hit"
                );
                bright = Some(result.result.expect("a proxy frame"));
            }
            assert!(Instant::now() < deadline, "the second proxy never came");
            std::thread::yield_now();
        }
        let (dark, bright) = (dark.unwrap(), bright.unwrap());
        assert_eq!((dark.width, dark.height), (bright.width, bright.height));
        let brighter = dark
            .rgba
            .chunks_exact(4)
            .zip(bright.rgba.chunks_exact(4))
            .filter(|(a, b)| b[0] > a[0])
            .count();
        assert!(
            brighter > (dark.width * dark.height / 2) as usize,
            "one stop more exposure brightens the cached proxy, not the cached settings"
        );
    }

    /// A luminance band on its own, which is what makes a mask read pixels.
    fn band_mask() -> Mask {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(Component::new(
            "Luminance range 1",
            ComponentMode::Add,
            "luminance-range",
            json!({"low": 15.0, "low_feather": 20.0, "high": 90.0, "high_feather": 20.0}),
        ));
        mask
    }

    /// A gradient intersected with a band: a value-based mask whose conservative rectangle is the
    /// gradient's, which is what the per-cell pixel read is skipped outside.
    fn mixed_mask() -> Mask {
        let mut mask = gradient_mask(0.3);
        mask.components.push(Component::new(
            "Luminance range 1",
            ComponentMode::Intersect,
            "luminance-range",
            json!({"low": 15.0, "low_feather": 20.0, "high": 90.0, "high_feather": 20.0}),
        ));
        mask
    }

    /// A value-based mask whose first bound layer sits behind a **spatial** layer has no grid, and the
    /// refusal names the cost rather than paying it.
    ///
    /// A point sample through a spatial segment is the declared exception to [performance rule
    /// 4](../../docs/engineering/performance-rules.md#rules): it evaluates the stage-aligned tiles its
    /// pixel needs plus the operation's halo, so asking it once per display cell over the whole stage
    /// would evaluate every tile of the picture on every overlay. That is not an overlay to ship
    /// slowly, so the grid is refused here on exactly the rule the unbound mask is refused on, and the
    /// 100% view still reads such a selection. The **geometric** half of the same stack is unaffected,
    /// because a position-only mask needs no pixel at all.
    #[test]
    fn a_value_based_mask_behind_a_spatial_layer_has_no_grid_and_says_what_it_would_cost() {
        let mask = band_mask();
        let geometric = gradient_mask(0.3);
        let presence = |mask: Option<&Mask>| Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"texture": 40.0}),
            mask: mask.map(|mask| mask.id.clone()),
            artifacts: Vec::new(),
        };
        for (held, absent) in [(&mask, true), (&geometric, false)] {
            let job = stacked_with_masks(
                64,
                48,
                vec![presence(None), presence(Some(held))],
                vec![held.clone()],
                None,
            );
            let request = MaskOverlayRequest {
                mask: held.id.clone(),
                component: None,
                cells_w: 8,
                cells_h: 6,
            };
            let frame = render(
                &job.registry,
                &job.source,
                &job.recipe,
                RenderOptions::default(),
                &job.context,
            )
            .expect("the stack compiles");
            let (grid, reason) = mask_overlay_for(
                &job.registry,
                &frame,
                &job.recipe,
                &request,
                &Cancel::never(),
                &job.context,
            );
            if absent {
                assert!(grid.is_none(), "a value-based mask behind a spatial layer");
                let reason = reason.expect("the host's own reason travels with the frame");
                assert!(
                    reason.contains("depends on the pixel it reads")
                        && reason.contains("tile per grid cell")
                        && reason.contains("100% view"),
                    "{reason}"
                );
            } else {
                assert!(
                    grid.is_some(),
                    "a position-only mask needs no pixel and keeps its grid: {reason:?}"
                );
                assert_eq!(reason, None);
            }
        }
    }

    /// What the coverage overlay costs the exact preview phase, on 24 MP and 60 MP, before and after
    /// a value-based component is in the mask — the measurement proposal P16 of
    /// `docs/design/range-study.md` was decided against.
    ///
    /// The "before" figure for a value-based mask is nothing at all, because such a mask was refused a
    /// grid; the geometric rows are the delivered cost of a grid and must not have moved. So the added
    /// cost is the band and mixed rows, and it is stated against the exact render of the same frame,
    /// which is the phase the grid is filled beside.
    ///
    /// The two grid sizes are the two a person actually asks for, from
    /// `state::histogram::overlay_cells`: at Fit one cell per physical pixel of the drawn photograph,
    /// and at 100% the delivered 4096-cell cap a side.
    ///
    /// Ignored by default because it is a measurement and not a pass/fail property. Run it with
    /// `cargo test --release --locked --package lightwell-core --lib -- --ignored --nocapture
    /// preview::tests::the_cost_of_a_coverage_grid`, and record the host's one-minute load average
    /// beside every figure.
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn the_cost_of_a_coverage_grid() {
        // A canvas 1728 px wide is the Develop workspace's photograph on the reference machine.
        let stages = [("24 MP", 6000_u32, 4000_u32), ("60 MP", 9504, 6336)];
        for (label, width, height) in stages {
            let fit = crate::analysis::MAX_OVERLAY_CELLS.min(1728);
            let fit_cells = (fit, (fit * height).div_ceil(width));
            let hundred = {
                let cap = f64::from(crate::analysis::MAX_OVERLAY_CELLS);
                let scale = (cap / f64::from(width)).min(cap / f64::from(height));
                (
                    (f64::from(width) * scale).round() as u32,
                    (f64::from(height) * scale).round() as u32,
                )
            };
            for (name, mask) in [
                ("gradient (delivered)", gradient_mask(0.3)),
                ("band", band_mask()),
                ("gradient ∩ band", mixed_mask()),
            ] {
                let job = stacked_with_masks(
                    width,
                    height,
                    masked_basic(&mask),
                    vec![mask.clone()],
                    None,
                );
                let registry = job.registry.clone();
                let source = job.source.clone();
                let recipe = job.recipe.clone();
                let snapshot = job.entry.snapshot.id.clone();
                // The phase the grid is filled beside, for the figures to be stated against.
                let started = Instant::now();
                let raster = source
                    .render(&registry, snapshot, &recipe)
                    .expect("the exact frame");
                let render = started.elapsed();
                std::hint::black_box(raster.rgba.len());
                for (view, (cells_w, cells_h)) in [("fit", fit_cells), ("100%", hundred)] {
                    let request = MaskOverlayRequest {
                        mask: mask.id.clone(),
                        component: None,
                        cells_w,
                        cells_h,
                    };
                    let context = crate::render::testing::context();
                    let frame = crate::render(
                        &registry,
                        &source,
                        &recipe,
                        RenderOptions::default(),
                        context,
                    )
                    .expect("the stack compiles");
                    let started = Instant::now();
                    let (grid, absent) = mask_overlay_for(
                        &registry,
                        &frame,
                        &recipe,
                        &request,
                        &Cancel::never(),
                        crate::render::testing::context(),
                    );
                    let elapsed = started.elapsed();
                    let cells = u64::from(cells_w) * u64::from(cells_h);
                    match grid {
                        Some(grid) => {
                            std::hint::black_box(grid.coverage.len());
                            println!(
                                "{label} {name} at {view}: {cells_w}x{cells_h} = {cells} cells in \
                             {:.1} ms ({:.1} ns/cell), beside a {:.1} ms exact render — {:.1}% of it",
                                elapsed.as_secs_f64() * 1000.0,
                                elapsed.as_secs_f64() * 1e9 / cells as f64,
                                render.as_secs_f64() * 1000.0,
                                100.0 * elapsed.as_secs_f64() / render.as_secs_f64(),
                            );
                        }
                        None => println!(
                            "{label} {name} at {view}: no grid — {}",
                            absent.unwrap_or_else(|| "nothing to describe".into())
                        ),
                    }
                }
            }
        }
    }

    /// A straightened crop over a RAW source with a Presence layer, previewed while another
    /// evaluation holds the whole spatial target: the pointer readout sampling through the same
    /// layer on the owner thread, which is how a committed RAW crop was once refused with "spatial
    /// processing needs … bytes, and … of the … byte spatial budget is in use" and left unshown.
    /// Both phases deliver the cropped frame, each byte for byte the frame the same stack renders
    /// with the target free, and every batch releases what it reserved.
    #[test]
    fn a_cropped_raw_preview_with_presence_renders_while_the_spatial_target_is_held() {
        use crate::{LinearImage, LinearSettings, PRESENCE_EFFECT};
        let _guard = crate::render::spatial::tests::spatial_guard();
        crate::render::testing::clear_estimates();
        // More than one 512 px tile each way, so the spatial pass runs in batches.
        let (width, height) = (1100_u32, 700_u32);
        let planes: Vec<f32> = (0..3 * width * height)
            .map(|index| 0.05 + (index % 1009) as f32 / 1400.0)
            .collect();
        let image =
            LinearImage::with_fingerprint(width, height, planes, "sha256:raw-crop").unwrap();
        let stage = CropStage {
            width,
            height,
            angle: 7.0,
        };
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: 0.0,
            y: 0.0,
            width: box_width,
            height: box_height,
        });
        let mut job = job(1, false);
        job.recipe.layers = vec![
            Layer {
                id: LayerId::new(),
                effect_id: PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"clarity": 60.0}),
                artifacts: Vec::new(),
                mask: None,
            },
            Layer::crop(fitted.normalized(&stage)),
        ];
        job.source = PreviewSource::Raw {
            image,
            settings: LinearSettings::default(),
        };
        let display = ProxyBounds {
            width: 480,
            height: 320,
        };
        job.proxy = Some(display);
        let registry = job.registry.clone();
        let recipe = job.recipe.clone();
        let snapshot = job.entry.snapshot.id.clone();
        let output = registry.compile(width, height, &recipe).unwrap().stage();
        assert!(
            output.width < width && output.height < height,
            "the crop trims the stage"
        );
        let plan = job
            .source
            .proxy_plan(&registry, &recipe, display)
            .unwrap()
            .expect("a proxy is worthwhile");
        let exact_reference = job
            .source
            .render(&registry, snapshot.clone(), &recipe)
            .expect("the stack renders with the target free");
        let proxy_reference = job
            .source
            .proxy(plan)
            .unwrap()
            .render(&registry, snapshot, &recipe)
            .expect("the proxy renders with the target free");

        let budget = crate::render::testing::context().spatial();
        let held = budget.reserve(budget.target(), 1);
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let results = drain_all(&mut queue);
        drop(held);
        assert_eq!(budget.in_use(), 0, "every batch released its reservation");
        assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
        let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
        assert_eq!(
            (proxy.generation, proxy.phase),
            (generation, PreviewPhase::Proxy)
        );
        assert_eq!(
            (exact.generation, exact.phase),
            (generation, PreviewPhase::Exact)
        );
        assert_eq!(exact.proxy_declined, None, "the proxy phase ran");
        let proxy = proxy
            .result
            .expect("the proxy phase renders beside a held target");
        assert_eq!(
            (proxy.width, proxy.height),
            (proxy_reference.width, proxy_reference.height)
        );
        assert!(
            proxy.rgba == proxy_reference.rgba,
            "the proxy frame differs"
        );
        let exact = exact
            .result
            .expect("the exact phase renders beside a held target");
        assert_eq!((exact.width, exact.height), (output.width, output.height));
        assert!(
            exact.rgba == exact_reference.rgba,
            "the exact frame differs"
        );
    }
}
