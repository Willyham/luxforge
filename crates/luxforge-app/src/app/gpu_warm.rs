//! The GPU warm-up at launch and open (`docs/design/gpu-preview.md`, "Warming at launch and
//! open"; `docs/design/gpu-first.md`, stage 5): the desktop's half, beside the photo surface's
//! compile thread.
//!
//! - **What is warmed, and in what order.** Every committed stack's job carries its warm list, the
//!   open stack's drags first and then the first drags of every module it does not hold
//!   (`PreviewJob::gpu_warm`, `gpu_warm_open`), which [`Editor::gpu_warm_from`] hands the surface.
//!   The surface asks for the picture on screen's own sequences — the picture at rest's view plan
//!   and its tiles — before it queues the list, so its compile thread takes them first.
//! - **Labelled until then.** While the photograph at rest is the reference renderer's frame
//!   because the GPU's picture at rest is still compiling its programs, the status bar says so
//!   ([`Editor::gpu_rest_compiling`], `status::rest_compiling_notice`); the GPU frame replaces it
//!   when they are ready, and the notice goes with it.
//! - **Recorded.** The compile thread times each warm-up, from a list handed to an idle thread
//!   until its queue drains, and wakes the desktop at its start and its end. The desktop lists a
//!   running warm-up on the owner's activity board as [`WARM_UP_KIND`], which the Performance
//!   section and `activity.list` read, ends it when the warm-up ends, and records each ended
//!   warm-up once as the `gpu_warm_up` event. No timer or poll: both wakes are the compile thread's.
use super::Editor;
use luxforge_core::activity::{Activity, ActivitySpec, Outcome};
use luxforge_ui::photo_surface::{GpuFallback as SurfaceFallback, WarmUpFigures};
use serde_json::{Value, json};

/// The activity board's kind for a warm-up, and the label the Performance section shows.
pub(crate) const WARM_UP_KIND: &str = "gpu.warm";
pub(crate) const WARM_UP_LABEL: &str = "Preparing GPU renderer";

/// The desktop's record of the compile thread's warm-ups.
#[derive(Debug, Default)]
pub(crate) struct WarmUpFollow {
    /// The running warm-up's period and its entry on the activity board.
    running: Option<(u64, Activity)>,
    /// The last warm-up recorded as ended.
    recorded: Option<u64>,
    /// What a test reports of the compile thread's warm-up, which no test draws.
    #[cfg(test)]
    pub(crate) figures: Option<WarmUpFigures>,
}

/// A warm-up's figures as evidence records them: `null` before any.
pub(crate) fn evidence(figures: Option<WarmUpFigures>) -> Value {
    figures.map_or(Value::Null, |figures| {
        json!({"period": figures.period, "version": figures.version,
            "sequences": figures.sequences, "open_sequences": figures.open_sequences,
            "open_ms": figures.open_us.map(|us| us as f64 / 1000.0),
            "ms": figures.us.map(|us| us as f64 / 1000.0),
            "running": figures.running(), "first": figures.period == 1})
    })
}

impl Editor {
    /// The compile thread's warm-up running, or its last.
    pub(crate) fn gpu_warm_up_figures(&self) -> Option<WarmUpFigures> {
        #[cfg(test)]
        if let Some(figures) = self.gpu.warm_up.figures {
            return Some(figures);
        }
        luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE).gpu_warm_up
    }

    /// After every message: a warm-up the compile thread runs is listed on the activity board
    /// until it ends, and each one that ended is recorded once, with its figures.
    pub(crate) fn gpu_follow_warm_up(&mut self) {
        let Some(figures) = self.gpu_warm_up_figures() else {
            return;
        };
        let follow = &mut self.gpu.warm_up;
        // A warm-up that is not the one listed has ended, whether or not its end was seen.
        if let Some((_, activity)) = follow
            .running
            .take_if(|(period, _)| !figures.running() || *period != figures.period)
        {
            activity.finish(Outcome::Completed);
        }
        if figures.running() {
            if follow.running.is_none() {
                let asset_id = self
                    .document
                    .state
                    .as_ref()
                    .map(|state| state.asset.id.clone());
                let activity = self.owner.activity().begin(ActivitySpec {
                    kind: WARM_UP_KIND,
                    label: WARM_UP_LABEL,
                    detail: None,
                    asset_id,
                    job_id: None,
                });
                self.gpu.warm_up.running = Some((figures.period, activity));
            }
            return;
        }
        if follow.recorded == Some(figures.period) {
            return;
        }
        follow.recorded = Some(figures.period);
        self.event("gpu_warm_up", || evidence(Some(figures)));
    }

    /// Whether the photograph at rest is the reference renderer's frame because the GPU's picture
    /// at rest is compiling its programs: no gesture is open, the committed stack's view plan is
    /// handed to the surface, and its last frame drew the CPU's frame naming `compiling`.
    pub(crate) fn gpu_rest_compiling(&self) -> bool {
        if self.gpu_rest_plan().is_none() {
            return false;
        }
        let report = self.surface_report();
        report.drawn.is_none() && report.fallback == Some(SurfaceFallback::Compiling)
    }
}
