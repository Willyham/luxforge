//! The desktop half of the CPU proxy: the drag path of a session the GPU does not draw at all — no
//! adapter, which a launch with `--no-gpu-render` or a host with only a software adapter answers
//! too, or a lost device (owner, 2026-10-06). The core half is `luxforge_core`'s `cpu_proxy`.
//! Everything only this path uses on the desktop lives here, and this module and its tests are the
//! one place to remove it.
//!
//! Every tick of such a drag asks the reference for the drafted stack's display-size proxy
//! (`PreviewIntent::Interactive`): at the view's own bounds below 100%, and at Fit's at 100% and
//! above, which the view magnifies. The queue's latest-wins slot bounds it: a newer tick replaces
//! the pending job and never stops the proxy in progress. The proxy's frame is presented as a
//! reduced frame marked with how it approximates the exact render ([`ProxyFrame`]); the release's
//! reference frame is sharp at rest.
//!
//! Its dispatch sites are named: `super::motion` asks for a tick's proxy ([`Editor::cpu_proxy_tick`]),
//! and `super::preview` sizes the job ([`Editor::cpu_proxy_bounds`]), tells a proxy job apart
//! ([`asks`]) and takes up its frame ([`frame`]). `cargo xtask check-repository` refuses the
//! proxy's items anywhere else (`cpu-proxy`).
use super::{Editor, preview::ReducedFrame};
use luxforge_core::{
    ExactOutcome, PhaseOutcome, PreviewIntent, PreviewJob, ProxyApproximation, ProxyBounds,
};
use std::{sync::Arc, time::Instant};

/// A frame drawn from the CPU proxy, and how it approximates the exact render at the display's
/// size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProxyFrame(ProxyApproximation);

impl ProxyFrame {
    /// Why the frame approximates the exact render at its size, in one sentence, or `None`.
    pub(crate) fn reason(self) -> Option<String> {
        self.0.reason()
    }

    /// Whether a restoration layer's full-resolution filters ran on averaged proxy pixels, which
    /// the status bar says.
    pub(crate) fn restoration(self) -> bool {
        self.0.restoration
    }
}

/// Whether `job` asks the worker for a CPU proxy: a drag tick of a session the GPU does not draw.
pub(crate) fn asks(job: &PreviewJob) -> bool {
    job.intent == PreviewIntent::Interactive
}

/// A proxy job's one phase taken up as the reduced frame it is presented as, generation
/// `generation`; any other outcome, the exact phase, handed back.
pub(crate) fn frame(
    outcome: PhaseOutcome,
    generation: u64,
    approximate_white_balance: bool,
    render_ms: f64,
) -> Result<ReducedFrame, Box<ExactOutcome>> {
    match outcome {
        PhaseOutcome::Proxy(proxy) => Ok(ReducedFrame {
            generation,
            raster: Arc::new(proxy.raster),
            proxy: Some(ProxyFrame(proxy.approximation)),
            approximate_white_balance,
            render_ms,
        }),
        PhaseOutcome::Exact(outcome) => Err(outcome),
    }
}

impl Editor {
    /// A drag tick's proxy, `job` with its preview's intent set: requested at once, latest winning.
    /// Answers the generation and the queue time of the job.
    pub(crate) fn cpu_proxy_tick(
        &mut self,
        mut job: PreviewJob,
        timed: bool,
    ) -> (u64, Option<Instant>) {
        job.intent = PreviewIntent::Interactive;
        self.request_preview_inner(job, timed)
    }

    /// The bounds a drag tick's proxy is rendered at: the view's own below 100%, and Fit's at 100%
    /// and above, magnified to the view.
    pub(crate) fn cpu_proxy_bounds(&self) -> Option<ProxyBounds> {
        self.proxy_bounds().or_else(|| self.fit_bounds())
    }
}
