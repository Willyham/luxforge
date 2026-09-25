//! Preview presentation: requesting preview jobs at the bounds the view calls for, taking up what
//! the preview worker finishes, and presenting the displayed frame — the display-size proxy, the
//! exact frame behind it, the histogram analysis and the crop draft's input stage — in the order
//! their generations allow.
use super::{
    Editor,
    evidence::Settle,
    gesture::Starting,
    message::{Message, PreviewMessage},
    tasks::{self, Upload, recipe_task},
};
use crate::{draft_photo, state, state::histogram::Analysis, view};
use iced::Task;
use iced_runtime::image as image_memory;
use lightwell_core::{CropStage, PhaseOutcome, PreviewPhase, ProxyBounds, Zoom};
use serde_json::json;
use std::{sync::Arc, time::Instant};

/// The display-proxy frame of one generation, retained beside the exact raster.
///
/// It is what a zoom back to Fit hands the surface again instead of rendering, and what the
/// clipping overlay is derived from while the exact phase of that generation is still outstanding.
/// Retaining it copies no pixels: it shares the render's own `Arc<[u8]>` with the surface itself.
///
/// Its generation is also the desktop's only record that the job of that generation *had* a proxy
/// phase. The exact result cannot say so: `proxy_declined` is `None` both for a job that asked for
/// no proxy and for one that got one.
pub(crate) struct ProxyFrame {
    pub(crate) generation: u64,
    pub(crate) raster: Arc<lightwell_core::Raster>,
    /// The proxy source dimensions the frame was rendered against.
    pub(crate) dimensions: (u32, u32),
    /// The proxy source was built for this frame rather than taken from the queue's cache.
    pub(crate) built: bool,
    /// Whether the frame approximates the exact render at display size, and why: a spatial layer
    /// whose neighbourhoods scale with the stage, a thin mask, or both.
    pub(crate) approximation: lightwell_core::ProxyApproximation,
    /// The frame approximates a drafted RAW white balance on planes developed at another one, as
    /// the exact phase of the same job does.
    pub(crate) approximate_white_balance: bool,
    /// The proxy phase's own worker time, so a zoom that hands this frame back to the surface
    /// reports how long this picture took rather than whatever was presented last.
    pub(crate) render_ms: f64,
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

impl Editor {
    /// One message about taking up or presenting a preview frame.
    pub(super) fn preview_update(&mut self, message: PreviewMessage) -> Task<Message> {
        match message {
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
                        self.editing = None;
                        self.dragging = None;
                        let entry = payload.job.entry.id.clone();
                        self.requested_render_entry = Some(payload.job.entry.clone());
                        self.show_entry(entry.clone());
                        self.preview_generation = self.request_preview(payload.job);
                        self.status = "Rendering selected history state…".into();
                        // The recipe rows follow the displayed entry: one payload read, no render.
                        if let Some(state) = &self.state {
                            return recipe_task(
                                self.owner.clone(),
                                self.client,
                                state.asset.id.clone(),
                                Some(entry),
                            );
                        }
                    }
                    Err(error) => self.status = error,
                }
            }
            PreviewMessage::Poll => {
                // Both workers wake the event loop through one channel; neither has a poll of its
                // own, and the subscription that carries their signals exists only while one of
                // them is busy. Each worker starts its next job by itself, so nothing here keeps
                // the work moving: this only takes up what has finished. `Poll` is idempotent, so
                // a signal that arrives late costs nothing.
                let mut tasks = Vec::new();
                while let Some(done) = self.overlay_queue.poll() {
                    tasks.push(self.overlay_ready(done));
                }
                tasks.push(self.deliver_previews());
                tasks.push(self.poll_again());
                return Task::batch(tasks);
            }
            PreviewMessage::DraftCut(upload, tiles) => {
                if Some(upload.generation) != self.draft_generation {
                    self.uploading = false;
                    return self.poll_again();
                }
                return self.upload_draft(upload, tiles);
            }
            PreviewMessage::DraftUploaded(upload, index, result) => {
                let current = Some(upload.generation) == self.draft_generation
                    && self
                        .draft_assembly
                        .as_ref()
                        .is_some_and(|assembly| assembly.generation == upload.generation);
                if !current {
                    // A tile of a stage the draft no longer waits for: nothing is assembled, and
                    // the upload gate opens.
                    self.draft_assembly = None;
                    self.uploading = false;
                    return self.poll_again();
                }
                match result {
                    Ok(allocation) => {
                        let Some(photo) = self
                            .draft_assembly
                            .as_mut()
                            .and_then(|assembly| assembly.arrived(index, allocation))
                        else {
                            // More tiles are still on their way.
                            return Task::none();
                        };
                        self.draft_assembly = None;
                        self.uploading = false;
                        self.draft_photo = Some(photo);
                        self.open_draft(CropStage {
                            width: upload.width,
                            height: upload.height,
                            angle: 0.0,
                        });
                    }
                    Err(_) => {
                        self.draft_assembly = None;
                        self.uploading = false;
                        self.set_crop_pending(None);
                        self.draft_generation = None;
                        self.status = "Could not upload the crop's input stage".into();
                        self.settle_step(Settle::Draft);
                    }
                }
                // The upload held back delivery, so ask for whatever finished meanwhile.
                return self.poll_again();
            }
        }
        Task::none()
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
        let workspace = &self.session.workspace;
        let surface = state::histogram::photo_surface(
            self.window,
            workspace.state_panel,
            workspace.tools_panel,
        );
        match self.session.preview.view.zoom {
            Zoom::Fit => {
                let padding = 2.0 * view::canvas::PHOTO_PADDING;
                bounds_of((
                    (surface.0 - padding).max(0.0) * self.scale_factor,
                    (surface.1 - padding).max(0.0) * self.scale_factor,
                ))
            }
            Zoom::Percent { value } => {
                let stage = self.dimensions?;
                let displayed = state::histogram::displayed_size(
                    state::canvas::ZoomView::Percent(value),
                    stage,
                    surface,
                    self.scale_factor,
                    view::canvas::PHOTO_PADDING,
                )?;
                // Strictly smaller in both axes, so a proxy is never asked for a frame that would
                // have to be magnified back up to show the detail the zoom asked for.
                (displayed.0 < stage.0 as f32 && displayed.1 < stage.1 as f32)
                    .then(|| bounds_of(displayed))
                    .flatten()
            }
        }
    }

    /// The proxy frame retained for the generation on screen, when there is one.
    pub(super) fn presented_proxy_frame(&self) -> Option<&ProxyFrame> {
        self.proxy_frame
            .as_ref()
            .filter(|frame| frame.generation == self.presented_generation)
    }

    /// The exact raster retained for the generation on screen, when its exact phase has landed.
    pub(super) fn presented_exact_raster(&self) -> Option<&Arc<lightwell_core::Raster>> {
        self.raster
            .as_ref()
            .filter(|(generation, _)| *generation == self.presented_generation)
            .map(|(_, raster)| raster)
    }

    /// A newer frame has been requested than the one the histogram describes, so the plotted counts
    /// are one generation behind and the plot says so rather than going blank.
    ///
    /// The comparison is against the generation of the newest **requested** preview, not against
    /// whether a worker happens to be busy: a crop draft's own truncated job shares the queue and is
    /// never analysed, so queue business alone would mark a perfectly current histogram stale.
    pub(crate) fn analysis_updating(&self) -> bool {
        match &self.analysis {
            Some(analysis) => analysis.generation != self.preview_generation,
            None => false,
        }
    }

    /// The next preview result that carries something to show: a frame or a failure.
    ///
    /// An exact phase a newer request stopped is delivered too, under its own generation, but it
    /// carries no frame, so it is taken up here and never reaches [`Self::preview_ready`]: it is
    /// recorded as that generation's, and when it was the crop draft's input stage the draft it was
    /// for ends, because no frame for it will come.
    pub(super) fn poll_preview(&mut self) -> Option<lightwell_core::PreviewResult> {
        loop {
            let result = self.preview_queue.poll()?;
            if !result.cancelled() {
                return Some(result);
            }
            let draft = Some(result.generation) == self.draft_generation;
            self.event(
                "preview_exact_cancelled",
                json!({ "generation": result.generation, "draft": draft }),
            );
            if draft {
                self.draft_preview_superseded(Some(result.generation));
            }
        }
    }

    /// Take up the finished preview results in order: every one that presents nothing — a stale
    /// or cancelled outcome, an exact phase adopted behind its proxy, a failure — and at most one
    /// that hands a frame to the display, after which the rest wait for the next `Poll`, so every
    /// presented frame is drawn by the redraw its own update requests.
    ///
    /// The crop draft's input stage still uploads through the toolkit, one such upload at a time.
    /// While it does, results wait in the queue — the worker goes on to its next job regardless,
    /// holding at most two finished results — and `DraftUploaded` asks for them as soon as that
    /// texture is on screen. The photograph itself never sets this flag: its raster becomes the
    /// surface's source in this same update.
    pub(super) fn deliver_previews(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while !self.uploading
            && let Some(result) = self.poll_preview()
        {
            let (task, presented) = self.preview_ready(result);
            tasks.push(task);
            if presented {
                break;
            }
        }
        Task::batch(tasks)
    }

    /// One more `Poll` while a worker still holds a finished result. The wake channel holds one
    /// signal and coalesces, so the signal of a result behind the one just presented may already
    /// have been spent; a result that finishes after this check posts its own. Preview results
    /// held back by the crop draft's upload are asked for by that upload's answer instead, never by
    /// a `Poll` that would find them still held.
    pub(super) fn poll_again(&self) -> Task<Message> {
        if self.overlay_queue.ready() || (!self.uploading && self.preview_queue.ready()) {
            Task::done(Message::Preview(PreviewMessage::Poll))
        } else {
            Task::none()
        }
    }

    /// Take up one preview result that carries something to show. Returns what it asks the runtime
    /// for, and whether it handed a frame to the display — the photograph's surface, or the crop
    /// draft's upload.
    pub(super) fn preview_ready(
        &mut self,
        result: lightwell_core::PreviewResult,
    ) -> (Task<Message>, bool) {
        // The crop draft's truncated preview shares the queue; its generation says which texture
        // the pixels belong to. It is never analysed, because its identity describes the whole
        // stack rather than the layer prefix it renders. A slider gesture's drafted preview is not
        // this: it renders the whole drafted stack into the ordinary photograph, and is adopted
        // like any other frame.
        let for_draft = Some(result.generation) == self.draft_generation;
        if let Some(queue_wait_ms) = result.queue_wait_ms {
            let phase = match result.phase() {
                PreviewPhase::Proxy => "proxy",
                PreviewPhase::Exact => "exact",
            };
            self.event(
                "preview_result_received",
                json!({
                    "generation":result.generation,
                    "phase":phase,
                    "queue_wait_ms":queue_wait_ms,
                    "render_ms":result.render_ms,
                }),
            );
        }
        // The delivery rule, the same monotone one the queue itself applies: present
        // whatever is not older than what is on screen. Rejecting everything but the
        // newest generation presents no frames at all under a sustained drag, because
        // a render almost always finishes after a newer job has been asked for. A
        // job's exact phase carries its proxy's own generation, so equality is
        // delivered too. `preview_queue.cancel()` is what makes work in flight stale.
        if !for_draft && result.generation < self.presented_generation {
            return (Task::none(), false);
        }
        // Taken apart before the frame is matched out of it, so the report and the
        // identity are still in hand on both paths below. What only one phase carries is
        // read from that phase's own outcome.
        let generation = result.generation;
        let identity = result.identity;
        let entry_id = result.entry_id;
        let draft_revision = result.draft_revision;
        let approximate_white_balance = result.approximate_white_balance;
        let render_ms = result.render_ms;
        let (proxy, frame, proxy_dimensions, proxy_built, proxy_approximation, exact) =
            match result.outcome {
                PhaseOutcome::Proxy(outcome) => (
                    true,
                    Ok(outcome.raster),
                    Some(outcome.dimensions),
                    outcome.built,
                    outcome.approximation,
                    None,
                ),
                PhaseOutcome::Exact(outcome) => (
                    false,
                    outcome.result,
                    None,
                    false,
                    lightwell_core::ProxyApproximation::default(),
                    Some((
                        outcome.report,
                        outcome.mask_overlay,
                        outcome.mask_overlay_absent,
                        outcome.proxy_declined,
                    )),
                ),
            };
        let (report, mask_overlay, mask_overlay_absent, proxy_declined) = exact.unwrap_or_default();
        // The mask overlay's coverage grid rides the frame the worker already produced,
        // so the overlay costs no second render. Only the exact phase fills it; the
        // proxy phase leaves the previous grid on screen until it lands. The upload is
        // handed to `update_inner`, which is the one place a task can be added to
        // whatever this arm returns.
        if let Some(overlay) = mask_overlay {
            self.mask_overlay_pending = Some((generation, overlay));
        } else if let Some(reason) = mask_overlay_absent {
            // The overlay was asked for and the host will not draw it: a mask whose
            // coverage depends on the pixel it reads has no grid until there is an
            // operation whose input to read that pixel from, and one it can afford to
            // read. The reason is the host's own and it is said rather than an absence —
            // an overlay switched on and silently not drawn is exactly what
            // "never silently omit an effect" forbids.
            self.mask_overlay_unavailable(generation, &reason);
        }
        // Only an exact result can say why a job that offered bounds has no proxy phase,
        // and it says nothing when the job had one.
        if !for_draft && !proxy {
            self.proxy_declined = proxy_declined;
        }
        match frame {
            Ok(raster) => {
                // The exact phase of a job whose proxy is already on screen, while the
                // view still wants a display-size frame: its report and its raster are
                // taken up and nothing is drawn. The proxy is the Fit view, so writing
                // the same picture again at four times the pixels would cost exactly
                // the work this design exists to remove.
                if !proxy
                    && !for_draft
                    && generation == self.presented_generation
                    && self.presented_proxy
                    && self.proxy_bounds().is_some()
                {
                    self.adopt_exact(
                        generation,
                        identity,
                        report,
                        raster,
                        render_ms,
                        approximate_white_balance,
                    );
                    return (Task::none(), false);
                }
                if for_draft {
                    // The crop draft's input stage is the one photo path left that
                    // takes the toolkit's image widget, so it still uploads, and holds
                    // back delivery while it does.
                    self.uploading = true;
                    self.status = "Preparing pixels for display…".into();
                }
                // The dimensions every pick, every percent-zoom box and every overlay
                // cell maps through are the **exact stage's**, whatever size the
                // texture is; the identity already carries them. A truncated crop job
                // renders a layer prefix its identity does not describe, so that one
                // keeps its own raster's size, as it always has.
                let stage = if for_draft || !proxy {
                    (raster.width, raster.height)
                } else {
                    (identity.width, identity.height)
                };
                if self.activity.pending && !for_draft {
                    self.activity.preview_dimensions = Some(stage);
                    self.event(
                        "decoded",
                        json!({"open_to_raster_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":[stage.0,stage.1],"proxy":proxy}),
                    );
                }
                if !for_draft {
                    // The report the worker reduced from exactly these pixels, and the
                    // pixels themselves, are retained here and adopted below, in the
                    // same update that hands the raster to the surface, so the plot,
                    // the overlays and the photograph are adopted together.
                    // Retaining the raster copies nothing: it shares the render's own
                    // `Arc<[u8]>` with the buffer the surface draws from.
                    let retained = Arc::new(raster.clone());
                    if proxy {
                        // A proxy raster is never reduced, so it replaces no report
                        // and no exact raster. It is retained so a zoom back to Fit
                        // hands it over again instead of rendering, and so the
                        // clipping overlay can follow the drag before the exact phase
                        // lands.
                        self.proxy_frame = Some(ProxyFrame {
                            generation,
                            raster: retained,
                            dimensions: proxy_dimensions.unwrap_or(stage),
                            built: proxy_built,
                            approximation: proxy_approximation,
                            approximate_white_balance,
                            render_ms,
                        });
                    } else {
                        self.exact_render_ms = Some((generation, render_ms));
                        match report {
                            Some(report) => {
                                self.incoming = Some((
                                    Analysis {
                                        generation,
                                        identity,
                                        report,
                                    },
                                    retained,
                                ));
                            }
                            // A frame with no reduction still replaces the retained
                            // raster now, so no overlay is derived from an older image.
                            None => self.retain_unreduced(
                                generation,
                                retained,
                                approximate_white_balance,
                            ),
                        }
                    }
                }
                let upload = Upload {
                    generation,
                    draft_revision,
                    width: stage.0,
                    height: stage.1,
                    entry_id,
                    snapshot_id: raster.snapshot_id.to_string(),
                    source_fingerprint: raster.source_fingerprint.clone(),
                    proxy,
                    proxy_dimensions,
                    proxy_built,
                    proxy_approximation,
                    approximate_white_balance,
                    reason: None,
                    render_ms: Some(render_ms),
                };
                if for_draft {
                    // A stage within one atlas layer is uploaded as it is; a larger
                    // one is cut into layer-sized tiles off this thread first, because
                    // the toolkit draws its own fragments of a rotated image wrongly.
                    if !draft_photo::needs_cutting(raster.width, raster.height) {
                        return (
                            self.upload_draft(upload, vec![draft_photo::whole(raster)]),
                            true,
                        );
                    }
                    return (
                        Task::perform(
                            async move { draft_photo::handles(draft_photo::cut(&raster)) },
                            move |tiles| {
                                Message::Preview(PreviewMessage::DraftCut(upload.clone(), tiles))
                            },
                        ),
                        true,
                    );
                }
                // The photograph reaches the screen from here: the raster becomes the
                // surface's source now and is drawn by the redraw this update requests,
                // with no allocation round trip in between.
                self.present(upload, &raster);
                // A zoom that changed while this frame was rendering is picked up by
                // `present_retained`.
                (self.present_retained(), true)
            }
            Err(error) => {
                if for_draft {
                    self.draft_preview_failed(&error);
                } else {
                    self.preview_failed(
                        generation,
                        proxy,
                        &entry_id,
                        result.draft_revision,
                        &error,
                    );
                }
                (Task::none(), false)
            }
        }
    }

    /// The exact phase of a job whose proxy is already on screen.
    ///
    /// Nothing is drawn: the frame the view wants is the proxy, and writing this raster's
    /// four-times larger texture is exactly the work this design exists to remove. Its report and
    /// its pixels are taken up as a presented frame would take them up, so the histogram, the
    /// clipping counters and the overlay describe the exact render of the picture on screen, and a
    /// later `analysis.request` for this identity is a cache hit instead of a second render.
    pub(super) fn adopt_exact(
        &mut self,
        generation: u64,
        identity: lightwell_core::analysis::AnalysisIdentity,
        report: Option<lightwell_core::analysis::Report>,
        raster: lightwell_core::Raster,
        render_ms: f64,
        approximate_white_balance: bool,
    ) {
        let dimensions = (identity.width, identity.height);
        // Recorded beside the retained raster, so a zoom to 100% that hands it to the surface
        // reports this render's time. The status bar keeps the proxy's figure meanwhile: the proxy
        // is the picture on screen.
        self.exact_render_ms = Some((generation, render_ms));
        // Shares the render's own `Arc<[u8]>`: retaining it copies no pixels.
        let retained = Arc::new(raster);
        match report {
            Some(report) => {
                self.incoming = Some((
                    Analysis {
                        generation,
                        identity,
                        report,
                    },
                    retained,
                ));
                // The pixels of this generation are already on screen — the proxy of the same
                // recipe — so the report is adopted now rather than waiting for a frame that will
                // not arrive.
                self.adopt_analysis(generation);
            }
            None => self.retain_unreduced(generation, retained, approximate_white_balance),
        }
        self.event(
            "preview_exact_adopted",
            json!({"generation":generation,"dimensions":[dimensions.0,dimensions.1],"render_ms":render_ms,"approximate_white_balance":approximate_white_balance}),
        );
        self.release_held(generation);
    }

    /// Retain an exact-phase raster that carries no report. It replaces the retained raster now,
    /// so no overlay is derived from an older image. A frame with no reduction for an ordinary
    /// reason clears the report too; a frame that approximates a drafted RAW white balance never
    /// had one to give, so the last exact report stays plotted, marked updating, until an exact
    /// frame's report replaces it: the histogram is never adopted from an approximate frame.
    pub(super) fn retain_unreduced(
        &mut self,
        generation: u64,
        raster: Arc<lightwell_core::Raster>,
        approximate_white_balance: bool,
    ) {
        self.incoming = None;
        if !approximate_white_balance {
            self.analysis = None;
        }
        self.raster = Some((generation, raster));
        self.raster_approximate_white_balance = approximate_white_balance;
    }

    /// Release what the presented proxy of this generation was holding back: the scripted step it
    /// settles and the open request it completes. Both describe the exact render, which has landed.
    pub(super) fn release_held(&mut self, generation: u64) {
        if self.held_by_proxy.as_ref().map(|held| held.generation) != Some(generation) {
            return;
        }
        let Some(held) = self.held_by_proxy.take() else {
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
                json!({"displayed_generation":self.activity.displayed}),
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
        entry: &lightwell_core::EntryId,
        draft_revision: Option<u64>,
        error: &lightwell_core::Error,
    ) {
        self.refit_pending = false;
        self.status = error.to_string();
        // The canvas explains the failure: the kind and the detail are all the view model needs to
        // name the cause and offer the allowed actions.
        self.render_error = Some((error.kind, error.detail.clone()));
        self.event(
            "preview_failed",
            json!({"generation":generation,"entry_id":entry,"draft_revision":draft_revision,"proxy":proxy,"error_code":error.kind.code(),"detail":error.detail}),
        );
        let shows_target = self.presented_entry.as_ref() == Some(entry)
            && self.displayed_draft_revision == draft_revision;
        if !shows_target && self.photo.is_some() {
            self.withdraw_photo(generation, entry, error);
        }
        // A scripted step waiting for the newest preview's pixels ends on its failure instead: the
        // failure is that step's outcome, and its frame shows it.
        if generation >= self.preview_generation {
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
            self.event("render_failed", json!({"error_code":error.kind.code()}));
            self.outcome_ready(true);
        }
    }

    /// Take the picture of an earlier entry or draft revision off the surface, with everything
    /// that describes it — the retained rasters, the histogram, the overlay's source, the readout
    /// and the render time — so nothing on screen claims to show a state it does not.
    pub(super) fn withdraw_photo(
        &mut self,
        generation: u64,
        target: &lightwell_core::EntryId,
        error: &lightwell_core::Error,
    ) {
        let shown = self.presented_entry.take();
        self.event(
            "preview_withdrawn",
            json!({
                "generation": generation,
                "presented_generation": self.presented_generation,
                "target_entry": target,
                "withdrawn_entry": shown,
                "error_code": error.kind.code(),
            }),
        );
        self.photo = None;
        self.proxy_frame = None;
        self.raster = None;
        self.raster_approximate_white_balance = false;
        self.exact_render_ms = None;
        self.incoming = None;
        self.analysis = None;
        self.held_by_proxy = None;
        self.presented_proxy = false;
        self.presented_approximate_white_balance = false;
        self.rendered_entry = None;
        self.displayed_draft_revision = None;
        self.readout = None;
        self.pending_sample = None;
        self.activity.render = None;
    }

    /// Upload the crop layer's input stage, tile by tile, and hold the queue until every tile is on
    /// the GPU: the draft opens on the whole stage or not at all.
    pub(super) fn upload_draft(
        &mut self,
        upload: Upload,
        tiles: Vec<(draft_photo::TileRect, iced::widget::image::Handle)>,
    ) -> Task<Message> {
        self.draft_assembly = Some(draft_photo::Assembly {
            generation: upload.generation,
            width: upload.width,
            height: upload.height,
            tiles: tiles.iter().map(|(rect, _)| (*rect, None)).collect(),
        });
        Task::batch(tiles.into_iter().enumerate().map(|(index, (_, handle))| {
            let upload = upload.clone();
            image_memory::allocate(handle).map(move |result| {
                Message::Preview(PreviewMessage::DraftUploaded(upload.clone(), index, result))
            })
        }))
    }

    /// The crop layer's input stage could not be rendered, so the draft it was for cannot open or
    /// rebase.
    pub(crate) fn draft_preview_failed(&mut self, error: &lightwell_core::Error) {
        self.end_pending_draft(
            format!("The crop's input stage could not be rendered: {error}"),
            error.kind.code(),
            &error.detail,
            None,
        );
    }

    /// The crop layer's input stage was superseded before it rendered: a newer preview request
    /// stopped its job or replaced it in the pending slot, or the stack changed while the owner was
    /// planning it (`generation` is then `None`). No frame will come, so the draft it was for ends
    /// as a failed one does.
    ///
    /// It is never re-requested and never shielded from the request that superseded it. Every such
    /// request but a view change comes from a change to the stack or the selection the job was
    /// planned from — another client's commit, this client's own command, undo or history
    /// selection — so its input stage and base revision are stale, and a draft opened on them would
    /// not even be marked conflicted. Shielding it would hold the newer state's frame behind the
    /// whole input-stage render, and requesting it again would stop that frame in turn.
    pub(crate) fn draft_preview_superseded(&mut self, generation: Option<u64>) {
        let Some(pending) = self.crop_pending() else {
            return;
        };
        let again = if pending.reapply { "reapply" } else { "start" };
        self.end_pending_draft(
            format!(
                "The crop's input stage was superseded by a newer preview: {again} the crop again"
            ),
            lightwell_core::ErrorKind::Cancelled.code(),
            "superseded by a newer preview",
            generation,
        );
    }

    /// A starting or reapplied draft whose input stage will not arrive ends here, explicitly,
    /// rather than waiting for pixels: a start returns to the pointer mode, a reapply keeps the
    /// draft it was rebasing, still conflicted. The photograph on screen is the current state and
    /// stays. The reason reaches the status bar and the log, and a scripted step waiting for the
    /// draft ends on it.
    pub(super) fn end_pending_draft(
        &mut self,
        status: String,
        error_code: &str,
        detail: &str,
        generation: Option<u64>,
    ) {
        let reapply = self.crop_pending().is_some_and(|pending| pending.reapply);
        self.set_crop_pending(None);
        self.draft_generation = None;
        if !reapply {
            self.end_draft();
        }
        self.status = status;
        self.event(
            "crop_draft_failed",
            json!({"reapply": reapply, "error_code": error_code, "detail": detail, "generation": generation}),
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
        if zoom == *previous || self.state.is_none() {
            return Task::none();
        }
        let wants_proxy = self.proxy_bounds().is_some();
        // Nothing presented yet, or the texture on screen is already the one this zoom wants: a
        // step from Fit to 50% keeps the proxy it has, and the rule that a view change re-renders
        // nothing survives every zoom but the one crossing between proxy and exact.
        if self.presented_generation == 0 || wants_proxy == self.presented_proxy {
            return Task::none();
        }
        // A failure withdrew the picture: nothing retained may be handed over in its place, and a
        // view change asks for no render. The next frame of the target puts a picture back.
        if self.photo.is_none() && self.render_error.is_some() {
            return Task::none();
        }
        if wants_proxy && self.presented_proxy_frame().is_none() {
            // Nothing to hand over: the frame on screen is a full-resolution render with no proxy
            // beside it. One preview job produces the display-size frame this zoom wants, and it is
            // the only render any view change asks for.
            self.event("preview_proxy_requested", json!({ "zoom": zoom }));
            self.await_requested_frame();
            return self.request_current_preview();
        }
        if !wants_proxy && self.presented_exact_raster().is_none() {
            // The exact phase of the frame on screen has not landed. It is already running, and
            // the `Poll` handler presents it when it arrives because the zoom now needs it.
            self.status = "Rendering at full resolution…".into();
            return Task::none();
        }
        self.present_retained()
    }

    /// Put the picture the view asks for on screen from pixels already in hand.
    ///
    /// It renders nothing, asks for nothing and writes nothing — the next redraw's `prepare` writes
    /// the texture — so it is safe to call after every presented frame, which is what it is for: a
    /// zoom that changed while a frame was rendering is picked up here rather than leaving the
    /// wrong picture on screen until the next zoom.
    pub(super) fn present_retained(&mut self) -> Task<Message> {
        if self.presented_generation == 0 {
            return Task::none();
        }
        let wants_proxy = self.proxy_bounds().is_some();
        if wants_proxy == self.presented_proxy {
            return Task::none();
        }
        if wants_proxy {
            let Some(frame) = self.presented_proxy_frame() else {
                return Task::none();
            };
            let (generation, raster, dimensions, built, approximation, white_balance, render_ms) = (
                frame.generation,
                frame.raster.clone(),
                frame.dimensions,
                frame.built,
                frame.approximation,
                frame.approximate_white_balance,
                frame.render_ms,
            );
            return self.hand_retained(
                generation,
                raster,
                Some(dimensions),
                built,
                approximation,
                white_balance,
                Some(render_ms),
            );
        }
        let Some(raster) = self.presented_exact_raster().cloned() else {
            return Task::none();
        };
        let generation = self.presented_generation;
        let render_ms = self
            .exact_render_ms
            .filter(|(recorded, _)| *recorded == generation)
            .map(|(_, ms)| ms);
        let white_balance = self.raster_approximate_white_balance;
        self.hand_retained(
            generation,
            raster,
            None,
            false,
            lightwell_core::ProxyApproximation::default(),
            white_balance,
            render_ms,
        )
    }

    /// One preview job for the entry on screen, at the bounds the view now asks for. The zoom rule
    /// is the only caller, and only when the pixels it needs do not exist.
    /// Queue one preview job with the bounds of this moment. Every job goes through here: the
    /// bounds a task carried from the owner are replaced by what the window, the panels and the
    /// display scale ask for now, so a job requested once the display scale is known is already at
    /// it and a job requested during a resize is sized for the window it will be shown in. A
    /// truncated job never gets a proxy.
    pub(crate) fn request_preview(&mut self, job: lightwell_core::PreviewJob) -> u64 {
        self.request_preview_inner(job, false).0
    }

    /// Queue a mask frame with phase timing for an evidence run. Ordinary preview requests use
    /// the untimed method and do not read the clock.
    pub(crate) fn request_mask_preview_timed(
        &mut self,
        job: lightwell_core::PreviewJob,
    ) -> (u64, Option<Instant>) {
        debug_assert!(self.diagnostics.is_some());
        self.request_preview_inner(job, true)
    }

    pub(super) fn request_preview_inner(
        &mut self,
        mut job: lightwell_core::PreviewJob,
        timed: bool,
    ) -> (u64, Option<Instant>) {
        job.proxy = if job.layer_count.is_some() {
            None
        } else {
            self.proxy_bounds()
        };
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
                    json!({"detail": error.detail.clone()}),
                ),
            }
        }
        let bounds = job.proxy;
        // The job still waiting in the pending slot is replaced by this one and never starts, so
        // nothing about it will ever be delivered: when it was the crop draft's input stage, the
        // draft it was for ends here, as a cancelled one does in `poll_preview`. The request names
        // it in the same step, because the worker takes a pending job by itself the moment the
        // active one ends: a job it took up meanwhile is running, and ends through `poll_preview`.
        let (queued, requested_at) = if timed {
            let (queued, requested_at) = self.preview_queue.request_timed(job);
            (queued, Some(requested_at))
        } else {
            (self.preview_queue.request_replacing(job), None)
        };
        let lightwell_core::Queued {
            generation,
            replaced,
        } = queued;
        self.pending_bounds.insert(generation, bounds);
        if replaced.is_some() && replaced == self.draft_generation {
            self.draft_preview_superseded(replaced);
        }
        (generation, requested_at)
    }

    /// Re-render the proxy on screen once when the bounds it was made for no longer match the
    /// window: a resize, a panel toggle or the display scale arriving. The queue coalesces a
    /// storm of these into one active and one pending job, and nothing is asked for while a
    /// gesture or a crop draft owns the preview, or while a refit is already on its way.
    pub(super) fn refit_proxy(&mut self) -> Task<Message> {
        if self.state.is_none()
            || self.gesture_refusal(Starting::Refit).is_some()
            || self.proxy_refit_deferred()
            || self.presented_generation == 0
            || !self.presented_proxy
            || self.refit_pending
        {
            return Task::none();
        }
        let Some(bounds) = self.proxy_bounds() else {
            return Task::none();
        };
        if self.presented_bounds == Some(bounds) {
            return Task::none();
        }
        self.refit_pending = true;
        self.event(
            "preview_proxy_requested",
            json!({"reason":"bounds","bounds":{"width":bounds.width,"height":bounds.height}}),
        );
        self.await_requested_frame();
        self.request_current_preview()
    }

    /// Drafts own the preview until they finish, so a layout change deliberately leaves their
    /// displayed proxy at its previous bounds instead of starting a competing refit.
    pub(super) fn proxy_refit_deferred(&self) -> bool {
        self.gesture_refusal(Starting::Refit).is_some()
    }

    /// A view change has just asked for the frame it needs. A scripted step whose frame is still
    /// to be captured — waiting on the session round trip, or already settled by it earlier in this
    /// same update — waits for that frame instead, so the capture never shows the picture the view
    /// has already replaced, such as a proxy of the previous bounds.
    pub(super) fn await_requested_frame(&mut self) {
        if let Some(evidence) = &mut self.evidence
            && (evidence.awaiting == Some(Settle::Session)
                || (evidence.awaiting.is_none() && evidence.capture_pending))
        {
            evidence.capture_pending = false;
            evidence.awaiting = Some(Settle::Preview);
        }
    }

    /// Evidence of a displayed proxy waits for the current layout when a refit is permitted.
    /// The exact phase of an open can arm a capture while its display-scale refit is rendering.
    /// Drafts deliberately defer such refits, and can supersede a queued one; their settled frame
    /// can be captured as shown even if that abandoned request left `refit_pending` set.
    pub(super) fn capture_proxy_ready(&self) -> bool {
        if !self.presented_proxy || self.render_error.is_some() || self.proxy_refit_deferred() {
            return true;
        }
        if self.refit_pending {
            return false;
        }
        match self.proxy_bounds() {
            Some(bounds) => self.presented_bounds == Some(bounds),
            // At 100% the exact frame is the target; the step's normal preview settlement
            // already waits for it, without requiring a proxy that cannot be requested.
            None => true,
        }
    }

    pub(super) fn request_current_preview(&mut self) -> Task<Message> {
        let Some(state) = &self.state else {
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

    /// Hand the surface pixels that are already in hand, with no render behind them. The zoom rule
    /// is the only caller: preferring a retained raster over a render whenever the pixels exist is
    /// what keeps a view change free. No write happens here at all — the next redraw's `prepare`
    /// puts these bytes in the texture — so a zoom costs the desktop one `Arc` clone.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hand_retained(
        &mut self,
        generation: u64,
        raster: Arc<lightwell_core::Raster>,
        proxy_dimensions: Option<(u32, u32)>,
        proxy_built: bool,
        proxy_approximation: lightwell_core::ProxyApproximation,
        approximate_white_balance: bool,
        render_ms: Option<f64>,
    ) -> Task<Message> {
        // The texture is a proxy exactly when there are proxy dimensions to describe it.
        let proxy = proxy_dimensions.is_some();
        // Every retained frame belongs to the generation on screen, so it is stamped with the entry
        // that generation rendered — never with the entry the desktop has asked for since, whose
        // frame may still be rendering or may have failed. Handing an older picture over under a
        // newer entry would present it as that entry's result.
        let Some((stage, entry)) = self.dimensions.zip(self.presented_entry.clone()) else {
            return Task::none();
        };
        let upload = Upload {
            generation,
            draft_revision: self.displayed_draft_revision,
            width: stage.0,
            height: stage.1,
            entry_id: entry,
            snapshot_id: raster.snapshot_id.to_string(),
            source_fingerprint: raster.source_fingerprint.clone(),
            proxy,
            proxy_dimensions,
            proxy_built,
            proxy_approximation,
            approximate_white_balance,
            reason: Some("zoom"),
            render_ms,
        };
        self.present(upload, &raster);
        Task::none()
    }

    /// Make `raster` the photo surface's source and record that it is on screen.
    ///
    /// This is what "presented" means from here on: the update in which the raster became the
    /// surface's source. The pixels are drawn by the redraw this update requests, which is the next
    /// frame — the primitive's `prepare` writes them into its own texture on the way — so there is
    /// no allocation round trip between a rendered frame and the screen, and no message to wait for.
    ///
    /// Retaining the raster copies nothing: the surface borrows the render's own `Arc<[u8]>`, which
    /// the desktop already holds as the proxy frame or the exact raster of this generation.
    pub(super) fn present(&mut self, upload: Upload, raster: &lightwell_core::Raster) {
        // Monotone in the version, so the primitive writes a frame exactly once however often the
        // same raster is drawn. Nothing but a new frame moves it.
        self.photo_version += 1;
        self.photo = lightwell_ui::PhotoRaster::new(
            raster.rgba.clone(),
            raster.width,
            raster.height,
            self.photo_version,
        );
        // The exact stage, whatever size the texture is: a proxy is drawn into this box, and every
        // pick, percent-zoom box and overlay cell keeps mapping to exact stage pixels.
        self.dimensions = Some((upload.width, upload.height));
        self.presented_generation = upload.generation;
        self.presented_proxy = upload.proxy;
        self.presented_approximate_white_balance = upload.approximate_white_balance;
        if let Some(bounds) = self.pending_bounds.remove(&upload.generation) {
            self.presented_bounds = bounds;
        }
        self.pending_bounds
            .retain(|generation, _| *generation > upload.generation);
        self.refit_pending = false;
        // A zoom hands over a retained frame of the entry already on screen; the entry the desktop
        // is waiting for stays the one picks, readouts and the next request are addressed to.
        if upload.reason.is_none() {
            self.show_entry(upload.entry_id.clone());
        }
        self.presented_entry = Some(upload.entry_id.clone());
        self.displayed_draft_revision = upload.draft_revision;
        self.adopt_analysis(upload.generation);
        if self
            .requested_render_entry
            .as_ref()
            .is_some_and(|entry| entry.id == upload.entry_id)
        {
            self.rendered_entry = self.requested_render_entry.clone();
        }
        // A frame on screen is the proof the last failure is over.
        self.render_error = None;
        // The status bar's figure is this frame's own render time, measured on the worker for the
        // phase that produced it — never the time since the last open or commit, which a drag, a
        // zoom hand-over or a refit presents long after.
        self.activity.render = upload.render_ms.map(|ms| state::status::RenderTime {
            ms,
            proxy: upload.proxy,
            approximate: upload.approximate_white_balance,
        });
        self.event(
            "preview_displayed",
            json!({
                "entry_id":upload.entry_id,
                "snapshot_id":upload.snapshot_id,
                "generation":upload.generation,
                "draft_revision":upload.draft_revision,
                "dimensions":[upload.width,upload.height],
                "path":"surface",
                "proxy":upload.proxy,
                "proxy_dimensions":upload.proxy_dimensions.map(|(width,height)| json!([width,height])),
                "proxy_built":upload.proxy_built,
                "proxy_approximate":upload.proxy_approximation.is_approximate(),
                "proxy_approximate_reason":upload.proxy_approximation.reason(),
                "approximate_white_balance":upload.approximate_white_balance,
                "reason":upload.reason,
                "render_ms":upload.render_ms,
            }),
        );
        // A scripted preview selection settles on these same pixels, whether or not this frame also
        // belongs to the one open request evidence tracks below. While a slider gesture is open the
        // drafted previews replace one another, so a scripted gesture waits for the one whose
        // settings are the newest.
        // A mask shape gesture drains the same way: while another `draft.set` or the commit is still
        // queued the frame on screen is not the one the step is evidence of, so the step waits for
        // the geometry that settles, and for the newest frame asked for rather than an older one
        // still arriving. A brush re-arming after its stroke committed asks for no frame of its
        // own, so the committed frame settles its step whether or not its `draft.begin` has
        // answered yet.
        let settle = match self.core_gesture() {
            Some(gesture)
                if gesture.draft.frame_pending() || upload.generation < self.preview_generation =>
            {
                None
            }
            Some(gesture) if gesture.slider().is_some() => Some(Settle::SliderDraft),
            _ => Some(Settle::Preview),
        };
        if upload.proxy {
            // The photograph is on screen, but every number a captured frame reports — the
            // histogram, the clipping counters, the overlay it is checked against — comes from the
            // exact render. So the step and the open request wait for this generation's exact phase.
            self.held_by_proxy = Some(HeldByProxy {
                generation: upload.generation,
                settle,
                ready: self.activity.pending,
            });
        } else {
            self.held_by_proxy = None;
            if let Some(settle) = settle {
                self.settle_step(settle);
            }
            if self.activity.pending {
                self.activity.pending = false;
                self.activity.displayed = self.activity.requested;
                self.activity.phase = "ready";
                self.event(
                    "render_ready",
                    json!({"displayed_generation":self.activity.displayed}),
                );
                self.outcome_ready(false);
            }
        }
        self.status = self.displayed_status(&upload);
    }

    /// Take up the report and the raster the preview worker produced for `generation`, now that its
    /// pixels are on screen, and hand the report to the owner's store so an API client's
    /// `analysis.request` for the same identity is a cache hit instead of a second render.
    pub(super) fn adopt_analysis(&mut self, generation: u64) {
        let Some((analysis, raster)) = self.incoming.take() else {
            return;
        };
        if analysis.generation != generation {
            // A report from a frame that is not the one just uploaded describes another image.
            return;
        }
        self.raster = Some((generation, raster));
        // A reduced frame is exact: an approximate one is never reduced.
        self.raster_approximate_white_balance = false;
        self.event(
            "analysis_adopted",
            json!({"generation":generation,"entry_id":analysis.identity.entry_id.as_str(),"draft_revision":analysis.identity.draft.as_ref().map(|draft| draft.draft_revision),"width":analysis.identity.width,"height":analysis.identity.height,"any_shadow":analysis.report.any_shadow,"any_highlight":analysis.report.any_highlight,"both":analysis.report.both}),
        );
        self.owner
            .submit_analysis(analysis.identity.clone(), analysis.report.clone());
        self.analysis = Some(analysis);
    }

    /// Point the canvas at another entry. A readout describes one pixel of one stack, so moving to
    /// another entry drops it and anything waiting to be sampled rather than leaving codes on screen
    /// that belong to an image no longer shown.
    pub(super) fn show_entry(&mut self, entry: lightwell_core::EntryId) {
        if self.display_entry.as_ref() != Some(&entry) {
            self.readout = None;
            self.pending_sample = None;
        }
        self.display_entry = Some(entry);
    }

    /// What the status bar says about the frame that just reached the screen. A historical preview
    /// names the entry it shows by the sequence number the history rows carry, so the status bar
    /// and the state panel agree about which entry is on screen.
    pub(super) fn displayed_status(&self, upload: &Upload) -> String {
        let marker = if self.session.preview.can_edit() {
            "Current".to_owned()
        } else {
            match self.sequence_of(&upload.entry_id) {
                Some(sequence) => format!("Previewing entry {sequence}"),
                None => "Previewing history".to_owned(),
            }
        };
        format!(
            "{marker} · {} × {} · entry {} · snapshot {} · source {}",
            upload.width,
            upload.height,
            short(upload.entry_id.as_str()),
            short(&upload.snapshot_id),
            short(&upload.source_fingerprint)
        )
    }

    /// The sequence number of a loaded entry, when the history page holds it.
    pub(super) fn sequence_of(&self, entry_id: &lightwell_core::EntryId) -> Option<u64> {
        self.history
            .entries
            .iter()
            .find(|entry| &entry.id == entry_id)
            .map(|entry| entry.sequence)
    }

    /// The crop draft is displayed instead of the plain preview only while its own input stage is on
    /// the GPU and the session shows the current state.
    pub(crate) fn drafting(&self) -> bool {
        self.crop().is_some() && self.draft_photo.is_some() && self.session.preview.can_edit()
    }
}

pub(crate) fn short(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}
