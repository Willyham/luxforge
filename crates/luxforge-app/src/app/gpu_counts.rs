//! The histogram and clipping counts on the GPU, and the picture the GPU presents with no CPU
//! render (`docs/design/gpu-first.md`, stage 2; `docs/design/basic-and-histogram.md`, "Histogram
//! and clipping contract").
//!
//! - **The GPU presents a committed stack.** A committed job of the whole stack — a commit, a
//!   release, a preset, a history selection or Return to current, a view's re-plan — whose picture
//!   the GPU draws at rest is presented by the GPU alone ([`Editor::present_on_gpu`]): no preview
//!   job is queued, so no exact frame is rendered on the CPU and none is reduced. The queue is
//!   cancelled, raising its floor, so nothing older reaches the screen; the presentation records
//!   the content, its stage and its entry under the floor's generation, the photograph's frame on
//!   the presenter — an earlier frame of the same photograph — stays the surface's base, and the
//!   GPU's picture, the view plan and then its tiles, is drawn over it.
//! - **Where it cannot, the reference.** The job is queued as before — the reference renderer's
//!   exact frame, its report and its view reduction — wherever the GPU cannot present the stack
//!   ([`Editor::gpu_presents`]): before the surface has checked its stage or with the stage refused
//!   or lost, the preference off, an open (no frame of the photograph on screen yet), a view plan
//!   or tiles the GPU cannot draw (`spatial-unit`, `budget-exceeded`, a refused conversion), a
//!   comparison, a crop draft's input stage in flight, or a content the surface refused after it
//!   was presented.
//! - **Its counts are its report.** The picture at rest's tiles, drawn at full resolution, are
//!   counted as they are drawn; where the view draws the stage at its own size or larger, or the
//!   clipping overlay's marks are the view plan's, the same tiles are drawn for their counts alone
//!   ([`Editor::gpu_counts_handed`]). Once the surface reads them back
//!   ([`luxforge_ui::photo_surface::surface_counts`]), they become the presented content's report
//!   ([`Report::of_counts`]), under the job's own identity, and are handed to the owner's store, so
//!   an API client's `analysis.request` for it is a hit, as the reference's report always was.
//! - **In motion.** While a gesture's ticks are drawn on the GPU the inspector plots the counts of
//!   the frame each drew, the frame on screen, marked updating, and hands them to no one.
//! - **The seam.** A content the GPU presented whose counts fail, whose tiles the surface falls
//!   back from, whose view plan's programs are still compiling, or whose stage is lost, is
//!   refused: it is asked for again and the reference renders it.
use super::{
    Before, Editor,
    outcome::{self, Outcome},
};
use crate::state::histogram::{Analysis, AnalysisSource};
use iced::Task;
use luxforge_core::{
    DraftStamp, PreviewIntent, PreviewJob, analysis::AnalysisIdentity, analysis::Report,
};
use luxforge_ui::photo_surface::{CountsOutcome, GpuStageState, gpu_preview::histogram::Counts};
use serde_json::json;

/// The content the GPU presents with no CPU render, whose report its tiles' counts are: the job's
/// identity, the generation it is presented under, its content serial and the versions its tiles
/// are handed to the surfaces under, the picture's and the counts' alone.
#[derive(Clone, Debug)]
pub(crate) struct CountsTarget {
    identity: AnalysisIdentity,
    generation: u64,
    content: u64,
    versions: [u64; 2],
}

/// A report of `counts`, the GPU's counts of a `width × height` frame, in the reducer's shape.
fn report_of(counts: &Counts, (width, height): (u32, u32)) -> Result<Report, String> {
    Report::of_counts(
        [counts.r, counts.g, counts.b],
        [
            counts.r0,
            counts.g0,
            counts.b0,
            counts.r255,
            counts.g255,
            counts.b255,
            counts.any_shadow,
            counts.any_highlight,
            counts.all_shadow,
            counts.all_highlight,
            counts.both,
        ],
        width,
        height,
    )
    .map_err(|error| error.detail)
}

impl Editor {
    /// Whether the GPU presents `job`, the committed whole stack of `content`, with no CPU render
    /// ([module documentation](self)): its view plan is held and not approximate, its tiles are
    /// held and cut from its source, the surface's stage is checked and able, nothing else owns the
    /// photograph, and a frame of the same photograph is on the presenter to draw over.
    pub(crate) fn gpu_presents(&self, job: &PreviewJob, content: u64) -> bool {
        job.layer_count.is_none()
            && job.evaluation.draft_revision().is_none()
            && matches!(job.intent, PreviewIntent::Immediate | PreviewIntent::Settle)
            && self.gpu_stage() == GpuStageState::Available
            && self.gpu_preview_allowed().is_ok()
            && self.core_gesture().is_none()
            && !self.drafting()
            && self.draft_generation().is_none()
            && self.presentation.compare_after.is_none()
            && self
                .evidence
                .as_ref()
                .is_none_or(|evidence| evidence.gpu_identity.is_none())
            && self.gpu.at_rest_drawable()
            && self
                .gpu
                .rest_versions(&job.evaluation.source().identity())
                .is_some()
            && self.presentation.presenter.photo().is_some()
            && self.presentation.presented_asset.as_ref() == Some(&job.evaluation.entry().asset_id)
            && self.gpu.refused_content != Some(content)
    }

    /// Present `job`, the committed whole stack of `content`, on the GPU alone
    /// ([module documentation](self)), and answer the generation it is presented under.
    pub(crate) fn present_on_gpu(&mut self, job: &PreviewJob, content: u64) -> u64 {
        // Nothing planned before this reaches the screen: the floor rises above it.
        let generation = self.presentation.cancel();
        self.view_plan.request_generation = None;
        let stage = (job.identity.width, job.identity.height);
        let entry = job.evaluation.entry().id.clone();
        self.presentation
            .show_gpu(generation, content, stage, &entry, job.proxy);
        // The counts of this content may be in hand already — a view planned again over the same
        // stack — and are then its report under its own identity; otherwise its tiles' are.
        let counted = self.presentation.analysis_content == Some(content)
            && self
                .presentation
                .analysis
                .as_ref()
                .is_some_and(|analysis| analysis.source != AnalysisSource::Motion);
        if counted {
            if let Some(analysis) = self
                .presentation
                .analysis
                .as_mut()
                .filter(|analysis| analysis.identity != job.identity)
            {
                analysis.identity = job.identity.clone();
                analysis.generation = generation;
                let (identity, report) = (analysis.identity.clone(), analysis.report.clone());
                self.owner.submit_analysis(identity, report);
            }
            self.gpu.counts = None;
            self.gpu.want_rest_counts(false);
        } else if let Some(versions) = self.gpu.rest_versions(&job.evaluation.source().identity()) {
            self.gpu.counts = Some(CountsTarget {
                identity: job.identity.clone(),
                generation,
                content,
                versions,
            });
            self.gpu.want_rest_counts(true);
        }
        self.outcome(Outcome::EntryShown(&entry));
        self.show_entry(entry.clone());
        if self.activity.pending {
            self.activity.preview_dimensions = Some(stage);
        }
        // The status bar names the GPU's picture once the surface draws it; until then it renders.
        self.activity.render = None;
        let snapshot = job.identity.snapshot_id.to_string();
        let fingerprint = job.identity.source_fingerprint.clone();
        self.event("preview_displayed", || {
            json!({
                "entry_id": entry,
                "snapshot_id": snapshot,
                "source_fingerprint": fingerprint,
                "generation": generation,
                "draft_revision": null,
                "dimensions": [stage.0, stage.1],
                "path": "gpu",
                "reduced": false,
                "proxy": false,
                "render_ms": null,
                "picture": "gpu",
            })
        });
        self.outcome(Outcome::Presented(outcome::Presented::Photo));
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.event(
                "render_ready",
                || json!({"displayed_generation": self.activity.displayed}),
            );
            self.outcome(Outcome::RequestEnded { failed: false });
        }
        self.status.text = self.displayed_status(&entry);
        self.refresh_overlay();
        generation
    }

    /// A job of the same pixels as the content the GPU presents, reusing them — a mask-only or a
    /// neutral commit — names the identity its counts, still to come, are the report of.
    pub(crate) fn retarget_gpu_counts(&mut self, identity: &AnalysisIdentity, content: u64) {
        if let Some(target) = self
            .gpu
            .counts
            .as_mut()
            .filter(|target| target.content == content)
        {
            target.identity = identity.clone();
        }
    }

    /// Whether a draft is open whose newest revision the counts plotted are not an exact report of:
    /// a GPU tick's frame in motion, or the report of a frame before it. The plot is then marked
    /// updating, as the counts during motion are.
    pub(crate) fn draft_counts_behind(&self) -> bool {
        let Some(draft) = self.session.draft.as_ref() else {
            return false;
        };
        self.presentation.shown_analysis().is_none_or(|analysis| {
            analysis.source == AnalysisSource::Motion
                || analysis.identity.draft.as_ref().is_none_or(|stamp| {
                    stamp.draft_id != draft.draft_id || stamp.draft_revision != draft.draft_revision
                })
        })
    }

    /// Whether the counts of the content the GPU presents are still to come: a capture waits for
    /// them, as it waited for the reference's report.
    pub(crate) fn gpu_counts_pending(&self) -> bool {
        self.gpu
            .counts
            .as_ref()
            .is_some_and(|target| target.content == self.presentation.presented_content)
    }

    /// After every message: take up the GPU's counts of the content it presents as its report, the
    /// counts of a gesture's newest tick as the frame in motion's, and refuse a content the
    /// surface could not draw or count, asking for it again from the reference.
    fn follow_gpu_counts(&mut self) -> Task<super::Message> {
        let counts =
            luxforge_ui::photo_surface::surface_counts(crate::view::canvas::DEVELOP_SURFACE);
        #[cfg(test)]
        let counts = self.gpu.counts_report.clone().unwrap_or(counts);
        self.follow_motion(counts.tick);
        // Motion's counts give way to the report of the content on screen once there is one.
        if self.session.draft.is_none()
            && self.presentation.analysis_content == Some(self.presentation.content_serial)
        {
            self.presentation.motion = None;
        }
        let Some(target) = self.gpu.counts.clone() else {
            return Task::none();
        };
        // A newer frame replaced the content before its counts came: they describe nothing on
        // screen.
        if self.presentation.presented_content != target.content
            || self.presentation.content_serial != target.content
        {
            self.gpu.counts = None;
            self.gpu.want_rest_counts(false);
            return Task::none();
        }
        if self.gpu_stage() != GpuStageState::Available {
            return self.refuse_gpu_content(&target, "stage-unavailable".to_owned());
        }
        // Its programs are still compiling, so the GPU draws nothing of it yet: the reference
        // renders it, as the warm-up's label says, rather than an earlier stack's frame standing.
        if self.gpu_rest_compiling() {
            return self.refuse_gpu_content(&target, "compiling".to_owned());
        }
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if let Some(figures) = drawn.gpu_rest.filter(|figures| {
            target.versions.contains(&figures.version) && figures.fallback.is_some()
        }) {
            let why = figures
                .fallback
                .map_or_else(String::new, |fallback| format!("{fallback:?}"));
            return self.refuse_gpu_content(&target, why);
        }
        let Some((version, outcome)) = counts.rest else {
            return Task::none();
        };
        if !target.versions.contains(&version) {
            return Task::none();
        }
        match outcome {
            CountsOutcome::Counting => Task::none(),
            CountsOutcome::Failed(error) => self.refuse_gpu_content(&target, error.to_string()),
            CountsOutcome::Ready(counts) => {
                let size = (target.identity.width, target.identity.height);
                match report_of(&counts, size) {
                    Ok(report) => {
                        self.adopt_gpu_counts(target, report);
                        Task::none()
                    }
                    Err(why) => self.refuse_gpu_content(&target, why),
                }
            }
        }
    }

    /// The GPU's counts of the content it presents are its report: plotted, and handed to the
    /// owner's store under the job's identity.
    fn adopt_gpu_counts(&mut self, target: CountsTarget, report: Report) {
        self.gpu.counts = None;
        self.gpu.want_rest_counts(false);
        let CountsTarget {
            identity,
            generation,
            content,
            ..
        } = target;
        self.event("analysis_adopted", || {
            json!({"generation": generation, "path": "gpu",
                "entry_id": identity.entry_id.as_str(), "draft_revision": null,
                "width": identity.width, "height": identity.height,
                "any_shadow": report.any_shadow, "any_highlight": report.any_highlight,
                "both": report.both})
        });
        self.owner.submit_analysis(identity.clone(), report.clone());
        self.presentation.analysis = Some(Analysis {
            generation,
            identity,
            report,
            source: AnalysisSource::Gpu,
        });
        self.presentation.analysis_content = Some(content);
        self.presentation.motion = None;
        if content == self.presentation.content_serial {
            self.view_plan.quiet_since = None;
        }
    }

    /// The surface could not draw or count the content the GPU presented: the reference renders
    /// it, and the GPU presents it no more.
    fn refuse_gpu_content(&mut self, target: &CountsTarget, why: String) -> Task<super::Message> {
        self.gpu.counts = None;
        self.gpu.want_rest_counts(false);
        self.gpu.refused_content = Some(target.content);
        self.presentation.gpu_presented = None;
        let content = target.content;
        self.event(
            "gpu_presented_refused",
            || json!({"content": content, "why": why}),
        );
        self.request_current_preview()
    }

    /// The counts of a gesture's newest tick the GPU drew, the frame on screen in motion: plotted
    /// in place of the report, marked updating, while that gesture's draft is open.
    fn follow_motion(&mut self, tick: Option<luxforge_ui::photo_surface::TickCounts>) {
        let (Some(tick), Some(draft)) = (tick, self.session.draft.as_ref()) else {
            return;
        };
        let key = (tick.boundary, tick.revision);
        if self.gpu.motion_tick == Some(key)
            || tick.revision > draft.draft_revision
            || self.gpu.held_version() != Some(tick.boundary)
        {
            return;
        }
        let CountsOutcome::Ready(counts) = tick.counts else {
            return;
        };
        let Some(base) = self.presentation.analysis.as_ref() else {
            return;
        };
        let Ok(report) = report_of(&counts, tick.size) else {
            return;
        };
        self.gpu.motion_tick = Some(key);
        let identity = AnalysisIdentity {
            draft: Some(DraftStamp {
                draft_id: draft.draft_id.clone(),
                draft_revision: tick.revision,
            }),
            width: tick.size.0,
            height: tick.size.1,
            ..base.identity.clone()
        };
        self.presentation.motion = Some(Analysis {
            generation: self.presentation.presented_generation,
            identity,
            report,
            source: AnalysisSource::Motion,
        });
    }
}

/// After every message: the GPU's counts followed ([`Editor::follow_gpu_counts`]).
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<super::Message> {
    editor.follow_gpu_counts()
}

#[cfg(test)]
#[path = "gpu_counts_tests.rs"]
mod tests;
