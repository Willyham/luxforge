//! Preview presentation: requesting preview jobs at the bounds the view calls for, taking up what
//! the preview worker finishes, and presenting the displayed frame — the display-size proxy, the
//! exact frame behind it, the histogram analysis and the crop draft's input stage — in the order
//! their generations allow.
//!
//! [`Presentation`] owns all of it: the one [`Presenter`] every frame goes to, the preview queue,
//! and the bookkeeping that decides which frame is on screen — generations, content serials, the
//! retained frames, the bounds each job was given and the entry and draft revision the frame on
//! screen was rendered for. The editor's methods here decide what the view wants and what a frame
//! settles; `Presentation` records what is shown.
use super::{
    Editor,
    evidence::Settle,
    gesture::Starting,
    message::{Message, preview::PreviewMessage},
    overlay::OverlayRequest,
    presenter::Presenter,
    tasks::{self, recipe_task},
};
use crate::app::{Before, waker};
use crate::{layout, state, state::histogram::Analysis, view};
use iced::{Subscription, Task};
use luxforge_core::{
    DraftId, EntryId, ExactOutcome, HistoryEntry, MaskOverlayOutcome, PhaseOutcome, PreviewIntent,
    PreviewJob, PreviewPhase, PreviewQueue, PreviewResult, ProxyBounds, Raster, Region,
    RegionOutcome, Zoom,
    analysis::{AnalysisIdentity, MaskOverlay},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

/// One policy for a paused input, whether its first frame was Fit or a visible region. The
/// evidence run measures this interval against render/round-trip timing before acceptance.
const QUIET_INTERVAL: Duration = Duration::from_millis(120);

/// The display-proxy frame of one generation, retained beside the exact raster.
///
/// It is what a zoom back to Fit hands the surface again instead of rendering, and what the
/// clipping overlay is derived from while the exact phase of that generation is still outstanding.
/// Retaining it copies no pixels: it shares the render's own `Arc<Vec<u8>>` with the surface.
///
/// Its generation is also the desktop's only record that the job of that generation *had* a proxy
/// phase. The exact result cannot say so: `proxy_declined` is `None` both for a job that asked for
/// no proxy and for one that got one.
#[derive(Clone)]
pub(crate) struct ProxyFrame {
    pub(crate) generation: u64,
    pub(crate) raster: Arc<luxforge_core::Raster>,
    /// The proxy source dimensions the frame was rendered against.
    pub(crate) dimensions: (u32, u32),
    /// The proxy source was built for this frame rather than taken from the queue's cache.
    pub(crate) built: bool,
    /// Whether the frame approximates the exact render at display size, and why: a spatial layer
    /// whose neighbourhoods scale with the stage, a thin mask, or both.
    pub(crate) approximation: luxforge_core::ProxyApproximation,
    /// The frame approximates a drafted RAW white balance on planes developed at another one, as
    /// the exact phase of the same job does.
    pub(crate) approximate_white_balance: bool,
    /// The proxy phase's own worker time, so a zoom that hands this frame back to the surface
    /// reports how long this picture took rather than whatever was presented last.
    pub(crate) render_ms: f64,
}

/// The exact phase's frame of one generation, retained beside the picture on screen so a clipping
/// overlay can be re-derived from it on a zoom, a pan or a toggle without a second render, and so a
/// zoom to 100% hands it to the surface instead of rendering. It shares the render's
/// `Arc<Vec<u8>>`: retaining it copies no pixels.
///
/// The generation travels with it because the overlay is keyed on **this** image rather than on
/// the newest preview asked for: a frame that has arrived re-derives the overlay, and a frame
/// still rendering does not, so the mask always describes the photograph on screen — a drafted
/// one during a gesture exactly as much as a committed one.
#[derive(Clone)]
pub(crate) struct ExactFrame {
    pub(crate) generation: u64,
    pub(crate) raster: Arc<Raster>,
    /// The raster approximates a drafted RAW white balance: it is the full-size phase of such a
    /// job, which carries no report. A clipping overlay derived from it says `approximate`, and it
    /// replaces no report.
    pub(crate) approximate_white_balance: bool,
    /// The exact phase's own worker time, so a zoom that hands this raster to the surface reports
    /// that picture's render time.
    pub(crate) render_ms: f64,
    /// The content serial of the job it was rendered for, while that job was still known.
    pub(crate) content: Option<u64>,
}

/// A frame the desktop holds for the generation on screen. Both phases reach the surface through
/// the one presenting function, [`Editor::present`], whether a render just produced them or a zoom
/// hands them back.
#[derive(Clone)]
pub(crate) enum Retained {
    Proxy(ProxyFrame),
    Exact(ExactFrame),
}

impl Retained {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Proxy(frame) => frame.generation,
            Self::Exact(frame) => frame.generation,
        }
    }

    pub(crate) fn raster(&self) -> &Arc<Raster> {
        match self {
            Self::Proxy(frame) => &frame.raster,
            Self::Exact(frame) => &frame.raster,
        }
    }

    pub(crate) fn approximate_white_balance(&self) -> bool {
        match self {
            Self::Proxy(frame) => frame.approximate_white_balance,
            Self::Exact(frame) => frame.approximate_white_balance,
        }
    }

    pub(crate) fn render_ms(&self) -> f64 {
        match self {
            Self::Proxy(frame) => frame.render_ms,
            Self::Exact(frame) => frame.render_ms,
        }
    }

    fn proxy(&self) -> Option<&ProxyFrame> {
        match self {
            Self::Proxy(frame) => Some(frame),
            Self::Exact(_) => None,
        }
    }
}

/// How a frame reaches the surface.
pub(crate) enum Arrival {
    /// The preview worker just rendered it for this entry and draft revision. `stage` is the exact
    /// stage every pick, percent-zoom box and overlay cell maps through, whatever size the texture
    /// is.
    Rendered {
        stage: (u32, u32),
        entry: EntryId,
        draft_revision: Option<u64>,
    },
    /// A zoom hands back a frame retained for the generation on screen, stamped with the entry and
    /// draft revision that generation rendered — never with the entry the desktop has asked for
    /// since, whose frame may still be rendering or may have failed.
    Zoom,
}

/// What every phase of one preview result says about the frame it belongs to.
pub(crate) struct Delivery {
    pub(crate) generation: u64,
    /// The exact stage the job was planned for, from its identity.
    pub(crate) stage: (u32, u32),
    /// The draft the job was planned from, from its identity.
    pub(crate) draft: Option<DraftId>,
    pub(crate) entry_id: EntryId,
    pub(crate) draft_revision: Option<u64>,
    pub(crate) intent: PreviewIntent,
    pub(crate) viewport_declined: Option<String>,
    pub(crate) approximate_white_balance: bool,
    pub(crate) render_ms: f64,
}

/// One preview result for the photograph, taken up by its phase ([`Presentation::take`]).
pub(crate) enum Presented {
    /// Older than the frame on screen: it presents nothing.
    Stale,
    /// The visible pixels of a percentage view, with the grid they carry.
    Region(Box<(Delivery, RegionOutcome)>),
    /// A job's coverage grid, following the proxy frame of its generation.
    Overlay(u64, MaskOverlayOutcome),
    /// The display-size frame, already retained for a zoom back to Fit.
    Proxy(Box<(Delivery, ProxyFrame)>),
    /// The exact phase — its frame, already received as the retained exact raster or with its
    /// report, or its failure — and the grid a job with no proxy frame carries beside it.
    Exact(
        Box<(
            Delivery,
            Result<ExactFrame, luxforge_core::Error>,
            MaskOverlayOutcome,
        )>,
    ),
}

/// What a zoom finds for the frame on screen ([`Presentation::zoom`]).
pub(crate) enum Zoomed {
    /// Nothing is on screen, or the texture on screen is already the phase the view wants.
    Kept,
    /// A failure withdrew the picture: nothing retained may be handed over in its place.
    Withdrawn,
    /// The phase the view wants is not retained for the generation on screen.
    Missing,
    /// The retained frame of the generation on screen that the view now wants.
    Hand(Retained),
}

/// What one request queued ([`Presentation::request`]).
pub(crate) struct Requested {
    pub(crate) generation: u64,
    /// The job it replaced in the pending slot, which never starts and is never delivered.
    pub(crate) replaced: Option<u64>,
    /// The visible rectangle it asked for.
    pub(crate) viewport: Option<Region>,
    /// The exact stage it was planned for.
    pub(crate) stage: (u32, u32),
    /// When the job was queued, for a timed request.
    pub(crate) at: Option<Instant>,
}

/// The one owner of what the photo surface shows and of the bookkeeping that decides it.
///
/// None of it is a GPU allocation — the surface owns the textures — so putting a frame on screen
/// costs an `Arc` clone. It keeps no job once the queue has it.
#[derive(Default)]
pub(crate) struct Presentation {
    /// Every frame the photo surface draws: the photograph, the crop draft's input stage and the
    /// overlays over the photograph.
    pub(crate) presenter: Presenter,
    /// One active and one replaceable pending preview job, off the UI thread.
    pub(crate) queue: PreviewQueue,
    /// The generation of the newest preview requested for the photograph.
    pub(crate) preview_generation: u64,
    /// The generation whose pixels are on screen.
    ///
    /// The delivery rule is the queue's own, monotone rather than newest-only: a delivered result
    /// is presented when it is not older than this. Under a sustained drag a render almost always
    /// finishes after a newer job was requested, so rejecting everything but the newest generation
    /// presents no frames at all. What makes work in flight stale is [`Self::cancel`], which an
    /// asset or selection change calls, and so does a discarded mask gesture whose drafted frames
    /// must not reach the screen; nothing else has to.
    pub(crate) presented_generation: u64,
    /// One opaque surface identity per evaluated content. A pan/zoom retains it; a new draft
    /// revision, history entry, source development or recipe gets another id.
    content_key: Option<(AnalysisIdentity, luxforge_core::ProxyIdentity)>,
    pub(crate) content_serial: u64,
    pub(crate) pending_content: BTreeMap<u64, u64>,
    pending_intent: BTreeMap<u64, PreviewIntent>,
    pub(crate) presented_content: u64,
    pub(crate) analysis_content: Option<u64>,
    pub(crate) viewport_disabled_content: Option<u64>,
    /// The exact stage of the frame on screen, whatever size its texture is: a proxy is drawn into
    /// this box, and every pick, percent-zoom box and overlay cell maps to exact stage pixels.
    pub(crate) dimensions: Option<(u32, u32)>,
    /// The proxy frame of the newest job that had a proxy phase.
    pub(crate) proxy_frame: Option<ProxyFrame>,
    /// The exact frame of the newest job whose exact phase landed.
    pub(crate) exact: Option<ExactFrame>,
    /// The visible pixels the region slot owns.
    pub(crate) region_raster: Option<PresentedRegion>,
    /// The displayed frame's histogram report, adopted with the pixels under the same generation.
    pub(crate) analysis: Option<Analysis>,
    /// The report and frame of an exact phase whose pixels have not reached the surface yet. The
    /// histogram and the photograph are adopted together, so the plot never describes a frame that
    /// is not on screen.
    pub(crate) incoming: Option<(Analysis, ExactFrame)>,
    /// The texture on screen is the display proxy rather than the exact render.
    pub(crate) presented_proxy: bool,
    /// The frame on screen approximates a drafted RAW white balance on planes developed at another
    /// one. The histogram is never adopted from such a frame.
    pub(crate) presented_approximate_white_balance: bool,
    /// The bounds each requested job was given, by generation, until its frame is presented. The
    /// bounds are decided when the job is requested, on this thread, so the frame reflects the
    /// window, the panels and the display scale of that moment rather than of the moment its
    /// owner task was created.
    pub(crate) pending_bounds: BTreeMap<u64, Option<ProxyBounds>>,
    /// The bounds the frame on screen was rendered for.
    pub(crate) presented_bounds: Option<ProxyBounds>,
    /// A refit of the proxy to new bounds has been asked for and has not been presented yet.
    pub(crate) refit_pending: bool,
    /// Why the newest job that offered bounds has no proxy phase, as the core reported it.
    pub(crate) proxy_declined: Option<String>,
    /// What the presented proxy is holding until its exact phase lands.
    pub(crate) held_by_proxy: Option<HeldByProxy>,
    /// Why the last preview failed, cleared by the next presented frame. The canvas turns this
    /// into the notice that names the cause; nothing here decides what it means.
    pub(crate) render_error: Option<luxforge_core::Error>,
    /// The entry whose pixels the photo surface holds: the entry the presented generation was
    /// rendered for. The editor's displayed entry moves to a newly requested entry as soon as its
    /// job is asked for; this moves only when that entry's frame is on screen, and is cleared when
    /// a failure withdraws the frame.
    pub(crate) presented_entry: Option<EntryId>,
    /// The entry the newest refresh or history selection asked to render.
    requested_render_entry: Option<Arc<HistoryEntry>>,
    /// The requested entry once its frame is on screen, for the evidence stack summary: a shared
    /// reference, so presenting a frame copies no entry.
    pub(crate) rendered_entry: Option<Arc<HistoryEntry>>,
    /// The draft revision the displayed preview was rendered from, for correlation.
    pub(crate) displayed_draft_revision: Option<u64>,
    /// Revisions are ordered only within this draft; a new draft starts at zero.
    pub(crate) displayed_draft_id: Option<DraftId>,
    /// Generations whose job asked for a coverage grid and has not presented its frame yet: a
    /// proxy frame of one of these is followed by its grid in an overlay phase of its own.
    pub(crate) pending_overlay: BTreeSet<u64>,
    /// The proxy frame on screen whose grid is still on its way. A captured frame waits for it, so
    /// evidence never records the photograph before the overlay that belongs over it.
    pub(crate) overlay_awaited: Option<u64>,
}

/// The one desired view the owner admits through the shared gate, and the quiet policy that
/// settles it: a view change marks it dirty, one plan is in flight at a time, and a view-only
/// request's generation is remembered so only it may be replaced by the next.
#[derive(Default)]
pub(crate) struct ViewPlan {
    /// The view on screen is not the one the zoom, pan and window ask for.
    pub(crate) dirty: bool,
    /// The generation of the standalone view request in the queue, which another view may replace.
    pub(crate) request_generation: Option<u64>,
    /// A `view` plan is on an owner task.
    pub(crate) in_flight: bool,
    /// Moves on every view motion; a plan carries the epoch it was asked under.
    pub(crate) epoch: u64,
    /// When the last view motion or drafted frame happened, while the 120 ms quiet policy runs.
    pub(crate) quiet_since: Option<Instant>,
    /// The quiet settle has been asked for, so its timer stops.
    pub(crate) quiet_settle_requested: bool,
    /// The core draft a release settled, whose drafted frames are no longer taken up.
    pub(crate) released_draft: Option<DraftId>,
}

impl Presentation {
    /// Key a job's content before it is queued: the same evaluated image keeps its serial across
    /// pans and zooms, and anything else gets the next one. A region the surface cannot allocate
    /// for this content is not asked for again; the job falls back to the whole frame and says
    /// why.
    pub(crate) fn admit(&mut self, job: &mut PreviewJob) -> u64 {
        let key = (job.identity.clone(), job.evaluation.source().identity());
        if self.content_key.as_ref() != Some(&key) {
            self.content_serial = self.content_serial.saturating_add(1);
            self.content_key = Some(key);
        }
        let content = self.content_serial;
        if self.viewport_disabled_content == Some(content) && job.viewport.is_some() {
            job.viewport = None;
            job.intent = PreviewIntent::Settle;
            job.viewport_declined =
                Some("region texture exceeds the surface allocation limit".into());
        }
        content
    }

    /// Queue one admitted job of `content` and record what its frame will need when it lands: the
    /// bounds it was given, its content and intent, and whether a coverage grid follows it. The
    /// job still waiting in the pending slot is replaced and forgotten.
    pub(crate) fn request(&mut self, job: PreviewJob, content: u64, timed: bool) -> Requested {
        let bounds = job.proxy;
        let intent = job.intent;
        let viewport = job.viewport;
        let stage = (job.identity.width, job.identity.height);
        let overlay = job.mask_overlay.is_some();
        let (queued, at) = if timed {
            let (queued, at) = self.queue.request_timed(job);
            (queued, Some(at))
        } else {
            (self.queue.request_replacing(job), None)
        };
        let luxforge_core::Queued {
            generation,
            replaced,
        } = queued;
        self.pending_bounds.insert(generation, bounds);
        self.pending_content.insert(generation, content);
        self.pending_intent.insert(generation, intent);
        if overlay {
            self.pending_overlay.insert(generation);
        }
        if let Some(replaced) = replaced {
            self.forget(replaced);
            self.pending_overlay.remove(&replaced);
        }
        Requested {
            generation,
            replaced,
            viewport,
            stage,
            at,
        }
    }

    /// Stop every job in flight and forget what they were asked for. Returns the generation below
    /// which nothing is delivered any more.
    pub(crate) fn cancel(&mut self) -> u64 {
        let generation = self.queue.cancel();
        self.pending_bounds.clear();
        self.pending_content.clear();
        self.pending_intent.clear();
        self.pending_overlay.clear();
        self.overlay_awaited = None;
        generation
    }

    /// Forget what a job whose last phase has been taken up was asked for.
    pub(crate) fn forget(&mut self, generation: u64) {
        self.pending_bounds.remove(&generation);
        self.pending_content.remove(&generation);
        self.pending_intent.remove(&generation);
    }

    /// Record the entry a refresh or a history selection asked to render, so the evidence stack
    /// summary can describe it once its frame is on screen. It is copied once per request and
    /// shared by every frame of it after.
    pub(crate) fn expect_entry(&mut self, entry: &HistoryEntry) {
        self.requested_render_entry = Some(Arc::new(entry.clone()));
    }

    /// The intent a job of `generation` was requested with, while it is still known.
    pub(crate) fn intent(&self, generation: u64) -> Option<PreviewIntent> {
        self.pending_intent.get(&generation).copied()
    }

    /// Take one result for the photograph apart by its phase.
    ///
    /// The delivery rule, the same monotone one the queue itself applies: present whatever is not
    /// older than what is on screen. Rejecting everything but the newest generation presents no
    /// frames at all under a sustained drag, because a render almost always finishes after a newer
    /// job has been asked for. A job's exact phase carries its proxy's own generation, so equality
    /// is delivered too.
    ///
    /// A proxy frame is retained here, so a zoom back to Fit hands it over again instead of
    /// rendering and the clipping overlay can follow the drag before the exact phase lands. An
    /// exact frame is received here: with its report it waits in `incoming` to be adopted with the
    /// pixels, and without one it replaces the retained exact raster now, so no overlay is derived
    /// from an older image. Only an exact result can say why a job that offered bounds has no
    /// proxy phase.
    pub(crate) fn take(&mut self, result: PreviewResult) -> Presented {
        if result.generation < self.presented_generation {
            return Presented::Stale;
        }
        let PreviewResult {
            generation,
            entry_id,
            identity,
            draft_revision,
            intent,
            viewport_declined,
            outcome,
            approximate_white_balance,
            render_ms,
            queue_wait_ms: _,
        } = result;
        let delivery = Delivery {
            generation,
            stage: (identity.width, identity.height),
            draft: identity.draft.as_ref().map(|stamp| stamp.draft_id.clone()),
            entry_id,
            draft_revision,
            intent,
            viewport_declined,
            approximate_white_balance,
            render_ms,
        };
        match outcome {
            PhaseOutcome::Region(region) => Presented::Region(Box::new((delivery, region))),
            PhaseOutcome::Overlay(overlay) => Presented::Overlay(generation, overlay),
            PhaseOutcome::Proxy(outcome) => {
                let frame = ProxyFrame {
                    generation,
                    raster: Arc::new(outcome.raster),
                    dimensions: outcome.dimensions,
                    built: outcome.built,
                    approximation: outcome.approximation,
                    approximate_white_balance,
                    render_ms,
                };
                self.proxy_frame = Some(frame.clone());
                Presented::Proxy(Box::new((delivery, frame)))
            }
            PhaseOutcome::Exact(outcome) => {
                let ExactOutcome {
                    result,
                    report,
                    mask_overlay,
                    proxy_declined,
                } = *outcome;
                self.proxy_declined = proxy_declined;
                let frame = result.map(|raster| ExactFrame {
                    generation,
                    raster: Arc::new(raster),
                    approximate_white_balance,
                    render_ms,
                    content: self.pending_content.get(&generation).copied(),
                });
                if let Ok(frame) = &frame {
                    let analysis = report.map(|report| Analysis {
                        generation,
                        identity,
                        report,
                    });
                    self.receive(frame.clone(), analysis);
                }
                Presented::Exact(Box::new((delivery, frame, mask_overlay)))
            }
        }
    }

    /// Take up an exact phase's frame. With a report it waits to be adopted with its pixels
    /// ([`Self::adopt`]). Without one it replaces the retained exact raster now, so no overlay is
    /// derived from an older image. A frame with no reduction for an ordinary reason clears the
    /// report too; a frame that approximates a drafted RAW white balance never had one to give, so
    /// the last exact report stays plotted, marked updating, until an exact frame's report replaces
    /// it: the histogram is never adopted from an approximate frame.
    pub(crate) fn receive(&mut self, frame: ExactFrame, analysis: Option<Analysis>) {
        match analysis {
            Some(analysis) => self.incoming = Some((analysis, frame)),
            None => {
                self.incoming = None;
                if !frame.approximate_white_balance {
                    self.analysis = None;
                }
                self.exact = Some(frame);
            }
        }
    }

    /// Adopt the report and frame waiting for `generation`, now that its pixels are on screen.
    /// `false` when nothing waits for it: a report from a frame that is not the one just shown
    /// describes another image.
    pub(crate) fn adopt(&mut self, generation: u64) -> bool {
        let Some((analysis, mut frame)) = self.incoming.take() else {
            return false;
        };
        if analysis.generation != generation {
            return false;
        }
        frame.content = self.pending_content.get(&generation).copied();
        // A reduced frame is exact: an approximate one is never reduced.
        frame.approximate_white_balance = false;
        self.exact = Some(frame);
        self.analysis = Some(analysis);
        self.analysis_content = self.pending_content.get(&generation).copied();
        true
    }

    /// The exact phase of a job whose proxy is already on screen moves the generation on screen
    /// to its own, carrying the proxy frame on screen with it: both are the same picture.
    pub(crate) fn restamp(&mut self, generation: u64) {
        if generation == self.presented_generation {
            return;
        }
        if let Some(proxy) = &mut self.proxy_frame
            && proxy.generation == self.presented_generation
        {
            proxy.generation = generation;
        }
        self.presented_generation = generation;
    }

    /// Make a frame the photograph on the presenter and record that it is on screen, rendered for
    /// `entry` at `draft_revision`. The surface borrows the frame's own buffer, so this copies no
    /// pixels. Returns the frame's content serial.
    pub(crate) fn show(
        &mut self,
        frame: &Retained,
        stage: (u32, u32),
        entry: &EntryId,
        draft_revision: Option<u64>,
    ) -> u64 {
        let generation = frame.generation();
        let proxy = frame.proxy().is_some();
        let content = self
            .pending_content
            .get(&generation)
            .copied()
            .unwrap_or(self.presented_content);
        // A new version, so the primitive writes the frame exactly once however often the same
        // raster is drawn. Nothing but a new frame moves it.
        self.presenter.clear_region();
        self.region_raster = None;
        if proxy {
            self.presenter.show_proxy(frame.raster(), content);
        } else {
            self.presenter.show_full(frame.raster(), content);
        }
        self.dimensions = Some(stage);
        self.presented_generation = generation;
        self.presented_content = content;
        self.presented_proxy = proxy;
        self.presented_approximate_white_balance = frame.approximate_white_balance();
        if let Some(bounds) = self.pending_bounds.remove(&generation) {
            self.presented_bounds = bounds;
        }
        self.pending_bounds
            .retain(|pending, _| *pending > generation);
        // A proxy frame whose job asked for a grid has it still to come, in the overlay phase that
        // follows it; any other frame carried its own, or none was asked for.
        self.overlay_awaited =
            (proxy && self.pending_overlay.contains(&generation)).then_some(generation);
        self.pending_overlay.retain(|pending| *pending > generation);
        self.refit_pending = false;
        self.presented_entry = Some(entry.clone());
        self.displayed_draft_revision = draft_revision;
        if self
            .requested_render_entry
            .as_ref()
            .is_some_and(|requested| requested.id == *entry)
        {
            self.rendered_entry = self.requested_render_entry.clone();
        }
        // A frame on screen is the proof the last failure is over.
        self.render_error = None;
        content
    }

    /// Make a visible region the region slot's pixels and record that it is on screen. `false`
    /// when the surface refused it, and then nothing is recorded.
    pub(crate) fn show_region(
        &mut self,
        delivery: &Delivery,
        frame: &luxforge_core::RegionFrame,
        quality: luxforge_ui::RegionQuality,
        content: u64,
    ) -> bool {
        let generation = delivery.generation;
        if !self
            .presenter
            .show_region(frame, quality, content, generation)
        {
            return false;
        }
        self.region_raster = Some(PresentedRegion {
            generation,
            content,
            raster: Arc::new(frame.raster.clone()),
            rect: frame.full_rect,
            full_stage: frame.full_stage,
            raster_rect: frame.rect,
            raster_stage: frame.stage,
            quality,
            approximate: quality == luxforge_ui::RegionQuality::Interactive
                || delivery.approximate_white_balance,
        });
        self.dimensions = Some((frame.full_stage.width, frame.full_stage.height));
        self.presented_generation = generation;
        self.presented_content = content;
        self.presented_entry = Some(delivery.entry_id.clone());
        self.displayed_draft_revision = delivery.draft_revision;
        self.displayed_draft_id = delivery.draft.clone();
        // `presented_proxy` means a whole-output display proxy for Fit/50% hand-over. A
        // half-detail viewport is a different slot and must not enter that zoom rule.
        self.presented_proxy = false;
        self.presented_approximate_white_balance = delivery.approximate_white_balance;
        self.refit_pending = false;
        self.render_error = None;
        // A region carries its own grid, so no proxy's grid is awaited over it.
        self.overlay_awaited = None;
        true
    }

    /// What a zoom that wants the display proxy — or, with `wants_proxy` false, the exact render —
    /// finds for the frame on screen.
    pub(crate) fn zoom(&self, wants_proxy: bool) -> Zoomed {
        if self.presented_generation == 0 || wants_proxy == self.presented_proxy {
            return Zoomed::Kept;
        }
        if self.presenter.photo().is_none() && self.render_error.is_some() {
            return Zoomed::Withdrawn;
        }
        let held = if wants_proxy {
            self.proxy().cloned().map(Retained::Proxy)
        } else {
            self.exact().cloned().map(Retained::Exact)
        };
        held.map_or(Zoomed::Missing, Zoomed::Hand)
    }

    /// Take the photograph off the surface with everything that describes it — the retained
    /// frames, the histogram and the entry and draft revision it was rendered for — so nothing on
    /// screen claims to show a state it does not. Returns the entry it showed.
    pub(crate) fn withdraw(&mut self) -> Option<EntryId> {
        self.presenter.withdraw_photo();
        self.region_raster = None;
        self.proxy_frame = None;
        self.exact = None;
        self.incoming = None;
        self.analysis = None;
        self.analysis_content = None;
        self.held_by_proxy = None;
        self.presented_proxy = false;
        self.presented_approximate_white_balance = false;
        self.rendered_entry = None;
        self.displayed_draft_revision = None;
        self.displayed_draft_id = None;
        self.presented_entry.take()
    }

    /// Every frame the canvas draws, for the view. An overlay is drawn only while it belongs to the
    /// frame on screen; the clipping overlay only while it is the one `clipping` asked for. The
    /// mask draft and the crop draft are the caller's to add.
    pub(crate) fn surfaces(&self, clipping: Option<&OverlayRequest>) -> view::Surfaces<'_> {
        view::Surfaces {
            photo: self.presenter.photo_for(self.presented_content),
            photo_content: self.presenter.full_content(),
            current_content: self.presented_content,
            region: self.presenter.region(),
            region_clipping: self.presenter.region_clipping(),
            region_coverage: self.presenter.region_coverage(),
            stage: self.presenter.stage(),
            clipping: self.clipping(clipping),
            coverage: self.coverage(),
            mask_draft: None,
            mask_map: None,
            draft: None,
        }
    }

    /// The clipping overlay to draw over the photograph: the one on the presenter, when it was
    /// derived for `request` and that request belongs to the frame on screen. An overlay derived
    /// from a superseded raster is held back rather than drawn over another image.
    pub(crate) fn clipping(&self, request: Option<&OverlayRequest>) -> Option<&luxforge_ui::Frame> {
        let request = request?;
        if request.generation != self.presented_generation {
            return None;
        }
        if let Some(region) = self.region_raster.as_ref().filter(|region| {
            request.region == Some(region.rect) && region.generation == request.generation
        }) {
            return self
                .presenter
                .region_clipping()
                .filter(|overlay| {
                    overlay.content_id == region.content
                        && overlay.generation == region.generation
                        && overlay.quality == region.quality
                })
                .map(|overlay| &overlay.frame);
        }
        self.presenter.clipping(request.generation)
    }

    /// The mask coverage to draw over the photograph: the one on the presenter, when it belongs to
    /// the frame that is on screen.
    pub(crate) fn coverage(&self) -> Option<&luxforge_ui::Frame> {
        if let Some(region) = self.region_raster.as_ref()
            && region.generation == self.presented_generation
        {
            return self
                .presenter
                .region_coverage()
                .filter(|overlay| {
                    overlay.content_id == region.content
                        && overlay.generation == region.generation
                        && overlay.quality == region.quality
                })
                .map(|overlay| &overlay.frame);
        }
        self.presenter.coverage(self.presented_generation)
    }

    /// The proxy frame retained for the generation on screen, when there is one.
    pub(crate) fn proxy(&self) -> Option<&ProxyFrame> {
        self.proxy_frame
            .as_ref()
            .filter(|frame| frame.generation == self.presented_generation)
    }

    /// The exact frame retained for the generation on screen, when its exact phase has landed.
    pub(crate) fn exact(&self) -> Option<&ExactFrame> {
        self.exact
            .as_ref()
            .filter(|frame| frame.generation == self.presented_generation)
    }

    /// The content serial of the retained exact raster.
    pub(crate) fn exact_content(&self) -> Option<u64> {
        self.exact.as_ref().and_then(|frame| frame.content)
    }

    /// A photograph or a region is on the surface.
    pub(crate) fn has_picture(&self) -> bool {
        self.presenter.photo().is_some() || self.presenter.region().is_some()
    }

    /// A newer frame has been requested than the one the histogram describes, so the plotted counts
    /// are one generation behind and the plot says so rather than going blank.
    ///
    /// The comparison is against the content of the newest **requested** preview, not against
    /// whether a worker happens to be busy: a crop draft's own truncated job shares the queue and is
    /// never analysed, so queue business alone would mark a perfectly current histogram stale.
    pub(crate) fn analysis_updating(&self) -> bool {
        match &self.analysis {
            Some(_) => self.analysis_content != Some(self.content_serial),
            None => false,
        }
    }
}

/// What a presented proxy frame holds back until its generation's exact phase lands.
///
/// A proxy is the photograph, but every number a captured frame reports — the histogram, the
/// clipping counters, the overlay it is checked against — comes from the exact render. So a
/// scripted step's settle and an open request's outcome both wait for that phase rather than
/// releasing on the proxy alone, which is what keeps every existing assertion about a drafted or
/// selected frame meaning what it meant before.
pub(crate) struct HeldByProxy {
    pub(crate) generation: u64,
    pub(crate) settle: Option<Settle>,
    /// An open request was still pending when the proxy was presented, so it completes when the
    /// exact phase lands rather than on the proxy alone.
    pub(crate) ready: bool,
}

/// The visible pixels the region slot owns, retained for a viewport-bounded clipping derivation.
/// Its raster shares the worker's allocation and is never used as a whole-image analysis input.
pub(crate) struct PresentedRegion {
    pub(crate) generation: u64,
    pub(crate) content: u64,
    pub(crate) raster: Arc<luxforge_core::Raster>,
    pub(crate) rect: Region,
    pub(crate) full_stage: luxforge_core::StageSize,
    /// The raster's footprint in its own stage. Half-detail scaled rectangles can extend beyond
    /// `rect` after floor/ceil rounding, and raster-derived clipping follows this footprint.
    pub(crate) raster_rect: Region,
    pub(crate) raster_stage: luxforge_core::StageSize,
    pub(crate) quality: luxforge_ui::RegionQuality,
    pub(crate) approximate: bool,
}

/// One rectangle of physical pixels as bounds the core will accept, or `None` when the surface has
/// no room at all. The core clamps them to its own limits; rounding here is the only conversion.
pub(super) fn bounds_of((width, height): (f32, f32)) -> Option<ProxyBounds> {
    (width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0).then(|| {
        ProxyBounds {
            width: width.round() as u32,
            height: height.round() as u32,
        }
        .clamped()
    })
}

/// The output pixels a percentage view can display now. The scrollable reports its offset in
/// logical pixels, while percent zoom is defined in physical pixels; the widget's box uses the
/// same division by display scale. One guard pixel covers snapped edges and linear sampling.
pub(super) fn viewport_rect(
    stage: (u32, u32),
    zoom: &Zoom,
    display_scale: f32,
    surface: (f32, f32),
    pan: (f32, f32),
) -> Option<Region> {
    let Zoom::Percent { value } = zoom else {
        return None;
    };
    if *value < 100.0 {
        return None;
    }
    let scale = *value / 100.0 / display_scale;
    if !(scale.is_finite()
        && scale > 0.0
        && surface.0 >= 1.0
        && surface.1 >= 1.0
        && stage.0 > 0
        && stage.1 > 0
        && pan.0.is_finite()
        && pan.1.is_finite())
    {
        return None;
    }
    let edge = |start: f32, length: f32, limit: u32| {
        let first = ((start / scale).floor() as i64 - 1).clamp(0, i64::from(limit)) as u32;
        let last = (((start + length) / scale).ceil() as i64 + 1)
            .clamp(i64::from(first), i64::from(limit)) as u32;
        (first, last)
    };
    let (x0, x1) = edge(pan.0, surface.0, stage.0);
    let (y0, y1) = edge(pan.1, surface.1, stage.1);
    (x1 > x0 && y1 > y0).then_some(Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

pub(super) fn contains_region(outer: Region, inner: Region) -> bool {
    outer.x0 <= inner.x0
        && outer.y0 <= inner.y0
        && outer.x1() >= inner.x1()
        && outer.y1() >= inner.y1()
}

pub(super) fn intersects_region(a: Region, b: Region) -> bool {
    a.x0 < b.x1() && b.x0 < a.x1() && a.y0 < b.y1() && b.y0 < a.y1()
}

pub(super) fn surface_photo_needs_update(
    gpu: &luxforge_ui::SurfaceDiagnostics,
    has_picture: bool,
    render_failed: bool,
) -> bool {
    has_picture && !render_failed && (gpu.drawn_stale_photo || gpu.drawn_photo_blank)
}

impl Editor {
    /// A deferred surface write may draw retained pixels after the presenter has adopted a new
    /// frame. The surface posts a one-shot wake when that condition begins or ends; this reads its
    /// last draw without polling or scheduling another render.
    pub(crate) fn surface_photo_updating(&self) -> bool {
        surface_photo_needs_update(
            &luxforge_ui::surface_diagnostics(),
            self.presentation.has_picture(),
            self.presentation.render_error.is_some(),
        )
    }

    pub(crate) fn visible_detail_updating(&self) -> bool {
        if self.surface_photo_updating() {
            return true;
        }
        let Some(stage) = self.presentation.dimensions else {
            return false;
        };
        let Some(wanted) = self.desired_view_for(stage) else {
            return false;
        };
        if self.presentation.presenter.full_content() == Some(self.presentation.presented_content)
            && self.presentation.exact_content() == Some(self.presentation.presented_content)
        {
            return false;
        }
        self.presentation
            .region_raster
            .as_ref()
            .is_none_or(|region| {
                region.content != self.presentation.presented_content
                    || region.quality != luxforge_ui::RegionQuality::Exact
                    || !contains_region(region.rect, wanted)
            })
    }
    pub(super) fn cancel_preview_queue(&mut self) -> u64 {
        let generation = self.presentation.cancel();
        self.view_plan.request_generation = None;
        self.view_plan.epoch = self.view_plan.epoch.saturating_add(1);
        self.view_plan.dirty = true;
        self.view_plan.quiet_since = None;
        generation
    }
    pub(super) fn desired_view_for(&self, stage: (u32, u32)) -> Option<Region> {
        viewport_rect(
            stage,
            &self.session.preview.view.zoom,
            self.view_state.scale_factor,
            layout::photo_surface(
                self.view_state.window,
                self.session.workspace.state_panel,
                self.session.workspace.tools_panel,
            ),
            self.view_state.local_pan,
        )
    }
    /// One message about taking up or presenting a preview frame.
    pub(super) fn preview_update(&mut self, message: PreviewMessage) -> Task<Message> {
        match message {
            PreviewMessage::ViewLoaded {
                epoch,
                intent,
                result,
            } => {
                self.view_plan.in_flight = false;
                if epoch != self.view_plan.epoch {
                    self.view_plan.dirty = true;
                    return Task::none();
                }
                match result {
                    Ok(job) => {
                        let mut job = *job;
                        if self.document.state.as_ref().map(|state| &state.asset.id)
                            != Some(&job.evaluation.entry().asset_id)
                            || self.displayed_entry().as_ref() != Some(&job.evaluation.entry().id)
                            || job.identity.draft.as_ref().map(|stamp| &stamp.draft_id)
                                != self.session.draft.as_ref().map(|draft| &draft.draft_id)
                            || job.evaluation.draft_revision()
                                != self
                                    .session
                                    .draft
                                    .as_ref()
                                    .map(|draft| draft.draft_revision)
                            || self.crop_gesture().is_some()
                            || self
                                .core_gesture()
                                .is_some_and(|gesture| !gesture.draft.drained())
                            || self.presentation.queue.pending_generation().is_some_and(
                                |generation| self.view_plan.request_generation != Some(generation),
                            )
                        {
                            self.view_plan.dirty = true;
                            return Task::none();
                        }
                        job.intent = intent;
                        let generation = self.request_preview(job);
                        self.presentation.preview_generation = generation;
                        self.view_plan.request_generation = Some(generation);
                        self.view_plan.dirty = false;
                        self.event("preview_view_requested", || json!({
                            "generation":generation,
                            "intent":if intent == PreviewIntent::Settle {"settle"} else {"interactive"},
                        }));
                    }
                    Err(error) => {
                        self.status.text = error;
                        self.view_plan.dirty = false;
                        self.view_plan.quiet_since = None;
                    }
                }
            }
            PreviewMessage::QuietTick => return self.quiet_refine(),
            PreviewMessage::ThumbnailSource(planned) => self.thumbnail_source_planned(planned),
            PreviewMessage::Loaded(result) => {
                if matches!(&result, Ok(payload) if self.preview_superseded(payload)) {
                    return Task::none();
                }
                match result {
                    Ok(payload) => {
                        let payload = *payload;
                        self.adopt(payload.session);
                        // History selection changes the authoritative values shown by generated
                        // controls. A field being edited in the previous entry must not pin its
                        // text while the selected entry is read-only; the entry's own values arrive
                        // with its recipe rows, below.
                        self.controls.editing = None;
                        self.controls.dragging = None;
                        let entry = payload.job.evaluation.entry().id.clone();
                        self.presentation
                            .expect_entry(payload.job.evaluation.entry());
                        self.show_entry(entry.clone());
                        self.presentation.preview_generation = self.request_preview(payload.job);
                        self.status.text = "Rendering selected history state…".into();
                        // The recipe rows follow the displayed entry: one payload read, no render.
                        if let Some(state) = &self.document.state {
                            return recipe_task(
                                self.owner.clone(),
                                self.client,
                                state.asset.id.clone(),
                                Some(entry),
                            );
                        }
                    }
                    Err(error) => self.status.text = error,
                }
            }
            PreviewMessage::Poll => {
                // Both workers wake the event loop through one channel; neither has a poll of its
                // own, and the subscription that carries their signals exists only while one of
                // them is busy. Each worker starts its next job by itself, so nothing here keeps
                // the work moving: this only takes up what has finished. `Poll` is idempotent, so
                // a signal that arrives late costs nothing.
                while let Some(done) = self.overlays.queue.poll() {
                    self.overlay_ready(done);
                }
                while let Some(done) = self.thumbnailer.queue.poll() {
                    self.thumbnails_ready(done);
                }
                let delivered = self.deliver_previews();
                return Task::batch([delivered, self.poll_again()]);
            }
        }
        Task::none()
    }

    pub(super) fn note_view_motion(&mut self) {
        self.view_plan.dirty = true;
        self.view_plan.quiet_since = Some(Instant::now());
        self.view_plan.quiet_settle_requested = false;
        self.view_plan.epoch = self.view_plan.epoch.saturating_add(1);
        if let (Some(stage), Some(region)) = (
            self.presentation.dimensions,
            self.presentation.region_raster.as_ref(),
        ) && self.presentation.presenter.full_content()
            != Some(self.presentation.presented_content)
            && self
                .desired_view_for(stage)
                .is_some_and(|wanted| !contains_region(region.rect, wanted))
            && let Some(render) = &mut self.activity.render
        {
            render.proxy = true;
        }
    }

    fn view_plan(&mut self, intent: PreviewIntent) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        let draft = match self.core_gesture() {
            Some(gesture) if gesture.draft.drained() => Some(gesture.draft.draft_id.clone()),
            Some(_) => return Task::none(),
            None => None,
        };
        self.view_plan.in_flight = true;
        tasks::view_preview_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            self.displayed_entry(),
            draft,
            self.view_plan.epoch,
            intent,
        )
    }

    /// Admit a view-only pan only after gesture and crop-owned requests drain. The local scroll
    /// offset is already updated; a delayed owner pan reply never chooses the rectangle.
    pub(super) fn reconcile_view(&mut self) -> Task<Message> {
        if !self.view_plan.dirty || self.view_plan.in_flight || self.crop_gesture().is_some() {
            return Task::none();
        }
        if self
            .presentation
            .queue
            .pending_generation()
            .is_some_and(|generation| self.view_plan.request_generation != Some(generation))
        {
            // Only another standalone view may be replaced. A draft.set or crop input stage owns
            // the single pending slot until its own frame or cancellation is delivered.
            return Task::none();
        }
        let Some(stage) = self.presentation.dimensions else {
            return Task::none();
        };
        let Some(wanted) = self.desired_view_for(stage) else {
            self.view_plan.dirty = false;
            if self.core_gesture().is_none() {
                self.view_plan.quiet_since = None;
            }
            return Task::none();
        };
        if self.presentation.presenter.full_content() == Some(self.presentation.content_serial)
            && self.presentation.exact.as_ref().is_some_and(|frame| {
                (frame.raster.width, frame.raster.height) == stage
                    && frame.content == Some(self.presentation.content_serial)
            })
        {
            self.view_plan.dirty = false;
            if self.presentation.analysis_content == Some(self.presentation.content_serial) {
                self.view_plan.quiet_since = None;
            }
            return Task::none();
        }
        if self
            .presentation
            .region_raster
            .as_ref()
            .is_some_and(|region| {
                region.content == self.presentation.content_serial
                    && region.quality == luxforge_ui::RegionQuality::Exact
                    && contains_region(region.rect, wanted)
            })
        {
            self.view_plan.dirty = false;
            return Task::none();
        }
        // A cancelled gesture or a returned history selection has no motion to debounce.
        // Its committed whole-frame settlement may already have been replaced by this view
        // retry, so the replacement must itself produce the exact report and retained raster.
        self.view_plan(
            if self.core_gesture().is_none() && self.view_plan.quiet_since.is_none() {
                PreviewIntent::Settle
            } else {
                PreviewIntent::Interactive
            },
        )
    }

    fn quiet_refine(&mut self) -> Task<Message> {
        let Some(since) = self.view_plan.quiet_since else {
            return Task::none();
        };
        if since.elapsed() < QUIET_INTERVAL
            || self.view_plan.quiet_settle_requested
            || self.view_plan.in_flight
            || self.view_plan.dirty
            || self.presentation.queue.is_busy()
            || self.crop_gesture().is_some()
            || self
                .core_gesture()
                .is_some_and(|gesture| !gesture.draft.drained())
        {
            return Task::none();
        }
        if self.presentation.analysis_content == Some(self.presentation.content_serial)
            && self.presentation.exact_content() == Some(self.presentation.content_serial)
        {
            self.view_plan.quiet_since = None;
            return Task::none();
        }
        self.view_plan.quiet_settle_requested = true;
        self.event("preview_quiet_refine", || {
            json!({
                "elapsed_ms":since.elapsed().as_secs_f64()*1000.0,
                "interval_ms":QUIET_INTERVAL.as_millis(),
            })
        });
        self.view_plan(PreviewIntent::Settle)
    }

    /// The physical pixels the photo area can show a frame in, when the view means a display-size
    /// render is what should be presented — or `None` when only the exact render will do.
    ///
    /// At Fit that is the photo surface less the canvas padding, scaled by the display factor:
    /// exactly the rectangle [`state::histogram::displayed_size`] fits an image into, so the proxy
    /// is rendered at the size the display was going to minify the exact frame down to anyway. It
    /// does not depend on the photograph, so the first job of an open already has it.
    ///
    /// At a percentage the bounds are the exact stage's own displayed size, and only while that is
    /// smaller than the stage in both axes. At 100% and above one physical pixel shows one stage
    /// pixel or more, so there is nothing to bound: `None`, which is what keeps the 100% view the
    /// exact render of the exact recipe. `None` as well when nothing is known yet.
    pub(crate) fn proxy_bounds(&self) -> Option<ProxyBounds> {
        self.proxy_bounds_for(self.presentation.dimensions)
    }

    /// [`Self::proxy_bounds`] for a frame whose exact stage is `stage`: the photograph's output
    /// stage, or a crop draft's input stage, which the same rule gives a proxy at Fit and at a
    /// percentage that draws it smaller than it is, and the exact render otherwise.
    pub(crate) fn proxy_bounds_for(&self, stage: Option<(u32, u32)>) -> Option<ProxyBounds> {
        match self.session.preview.view.zoom {
            Zoom::Fit => {
                let workspace = &self.session.workspace;
                let surface = layout::photo_surface(
                    self.view_state.window,
                    workspace.state_panel,
                    workspace.tools_panel,
                );
                let inset = layout::FIT_INSET;
                bounds_of((
                    (surface.0 - inset.0).max(0.0) * self.view_state.scale_factor,
                    (surface.1 - inset.1).max(0.0) * self.view_state.scale_factor,
                ))
            }
            Zoom::Percent { .. } => {
                let stage = stage?;
                let displayed = self.displayed_size(stage)?;
                // Strictly smaller in both axes, so a proxy is never asked for a frame that would
                // have to be magnified back up to show the detail the zoom asked for.
                (displayed.0 < stage.0 as f32 && displayed.1 < stage.1 as f32)
                    .then(|| bounds_of(displayed))
                    .flatten()
            }
        }
    }

    /// The physical size a frame whose exact stage is `stage` is drawn at in the current view,
    /// through [`state::histogram::displayed_size`]: the zoom, the photo surface this window and
    /// these panels leave, the display scale and the Fit inset. The one place the current view's
    /// displayed size is assembled; the proxy bounds at a percentage and the clipping and coverage
    /// grids all read it.
    pub(crate) fn displayed_size(&self, stage: (u32, u32)) -> Option<(f32, f32)> {
        let workspace = &self.session.workspace;
        state::histogram::displayed_size(
            match self.session.preview.view.zoom {
                Zoom::Fit => state::canvas::ZoomView::Fit,
                Zoom::Percent { value } => state::canvas::ZoomView::Percent(value),
            },
            stage,
            layout::photo_surface(
                self.view_state.window,
                workspace.state_panel,
                workspace.tools_panel,
            ),
            self.view_state.scale_factor,
            layout::FIT_INSET,
        )
    }

    /// The next preview result that carries something to show: a frame or a failure.
    ///
    /// An exact phase a newer request stopped is delivered too, under its own generation, but it
    /// carries no frame, so it is taken up here and never reaches [`Self::preview_ready`]: it is
    /// recorded as that generation's, and when it was the crop draft's input stage the draft it was
    /// for ends, because no frame for it will come.
    pub(super) fn poll_preview(&mut self) -> Option<PreviewResult> {
        loop {
            let result = self.presentation.queue.poll()?;
            if !result.cancelled() {
                return Some(result);
            }
            let draft = Some(result.generation) == self.draft_generation();
            self.presentation.forget(result.generation);
            if self.view_plan.request_generation == Some(result.generation) {
                self.view_plan.request_generation = None;
                self.view_plan.dirty = true;
            }
            self.event(
                "preview_exact_cancelled",
                || json!({ "generation": result.generation, "draft": draft }),
            );
            if draft {
                self.draft_preview_superseded(Some(result.generation));
            }
        }
    }

    /// Take up the finished preview results in order: every one that presents nothing — a stale
    /// or cancelled outcome, an exact phase adopted behind its proxy, a failure — and at most one
    /// that hands a frame to the display, after which the rest wait for the next `Poll`, so every
    /// presented frame is drawn by the redraw its own update requests. The photograph and the crop
    /// draft's input stage alike become the surface's source in the update that takes them up.
    pub(super) fn deliver_previews(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while let Some(result) = self.poll_preview() {
            let generation = result.generation;
            let terminal = result.phase() == PreviewPhase::Exact
                || (result.intent == PreviewIntent::Interactive
                    && result.phase() != PreviewPhase::Exact);
            let (task, presented) = self.preview_ready(result);
            if terminal {
                self.presentation.forget(generation);
            }
            tasks.push(task);
            if presented {
                break;
            }
        }
        Task::batch(tasks)
    }

    /// One more `Poll` while a worker still holds a finished result. The wake channel holds one
    /// signal and coalesces, so the signal of a result behind the one just presented may already
    /// have been spent; a result that finishes after this check posts its own.
    pub(super) fn poll_again(&self) -> Task<Message> {
        if self.overlays.queue.ready()
            || self.presentation.queue.ready()
            || self.thumbnailer.queue.ready()
        {
            Task::done(Message::Preview(PreviewMessage::Poll))
        } else {
            Task::none()
        }
    }

    /// Take up one preview result that carries something to show. Returns what it asks the runtime
    /// for, and whether it handed a frame to the display — the photograph, or the crop draft's
    /// input stage.
    pub(super) fn preview_ready(&mut self, result: PreviewResult) -> (Task<Message>, bool) {
        // The crop draft's truncated preview shares the queue; its generation says which texture
        // the pixels belong to. It is never analysed, because its identity describes the whole
        // stack rather than the layer prefix it renders. A slider gesture's drafted preview is not
        // this: it renders the whole drafted stack into the ordinary photograph, and is adopted
        // like any other frame.
        let for_draft = Some(result.generation) == self.draft_generation();
        if let Some(stamp) = &result.identity.draft
            && (self.view_plan.released_draft.as_ref() == Some(&stamp.draft_id)
                || self.session.draft.as_ref().map(|draft| &draft.draft_id)
                    != Some(&stamp.draft_id))
        {
            return (Task::none(), false);
        }
        if let Some(queue_wait_ms) = result.queue_wait_ms {
            let phase = match result.phase() {
                PreviewPhase::Proxy => "proxy",
                PreviewPhase::Region => "region",
                PreviewPhase::Overlay => "overlay",
                PreviewPhase::Exact => "exact",
            };
            self.event("preview_result_received", || {
                json!({
                    "generation":result.generation,
                    "phase":phase,
                    "queue_wait_ms":queue_wait_ms,
                    "render_ms":result.render_ms,
                })
            });
        }
        if for_draft {
            return self.stage_ready(result);
        }
        match self.presentation.take(result) {
            Presented::Stale => (Task::none(), false),
            Presented::Region(region) => {
                let (delivery, region) = *region;
                self.region_ready(delivery, region)
            }
            Presented::Overlay(generation, overlay) => {
                self.coverage_ready(generation, overlay);
                (Task::none(), false)
            }
            Presented::Proxy(proxy) => {
                let (delivery, frame) = *proxy;
                self.view_fallback(delivery.generation, &delivery.viewport_declined, "proxy");
                // A proxy frame carries no grid: its job's grid follows it as an overlay phase
                // ([`Self::coverage_ready`]), so the overlay costs no second render and follows a
                // drag at the proxy's pace without holding the frame back.
                self.frame_ready(delivery, Ok(Retained::Proxy(frame)))
            }
            Presented::Exact(exact) => {
                let (delivery, frame, grid) = *exact;
                let generation = delivery.generation;
                self.view_fallback(generation, &delivery.viewport_declined, "exact");
                // A job without a proxy frame carries its coverage grid beside its one frame, and
                // it is laid over that frame once the frame is on screen. The exact phase behind a
                // proxy carries none and leaves that proxy's grid on screen, since both share one
                // generation.
                let grid = self.grid_arrived(generation, grid);
                let taken = self.frame_ready(delivery, frame.map(Retained::Exact));
                if let Some(grid) = grid {
                    self.present_mask_overlay(generation, grid);
                }
                taken
            }
        }
    }

    /// A viewport the worker could not render as a region says which whole-frame path it took.
    fn view_fallback(&self, generation: u64, declined: &Option<String>, phase: &str) {
        if let Some(reason) = declined {
            self.event(
                "preview_view_fallback",
                || json!({"generation":generation,"reason":reason,"phase":phase}),
            );
        }
    }

    /// The coverage grid a phase carries, to lay over its frame once that frame is taken up. When
    /// the overlay was asked for and the host will not draw it — a mask whose coverage depends on
    /// the pixel it reads has no grid until there is an operation whose input to read that pixel
    /// from, and one it can afford to read — the host's own reason is said now rather than an
    /// absence: an overlay switched on and silently not drawn is exactly what "never silently omit
    /// an effect" forbids.
    fn grid_arrived(
        &mut self,
        generation: u64,
        outcome: MaskOverlayOutcome,
    ) -> Option<MaskOverlay> {
        let MaskOverlayOutcome { grid, absent } = outcome;
        if grid.is_none()
            && let Some(reason) = absent
        {
            self.mask_overlay_unavailable(generation, &reason);
        }
        grid
    }

    /// One whole frame of the photograph, or its failure.
    fn frame_ready(
        &mut self,
        delivery: Delivery,
        frame: Result<Retained, luxforge_core::Error>,
    ) -> (Task<Message>, bool) {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                // Only an exact phase fails: a proxy phase that cannot render is declined instead.
                self.preview_failed(
                    delivery.generation,
                    false,
                    &delivery.entry_id,
                    delivery.draft_revision,
                    &error,
                );
                return (Task::none(), false);
            }
        };
        let generation = delivery.generation;
        let proxy = frame.proxy().is_some();
        // The exact phase of a job whose proxy is already on screen, while the view still wants a
        // display-size frame: its report and its raster are taken up and nothing is drawn. The
        // proxy is the Fit view, so writing the same picture again at four times the pixels would
        // cost exactly the work this design exists to remove.
        if !proxy
            && self.presentation.presented_proxy
            && self.proxy_bounds().is_some()
            // A layout refit can make a formerly useful proxy unnecessary (the source now fits the
            // physical Fit bounds). Its exact-only result must replace the old, undersized proxy
            // and clear `refit_pending`; adopting only the report here would leave evidence and the
            // visible view waiting forever. An older exact phase may still be retained while the
            // newer refit is in flight.
            && (self.presentation.presented_bounds == self.proxy_bounds()
                || generation < self.presentation.preview_generation
                || self.proxy_refit_deferred())
            && self.presentation.pending_content.get(&generation)
                == Some(&self.presentation.presented_content)
            && self.presentation.dimensions == Some(delivery.stage)
        {
            self.presentation.restamp(generation);
            self.adopt_exact(
                generation,
                delivery.stage,
                delivery.render_ms,
                delivery.approximate_white_balance,
            );
            self.settle_step(Settle::Preview);
            if self.activity.pending {
                self.activity.pending = false;
                self.activity.displayed = self.activity.requested;
                self.activity.phase = "ready";
                self.outcome_ready(false);
            }
            return (Task::none(), false);
        }
        // The dimensions every pick, every percent-zoom box and every overlay cell maps through
        // are the **exact stage's**, whatever size the texture is; the identity already carries
        // them.
        let stage = if proxy {
            delivery.stage
        } else {
            (frame.raster().width, frame.raster().height)
        };
        if self.activity.pending {
            self.activity.preview_dimensions = Some(stage);
            self.event(
                "decoded",
                || json!({"open_to_raster_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":[stage.0,stage.1],"proxy":proxy}),
            );
        }
        // The photograph reaches the screen from here: the raster becomes the surface's source now
        // and is drawn by the redraw this update requests, with no allocation round trip in
        // between. The report the worker reduced from exactly these pixels was received with them
        // and is adopted in the same update, so the plot, the overlays and the photograph are
        // adopted together.
        self.present(
            frame,
            Arrival::Rendered {
                stage,
                entry: delivery.entry_id,
                draft_revision: delivery.draft_revision,
            },
        );
        // A zoom that changed while this frame was rendering is picked up by `present_retained`.
        self.present_retained();
        (Task::none(), true)
    }

    /// The crop layer's input stage, shown in place of the photograph from the render's own buffer
    /// with the open frame drawn over it in this same update: nothing is uploaded through the
    /// runtime, so nothing waits for it. Like the photograph's, its proxy is the Fit view and its
    /// exact phase the percentage zoom's; neither is ever reduced, sampled or committed, retained
    /// as the photograph's or held to the photograph's delivery rule. A stage job asks for no
    /// viewport, so it has no region phase.
    fn stage_ready(&mut self, result: PreviewResult) -> (Task<Message>, bool) {
        let generation = result.generation;
        let interactive = result.intent == PreviewIntent::Interactive;
        let phase = if result.proxy().is_some() {
            "proxy"
        } else {
            "exact"
        };
        let (frame, proxy, bounded, grid) = match result.outcome {
            PhaseOutcome::Region(_) => return (Task::none(), false),
            PhaseOutcome::Overlay(overlay) => {
                self.coverage_ready(generation, overlay);
                return (Task::none(), false);
            }
            PhaseOutcome::Proxy(outcome) => (
                Ok(outcome.raster),
                true,
                true,
                MaskOverlayOutcome::default(),
            ),
            // A stage frame is bounded when its job offered bounds, whichever phase answered them.
            PhaseOutcome::Exact(outcome) => {
                let ExactOutcome {
                    result,
                    mask_overlay,
                    proxy_declined,
                    ..
                } = *outcome;
                (result, false, proxy_declined.is_some(), mask_overlay)
            }
        };
        self.view_fallback(generation, &result.viewport_declined, phase);
        let grid = self.grid_arrived(generation, grid);
        let taken = match frame {
            Ok(raster) => {
                let (presented, planned) =
                    self.crop_stage_ready(&raster, proxy, bounded, !proxy || interactive);
                (planned, presented)
            }
            Err(error) => {
                self.draft_preview_failed(&error);
                (Task::none(), false)
            }
        };
        if let Some(grid) = grid {
            self.present_mask_overlay(generation, grid);
        }
        taken
    }

    /// A job's coverage grid, arriving after the proxy frame it describes.
    ///
    /// It is drawn only over the frame of its own generation. A grid whose frame is not the one on
    /// screen — a newer frame was presented meanwhile, or its frame was never presented at all —
    /// describes other pixels, so it is dropped and the presenter keeps what it has: the grid of
    /// the frame on screen, or that frame's reason for having none, is still to come or has
    /// already been taken up. Until the grid arrives the frame is drawn without one, since the
    /// grid of an older frame is never drawn over a newer one.
    fn coverage_ready(&mut self, generation: u64, overlay: MaskOverlayOutcome) {
        if self.presentation.overlay_awaited == Some(generation) {
            self.presentation.overlay_awaited = None;
        }
        if generation != self.presentation.presented_generation
            || self.presentation.region_raster.is_some()
        {
            self.event(
                "mask_overlay_dropped",
                || json!({"generation":generation,"presented_generation":self.presentation.presented_generation}),
            );
            return;
        }
        if let Some(grid) = self.grid_arrived(generation, overlay) {
            self.present_mask_overlay(generation, grid);
        }
    }

    /// Publish visible pixels without treating them as a whole-image report or retained full
    /// raster. The worker's region carries its own stage coordinates; the surface maps those
    /// coordinates through the full output stage, including odd dimensions at half detail.
    pub(super) fn region_ready(
        &mut self,
        delivery: Delivery,
        region: RegionOutcome,
    ) -> (Task<Message>, bool) {
        let generation = delivery.generation;
        let intent = delivery.intent;
        let content = self
            .presentation
            .pending_content
            .get(&generation)
            .copied()
            .unwrap_or(self.presentation.presented_content);
        let draft_revision = delivery.draft_revision;
        let render_ms = delivery.render_ms;
        let approximate_white_balance = delivery.approximate_white_balance;
        let RegionOutcome {
            frame,
            mask_overlay,
        } = region;
        let stage = (frame.full_stage.width, frame.full_stage.height);
        let covered = self
            .desired_view_for(stage)
            .is_some_and(|wanted| contains_region(frame.full_rect, wanted));
        if stage != delivery.stage {
            self.view_plan.dirty = true;
            return (Task::none(), false);
        }
        if delivery.draft.as_ref() == self.presentation.displayed_draft_id.as_ref()
            && draft_revision.is_some()
            && self.presentation.displayed_draft_revision.is_some()
            && draft_revision < self.presentation.displayed_draft_revision
        {
            return (Task::none(), false);
        }
        if self
            .desired_view_for(stage)
            .is_some_and(|wanted| !intersects_region(frame.full_rect, wanted))
        {
            self.view_plan.dirty = true;
            return (Task::none(), false);
        }
        if !luxforge_ui::region_texture_admissible((frame.raster.width, frame.raster.height), 8192)
        {
            self.event("preview_region_declined", || json!({
                "generation":generation,"reason":"region texture exceeds the surface allocation limit"
            }));
            self.presentation.viewport_disabled_content = Some(content);
            self.view_plan.dirty = true;
            return (Task::none(), false);
        }
        let quality = if frame.stage == frame.full_stage && !frame.approximation.is_approximate() {
            luxforge_ui::RegionQuality::Exact
        } else {
            luxforge_ui::RegionQuality::Interactive
        };
        if !self
            .presentation
            .show_region(&delivery, &frame, quality, content)
        {
            self.status.text = "Could not show the visible photograph region".into();
            return (Task::none(), false);
        }
        let entry_id = delivery.entry_id;
        self.show_entry(entry_id.clone());
        self.activity.render = Some(state::status::RenderTime {
            ms: render_ms,
            proxy: quality == luxforge_ui::RegionQuality::Interactive || !covered,
            approximate: approximate_white_balance,
        });
        // A region carries its own grid, laid over it once it is on screen.
        let grid = self.grid_arrived(generation, mask_overlay);
        self.event("preview_displayed", || json!({
            "generation":generation,"entry_id":entry_id,
            "draft_revision":draft_revision,"snapshot_id":frame.raster.snapshot_id.to_string(),
            "source_fingerprint":frame.raster.source_fingerprint,
            "dimensions":[stage.0,stage.1],"path":"region",
            "region":[frame.full_rect.x0,frame.full_rect.y0,frame.full_rect.x1(),frame.full_rect.y1()],
            "region_stage":[frame.stage.width,frame.stage.height],
            "quality":if quality == luxforge_ui::RegionQuality::Exact {"exact"} else {"interactive"},
            "proxy_approximate":frame.approximation.is_approximate(),
            "proxy_approximate_reason":frame.approximation.reason(),
            "approximate_white_balance":approximate_white_balance,
            "viewport_declined":delivery.viewport_declined,"render_ms":render_ms,
        }));
        if let Some(wanted) = self.desired_view_for(stage) {
            self.view_plan.dirty = !contains_region(frame.full_rect, wanted);
        }
        if self.view_plan.request_generation == Some(generation) {
            self.view_plan.request_generation = None;
        }
        if intent == PreviewIntent::Interactive && self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.outcome_ready(false);
        }
        if intent == PreviewIntent::Interactive {
            let settle = match self.core_gesture() {
                Some(gesture)
                    if !gesture.draft.drained()
                        || generation < self.presentation.preview_generation =>
                {
                    None
                }
                Some(gesture) if gesture.slider().is_some() => Some(Settle::SliderDraft),
                Some(_) => Some(Settle::Preview),
                // History selection, Return to current and committed edits keep their Preview
                // evidence step open until the whole-frame result updates the displayed stack
                // and exact report. A region proves visible pixels, but cannot settle those
                // correlated state fields; capturing here labelled current pixels as the old
                // history entry until the following evidence tick.
                None => None,
            };
            if let Some(settle) = settle {
                self.settle_step(settle);
            }
            if self.core_gesture().is_none()
                && (self.presentation.analysis_content != Some(content)
                    || self.presentation.exact_content() != Some(content))
            {
                // An interactive view can supersede a committed render, including after a
                // cancelled draft. Leave a timer to replace it with a settled job when the view
                // stops moving; otherwise the histogram can remain stale indefinitely.
                self.view_plan.quiet_since.get_or_insert_with(Instant::now);
                self.view_plan.quiet_settle_requested = false;
            }
        }
        self.refresh_overlay();
        if let Some(grid) = grid {
            self.present_mask_overlay(generation, grid);
        }
        (Task::none(), true)
    }

    /// The exact phase of a job whose proxy is already on screen, received by
    /// [`Presentation::take`].
    ///
    /// Nothing is drawn: the frame the view wants is the proxy, and writing this raster's
    /// four-times larger texture is exactly the work this design exists to remove. Its report and
    /// its pixels are taken up as a presented frame would take them up, so the histogram, the
    /// clipping counters and the overlay describe the exact render of the picture on screen, and a
    /// later `analysis.request` for this identity is a cache hit instead of a second render. The
    /// status bar keeps the proxy's render time meanwhile: the proxy is the picture on screen.
    pub(super) fn adopt_exact(
        &mut self,
        generation: u64,
        stage: (u32, u32),
        render_ms: f64,
        approximate_white_balance: bool,
    ) {
        // The pixels of this generation are already on screen — the proxy of the same recipe — so
        // the report is adopted now rather than waiting for a frame that will not arrive.
        self.adopt_analysis(generation);
        self.event(
            "preview_exact_adopted",
            || json!({"generation":generation,"dimensions":[stage.0,stage.1],"render_ms":render_ms,"approximate_white_balance":approximate_white_balance}),
        );
        self.release_held(generation);
    }

    /// Release what the presented proxy of this generation was holding back: the scripted step it
    /// settles and the open request it completes. Both describe the exact render, which has landed.
    pub(super) fn release_held(&mut self, generation: u64) {
        if self
            .presentation
            .held_by_proxy
            .as_ref()
            .map(|held| held.generation)
            != Some(generation)
        {
            return;
        }
        let Some(held) = self.presentation.held_by_proxy.take() else {
            return;
        };
        if let Some(settle) = held.settle {
            self.settle_step(settle);
        }
        if held.ready && self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.event(
                "render_ready",
                || json!({"displayed_generation":self.activity.displayed}),
            );
            self.outcome_ready(false);
        }
    }

    /// A preview of the displayed target failed: say so on the canvas, and never leave another
    /// entry's picture on screen as though it were this one.
    ///
    /// The frame on screen stays only when it is the target that failed — the display proxy of the
    /// same entry and draft revision, whose full-resolution phase is what failed — because then it
    /// still shows that state. Any other frame belongs to an earlier entry or draft revision: after
    /// a commit whose render failed it is the picture from before the edit, while history and the
    /// recipe already name the edit, so it is withdrawn with everything derived from it and the
    /// canvas shows the failure in its place. The edit itself is untouched; the next frame that
    /// renders puts a picture back.
    pub(super) fn preview_failed(
        &mut self,
        generation: u64,
        proxy: bool,
        entry: &luxforge_core::EntryId,
        draft_revision: Option<u64>,
        error: &luxforge_core::Error,
    ) {
        self.presentation.refit_pending = false;
        self.status.text = error.to_string();
        // The canvas explains the failure from its kind, detail and data: the data names what an
        // unavailable effect is, so the cause is never parsed out of the message.
        self.presentation.render_error = Some(error.clone());
        self.event(
            "preview_failed",
            || json!({"generation":generation,"entry_id":entry,"draft_revision":draft_revision,"proxy":proxy,"error_code":error.kind.code(),"detail":error.detail}),
        );
        let shows_target = self.presentation.presented_entry.as_ref() == Some(entry)
            && self.presentation.displayed_draft_revision == draft_revision
            && self.presentation.displayed_draft_id
                == self
                    .session
                    .draft
                    .as_ref()
                    .map(|draft| draft.draft_id.clone());
        if !shows_target
            && (self.presentation.presenter.photo().is_some()
                || self.presentation.presenter.region().is_some())
        {
            self.withdraw_photo(generation, entry, error);
        }
        // A scripted step waiting for the newest preview's pixels ends on its failure instead: the
        // failure is that step's outcome, and its frame shows it.
        if generation >= self.presentation.preview_generation {
            self.settle_step(Settle::Preview);
        }
        // A failed exact phase releases whatever its proxy was holding, so a scripted step ends on
        // the failure rather than waiting for a frame that will never arrive.
        if !proxy {
            self.release_held(generation);
        }
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.phase = "error";
            self.activity.error_code = Some(error.kind.code().into());
            self.event("render_failed", || json!({"error_code":error.kind.code()}));
            self.outcome_ready(true);
        }
    }

    /// Take the picture of an earlier entry or draft revision off the surface, with everything
    /// that describes it — the retained rasters, the histogram, the overlay's source, the readout
    /// and the render time — so nothing on screen claims to show a state it does not.
    pub(super) fn withdraw_photo(
        &mut self,
        generation: u64,
        target: &luxforge_core::EntryId,
        error: &luxforge_core::Error,
    ) {
        let shown = self.presentation.withdraw();
        self.event("preview_withdrawn", || {
            json!({
                "generation": generation,
                "presented_generation": self.presentation.presented_generation,
                "target_entry": target,
                "withdrawn_entry": shown,
                "error_code": error.kind.code(),
            })
        });
        self.hover.readout = None;
        self.hover.sample.drop_pending();
        self.activity.render = None;
    }

    /// The crop layer's input stage could not be rendered, so the draft it was for cannot open or
    /// rebase.
    pub(crate) fn draft_preview_failed(&mut self, error: &luxforge_core::Error) {
        if self.crop_stage() == Some(crate::app::crop::StageView::Shown) {
            // The stage on screen stays under the frame; only its other phase failed, and says so.
            self.set_draft_generation(None);
            self.status.text = format!("The crop's input stage could not be rendered: {error}");
            self.settle_step(Settle::Draft);
            return;
        }
        self.end_pending_draft(
            format!("The crop's input stage could not be rendered: {error}"),
            error.kind.code(),
            &error.detail,
            None,
        );
    }

    /// The crop layer's input stage was superseded before it rendered: a newer preview request
    /// stopped its job or replaced it in the pending slot, or the stack changed while the owner was
    /// planning it (`generation` is then `None`). No frame will come, so the start it was for ends
    /// as a failed one does, and a reapply keeps its draft.
    ///
    /// It is never re-requested and never shielded from the request that superseded it. Every such
    /// request but a view change comes from a change to the stack or the selection the job was
    /// planned from — another client's commit, this client's own command, undo or history
    /// selection — so its input stage and base revision are stale, and a draft opened on them would
    /// not even be marked conflicted. Shielding it would hold the newer state's frame behind the
    /// whole input-stage render, and requesting it again would stop that frame in turn.
    pub(crate) fn draft_preview_superseded(&mut self, generation: Option<u64>) {
        let Some(crate::app::crop::StageView::Rendering { reapply, .. }) = self.crop_stage() else {
            // A stage already on screen keeps its frame: only the request for its other phase
            // ended, and the next zoom that needs that phase asks for it again.
            self.set_draft_generation(None);
            return;
        };
        let again = if reapply { "reapply" } else { "start" };
        self.end_pending_draft(
            format!(
                "The crop's input stage was superseded by a newer preview: {again} the crop again"
            ),
            luxforge_core::ErrorKind::Cancelled.code(),
            "superseded by a newer preview",
            generation,
        );
    }

    /// A starting or reapplied draft whose input stage will not arrive ends its wait here,
    /// explicitly, rather than waiting for pixels: a start is discarded and returns to the pointer
    /// mode, a reapply keeps the draft it rebased. The photograph on screen is the current state
    /// and stays. The reason reaches the status bar and the log, and a scripted step waiting for
    /// the draft ends on it.
    pub(super) fn end_pending_draft(
        &mut self,
        status: String,
        error_code: &str,
        detail: &str,
        generation: Option<u64>,
    ) {
        let reapply = self.crop_stage_lost();
        self.status.text = status;
        self.event(
            "crop_draft_failed",
            || json!({"reapply": reapply, "error_code": error_code, "detail": detail, "generation": generation}),
        );
        self.settle_step(Settle::Draft);
    }

    /// The zoom changed. This is the **one** place a view change can ask for a render, and it only
    /// does so when the pixels it needs do not exist yet.
    ///
    /// Which texture the view wants is decided by [`Self::proxy_bounds`]: a display-size proxy when
    /// the frame is drawn smaller than the exact stage, the exact render at 100% and above. While
    /// that answer is unchanged there is nothing to do at all — a zoom from Fit to 50% keeps the
    /// proxy it already has — so the rule "a view change re-renders nothing" survives every step
    /// but the one crossing between the two.
    ///
    /// Crossing to the exact render makes the retained exact raster of the frame on screen the
    /// surface's source. When its exact phase is still outstanding there is nothing to hand over
    /// and nothing to ask for: that phase is already running and is presented when it arrives,
    /// because the zoom now needs it, so the view waits with the ordinary loading state.
    ///
    /// Crossing back hands the retained proxy of the frame on screen over again. Only when there is
    /// none — the frame on screen was rendered exactly, at 100% — does this request one preview
    /// job.
    pub(super) fn zoom_changed(&mut self, previous: &Zoom) -> Task<Message> {
        let zoom = self.session.preview.view.zoom.clone();
        if zoom == *previous || self.document.state.is_none() {
            return Task::none();
        }
        // A crop draft's input stage is what the view shows, or is about to: the same rule is
        // answered over the stage, and the photograph behind it asks for nothing that would
        // supersede the stage. The draft's end always brings a new photograph at the zoom then.
        if self.crop_stage_owns_view() {
            return self.present_crop_stage();
        }
        let wants_proxy = self.proxy_bounds().is_some();
        match self.presentation.zoom(wants_proxy) {
            // Nothing presented yet, or the texture on screen is already the one this zoom wants:
            // a step from Fit to 50% keeps the proxy it has, and the rule that a view change
            // re-renders nothing survives every zoom but the one crossing between proxy and exact.
            // Or a failure withdrew the picture: nothing retained may be handed over in its place,
            // and a view change asks for no render. The next frame of the target puts a picture
            // back.
            Zoomed::Kept | Zoomed::Withdrawn => Task::none(),
            Zoomed::Missing if wants_proxy => {
                // Nothing to hand over: the frame on screen is a full-resolution render with no
                // proxy beside it. One preview job produces the display-size frame this zoom wants,
                // and it is the only render any view change asks for.
                self.event("preview_proxy_requested", || json!({ "zoom": zoom }));
                self.await_requested_frame();
                self.request_current_preview()
            }
            Zoomed::Missing => {
                // The exact phase of the frame on screen has not landed. It is already running, and
                // the `Poll` handler presents it when it arrives because the zoom now needs it.
                self.status.text = "Rendering at full resolution…".into();
                Task::none()
            }
            Zoomed::Hand(frame) => {
                self.present(frame, Arrival::Zoom);
                Task::none()
            }
        }
    }

    /// Put the picture the view asks for on screen from pixels already in hand.
    ///
    /// It renders nothing, asks for nothing and writes nothing — the next redraw's `prepare` writes
    /// the texture — so it is safe to call after every presented frame, which is what it is for: a
    /// zoom that changed while a frame was rendering is picked up here rather than leaving the
    /// wrong picture on screen until the next zoom. Preferring a retained frame over a render
    /// whenever the pixels exist is what keeps a view change free: it costs one `Arc` clone.
    pub(super) fn present_retained(&mut self) {
        if let Zoomed::Hand(frame) = self.presentation.zoom(self.proxy_bounds().is_some()) {
            self.present(frame, Arrival::Zoom);
        }
    }

    /// One preview job for the entry on screen, at the bounds the view now asks for. The zoom rule
    /// is the only caller, and only when the pixels it needs do not exist.
    /// Queue one preview job with the bounds of this moment. Every job goes through here: the
    /// bounds a task carried from the owner are replaced by what the window, the panels and the
    /// display scale ask for now, so a job requested once the display scale is known is already at
    /// it and a job requested during a resize is sized for the window it will be shown in. A
    /// truncated job — a crop draft's input stage — is bounded by that stage's own displayed size.
    pub(crate) fn request_preview(&mut self, job: luxforge_core::PreviewJob) -> u64 {
        self.request_preview_inner(job, false).0
    }

    /// Queue a mask frame with phase timing for an evidence run. Ordinary preview requests use
    /// the untimed method and do not read the clock.
    pub(crate) fn request_mask_preview_timed(
        &mut self,
        job: luxforge_core::PreviewJob,
    ) -> (u64, Option<Instant>) {
        debug_assert!(self.log.diagnostics.is_some());
        self.request_preview_inner(job, true)
    }

    pub(super) fn request_preview_inner(
        &mut self,
        mut job: luxforge_core::PreviewJob,
        timed: bool,
    ) -> (u64, Option<Instant>) {
        if job.layer_count.is_none() {
            job.viewport = match self.session.preview.view.zoom {
                Zoom::Percent { value } if value >= 100.0 => {
                    self.desired_view_for((job.identity.width, job.identity.height))
                }
                _ => None,
            };
            if job.intent == PreviewIntent::Immediate {
                job.intent = if job.evaluation.draft_revision().is_some() {
                    PreviewIntent::Interactive
                } else if job.viewport.is_some() {
                    PreviewIntent::Settle
                } else {
                    PreviewIntent::Immediate
                };
            }
        }
        job.proxy = if job.viewport.is_some() {
            None
        } else if job.layer_count.is_some() {
            // A crop draft's input stage takes the photograph's own rule over its own stage: a
            // display-size proxy of the layer prefix wherever the view draws the stage smaller than
            // it is, and alone, because nothing is ever reduced from the stage. Its exact phase
            // is asked for only by a view that needs it ([`Self::present_crop_stage`]).
            let bounds = self.crop_stage_bounds();
            if bounds.is_some() && job.intent == PreviewIntent::Immediate {
                job.intent = PreviewIntent::Interactive;
            }
            bounds
        } else {
            self.proxy_bounds()
        };
        let content = self.presentation.admit(&mut job);
        // The mask overlay's coverage grid rides whichever frame is about to be rendered, so it is
        // attached here rather than by each task that builds a job: one rule, every preview path,
        // and no second render for the overlay. The core validates the request against the stack
        // this job will render, so a mask the stack does not hold leaves the frame without a grid
        // instead of failing the render.
        if let Some(overlay) = self.mask_overlay_request() {
            match job.clone().with_mask_overlay(overlay) {
                Ok(with_overlay) => job = with_overlay,
                Err(error) => self.event(
                    "mask_overlay_refused",
                    || json!({"detail": error.detail.clone()}),
                ),
            }
        }
        self.note_thumbnail_source(&job);
        // The job still waiting in the pending slot is replaced by this one and never starts, so
        // nothing about it will ever be delivered: when it was the crop draft's input stage, the
        // draft it was for ends here, as a cancelled one does in `poll_preview`. The request names
        // it in the same step, because the worker takes a pending job by itself the moment the
        // active one ends: a job it took up meanwhile is running, and ends through `poll_preview`.
        let Requested {
            generation,
            replaced,
            viewport,
            stage,
            at,
        } = self.presentation.request(job, content, timed);
        if replaced.is_some() && replaced == self.view_plan.request_generation {
            self.view_plan.request_generation = None;
            self.view_plan.dirty = true;
        }
        if replaced.is_some() && replaced == self.draft_generation() {
            self.draft_preview_superseded(replaced);
        }
        if viewport.is_some_and(|rect| {
            self.desired_view_for(stage)
                .is_some_and(|wanted| contains_region(rect, wanted))
        }) {
            self.view_plan.dirty = false;
        }
        (generation, at)
    }

    /// Re-render the proxy on screen once when the bounds it was made for no longer match the
    /// window: a resize, a panel toggle or the display scale arriving. The queue coalesces a
    /// storm of these into one active and one pending job, and nothing is asked for while a
    /// gesture or a crop draft owns the preview, or while a refit is already on its way.
    pub(super) fn refit_proxy(&mut self) -> Task<Message> {
        if self.document.state.is_none()
            || self.proxy_refit_deferred()
            || self.presentation.presented_generation == 0
            || !self.presentation.presented_proxy
            || self.presentation.refit_pending
        {
            return Task::none();
        }
        let Some(bounds) = self.proxy_bounds() else {
            return Task::none();
        };
        if self.presentation.presented_bounds == Some(bounds) {
            return Task::none();
        }
        self.presentation.refit_pending = true;
        self.event(
            "preview_proxy_requested",
            || json!({"reason":"bounds","bounds":{"width":bounds.width,"height":bounds.height}}),
        );
        self.await_requested_frame();
        self.request_current_preview()
    }

    /// Drafts own the preview until they finish, so a layout change deliberately leaves their
    /// displayed proxy at its previous bounds instead of starting a competing refit. A refit takes
    /// the one-draft rule alone ([`Starting::Refit`]): it still runs while a request is in flight
    /// or a history entry is previewed.
    pub(super) fn proxy_refit_deferred(&self) -> bool {
        self.gesture_refusal(Starting::Refit).is_some()
    }

    /// A view change has just asked for the frame it needs. A scripted step whose frame is still
    /// to be captured — waiting on the session round trip, or already settled by it earlier in this
    /// same update — waits for that frame instead, so the capture never shows the picture the view
    /// has already replaced, such as a proxy of the previous bounds.
    pub(super) fn await_requested_frame(&mut self) {
        self.await_frame(Settle::Preview);
    }

    /// [`Self::await_requested_frame`] for a frame that settles `settle`: the crop draft's input
    /// stage settles [`Settle::Draft`].
    pub(super) fn await_frame(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence
            && (evidence.awaiting == Some(Settle::Session)
                || (evidence.awaiting.is_none() && evidence.capture_pending))
        {
            evidence.capture_pending = false;
            evidence.awaiting = Some(settle);
        }
    }

    /// Evidence of a displayed proxy waits for the current layout when a refit is permitted.
    /// The exact phase of an open can arm a capture while its display-scale refit is rendering.
    /// Drafts deliberately defer such refits, and can supersede a queued one; their settled frame
    /// can be captured as shown even if that abandoned request left `refit_pending` set.
    pub(super) fn capture_proxy_ready(&self) -> bool {
        if !self.presentation.presented_proxy
            || self.presentation.render_error.is_some()
            || self.proxy_refit_deferred()
        {
            return true;
        }
        if self.presentation.refit_pending {
            return false;
        }
        match self.proxy_bounds() {
            Some(bounds) => self.presentation.presented_bounds == Some(bounds),
            // At 100% the exact frame is the target; the step's normal preview settlement
            // already waits for it, without requiring a proxy that cannot be requested.
            None => true,
        }
    }

    pub(super) fn request_current_preview(&mut self) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        tasks::current_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            entry,
            self.proxy_bounds(),
        )
    }

    /// Make a frame the photograph on the presenter and record that it is on screen: the one
    /// function every whole frame reaches the surface through, whether a render just produced it or
    /// a zoom hands back one retained for the generation on screen.
    ///
    /// This is what "presented" means from here on: the update in which the raster became the
    /// surface's source. The pixels are drawn by the redraw this update requests, which is the next
    /// frame — the primitive's `prepare` writes them into its own texture on the way — so there is
    /// no allocation round trip between a rendered frame and the screen, and no message to wait for.
    ///
    /// Retaining the raster copies nothing: the surface borrows the render's own `Arc<Vec<u8>>`,
    /// which the desktop already holds as the proxy frame or the exact raster of this generation.
    pub(super) fn present(&mut self, frame: Retained, arrival: Arrival) {
        let (stage, entry, draft_revision, zoom) = match arrival {
            Arrival::Rendered {
                stage,
                entry,
                draft_revision,
            } => (stage, entry, draft_revision, false),
            Arrival::Zoom => {
                let Some((stage, entry)) = self
                    .presentation
                    .dimensions
                    .zip(self.presentation.presented_entry.clone())
                else {
                    return;
                };
                (
                    stage,
                    entry,
                    self.presentation.displayed_draft_revision,
                    true,
                )
            }
        };
        let generation = frame.generation();
        let proxy = frame.proxy();
        self.presentation
            .show(&frame, stage, &entry, draft_revision);
        // A zoom hands over a retained frame of the entry already on screen; the entry the desktop
        // is waiting for stays the one picks, readouts and the next request are addressed to.
        if !zoom {
            self.show_entry(entry.clone());
            self.presentation.displayed_draft_id = self
                .session
                .draft
                .as_ref()
                .map(|draft| draft.draft_id.clone());
        }
        self.adopt_analysis(generation);
        // The status bar's figure is this frame's own render time, measured on the worker for the
        // phase that produced it — never the time since the last open or commit, which a drag, a
        // zoom hand-over or a refit presents long after.
        self.activity.render = Some(state::status::RenderTime {
            ms: frame.render_ms(),
            proxy: proxy.is_some(),
            approximate: frame.approximate_white_balance(),
        });
        let raster = frame.raster();
        self.event(
            "preview_displayed",
            || json!({
                "entry_id":entry,
                "snapshot_id":raster.snapshot_id.to_string(),
                // The status bar no longer shows the source hash, so the log is where a frame is
                // correlated with it.
                "source_fingerprint":raster.source_fingerprint,
                "generation":generation,
                "draft_revision":draft_revision,
                "dimensions":[stage.0,stage.1],
                "path":"surface",
                "proxy":proxy.is_some(),
                "proxy_dimensions":proxy.map(|frame| json!([frame.dimensions.0,frame.dimensions.1])),
                "proxy_built":proxy.is_some_and(|frame| frame.built),
                "proxy_approximate":proxy.is_some_and(|frame| frame.approximation.is_approximate()),
                "proxy_approximate_reason":proxy.and_then(|frame| frame.approximation.reason()),
                "approximate_white_balance":frame.approximate_white_balance(),
                "reason":zoom.then_some("zoom"),
                "render_ms":frame.render_ms(),
            }),
        );
        // A scripted preview selection settles on these same pixels, whether or not this frame also
        // belongs to the one open request evidence tracks below. While a slider gesture is open the
        // drafted previews replace one another, so a scripted gesture waits for the one whose
        // settings are the newest.
        // A mask shape gesture drains the same way: while another `draft.set` or the commit is still
        // queued the frame on screen is not the one the step is evidence of, so the step waits for
        // the geometry that settles, and for the newest frame asked for rather than an older one
        // still arriving. A brush back in hand after its stroke committed holds no draft and asks
        // for no frame of its own, so the committed frame settles its step.
        let settle = match self.core_gesture() {
            Some(gesture)
                if gesture.draft.frame_pending()
                    || generation < self.presentation.preview_generation =>
            {
                None
            }
            Some(gesture) if gesture.slider().is_some() => Some(Settle::SliderDraft),
            _ => Some(Settle::Preview),
        };
        if proxy.is_some()
            && self.presentation.intent(generation) != Some(PreviewIntent::Interactive)
        {
            // The photograph is on screen, but every number a captured frame reports — the
            // histogram, the clipping counters, the overlay it is checked against — comes from the
            // exact render. So the step and the open request wait for this generation's exact phase.
            self.presentation.held_by_proxy = Some(HeldByProxy {
                generation,
                settle,
                ready: self.activity.pending,
            });
        } else {
            self.presentation.held_by_proxy = None;
            if let Some(settle) = settle {
                self.settle_step(settle);
            }
            if self.activity.pending {
                self.activity.pending = false;
                self.activity.displayed = self.activity.requested;
                self.activity.phase = "ready";
                self.event(
                    "render_ready",
                    || json!({"displayed_generation":self.activity.displayed}),
                );
                self.outcome_ready(false);
            }
        }
        self.status.text = self.displayed_status(&entry);
    }

    /// Take up the report and the raster the preview worker produced for `generation`, now that its
    /// pixels are on screen, and hand the report to the owner's store so an API client's
    /// `analysis.request` for the same identity is a cache hit instead of a second render.
    pub(super) fn adopt_analysis(&mut self, generation: u64) {
        if !self.presentation.adopt(generation) {
            return;
        }
        let Some(analysis) = &self.presentation.analysis else {
            return;
        };
        self.event(
            "analysis_adopted",
            || json!({"generation":generation,"entry_id":analysis.identity.entry_id.as_str(),"draft_revision":analysis.identity.draft.as_ref().map(|draft| draft.draft_revision),"width":analysis.identity.width,"height":analysis.identity.height,"any_shadow":analysis.report.any_shadow,"any_highlight":analysis.report.any_highlight,"both":analysis.report.both}),
        );
        self.owner
            .submit_analysis(analysis.identity.clone(), analysis.report.clone());
        if self.presentation.analysis_content == Some(self.presentation.content_serial) {
            self.view_plan.quiet_since = None;
        }
    }

    /// Point the canvas at another entry. A readout describes one pixel of one stack, so moving to
    /// another entry drops it and anything waiting to be sampled rather than leaving codes on screen
    /// that belong to an image no longer shown.
    pub(super) fn show_entry(&mut self, entry: luxforge_core::EntryId) {
        if self.document.display_entry.as_ref() != Some(&entry) {
            self.hover.readout = None;
            self.hover.sample.drop_pending();
        }
        self.document.display_entry = Some(entry);
    }

    /// What the status bar says about the frame that just reached the screen: what last happened
    /// to the photograph when it is the current state, and which entry is shown during a historical
    /// preview, by the sequence number and label the history rows carry, so the status bar and the
    /// state panel agree about which entry is on screen. It names no identity, snapshot or source
    /// hash; those stay with the API and the evidence state.
    pub(super) fn displayed_status(&self, entry: &EntryId) -> String {
        if !self.session.preview.can_edit() {
            if self.document.compare_return.is_some() {
                return state::status::COMPARING.to_owned();
            }
            return state::status::previewing(
                self.document
                    .history
                    .entries
                    .iter()
                    .find(|row| row.id == *entry)
                    .map(|row| (row.sequence, row.label.as_str())),
            );
        }
        // A frame of an open mask gesture says what the gesture is and how it becomes history,
        // rather than repeating the entry it was drawn on.
        if let Some(line) = self.mask_gesture_status() {
            return line;
        }
        let sentence = match (&self.status.happened, &self.document.state) {
            (Some(happened), _) => happened.sentence(),
            (None, Some(state)) => {
                state::status::showing(state.current_entry.sequence, &state.current_entry.label)
            }
            (None, None) => String::new(),
        };
        match &self.status.skipped {
            Some(skipped) => format!("{sentence} \u{b7} {skipped}"),
            None => sentence,
        }
    }

    /// The crop draft is displayed instead of the plain preview only while its own input stage is on
    /// the presenter and the session shows the current state.
    pub(crate) fn drafting(&self) -> bool {
        self.crop().is_some()
            && self.presentation.presenter.stage().is_some()
            && self.session.preview.can_edit()
    }
}

/// After every message: a changed zoom is answered in one place ([`Editor::zoom_changed`]), the
/// proxy on screen is refitted to new bounds, and the desired view is admitted once nothing owns
/// the pending slot.
pub(super) fn after_message(editor: &mut Editor, before: &Before) -> Task<Message> {
    let zoomed = editor.zoom_changed(&before.zoom);
    let refit = editor.refit_proxy();
    let view = editor.reconcile_view();
    Task::batch([zoomed, refit, view])
}

/// The workers' wake and the quiet settle's timer. A blocked channel stream costs no idle work: it
/// stays installed while a photograph is open, because the surface may defer an upload in
/// `prepare`, after this update's subscription set was computed, and its retirement wake must have
/// a listener then. The 25 ms timer runs only while the quiet policy waits to settle.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let mut subscriptions = Vec::new();
    if editor.preview_wake_needed() {
        subscriptions.push(waker::subscription());
    }
    if editor.view_plan.quiet_since.is_some() && !editor.view_plan.quiet_settle_requested {
        subscriptions.push(
            iced::time::every(Duration::from_millis(25))
                .map(|_| Message::Preview(PreviewMessage::QuietTick)),
        );
    }
    Subscription::batch(subscriptions)
}
