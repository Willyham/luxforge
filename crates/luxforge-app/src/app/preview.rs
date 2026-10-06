//! Preview presentation: requesting the reference renderer's frames at the bounds the view calls
//! for, taking up what the preview worker finishes, and presenting the displayed frame — the exact
//! frame, its reduction to the view, the histogram analysis and the crop draft's input stage — in
//! the order their generations allow.
//!
//! Every job of the photograph is the reference's: its whole frame and report, reduced to the
//! view where the view draws the stage smaller than it is, or a retained exact frame reduced again
//! with no render. The GPU draws everything else: a committed stack at rest
//! ([`super::gpu_counts`]) and a gesture's ticks ([`super::gpu_preview`], [`super::motion`]).
//!
//! [`Presentation`] owns all of it: the one [`Presenter`] every frame goes to, the preview queue,
//! and the bookkeeping that decides which frame is on screen — generations, content serials, the
//! retained frames, the bounds each job was given and the entry and draft revision the frame on
//! screen was rendered for. The editor's methods here decide what the view wants and report what
//! reached the surface ([`Outcome::Presented`]); `Presentation` records what is shown.
use super::{
    Editor,
    gesture::Starting,
    message::{Message, preview::PreviewMessage},
    outcome::{self, Outcome},
    overlay::OverlayRequest,
    presenter::Presenter,
    tasks::{self, recipe_task},
};
use crate::app::{Before, waker};
use crate::{
    layout, state,
    state::histogram::{Analysis, AnalysisSource},
    view,
};
use iced::{Subscription, Task};
use luxforge_core::{
    DraftId, EntryId, ExactOutcome, PhaseOutcome, PreviewIntent, PreviewJob, PreviewQueue,
    PreviewResult, ProxyBounds, Raster, Region, Zoom,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Instant};

/// The exact frame of one generation reduced to its view's bounds: the reference frame of a whole
/// stack where the view draws it smaller than it is, retained beside the exact raster.
///
/// It is what a zoom back to Fit hands the surface again instead of rendering. Retaining it copies
/// no pixels: it shares the reduction's own `Arc<Vec<u8>>` with the surface.
#[derive(Clone)]
pub(crate) struct ReducedFrame {
    pub(crate) generation: u64,
    pub(crate) raster: Arc<luxforge_core::Raster>,
    /// The reduction is of a frame that approximates a drafted RAW white balance.
    pub(crate) approximate_white_balance: bool,
    /// The exact phase's own worker time, so a zoom that hands this frame back to the surface
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

/// A frame the desktop holds for the generation on screen. Both reach the surface through the one
/// presenting function, [`Editor::present`], whether a render just produced them or a zoom hands
/// them back.
#[derive(Clone)]
pub(crate) enum Retained {
    /// The exact frame reduced to the view's bounds: the reference frame of a whole stack where
    /// the view draws it smaller than it is.
    Reduced(ReducedFrame),
    Exact(ExactFrame),
}

impl Retained {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Reduced(frame) => frame.generation,
            Self::Exact(frame) => frame.generation,
        }
    }

    pub(crate) fn raster(&self) -> &Arc<Raster> {
        match self {
            Self::Reduced(frame) => &frame.raster,
            Self::Exact(frame) => &frame.raster,
        }
    }

    pub(crate) fn approximate_white_balance(&self) -> bool {
        match self {
            Self::Reduced(frame) => frame.approximate_white_balance,
            Self::Exact(frame) => frame.approximate_white_balance,
        }
    }

    pub(crate) fn render_ms(&self) -> f64 {
        match self {
            Self::Reduced(frame) => frame.render_ms,
            Self::Exact(frame) => frame.render_ms,
        }
    }

    fn reduced(&self) -> bool {
        matches!(self, Self::Reduced(_))
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

/// What one preview result says about the frame it belongs to.
pub(crate) struct Delivery {
    pub(crate) generation: u64,
    /// The exact stage the job was planned for, from its identity.
    pub(crate) stage: (u32, u32),
    pub(crate) entry_id: EntryId,
    pub(crate) draft_revision: Option<u64>,
}

/// One preview result for the photograph, taken up ([`Presentation::take`]).
pub(crate) enum Presented {
    /// Older than the frame on screen, or not a frame of the photograph: it presents nothing.
    Stale,
    /// The exact phase's frame reduced to the view's bounds, its exact frame already received.
    Reduced(Box<(Delivery, ReducedFrame)>),
    /// The exact phase — its frame, already received as the retained exact raster or with its
    /// report, or its failure.
    Exact(Box<(Delivery, Result<ExactFrame, luxforge_core::Error>)>),
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
    /// The slider's immutable After frame, sharing the existing render allocation, and the display
    /// reduction that stands in for it at Fit when it cannot be drawn below its size cleanly.
    pub(crate) compare_after: Option<super::compare_after::CompareAfter>,
    /// One active and one replaceable pending preview job, off the UI thread.
    pub(crate) queue: PreviewQueue,
    /// The generation of the newest preview requested for the photograph.
    pub(crate) preview_generation: u64,
    /// The generation of the newest job queued, of the photograph or the crop draft's input stage.
    pub(crate) requested: u64,
    /// The generation whose pixels are on screen.
    ///
    /// The delivery rule is the queue's own, monotone rather than newest-only: a delivered result
    /// is presented when it is not older than this. Under a sustained drag a render almost always
    /// finishes after a newer job was requested, so rejecting everything but the newest generation
    /// presents no frames at all. What makes work in flight stale is [`Self::cancel`], which an
    /// asset or selection change calls, and so does a discarded mask gesture whose drafted frames
    /// must not reach the screen; nothing else has to.
    pub(crate) presented_generation: u64,
    /// One opaque surface identity per photograph content. A pan/zoom or unbound mask edit
    /// retains it; bound edits, layer changes and source development get another id.
    content_key: Option<(u64, luxforge_core::ProxyIdentity)>,
    pub(crate) content_serial: u64,
    /// A draft or commit whose unchanged pixels were reused instead of rendered, waiting to be
    /// settled once the draft driver has drained ([`Editor::settle_reused_pixels`]).
    pub(crate) reused: Option<luxforge_core::analysis::AnalysisIdentity>,
    pub(crate) pending_content: BTreeMap<u64, u64>,
    pub(crate) presented_content: u64,
    pub(crate) analysis_content: Option<u64>,
    /// The exact stage of the frame on screen, whatever size its texture is: a reduction is drawn
    /// into this box, and every pick, percent-zoom box and overlay cell maps to exact stage pixels.
    pub(crate) dimensions: Option<(u32, u32)>,
    /// The exact frame of the newest whole stack, reduced to its view's bounds.
    pub(crate) reduced_frame: Option<ReducedFrame>,
    /// The newest whole stack's job, whose retained exact frame a Fit resize reduces again with no
    /// render where the reference renderer draws the picture at rest.
    reduction_job: Option<PreviewJob>,
    /// Whether the frame on screen is the reduced one.
    pub(crate) presented_reduced: bool,
    /// The exact frame of the newest job whose exact phase landed.
    pub(crate) exact: Option<ExactFrame>,
    /// The displayed frame's histogram report, adopted with the pixels under the same generation:
    /// the reference frame's, or the GPU's counts of the stack it presents
    /// ([`super::gpu_counts`]).
    pub(crate) analysis: Option<Analysis>,
    /// The counts of the frame on screen in motion, a gesture's GPU tick: shown in place of the
    /// report, marked updating, and never handed to the owner.
    pub(crate) motion: Option<Analysis>,
    /// The content the GPU presents on its own, with no CPU frame of it rendered
    /// ([`Editor::present_on_gpu`]): the photograph's frame on the presenter is an earlier one,
    /// which the GPU's picture is drawn over.
    pub(crate) gpu_presented: Option<u64>,
    /// The photograph whose frame the presenter holds.
    pub(crate) presented_asset: Option<luxforge_core::AssetId>,
    /// The report and frame of an exact phase whose pixels have not reached the surface yet. The
    /// histogram and the photograph are adopted together, so the plot never describes a frame that
    /// is not on screen.
    pub(crate) incoming: Option<(Analysis, ExactFrame)>,
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
    /// A refit of the picture to new bounds has been asked for and has not been presented yet.
    pub(crate) refit_pending: bool,
    /// Why the last preview failed, cleared by the next presented frame. The canvas turns this
    /// into the notice that names the cause; nothing here decides what it means.
    pub(crate) render_error: Option<luxforge_core::Error>,
    /// The entry whose pixels the photo surface holds: the entry the presented generation was
    /// rendered for. The editor's displayed entry moves to a newly requested entry as soon as its
    /// job is asked for; this moves only when that entry's frame is on screen, and is cleared when
    /// a failure withdraws the frame.
    pub(crate) presented_entry: Option<EntryId>,
    /// The draft revision the displayed preview was rendered from, for correlation.
    pub(crate) displayed_draft_revision: Option<u64>,
    /// Revisions are ordered only within this draft; a new draft starts at zero.
    pub(crate) displayed_draft_id: Option<DraftId>,
}

/// The one desired view the owner admits through the shared gate: a view change marks it dirty,
/// one plan is in flight at a time, and a view-only request's generation is remembered so only it
/// may be replaced by the next. A view at 100% and above is planned at once, the GPU's region of
/// it cut from the source it holds, or the reference's whole frame where the GPU cannot draw it.
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
    /// The core draft a release settled, whose drafted frames are no longer taken up.
    pub(crate) released_draft: Option<DraftId>,
}

impl Presentation {
    pub(super) fn matches_pixel_content(&self, job: &PreviewJob) -> bool {
        job.evaluation.pixel_content_key().ok().is_some_and(|key| {
            self.content_key.as_ref() == Some(&(key, job.evaluation.source().identity()))
        })
    }
    /// Key a job's content before it is queued: the same evaluated image keeps its serial across
    /// pans and zooms, and anything else gets the next one.
    pub(crate) fn admit(&mut self, job: &PreviewJob) -> u64 {
        let key = job
            .evaluation
            .pixel_content_key()
            .ok()
            .map(|pixels| (pixels, job.evaluation.source().identity()));
        if key.is_none() || self.content_key != key {
            self.content_serial = self.content_serial.saturating_add(1);
            self.content_key = key;
        }
        self.content_serial
    }

    /// Queue one admitted job of `content` and record what its frame will need when it lands: the
    /// bounds it was given, and its content and intent. The job still waiting in the pending slot
    /// is replaced and forgotten.
    pub(crate) fn request(&mut self, job: PreviewJob, content: u64, timed: bool) -> Requested {
        let bounds = job.proxy;
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
        self.requested = generation;
        self.pending_bounds.insert(generation, bounds);
        self.pending_content.insert(generation, content);
        if let Some(replaced) = replaced {
            self.forget(replaced);
        }
        Requested {
            generation,
            replaced,
            at,
        }
    }

    /// Stop every job in flight and forget what they were asked for. Returns the generation below
    /// which nothing is delivered any more.
    pub(crate) fn cancel(&mut self) -> u64 {
        let generation = self.queue.cancel();
        self.reused = None;
        self.reduction_job = None;
        self.pending_bounds.clear();
        self.pending_content.clear();
        generation
    }

    /// Retain the whole-image plan whose exact pixels a later Fit resize reduces again where the
    /// reference renderer draws the picture at rest (`reference`). Crop input stages do not
    /// replace that plan; every new whole stack does, and where the GPU draws the picture at rest
    /// none is kept, since a resize plans that picture again. A reused recipe still needs the new
    /// entry and draft identity.
    pub(crate) fn remember_reduction_job(&mut self, job: &PreviewJob, reference: bool) {
        if job.layer_count.is_none() && job.intent != PreviewIntent::Reduce {
            self.reduction_job = reference.then(|| job.clone());
        }
    }

    /// Forget what a job whose last phase has been taken up was asked for.
    pub(crate) fn forget(&mut self, generation: u64) {
        self.pending_bounds.remove(&generation);
        self.pending_content.remove(&generation);
    }

    /// Take up one result for the photograph.
    ///
    /// The delivery rule, the same monotone one the queue itself applies: present whatever is not
    /// older than what is on screen. Rejecting everything but the newest generation presents no
    /// frames at all under a sustained drag the reference draws, because a render almost always
    /// finishes after a newer tick has been asked for.
    ///
    /// The exact frame is received here: with its report it waits in `incoming` to be adopted with
    /// the pixels, and without one it replaces the retained exact raster now, so no overlay is
    /// derived from an older image. Its reduction to the view, when it has one, is retained for a
    /// zoom back to Fit and is the frame presented.
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
            outcome,
            approximate_white_balance,
            render_ms,
            ..
        } = result;
        let delivery = Delivery {
            generation,
            stage: (identity.width, identity.height),
            entry_id,
            draft_revision,
        };
        // The photograph's jobs ask for no proxy phase and no region: every one is its exact
        // phase alone.
        let PhaseOutcome::Exact(outcome) = outcome else {
            return Presented::Stale;
        };
        let ExactOutcome {
            result,
            report,
            display,
            ..
        } = *outcome;
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
                source: AnalysisSource::Reference,
            });
            if intent == PreviewIntent::Reduce {
                if let Some(exact) = &mut self.exact {
                    exact.generation = generation;
                }
                if let Some(analysis) = &mut self.analysis {
                    analysis.generation = generation;
                }
            } else {
                self.receive(frame.clone(), analysis);
            }
        }
        match display {
            Some(raster) => {
                let reduced = ReducedFrame {
                    generation,
                    raster: Arc::new(raster),
                    approximate_white_balance,
                    render_ms,
                };
                self.reduced_frame = Some(reduced.clone());
                Presented::Reduced(Box::new((delivery, reduced)))
            }
            None => Presented::Exact(Box::new((delivery, frame))),
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
        // A frame with a report is exact: an approximate one is never reduced into one.
        frame.approximate_white_balance = false;
        self.exact = Some(frame);
        self.analysis = Some(analysis);
        self.motion = None;
        self.analysis_content = self.pending_content.get(&generation).copied();
        true
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
        let content = self
            .pending_content
            .get(&generation)
            .copied()
            .unwrap_or(self.presented_content);
        // A new version, so the primitive writes the frame exactly once however often the same
        // raster is drawn. Nothing but a new frame moves it.
        self.gpu_presented = None;
        if frame.reduced() {
            self.presenter.show_reduced(frame.raster(), content);
        } else {
            self.presenter.show_full(frame.raster(), content);
        }
        self.dimensions = Some(stage);
        self.presented_generation = generation;
        self.presented_content = content;
        self.presented_reduced = frame.reduced();
        self.presented_approximate_white_balance = frame.approximate_white_balance();
        if let Some(bounds) = self.pending_bounds.get(&generation).copied() {
            self.presented_bounds = bounds;
        }
        self.pending_bounds
            .retain(|pending, _| *pending >= generation);
        self.refit_pending = false;
        self.presented_entry = Some(entry.clone());
        self.displayed_draft_revision = draft_revision;
        // A frame on screen is the proof the last failure is over.
        self.render_error = None;
        content
    }

    /// What a zoom that wants the exact frame reduced to the view — or, with `wants_reduced`
    /// false, the exact frame itself — finds for the frame on screen.
    pub(crate) fn zoom(&self, wants_reduced: bool) -> Zoomed {
        if self.presented_generation == 0 || wants_reduced == self.presented_reduced {
            return Zoomed::Kept;
        }
        if self.presenter.photo().is_none() && self.render_error.is_some() {
            return Zoomed::Withdrawn;
        }
        let held = if wants_reduced {
            self.reduced().cloned().map(Retained::Reduced)
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
        self.reduced_frame = None;
        self.reduction_job = None;
        self.exact = None;
        self.incoming = None;
        self.analysis = None;
        self.motion = None;
        self.analysis_content = None;
        self.gpu_presented = None;
        self.presented_asset = None;
        self.presented_reduced = false;
        self.presented_approximate_white_balance = false;
        self.displayed_draft_revision = None;
        self.displayed_draft_id = None;
        self.presented_entry.take()
    }

    /// Every frame the canvas draws, for the view. An overlay is drawn only while it belongs to the
    /// frame on screen; the clipping overlay only while it is the one `clipping` asked for. The
    /// mask draft and the crop draft are the caller's to add.
    pub(crate) fn surfaces(&self, clipping: Option<&OverlayRequest>) -> view::Surfaces<'_> {
        view::Surfaces {
            comparison: None,
            photo: self.presenter.photo_for(self.presented_content),
            photo_content: self.presenter.full_content(),
            current_content: self.presented_content,
            region_coverage: self.presenter.region_coverage(self.presented_generation),
            stage: self.presenter.stage(),
            clipping: self.clipping(clipping),
            coverage: self.coverage(),
            mask_draft: None,
            mask_map: None,
            draft: None,
            gpu: None,
            gpu_hold: false,
            gpu_tag: None,
            gpu_change: None,
            dissolve: None,
            gpu_warm: None,
            gpu_source: None,
            gpu_rest: None,
            stage_rest: None,
            gpu_counts: None,
            compare_gpu: None,
            compare_change: None,
            compare_rest: None,
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
        self.presenter.clipping(request.generation)
    }

    /// The whole-frame mask coverage to draw over the photograph: the one on the presenter, when
    /// it belongs to the frame that is on screen. A region's is the surface's to place
    /// ([`super::presenter::Presenter::region_coverage`]).
    pub(crate) fn coverage(&self) -> Option<&luxforge_ui::Frame> {
        self.presenter.coverage(self.presented_generation)
    }

    /// The reduced frame retained for the generation on screen, when there is one.
    pub(crate) fn reduced(&self) -> Option<&ReducedFrame> {
        self.reduced_frame
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

    /// A photograph is on the surface.
    pub(crate) fn has_picture(&self) -> bool {
        self.presenter.photo().is_some()
    }

    /// A newer frame has been requested than the one the histogram describes, so the plotted counts
    /// are one generation behind and the plot says so rather than going blank.
    ///
    /// The comparison is against the content of the newest **requested** preview, not against
    /// whether a worker happens to be busy: a crop draft's own truncated job shares the queue and is
    /// never analysed, so queue business alone would mark a perfectly current histogram stale.
    pub(crate) fn analysis_updating(&self) -> bool {
        if self.motion.is_some() {
            return true;
        }
        match &self.analysis {
            Some(_) => self.analysis_content != Some(self.content_serial),
            None => false,
        }
    }

    /// The report the inspector plots: the counts of the frame on screen in motion while a
    /// gesture's GPU ticks give them, otherwise the displayed frame's report.
    pub(crate) fn shown_analysis(&self) -> Option<&Analysis> {
        self.motion.as_ref().or(self.analysis.as_ref())
    }

    /// Record the GPU's picture of `content`, rendered for `entry` at its exact stage `stage`, as
    /// on screen under `generation`, with no CPU frame of it ([`Editor::present_on_gpu`]): the
    /// presenter's frame, an earlier one of the same photograph, stays the surface's base, retagged
    /// with the content, and the GPU's picture is drawn over it. Nothing CPU-rendered of another
    /// content — an exact frame or its reduction — is kept as this content's.
    pub(crate) fn show_gpu(
        &mut self,
        generation: u64,
        content: u64,
        stage: (u32, u32),
        entry: &EntryId,
        bounds: Option<ProxyBounds>,
    ) {
        self.presenter.retag(content);
        self.reduced_frame = None;
        self.exact = None;
        self.incoming = None;
        self.dimensions = Some(stage);
        self.presented_generation = generation;
        self.preview_generation = generation;
        self.presented_content = content;
        self.presented_reduced = false;
        self.presented_approximate_white_balance = false;
        self.presented_bounds = bounds;
        self.refit_pending = false;
        self.presented_entry = Some(entry.clone());
        self.displayed_draft_revision = None;
        self.displayed_draft_id = None;
        self.render_error = None;
        self.gpu_presented = Some(content);
    }
}

/// The region of the output stage the GPU draws at 100% and above, which a mask's region coverage
/// is computed for and laid over: its rectangle, the stage it is a region of, and the content and
/// generation on screen it belongs to ([`Editor::gpu_view_region`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewRegion {
    pub(crate) rect: Region,
    pub(crate) stage: (u32, u32),
    pub(crate) content: u64,
    pub(crate) generation: u64,
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
///
/// The offset is the scrollable's, which it holds between zero and how far the zoomed photograph
/// overhangs the surface on each axis, as the canvas places the photograph ([`crate::view::canvas`]'s
/// `drawn_photo`). A `pan` past that — an offset kept from a deeper zoom, which the scrollable never
/// reports again once the photograph fits the surface — is held to it here too.
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
        let start = start.clamp(0.0, (limit as f32 * scale - length).max(0.0));
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
            &luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE),
            self.presentation.has_picture(),
            self.presentation.render_error.is_some(),
        ) || self.surfaces().comparison.is_some_and(|_| {
            surface_photo_needs_update(
                &luxforge_ui::surface_diagnostics(crate::view::canvas::COMPARE_SURFACE),
                true,
                false,
            )
        })
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
        // A gesture's GPU frame of a region holding the view is at full detail, once the surface
        // has evaluated it.
        if self.gpu_draws_view(wanted) {
            return false;
        }
        // The GPU's picture at rest of the content on screen holds the view at full detail, once
        // the surface has evaluated it.
        if self.gpu_holds_view(wanted)
            && self.gpu_rest_plan().is_some_and(|(plan, _)| {
                luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE)
                    .gpu_ready_boundary
                    == Some(plan.boundary.version())
            })
        {
            return false;
        }
        // The reference's whole frame of the content on screen holds every view.
        !(self.presentation.presenter.full_content() == Some(self.presentation.presented_content)
            && self.presentation.exact_content() == Some(self.presentation.presented_content))
    }
    /// Settle a draft or commit whose photograph pixels were reused instead of rendered, once the
    /// shared draft driver has drained: the entry, draft and analysis identity advance together.
    pub(crate) fn settle_reused_pixels(&mut self) {
        let Some(identity) = self.presentation.reused.clone() else {
            return;
        };
        if self
            .core_gesture()
            .is_some_and(|gesture| !gesture.draft.drained())
        {
            return;
        }
        self.presentation.reused = None;
        self.show_entry(identity.entry_id.clone());
        self.presentation.presented_entry = Some(identity.entry_id.clone());
        self.presentation.displayed_draft_id = identity.draft.as_ref().map(|d| d.draft_id.clone());
        self.presentation.displayed_draft_revision =
            identity.draft.as_ref().map(|d| d.draft_revision);
        // A mask-only or neutral ancillary edit reuses pixel allocations, while its immutable
        // recipe has a new snapshot. Restamp the small raster headers so reduce-only validation
        // can verify that identity without copying the shared RGBA buffers.
        if let Some(frame) = self.presentation.exact.as_mut() {
            Arc::make_mut(&mut frame.raster).snapshot_id = identity.snapshot_id.clone();
        }
        if let Some(frame) = self.presentation.reduced_frame.as_mut() {
            Arc::make_mut(&mut frame.raster).snapshot_id = identity.snapshot_id.clone();
        }
        let generation = self.presentation.presented_generation;
        if self.presentation.analysis_content == Some(self.presentation.presented_content)
            && let Some(analysis) = self.presentation.analysis.as_mut()
        {
            analysis.identity = identity.clone();
            let report = analysis.report.clone();
            self.owner.submit_analysis(identity.clone(), report);
            self.event(
                "analysis_reused",
                || json!({"generation":generation,"identity":identity}),
            );
        }
        self.event(
            "preview_pixels_reused",
            || json!({"generation":generation,"identity":identity}),
        );
        self.outcome(Outcome::EntryShown(&identity.entry_id));
        let presented = match self.core_gesture() {
            Some(gesture) => outcome::Presented::Draft {
                slider: gesture.slider().is_some(),
                newest: true,
            },
            None => outcome::Presented::Photo,
        };
        self.outcome(Outcome::Presented(presented));
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.outcome(Outcome::RequestEnded { failed: false });
        }
        self.status.text = self.displayed_status(&identity.entry_id);
    }

    pub(super) fn cancel_preview_queue(&mut self) -> u64 {
        self.invalidate_mask_coverage();
        let generation = self.presentation.cancel();
        self.view_plan.request_generation = None;
        self.view_plan.epoch = self.view_plan.epoch.saturating_add(1);
        self.view_plan.dirty = true;
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
            PreviewMessage::MaskCoverageSource { epoch, result } => {
                self.mask_coverage_source_planned(epoch, result);
            }
            PreviewMessage::ViewLoaded { epoch, result } => {
                self.view_plan.in_flight = false;
                if epoch != self.view_plan.epoch {
                    self.view_plan.dirty = true;
                    return Task::none();
                }
                match result {
                    Ok(job) => {
                        let job = *job;
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
                        self.view_plan.dirty = false;
                        // A paused draft's view is its next tick, over the region the pan moved
                        // to: drawn on the GPU, or held or rendered as any tick the GPU does not
                        // draw.
                        if job.evaluation.draft_revision().is_some() {
                            self.event("preview_view_requested", || json!({"draft": true}));
                            self.drag_view_planned(job);
                            return Task::none();
                        }
                        let generation = self.request_preview(job);
                        self.presentation.preview_generation = generation;
                        self.view_plan.request_generation = Some(generation);
                        self.event(
                            "preview_view_requested",
                            || json!({"generation":generation,"draft":false}),
                        );
                    }
                    Err(error) => {
                        self.status.text = error;
                        self.view_plan.dirty = false;
                    }
                }
            }
            PreviewMessage::GridReady(answer) => self.gpu_grid_ready(*answer),
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
                        self.outcome(Outcome::EntryRequested(payload.job.evaluation.entry()));
                        self.show_entry(entry.clone());
                        // Said before the request, which a picture the GPU presents at once
                        // answers with what is on screen.
                        self.status.text = "Rendering selected history state…".into();
                        self.presentation.preview_generation = self.request_preview(payload.job);
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
                while let Some(done) = self.coverage_worker.queue.poll() {
                    self.mask_coverage_ready(done);
                }
                let delivered = self.deliver_previews();
                return Task::batch([delivered, self.poll_again()]);
            }
        }
        Task::none()
    }

    pub(super) fn note_view_motion(&mut self) {
        self.view_plan.dirty = true;
        self.view_plan.epoch = self.view_plan.epoch.saturating_add(1);
    }

    /// Plan the view at 100% and above: the displayed stack's job with its GPU picture over the
    /// visible region, or a paused draft's next tick over it. Nothing while a gesture's draft is
    /// still draining: its next tick plans the view itself.
    fn view_plan(&mut self) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        let draft = match self.core_gesture() {
            Some(gesture) if gesture.draft.drained() => Some(gesture.draft.draft_id.clone()),
            Some(_) => return Task::none(),
            None => None,
        };
        self.view_plan.in_flight = true;
        let gpu = match self.gpu_preview_allowed() {
            Ok(()) => self.gpu_ask(),
            Err(_) => super::gpu_preview::GpuAsk::Off,
        };
        tasks::view_preview_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            self.displayed_entry(),
            draft,
            self.view_plan.epoch,
            gpu,
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
            return Task::none();
        };
        // A gesture's GPU frame of a region holding the view, or the GPU's picture at rest of the
        // content asked for, holds the view: nothing to plan.
        if self.gpu_draws_view(wanted) || self.gpu_holds_view(wanted) {
            self.view_plan.dirty = false;
            return Task::none();
        }
        // The reference's whole frame of the state asked for, on screen or on its way, holds every
        // view. A gesture's ticks the GPU draws queue no job, so its state is the draft's newest
        // revision, not the content last queued.
        let whole = match self
            .session
            .draft
            .as_ref()
            .filter(|_| self.core_gesture().is_some())
        {
            Some(draft) => {
                (self.presentation.displayed_draft_id.as_ref() == Some(&draft.draft_id)
                    && self.presentation.displayed_draft_revision == Some(draft.draft_revision)
                    && self.presentation.presenter.full_content()
                        == Some(self.presentation.presented_content))
                    || self.motion_in_flight()
            }
            None => {
                let content = self.presentation.content_serial;
                (self.presentation.presenter.full_content() == Some(content)
                    && self.presentation.exact.as_ref().is_some_and(|frame| {
                        (frame.raster.width, frame.raster.height) == stage
                            && frame.content == Some(content)
                    }))
                    || self
                        .presentation
                        .pending_content
                        .values()
                        .any(|pending| *pending == content)
            }
        };
        if whole {
            self.view_plan.dirty = false;
            return Task::none();
        }
        // Planned at once: where the GPU presents the content its region is cut from the source
        // it holds, and where it cannot the reference renders the whole frame.
        self.view_plan()
    }

    /// The region the GPU draws the photograph from at 100% and above: the committed stack's view
    /// plan at rest, or the open gesture's plan where the surface draws it in place of the frame.
    /// `None` below 100%, and wherever the frame on screen is the reference's, whose whole frame
    /// holds every view. The overlays' region grids are keyed on it.
    pub(crate) fn gpu_view_region(&self) -> Option<ViewRegion> {
        if !matches!(self.session.preview.view.zoom, Zoom::Percent { value } if value >= 100.0) {
            return None;
        }
        let plan = match self.gpu_rest_plan() {
            Some((plan, _)) => plan,
            None => self
                .gesture_gpu_plan()
                .filter(|_| !self.gpu_held())
                .map(|(plan, _)| plan)?,
        };
        // The softer drag frame is a reduced whole frame placed over the full stage: its coverage is
        // the whole stage's grid, as below 100%.
        let region = plan
            .region
            .filter(|region| region.stage == region.full_stage)?;
        let [x0, y0, x1, y1] = region.rect;
        Some(ViewRegion {
            rect: Region {
                x0,
                y0,
                width: x1.saturating_sub(x0),
                height: y1.saturating_sub(y0),
            },
            stage: region.stage,
            content: self.presentation.presented_content,
            generation: self.presentation.presented_generation,
        })
    }

    /// Whether the GPU presents the content asked for and its view plan at rest, a region's at
    /// 100% and above, holds `wanted`.
    fn gpu_holds_view(&self, wanted: Region) -> bool {
        self.presentation.gpu_presented == Some(self.presentation.content_serial)
            && self.gpu_rest_plan().is_some_and(|(plan, _)| {
                plan.region.is_some_and(|region| {
                    let [x0, y0, x1, y1] = region.rect;
                    contains_region(
                        Region {
                            x0,
                            y0,
                            width: x1 - x0,
                            height: y1 - y0,
                        },
                        wanted,
                    )
                })
            })
    }

    /// The physical pixels the photo area can show a frame in, when the view means a display-size
    /// frame is what should be presented — or `None` when only the exact render will do.
    ///
    /// At Fit that is the photo surface less the canvas padding, scaled by the display factor:
    /// exactly the rectangle [`state::histogram::displayed_size`] fits an image into, so the exact
    /// frame is reduced to the size the display was going to minify it down to anyway, and the
    /// GPU's picture planned at it. It does not depend on the photograph, so the first job of an
    /// open already has it.
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
                // Strictly smaller in both axes, so a reduction is never asked for a frame that
                // would have to be magnified back up to show the detail the zoom asked for.
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
    /// or cancelled outcome, a failure — and at most one that hands a frame to the display, after
    /// which the rest wait for the next `Poll`, so every presented frame is drawn by the redraw its
    /// own update requests. The photograph and the crop draft's input stage alike become the
    /// surface's source in the update that takes them up. Every job delivers one result, its last.
    pub(super) fn deliver_previews(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while let Some(result) = self.poll_preview() {
            let generation = result.generation;
            let (task, presented) = self.preview_ready(result);
            self.presentation.forget(generation);
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
            || self.coverage_worker.queue.ready()
        {
            Task::done(Message::Preview(PreviewMessage::Poll))
        } else {
            Task::none()
        }
    }

    /// Take up one preview result that carries something to show. Returns what it asks the runtime
    /// for, and whether it handed a frame to the display — the photograph, or the crop draft's
    /// input stage.
    pub(super) fn preview_ready(&mut self, mut result: PreviewResult) -> (Task<Message>, bool) {
        // An exact display reduction belongs to one view and one current content generation; the
        // crop draft's input stage's, to the stage's own bounds alone, its generation the draft's.
        // Its full raster can still be retained when a resize invalidates only the reduction.
        let stage = Some(result.generation) == self.draft_generation();
        let pending_bounds = self
            .presentation
            .pending_bounds
            .get(&result.generation)
            .copied()
            .flatten();
        let stale = if stage {
            pending_bounds != self.crop_stage_bounds()
        } else {
            result.generation != self.presentation.preview_generation
                || self.presentation.pending_content.get(&result.generation)
                    != Some(&self.presentation.content_serial)
                || pending_bounds != self.proxy_bounds()
        };
        if let PhaseOutcome::Exact(exact) = &mut result.outcome
            && exact.display.is_some()
            && stale
        {
            exact.display = None;
            if !stage {
                self.presentation.refit_pending = false;
            }
        }
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
            let phase = "exact";
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
            Presented::Reduced(reduced) => {
                let (delivery, frame) = *reduced;
                self.frame_ready(delivery, Ok(Retained::Reduced(frame)))
            }
            Presented::Exact(exact) => {
                let (delivery, frame) = *exact;
                self.frame_ready(delivery, frame.map(Retained::Exact))
            }
        }
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
                self.preview_failed(
                    delivery.generation,
                    &delivery.entry_id,
                    delivery.draft_revision,
                    &error,
                );
                return (Task::none(), false);
            }
        };
        let reduced = frame.reduced();
        // The dimensions every pick, every percent-zoom box and every overlay cell maps through
        // are the **exact stage's**, whatever size the texture is; the identity already carries
        // them.
        let stage = if reduced {
            delivery.stage
        } else {
            (frame.raster().width, frame.raster().height)
        };
        if self.activity.pending {
            self.activity.preview_dimensions = Some(stage);
            self.event(
                "decoded",
                || json!({"open_to_raster_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":[stage.0,stage.1],"reduced":reduced}),
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
        // Compare waited for a frame of the content the GPU presented, its After side.
        if self.gpu.compare_waits
            && self.presentation.gpu_presented != Some(self.presentation.presented_content)
        {
            self.gpu.compare_waits = false;
            if self.presentation.compare_after.is_none() {
                return (self.compare_toggle(), true);
            }
        }
        (Task::none(), true)
    }

    /// The crop layer's input stage, shown in place of the photograph from the render's own buffer
    /// with the open frame drawn over it in this same update: nothing is uploaded through the
    /// runtime, so nothing waits for it. Its proxy is the Fit view and its exact phase the
    /// percentage zoom's; neither is ever reduced, sampled or committed, retained as the
    /// photograph's or held to the photograph's delivery rule.
    fn stage_ready(&mut self, result: PreviewResult) -> (Task<Message>, bool) {
        // The reference's one frame of the stage: reduced to the bounds the job offered, or its
        // exact stage where the job offered none or the stage already fits them.
        let bounded = self.crop_stage_bounds().is_some();
        let (frame, proxy, bounded) = match result.outcome {
            PhaseOutcome::Exact(outcome) => {
                let ExactOutcome {
                    result, display, ..
                } = *outcome;
                match display {
                    Some(reduced) => (Ok(reduced), true, true),
                    None => (result, false, bounded),
                }
            }
            // A stage job asks for no viewport and no proxy phase.
            _ => return (Task::none(), false),
        };
        match frame {
            Ok(raster) => {
                let (presented, planned) = self.crop_stage_ready(&raster, proxy, bounded, true);
                (planned, presented)
            }
            Err(error) => {
                self.draft_preview_failed(&error);
                (Task::none(), false)
            }
        }
    }

    /// A preview of the displayed target failed: say so on the canvas, and never leave another
    /// entry's picture on screen as though it were this one.
    ///
    /// The frame on screen stays only when it is the target that failed — a frame of the same
    /// entry and draft revision — because then it still shows that state. Any other frame belongs to an earlier entry or draft revision: after
    /// a commit whose render failed it is the picture from before the edit, while history and the
    /// recipe already name the edit, so it is withdrawn with everything derived from it and the
    /// canvas shows the failure in its place. The edit itself is untouched; the next frame that
    /// renders puts a picture back.
    pub(super) fn preview_failed(
        &mut self,
        generation: u64,
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
            || json!({"generation":generation,"entry_id":entry,"draft_revision":draft_revision,"error_code":error.kind.code(),"detail":error.detail}),
        );
        let shows_target = self.presentation.presented_entry.as_ref() == Some(entry)
            && self.presentation.displayed_draft_revision == draft_revision
            && self.presentation.displayed_draft_id
                == self
                    .session
                    .draft
                    .as_ref()
                    .map(|draft| draft.draft_id.clone());
        if !shows_target && self.presentation.presenter.photo().is_some() {
            self.withdraw_photo(generation, entry, error);
        }
        if !self.presentation.has_picture() && self.mask_coverage_target().is_some() {
            self.invalidate_mask_coverage();
            self.mask_overlay_unavailable(generation, &error.detail);
        }
        self.outcome(Outcome::PreviewFailed {
            newest: generation >= self.presentation.preview_generation,
        });
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.phase = "error";
            self.activity.error_code = Some(error.kind.code().into());
            self.event("render_failed", || json!({"error_code":error.kind.code()}));
            self.outcome(Outcome::RequestEnded { failed: true });
        }
    }

    /// Take the picture of an earlier entry or draft revision off the surface, with everything
    /// that describes it — the retained rasters, the histogram, the overlay's source and the render
    /// time — so nothing on screen claims to show a state it does not.
    pub(super) fn withdraw_photo(
        &mut self,
        generation: u64,
        target: &luxforge_core::EntryId,
        error: &luxforge_core::Error,
    ) {
        let shown = self.presentation.withdraw();
        self.outcome(Outcome::Withdrawn);
        self.event("preview_withdrawn", || {
            json!({
                "generation": generation,
                "presented_generation": self.presentation.presented_generation,
                "target_entry": target,
                "withdrawn_entry": shown,
                "error_code": error.kind.code(),
            })
        });
        self.activity.render = None;
    }

    /// The crop layer's input stage could not be rendered, so the draft it was for cannot open or
    /// rebase.
    pub(crate) fn draft_preview_failed(&mut self, error: &luxforge_core::Error) {
        if self.crop_stage() == Some(crate::app::crop::StageView::Shown) {
            // The stage on screen stays under the frame; only its other phase failed, and says so.
            self.set_draft_generation(None);
            self.status.text = format!("The crop's input stage could not be rendered: {error}");
            self.outcome(Outcome::CropStage);
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
    /// and stays. The reason reaches the status bar and the log, and the wait for the stage ends.
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
        self.outcome(Outcome::CropStage);
    }

    /// The zoom changed. This is the **one** place a view change can ask for a reference frame,
    /// and it only does so when the pixels it needs do not exist yet.
    ///
    /// Which texture the view wants is decided by [`Self::proxy_bounds`]: the exact frame reduced
    /// to the view when the frame is drawn smaller than the exact stage, the exact frame itself at
    /// 100% and above. While that answer is unchanged there is nothing to do at all — a zoom from
    /// Fit to 50% keeps the reduction it already has — so the rule "a view change re-renders
    /// nothing" survives every step but the one crossing between the two.
    ///
    /// Crossing to the exact frame makes the retained exact raster of the frame on screen the
    /// surface's source. When its job is still outstanding there is nothing to hand over and
    /// nothing to ask for: it is presented when it arrives, so the view waits with the ordinary
    /// loading state.
    ///
    /// Crossing back hands the retained reduction of the frame on screen over again, and with none
    /// reduces the retained exact frame again with no render, or, where the GPU draws the picture
    /// at rest, plans that picture for the view.
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
        let wants_reduced = self.proxy_bounds().is_some();
        match self.presentation.zoom(wants_reduced) {
            // Nothing presented yet, or the texture on screen is already the one this zoom wants:
            // a step from Fit to 50% keeps the reduction it has, and the rule that a view change
            // re-renders nothing survives every zoom but the one crossing between the two.
            // Or a failure withdrew the picture: nothing retained may be handed over in its place,
            // and a view change asks for no render. The next frame of the target puts a picture
            // back.
            Zoomed::Kept | Zoomed::Withdrawn => Task::none(),
            Zoomed::Missing if wants_reduced => {
                // Nothing to hand over: the frame on screen is a full-resolution render with no
                // display-size frame beside it. The retained exact allocation is reduced again
                // where the reference renderer draws the picture; where the GPU does, a job plans
                // its picture at the bounds this zoom wants.
                self.event("preview_proxy_requested", || json!({ "zoom": zoom }));
                self.outcome(Outcome::FrameRequested(outcome::Requested::Photo));
                if let Some(bounds) = self.proxy_bounds()
                    && self.reduce_for_reference(bounds)
                {
                    Task::none()
                } else {
                    self.request_current_preview()
                }
            }
            Zoomed::Missing => {
                // The exact frame on screen has not landed. Its job is already running, and the
                // `Poll` handler presents it when it arrives because the zoom now needs it.
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

    /// One job of the photograph, or of the crop draft's input stage, with the bounds of this
    /// moment: the reference's whole frame and report, reduced to the view where the view draws the
    /// stage smaller than it is; or nothing queued at all where the frame on screen serves it or
    /// the GPU presents it ([`Editor::gpu_presents`]).
    pub(super) fn request_preview_inner(
        &mut self,
        mut job: luxforge_core::PreviewJob,
        timed: bool,
    ) -> (u64, Option<Instant>) {
        // The bounds the owner planned the job's GPU picture at rest at, which the display scale's
        // arrival or a resize since can have left behind the bounds its frame is drawn at now.
        let planned_at = job.proxy;
        job.proxy = if job.layer_count.is_some() {
            // A crop draft's input stage takes the photograph's rule over its own stage: the
            // reference's frame of the layer prefix reduced to the bounds wherever the view draws
            // the stage smaller than it is, where the GPU does not draw it, and the exact stage
            // only for a view that needs it ([`Self::present_crop_stage`]). No report is reduced
            // from the stage.
            self.crop_stage_bounds()
        } else {
            self.proxy_bounds()
        };
        let content = self.presentation.admit(&job);
        self.request_mask_coverage(&job, content);
        self.gpu_warm_from(job.gpu_warm.as_deref());
        // Every boundary is derived from the job's own source on the GPU: the surface is handed it
        // before any plan over it.
        self.gpu_hold_source(job.evaluation.source());
        let committed = job.layer_count.is_none() && job.evaluation.draft_revision().is_none();
        // At 100% and above the GPU's picture at rest is a region's, planned for the visible
        // region once the photograph's stage is known.
        let region_view = self
            .desired_view_for((job.identity.width, job.identity.height))
            .is_some();
        // The displayed stack's picture at rest in tiles: held for the surfaces to draw, or let go
        // where its view draws the stack at its own size or larger or the GPU cannot draw them.
        if committed && let Some(rest) = job.gpu_rest.as_mut() {
            let tiles = rest.tiles.take().and_then(Result::ok);
            self.gpu_rest_from(tiles);
            self.gpu.rest_planned_at = planned_at.filter(|_| !region_view);
        }
        self.gpu_resident_from(job.gpu_rest.take(), region_view, committed);
        // A job that asks for a report needs one of this content.
        let complete_analysis = !job.analyse
            || (self.presentation.analysis_content == Some(content)
                && self.presentation.analysis.is_some());
        // An exact frame of this stack on screen at Fit, where the job's bounds draw the stage
        // smaller than it is — an open whose reduction the display scale's arrival made stale, or
        // a return from a percentage view: its retained raster is reduced to the bounds on the
        // preview worker, with no render, for the reference frame behind the GPU's picture at
        // rest planned for them above.
        let reduces_exact = self
            .presentation
            .exact
            .as_ref()
            .filter(|frame| {
                job.layer_count.is_none()
                    && content == self.presentation.presented_content
                    && !self.presentation.presented_reduced
                    && job.proxy.is_some_and(|bounds| {
                        job.identity.width > bounds.width || job.identity.height > bounds.height
                    })
                    && frame.content == Some(content)
                    && !frame.approximate_white_balance
                    && frame.raster.snapshot_id == job.identity.snapshot_id
                    && frame.raster.source_fingerprint == job.identity.source_fingerprint
                    && (frame.raster.width, frame.raster.height)
                        == (job.identity.width, job.identity.height)
            })
            .map(|frame| Arc::clone(&frame.raster));
        if let Some(raster) = reduces_exact.clone() {
            job.reduce = Some(raster);
            job.intent = PreviewIntent::Reduce;
            job.analyse = false;
        }
        // Photograph bytes can be reused for an unbound candidate or a mask-only commit. Bound
        // mask edits have a different pixel key and still use the ordinary rendering path.
        let reusable = reduces_exact.is_none()
            && job.layer_count.is_none()
            && content == self.presentation.presented_content
            && self.presentation.has_picture()
            && self.presentation.render_error.is_none()
            && !self.presentation.presented_approximate_white_balance
            && !self.presentation.queue.is_busy()
            && !self.presentation.queue.ready()
            && complete_analysis
            // The GPU presented this content with no CPU frame and the reference is asked for one.
            && !(self.presentation.gpu_presented == Some(content)
                && self.gpu.refused_content == Some(content))
            && match job.proxy {
                // At 100% and above the exact frame of this content serves every view.
                None => self.presentation.presenter.full_content() == Some(content),
                Some(_) => {
                    !self.presentation.presented_reduced
                        || self.presentation.presented_bounds == job.proxy
                }
            };
        self.note_thumbnail_source(&job);
        let reference = !self.gpu_at_rest();
        if reusable {
            // The frame on screen serves this job's bounds — an exact frame whose stage they fit,
            // or a reduction made for them — and the GPU's picture at rest was planned for them
            // above, so a refit it answers has landed.
            if !region_view {
                self.presentation.presented_bounds = job.proxy;
                self.presentation.refit_pending = false;
            }
            self.presentation.remember_reduction_job(&job, reference);
            self.presentation.reused = Some(job.identity.clone());
            self.retarget_gpu_counts(&job.identity, content);
            self.presentation.preview_generation = self.presentation.presented_generation;
            self.view_plan.dirty = false;
            return (
                self.presentation.presented_generation,
                timed.then(Instant::now),
            );
        }
        self.presentation.reused = None;
        // A committed whole stack the GPU draws at rest is presented by the GPU alone: no job is
        // queued, so no exact frame is rendered or reduced, and the counts of its tiles are its
        // report (`docs/design/gpu-first.md`, stage 2). The reference renders the rest.
        if self.gpu_presents(&job, content) {
            let generation = self.present_on_gpu(&job, content);
            self.view_plan.dirty = false;
            return (generation, timed.then(Instant::now));
        }
        // The job still waiting in the pending slot is replaced by this one and never starts, so
        // nothing about it will ever be delivered: when it was the crop draft's input stage, the
        // draft it was for ends here, as a cancelled one does in `poll_preview`. The request names
        // it in the same step, because the worker takes a pending job by itself the moment the
        // active one ends: a job it took up meanwhile is running, and ends through `poll_preview`.
        let layer_count = job.layer_count;
        self.presentation.remember_reduction_job(&job, reference);
        let Requested {
            generation,
            replaced,
            at,
        } = self.presentation.request(job, content, timed);
        self.event(
            "preview_job_requested",
            || json!({"generation":generation,"layer_count":layer_count}),
        );
        if replaced.is_some() && replaced == self.view_plan.request_generation {
            self.view_plan.request_generation = None;
            self.view_plan.dirty = true;
        }
        if replaced.is_some() && replaced == self.draft_generation() {
            self.draft_preview_superseded(replaced);
        }
        // The whole frame holds every view at 100% and above.
        if layer_count.is_none() {
            self.view_plan.dirty = false;
        }
        (generation, at)
    }

    /// Reduce the reference frame on screen again once when the bounds it was made for no longer
    /// match the window: a resize, a panel toggle or the display scale arriving. Where the GPU
    /// draws the picture at rest, its plans were made for the bounds of the job that planned them,
    /// so they are refitted whatever frame stands behind them, an exact one included. The queue
    /// coalesces a storm of these into one active and one pending job, and nothing is asked for
    /// while a gesture or a crop draft owns the preview, or while a refit is already on its way.
    pub(super) fn refit_view(&mut self) -> Task<Message> {
        if self.document.state.is_none()
            || self.refit_deferred()
            || self.presentation.presented_generation == 0
            || !(self.presentation.presented_reduced || self.gpu_at_rest())
            || self.presentation.refit_pending
        {
            return Task::none();
        }
        let Some(bounds) = self.proxy_bounds() else {
            return Task::none();
        };
        // The GPU's picture at rest is planned for the bounds its job was planned at, which the
        // frame's own bounds, replaced when the job was requested, do not show.
        let gpu_stale = self.gpu_at_rest()
            && self
                .gpu
                .rest_planned_at
                .is_some_and(|planned| planned != bounds);
        if self.presentation.presented_bounds == Some(bounds) && !gpu_stale {
            return Task::none();
        }
        self.presentation.refit_pending = true;
        self.event(
            "preview_proxy_requested",
            || json!({"reason":"bounds","bounds":{"width":bounds.width,"height":bounds.height}}),
        );
        self.outcome(Outcome::FrameRequested(outcome::Requested::Photo));
        if self.reduce_for_reference(bounds) {
            Task::none()
        } else {
            self.request_current_preview()
        }
    }

    /// Reduce the retained exact frame to `bounds` where the reference renderer draws the picture
    /// at rest. Where the GPU draws it its plans are made for the view a job is planned at, so
    /// this is `false` and the caller asks for a job that plans them for this one.
    fn reduce_for_reference(&mut self, bounds: ProxyBounds) -> bool {
        !self.gpu_at_rest() && self.reduce_retained(bounds)
    }

    /// Schedule only reduction when this exact frame still describes the displayed content.
    fn reduce_retained(&mut self, bounds: ProxyBounds) -> bool {
        let Some(exact) = self.presentation.exact.as_ref().filter(|frame| {
            !frame.approximate_white_balance
                && frame.content == Some(self.presentation.presented_content)
                && self.presentation.presented_content == self.presentation.content_serial
        }) else {
            return false;
        };
        let Some(mut job) = self.presentation.reduction_job.clone() else {
            return false;
        };
        if Some(&job.evaluation.entry().id) != self.presentation.presented_entry.as_ref()
            || job.evaluation.draft_revision() != self.presentation.displayed_draft_revision
        {
            return false;
        }
        job.reduce = Some(exact.raster.clone());
        job.proxy = Some(bounds);
        job.intent = PreviewIntent::Reduce;
        job.analyse = false;
        let content = self.presentation.presented_content;
        let requested = self.presentation.request(job, content, false);
        self.presentation.preview_generation = requested.generation;
        self.event(
            "preview_reduce_requested",
            || json!({"generation":requested.generation,"bounds":bounds}),
        );
        true
    }

    /// Drafts own the preview until they finish, so a layout change deliberately leaves their
    /// displayed frame at its previous bounds instead of starting a competing refit. A refit takes
    /// the one-draft rule alone ([`Starting::Refit`]): it still runs while a request is in flight
    /// or a history entry is previewed.
    pub(super) fn refit_deferred(&self) -> bool {
        self.gesture_refusal(Starting::Refit).is_some()
    }

    /// Evidence of a displayed reduction, or of the GPU's picture at rest, waits for the current
    /// layout when a refit is permitted. The frame of an open can arm a capture while its
    /// display-scale refit is on its way. Drafts deliberately defer such refits, and can supersede
    /// a queued one; their settled frame can be captured as shown even if that abandoned request
    /// left `refit_pending` set.
    pub(super) fn capture_refit_ready(&self) -> bool {
        if !(self.presentation.presented_reduced || self.gpu_at_rest())
            || self.presentation.render_error.is_some()
            || self.refit_deferred()
        {
            return true;
        }
        if self.presentation.refit_pending {
            return false;
        }
        match self.proxy_bounds() {
            Some(bounds) => self.presentation.presented_bounds == Some(bounds),
            // At 100% the exact frame is the target; the step's normal preview settlement
            // already waits for it, without requiring a reduction that cannot be requested.
            None => true,
        }
    }

    pub(super) fn request_current_preview(&mut self) -> Task<Message> {
        let Some(state) = &self.document.state else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        tasks::current_preview_task(self.owner.clone(), self.client, asset, entry, self.drawn())
    }

    /// Where a displayed entry's frame is drawn now ([`tasks::Drawn`]): the display bounds, and
    /// the view its GPU picture at rest is planned at — the whole frame at those bounds at Fit and
    /// below 100%, the visible region at 100% and above.
    pub(crate) fn drawn(&self) -> tasks::Drawn {
        tasks::Drawn {
            proxy: self.proxy_bounds(),
            gpu: self.gpu_ask(),
        }
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
    /// which the desktop already holds as the reduced frame or the exact raster of this generation.
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
        self.presentation
            .show(&frame, stage, &entry, draft_revision);
        if !zoom {
            self.presentation.presented_asset = self
                .document
                .state
                .as_ref()
                .map(|state| state.asset.id.clone());
        }
        self.outcome(Outcome::EntryShown(&entry));
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
            approximate: frame.approximate_white_balance(),
        });
        let raster = frame.raster();
        let picture = self.displayed_picture();
        self.event("preview_displayed", || {
            json!({
                "entry_id":entry,
                "snapshot_id":raster.snapshot_id.to_string(),
                // The status bar no longer shows the source hash, so the log is where a frame is
                // correlated with it.
                "source_fingerprint":raster.source_fingerprint,
                "generation":generation,
                "draft_revision":draft_revision,
                "dimensions":[stage.0,stage.1],
                "path":"surface",
                "reduced":frame.reduced(),
                "approximate_white_balance":frame.approximate_white_balance(),
                "reason":zoom.then_some("zoom"),
                "render_ms":frame.render_ms(),
                // Whose picture of this content is on screen: the GPU's at rest, this frame
                // behind it, or this reference frame itself.
                "picture":picture,
            })
        });
        // These pixels are presented whether or not this frame also belongs to the one open
        // request tracked below. While a slider gesture is open the drafted frames replace one
        // another, so only the one whose settings are the newest is the draft's newest: not while
        // another `draft.set` or the commit is still queued, a newer frame was asked for, or a
        // newer tick waits for its own ([`super::motion`]). A brush back in hand after its stroke
        // committed holds no draft and asks for no frame of its own, so the committed frame is the
        // photograph.
        let waiting = self.drag_frame_waiting();
        let presented = match self.core_gesture() {
            Some(gesture) => outcome::Presented::Draft {
                slider: gesture.slider().is_some(),
                newest: !gesture.draft.frame_pending()
                    && !waiting
                    && generation >= self.presentation.preview_generation,
            },
            None => outcome::Presented::Photo,
        };
        self.outcome(Outcome::Presented(presented));
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.event(
                "render_ready",
                || json!({"displayed_generation":self.activity.displayed}),
            );
            self.outcome(Outcome::RequestEnded { failed: false });
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
    }

    /// Point the canvas at another entry.
    pub(super) fn show_entry(&mut self, entry: luxforge_core::EntryId) {
        self.document.display_entry = Some(entry);
    }

    /// What the status bar says about the frame that just reached the screen: what last happened
    /// to the photograph when it is the current state, and which entry is shown during a historical
    /// preview, by the sequence number and label the history rows carry, so the status bar and the
    /// state panel agree about which entry is on screen. It names no identity, snapshot or source
    /// hash; those stay with the API and the evidence state.
    pub(super) fn displayed_status(&self, entry: &EntryId) -> String {
        if !self.at_current() {
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
        self.crop().is_some() && self.presentation.presenter.stage().is_some() && self.at_current()
    }
}

/// After every message: a changed zoom is answered in one place ([`Editor::zoom_changed`]), the
/// picture on screen is refitted to new bounds, and the desired view is admitted once nothing owns
/// the pending slot.
pub(super) fn after_message(editor: &mut Editor, before: &Before) -> Task<Message> {
    let zoomed = editor.zoom_changed(&before.zoom);
    let refit = editor.refit_view();
    let view = editor.reconcile_view();
    editor.settle_reused_pixels();
    Task::batch([zoomed, refit, view])
}

/// The workers' wake and the deadlines the desktop waits out. A blocked channel stream costs no
/// idle work: it stays installed while a photograph is open, because the surface may defer an
/// upload in `prepare`, after this update's subscription set was computed, and its retirement wake
/// must have a listener then. A held tick is looked at every 50 ms while it holds, at most half a
/// second ([`super::motion`]).
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let mut subscriptions = Vec::new();
    if editor.preview_wake_needed() {
        subscriptions.push(waker::subscription());
    }
    // A stack the GPU presented waiting on its compile is looked at again at the `compiling`
    // threshold, once, unless the compile thread's wake comes first and ends the wait.
    if editor.gpu_compile_deadline() {
        subscriptions.push(
            iced::time::every(crate::state::status::COMPILING_AFTER)
                .map(|_| Message::Preview(PreviewMessage::Poll)),
        );
    }
    if editor.motion_hold_pending() {
        subscriptions.push(
            iced::time::every(super::motion::HOLD_LOOK)
                .map(|_| Message::Preview(PreviewMessage::Poll)),
        );
    }
    Subscription::batch(subscriptions)
}

#[cfg(test)]
mod reduction_tests {
    use super::*;
    use crate::app::testing::{self, finish, opened};
    use luxforge_core::{
        Evaluation, Layer, ModuleRegistry, PreviewSource, RenderContext, SourceImage,
    };

    fn job(editor: &Editor) -> PreviewJob {
        let entry = editor
            .document
            .state
            .as_ref()
            .unwrap()
            .current_entry
            .clone();
        let recipe = entry.snapshot.recipe.clone();
        PreviewJob::new(Evaluation::new(
            Arc::new(ModuleRegistry::builtin()),
            RenderContext::new(),
            PreviewSource::Jpeg(SourceImage {
                width: 2,
                height: 1,
                rgba: vec![20, 40, 60, 255, 40, 60, 80, 255].into(),
                fingerprint: "reduction-test".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            entry,
            recipe,
            None,
        ))
        .unwrap()
    }

    #[test]
    fn the_reduction_plan_is_the_newest_whole_stacks_and_cancel_releases_it() {
        let (mut editor, catalog, _, _) = opened(
            vec![Layer::new(
                luxforge_core::DETAIL_EFFECT,
                json!({"sharpening":60}),
            )],
            4,
        );
        let mut active = job(&editor);
        let (stack, held) = testing::fresh_stack(&active.evaluation);
        active.evaluation = stack;
        editor.presentation.remember_reduction_job(&active, true);
        drop(active);
        assert!(held.upgrade().is_some());
        let mut plain = job(&editor);
        testing::rebuild(&mut plain, |parts| parts.recipe.layers.clear());
        editor.presentation.remember_reduction_job(&plain, true);
        assert!(
            held.upgrade().is_none(),
            "a new whole stack releases the preceding source"
        );
        assert!(
            editor.presentation.reduction_job.is_some(),
            "a stack without restoration is retained as well"
        );
        let mut active = job(&editor);
        let (stack, held) = testing::fresh_stack(&active.evaluation);
        active.evaluation = stack;
        editor.presentation.remember_reduction_job(&active, true);
        drop(active);
        assert!(held.upgrade().is_some());
        editor.presentation.cancel();
        assert!(
            held.upgrade().is_none(),
            "cancellation releases its retained reduction plan"
        );
        // Where the GPU draws the picture at rest none is kept: a resize plans that picture again.
        let mut active = job(&editor);
        let (stack, held) = testing::fresh_stack(&active.evaluation);
        active.evaluation = stack;
        editor.presentation.remember_reduction_job(&active, false);
        drop(active);
        assert!(
            held.upgrade().is_none() && editor.presentation.reduction_job.is_none(),
            "no plan is retained behind the GPU's picture at rest"
        );
        finish(editor, catalog);
    }

    #[test]
    fn reused_pixels_resize_with_new_snapshot_without_copying_pixels() {
        let (mut editor, catalog, _, _) = opened(
            vec![Layer::new(
                luxforge_core::DETAIL_EFFECT,
                json!({"sharpening":60}),
            )],
            4,
        );
        // `opened` queues its fixture's preview. This test installs a completed presentation
        // directly, so retire that fixture job before exercising the idle reuse path.
        editor.presentation.queue = luxforge_core::PreviewQueue::default();
        // The reference renderer draws the picture at rest, so a resize reduces its exact frame.
        editor.renderer.stage = Some(luxforge_ui::photo_surface::GpuStageState::DeviceLost);
        let original = job(&editor);
        let content = editor.presentation.admit(&original);
        editor.presentation.pending_content.insert(8, content);
        let pixels = Arc::new(vec![20, 40, 60, 255, 40, 60, 80, 255]);
        let raster = Arc::new(Raster {
            width: 2,
            height: 1,
            rgba: pixels.clone(),
            source_fingerprint: original.identity.source_fingerprint.clone(),
            snapshot_id: original.identity.snapshot_id.clone(),
        });
        let mut frame = testing::exact(8, raster, 1.0);
        frame.content = Some(content);
        editor.present(
            Retained::Exact(frame.clone()),
            Arrival::Rendered {
                stage: (2, 1),
                entry: original.identity.entry_id.clone(),
                draft_revision: None,
            },
        );
        editor.presentation.exact = Some(frame);
        editor.presentation.remember_reduction_job(&original, true);
        let mut changed = original.clone();
        testing::rebuild(&mut changed, |parts| {
            parts.entry.id = luxforge_core::EntryId::new();
            parts.entry.snapshot.id = luxforge_core::SnapshotId::new();
        });
        assert!(editor.presentation.matches_pixel_content(&changed));
        assert_eq!(editor.presentation.presented_content, content);
        assert!(editor.presentation.has_picture());
        assert!(!editor.presentation.queue.is_busy());
        assert!(!editor.presentation.queue.ready());
        assert!(editor.presentation.render_error.is_none());
        assert_eq!(
            editor.request_preview(changed.clone()),
            8,
            "same pixel content does not rerender Detail"
        );
        editor.settle_reused_pixels();
        let exact = editor.presentation.exact.as_ref().unwrap();
        assert_eq!(exact.raster.snapshot_id, changed.identity.snapshot_id);
        assert!(Arc::ptr_eq(&exact.raster.rgba, &pixels));
        assert!(
            editor.reduce_retained(ProxyBounds {
                width: 1,
                height: 1
            }),
            "resize uses the refreshed immutable plan"
        );
        let result = luxforge_testbase::wait_for("the resize's exact-derived Fit", || {
            editor.presentation.queue.poll()
        });
        let luxforge_core::PhaseOutcome::Exact(outcome) = result.outcome else {
            panic!("reduction has only an exact phase");
        };
        assert!(outcome.result.is_ok(), "{outcome:?}");
        assert!(Arc::ptr_eq(&outcome.result.unwrap().rgba, &pixels));
        assert_eq!(outcome.display.unwrap().width, 1);
        finish(editor, catalog);
    }

    /// Where the GPU draws the picture at rest its plans were made for the bounds of the job that
    /// planned them, so an exact frame behind it at Fit is refitted when the bounds change, as a
    /// proxy is — the display scale arriving after an open whose frame was planned without it —
    /// and the job that answers reuses the exact pixels on screen and ends the refit. Where the
    /// reference renderer draws the picture, the exact frame serves any bounds and asks for none.
    #[test]
    fn an_exact_frame_behind_the_gpus_picture_at_rest_is_refitted() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.presentation.queue = luxforge_core::PreviewQueue::default();
        editor.session.preview.view.zoom = Zoom::Fit;
        let original = job(&editor);
        let content = editor.presentation.admit(&original);
        editor.presentation.pending_content.insert(8, content);
        let raster = Arc::new(Raster {
            width: 2,
            height: 1,
            rgba: Arc::new(vec![20, 40, 60, 255, 40, 60, 80, 255]),
            source_fingerprint: original.identity.source_fingerprint.clone(),
            snapshot_id: original.identity.snapshot_id.clone(),
        });
        let mut frame = testing::exact(8, raster, 1.0);
        frame.content = Some(content);
        editor.present(
            Retained::Exact(frame.clone()),
            Arrival::Rendered {
                stage: (2, 1),
                entry: original.identity.entry_id.clone(),
                draft_revision: None,
            },
        );
        editor.presentation.exact = Some(frame);
        editor.presentation.presented_bounds = None;
        assert!(!editor.presentation.presented_reduced && editor.gpu_at_rest());
        let bounds = editor.proxy_bounds().expect("Fit's bounds");

        let _ = editor.refit_view();
        assert!(
            editor.presentation.refit_pending,
            "the GPU's picture at rest is planned again for the window's bounds"
        );
        assert_eq!(
            editor.request_preview(original.clone()),
            8,
            "the exact pixels on screen answer the refit"
        );
        assert!(!editor.presentation.refit_pending);
        assert_eq!(editor.presentation.presented_bounds, Some(bounds));

        // A frame at the window's bounds whose picture at rest its job planned at others — an
        // open planned before the display scale arrived — is refitted too, once.
        editor.gpu.rest_planned_at = Some(ProxyBounds {
            width: bounds.width / 2,
            height: bounds.height / 2,
        });
        let _ = editor.refit_view();
        assert!(
            editor.presentation.refit_pending,
            "the picture at rest is planned again for the window's bounds"
        );
        editor.presentation.refit_pending = false;
        editor.gpu.rest_planned_at = Some(bounds);
        let _ = editor.refit_view();
        assert!(
            !editor.presentation.refit_pending,
            "planned for these bounds"
        );

        editor.presentation.presented_bounds = None;
        editor.renderer.stage = Some(luxforge_ui::photo_surface::GpuStageState::DeviceLost);
        let _ = editor.refit_view();
        assert!(
            !editor.presentation.refit_pending,
            "the reference renderer's exact frame serves any bounds"
        );
        finish(editor, catalog);
    }

    /// Where the GPU draws the picture at rest, a job of the stack whose exact frame is on screen
    /// at Fit, at bounds that draw the stage smaller than it is, renders nothing: its retained
    /// raster is reduced to them on the preview worker, sharing the allocation, and the reduction
    /// becomes the reference frame behind the GPU's picture, made for those bounds.
    #[test]
    fn a_job_over_an_exact_frame_at_fit_reduces_its_retained_raster() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.presentation.queue = luxforge_core::PreviewQueue::default();
        let completed_generation = editor.presentation.queue.cancel();
        editor.view_state.window = (800.0, 600.0);
        editor.view_state.scale_factor = 1.0;
        editor.session.preview.view.zoom = Zoom::Fit;
        let mut original = job(&editor);
        testing::rebuild(&mut original, |parts| {
            parts.source = PreviewSource::Jpeg(SourceImage {
                width: 640,
                height: 480,
                rgba: vec![40; 640 * 480 * 4].into(),
                fingerprint: "exact-at-fit-test".into(),
                orientation: 1,
                capture: Default::default(),
            });
        });
        let content = editor.presentation.admit(&original);
        editor
            .presentation
            .pending_content
            .insert(completed_generation, content);
        let pixels = Arc::new(vec![40; 640 * 480 * 4]);
        let raster = Arc::new(Raster {
            width: 640,
            height: 480,
            rgba: pixels.clone(),
            source_fingerprint: original.identity.source_fingerprint.clone(),
            snapshot_id: original.identity.snapshot_id.clone(),
        });
        let mut frame = testing::exact(completed_generation, raster, 1.0);
        frame.content = Some(content);
        editor.present(
            Retained::Exact(frame.clone()),
            Arrival::Rendered {
                stage: (640, 480),
                entry: original.identity.entry_id.clone(),
                draft_revision: None,
            },
        );
        editor.presentation.exact = Some(frame);
        assert!(!editor.presentation.presented_reduced && editor.gpu_at_rest());
        let bounds = editor
            .proxy_bounds()
            .filter(|bounds| bounds.width < 640 || bounds.height < 480)
            .expect("Fit draws this stage smaller than it is");

        let generation = editor.request_preview(original.clone());
        editor.presentation.preview_generation = generation;
        assert_ne!(generation, completed_generation, "a job was queued");
        let result =
            luxforge_testbase::wait_for("the reduction", || editor.presentation.queue.poll());
        assert_eq!(result.generation, generation);
        assert_eq!(
            result.intent,
            PreviewIntent::Reduce,
            "the exact pixels on screen are reduced rather than rendered again"
        );
        assert!(Arc::ptr_eq(
            &result.exact().unwrap().result.as_ref().unwrap().rgba,
            &pixels
        ));
        let (_, shown) = editor.preview_ready(result);
        assert!(shown && editor.presentation.presented_reduced);
        assert_eq!(editor.presentation.presented_bounds, Some(bounds));
        assert!(!editor.presentation.refit_pending);
        finish(editor, catalog);
    }

    /// Returning from a percentage view after a reused commit must schedule reduction before the
    /// ordinary same-content shortcut can accept its exact pixels as the requested Fit display.
    #[test]
    fn a_reused_commit_zoom_to_fit_reduces_retained_exact_pixels() {
        let (mut editor, catalog, _, _) = opened(
            vec![Layer::new(
                luxforge_core::DETAIL_EFFECT,
                json!({"sharpening":60}),
            )],
            4,
        );
        editor.presentation.queue = luxforge_core::PreviewQueue::default();
        let completed_generation = editor.presentation.queue.cancel();
        // The reference renderer draws the picture at rest: with the GPU stage lost, a resize or
        // a zoom to Fit reduces the retained exact frame rather than planning a GPU picture.
        editor.renderer.stage = Some(luxforge_ui::photo_surface::GpuStageState::DeviceLost);
        editor.view_state.window = (800.0, 600.0);
        editor.view_state.scale_factor = 1.0;
        editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
        let mut original = job(&editor);
        testing::rebuild(&mut original, |parts| {
            parts.source = PreviewSource::Jpeg(SourceImage {
                width: 640,
                height: 480,
                rgba: vec![40; 640 * 480 * 4].into(),
                fingerprint: "reduction-zoom-test".into(),
                orientation: 1,
                capture: Default::default(),
            });
        });
        let content = editor.presentation.admit(&original);
        editor
            .presentation
            .pending_content
            .insert(completed_generation, content);
        let pixels = Arc::new(vec![40; 640 * 480 * 4]);
        let raster = Arc::new(Raster {
            width: 640,
            height: 480,
            rgba: pixels.clone(),
            source_fingerprint: original.identity.source_fingerprint.clone(),
            snapshot_id: original.identity.snapshot_id.clone(),
        });
        let mut frame = testing::exact(completed_generation, raster, 1.0);
        frame.content = Some(content);
        editor.present(
            Retained::Exact(frame.clone()),
            Arrival::Rendered {
                stage: (640, 480),
                entry: original.identity.entry_id.clone(),
                draft_revision: None,
            },
        );
        editor.presentation.exact = Some(frame);
        editor.presentation.analysis_content = Some(content);
        editor.presentation.analysis = Some(Analysis {
            generation: completed_generation,
            identity: original.identity.clone(),
            report: luxforge_core::analysis::reduce(
                &pixels,
                640,
                480,
                &luxforge_core::Cancel::never(),
            )
            .unwrap(),
            source: AnalysisSource::Reference,
        });
        editor.presentation.remember_reduction_job(&original, true);

        let mut committed = original.clone();
        testing::rebuild(&mut committed, |parts| {
            parts.entry.id = luxforge_core::EntryId::new();
            parts.entry.snapshot.id = luxforge_core::SnapshotId::new();
        });
        assert_eq!(
            editor.request_preview(committed.clone()),
            completed_generation,
            "the commit reuses identical pixels"
        );
        editor.settle_reused_pixels();
        assert_eq!(
            editor.presentation.presented_entry.as_ref(),
            Some(&committed.identity.entry_id)
        );
        assert_eq!(
            editor
                .presentation
                .exact
                .as_ref()
                .unwrap()
                .raster
                .snapshot_id,
            committed.identity.snapshot_id
        );
        assert!(editor.presentation.reduced_frame.is_none());
        let report = editor
            .presentation
            .analysis
            .as_ref()
            .unwrap()
            .report
            .clone();

        editor.session.preview.view.zoom = Zoom::Fit;
        let bounds = editor
            .proxy_bounds()
            .expect("the Fit display is smaller than this exact image");
        let task = editor.zoom_changed(&Zoom::Percent { value: 200.0 });
        assert_eq!(
            task.units(),
            0,
            "Fit reduction needs no owner planning round trip"
        );
        let generation = editor.presentation.preview_generation;
        let result = luxforge_testbase::wait_for("the zoom's exact-derived Fit", || {
            editor.presentation.queue.poll()
        });
        assert_eq!(result.generation, generation);
        assert_eq!(
            result.intent,
            PreviewIntent::Reduce,
            "returning to Fit must bypass the same-content request shortcut"
        );
        assert_eq!(result.identity, committed.identity);
        assert!(Arc::ptr_eq(
            &result.exact().unwrap().result.as_ref().unwrap().rgba,
            &pixels
        ));
        let (_, shown) = editor.preview_ready(result);
        assert!(shown && editor.presentation.presented_reduced);
        assert_eq!(editor.presentation.presented_bounds, Some(bounds));
        assert_eq!(editor.presentation.presenter.full_content(), None);
        assert!(Arc::ptr_eq(
            &editor.presentation.exact.as_ref().unwrap().raster.rgba,
            &pixels
        ));
        let analysis = editor.presentation.analysis.as_ref().unwrap();
        assert_eq!(analysis.identity, committed.identity);
        assert_eq!(
            analysis.report, report,
            "Fit reduction does not replace exact histogram counts"
        );
        assert!(!editor.presentation.analysis_updating());
        finish(editor, catalog);
    }
}
