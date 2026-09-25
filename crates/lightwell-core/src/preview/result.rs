//! What the preview worker delivers: one result per phase, typed by the phase that produced it.

#[cfg(doc)]
use super::{PreviewQueue, PreviewSource};
use crate::{
    EntryId, Error, ErrorKind, ProxyApproximation, Raster,
    analysis::{AnalysisIdentity, MaskOverlay, Report},
};

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

/// One phase of one preview job.
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
    /// What this phase produced, and what only that phase can say.
    pub outcome: PhaseOutcome,
    /// Whether this frame approximates a RAW white balance the developed planes do not hold — a
    /// drafted temperature or tint, previewed during its gesture before the release redevelops the
    /// mosaic ([`PreviewSource::approximate_white_balance`]). Set on **both** phases of such a job:
    /// the matrix is linear and the proxy's box filter is linear, so it applies to the proxy
    /// exactly as it does to the full frame, and neither phase is the exact picture. Such a job
    /// never carries a [`ExactOutcome::report`], even when it asked for one.
    pub approximate_white_balance: bool,
    /// Milliseconds of wall-clock time the preview worker spent producing this phase's result, and
    /// nothing else.
    ///
    /// - [`PreviewPhase::Proxy`]: compiling the job's stack, planning the proxy from it, building
    ///   its source when this job built it ([`ProxyOutcome::built`]), compiling and rendering the
    ///   recipe against it, and filling [`ProxyOutcome::mask_overlay`]'s coverage grid when the job
    ///   asked for one. A cache hit costs the compiles, the plan, the render and the grid.
    /// - [`PreviewPhase::Exact`]: rendering the prepared source, plus reducing the frame into
    ///   [`ExactOutcome::report`] when the job asked for it, and filling
    ///   [`ExactOutcome::mask_overlay`]'s coverage grid when the job asked for one and no proxy
    ///   frame carried it. The job compiles its stack once for both phases, and that compile is
    ///   counted here only when no proxy frame came before. A proxy phase that was attempted and
    ///   declined is not counted here; it produced no frame.
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

/// What each phase produces. A proxy phase is only ever delivered with a frame: one that fails or
/// is cancelled records its reason on its job's exact outcome instead. The exact outcome is boxed
/// because its report is about 6 KiB, which the proxy phase would otherwise carry for nothing.
#[derive(Debug)]
pub enum PhaseOutcome {
    Proxy(ProxyOutcome),
    Exact(Box<ExactOutcome>),
}

/// The display-size frame a job presents first.
#[derive(Debug)]
pub struct ProxyOutcome {
    pub raster: Raster,
    /// The proxy source dimensions this frame was rendered against.
    pub dimensions: (u32, u32),
    /// Whether this frame's proxy source was built for this job rather than taken from the worker's
    /// cache.
    pub built: bool,
    /// Whether this frame is an approximation of the exact render at display size, and why: a
    /// spatial-stage layer whose neighbourhoods scale with the stage, a mask drawing a feature
    /// narrower than two proxy pixels, or both.
    pub approximation: ProxyApproximation,
    /// The coverage grid the job asked for, delivered with the first frame the job presents. It
    /// reads no pixel of the exact frame — only the geometry tail of the job's exact compilation,
    /// and for a mask that reads pixels, the input of its first bound layer — so it does not wait
    /// for the exact render, and it is the same grid, byte for byte, that phase would have carried.
    /// The job's exact phase then carries none.
    pub mask_overlay: MaskOverlayOutcome,
}

/// A job's coverage grid, or the reason it has none, on the phase that carries it: the proxy phase
/// when the job has one, and otherwise its one exact phase. Empty on every other phase, and on a
/// job that asked for no overlay.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaskOverlayOutcome {
    /// The coverage grid of the mask the job named, over the job's exact output stage, under the
    /// generation of the frame it arrives with. `None` means the job did not ask, this phase does
    /// not carry it, the render failed, or the mask had nothing to describe. It never means a mask
    /// whose coverage happens to be zero everywhere: that is a grid of zeros, and this is its
    /// absence.
    pub grid: Option<MaskOverlay>,
    /// Why the grid the job asked for is not in `grid`, in the host's own words.
    ///
    /// A client that asked for an overlay and waits for its texture has to be able to stop waiting:
    /// the grid is refused for reasons that belong to the mask rather than to the frame — a mask
    /// whose coverage depends on the pixel it reads has no grid at all
    /// ([proposal P16](../../docs/design/range-study.md#proposals)) — and an absence with no reason
    /// beside it is indistinguishable from a grid still on its way. `None` means the job asked for
    /// no overlay, this phase does not carry it, the render itself failed, or a newer request is
    /// coming with its own grid; in the last case the wait is correct and this must stay empty.
    pub absent: Option<String>,
}

/// The full-resolution phase, the one every number comes from: every job that starts ends with
/// exactly one.
#[derive(Debug)]
pub struct ExactOutcome {
    /// The frame, the failure, or [`ErrorKind::Cancelled`] when a newer request or
    /// [`PreviewQueue::cancel`] stopped it.
    pub result: Result<Raster, Error>,
    /// The exact reduction of the raster in `result`, when the job asked for it. `None` means the
    /// job did not ask, the render failed, or the frame approximates its white balance
    /// ([`PreviewResult::approximate_white_balance`]), which is never reduced. It never means an
    /// empty histogram.
    pub report: Option<Report>,
    /// The coverage grid the job asked for, when this is the job's one phase: a job whose proxy
    /// frame was delivered carries it there ([`ProxyOutcome::mask_overlay`]) and leaves this empty.
    /// On this phase it is filled only beside a rendered frame.
    pub mask_overlay: MaskOverlayOutcome,
    /// Why a job that asked for a proxy phase has none: the ineligible layer, a scale of one, or
    /// the failure that building or rendering the proxy returned. `None` when the job asked for no
    /// proxy or got one.
    pub proxy_declined: Option<String>,
}

impl PreviewResult {
    /// Which phase produced this result.
    pub fn phase(&self) -> PreviewPhase {
        match self.outcome {
            PhaseOutcome::Proxy(_) => PreviewPhase::Proxy,
            PhaseOutcome::Exact(_) => PreviewPhase::Exact,
        }
    }

    /// This phase's frame, or the reason the exact phase has none.
    pub fn raster(&self) -> Result<&Raster, &Error> {
        match &self.outcome {
            PhaseOutcome::Proxy(proxy) => Ok(&proxy.raster),
            PhaseOutcome::Exact(exact) => exact.result.as_ref(),
        }
    }

    /// This phase's frame, taken out of the result.
    pub fn into_raster(self) -> Result<Raster, Error> {
        match self.outcome {
            PhaseOutcome::Proxy(proxy) => Ok(proxy.raster),
            PhaseOutcome::Exact(exact) => exact.result,
        }
    }

    /// The proxy phase's own outcome, or `None` on the exact phase.
    pub fn proxy(&self) -> Option<&ProxyOutcome> {
        match &self.outcome {
            PhaseOutcome::Proxy(proxy) => Some(proxy),
            PhaseOutcome::Exact(_) => None,
        }
    }

    /// The exact phase's own outcome, or `None` on the proxy phase.
    pub fn exact(&self) -> Option<&ExactOutcome> {
        match &self.outcome {
            PhaseOutcome::Proxy(_) => None,
            PhaseOutcome::Exact(exact) => Some(exact.as_ref()),
        }
    }

    /// The coverage grid this phase carries, or the reason it has none. Read from whichever phase
    /// arrives: a job's grid is on exactly one of them, the first it delivers.
    pub fn mask_overlay(&self) -> &MaskOverlayOutcome {
        match &self.outcome {
            PhaseOutcome::Proxy(proxy) => &proxy.mask_overlay,
            PhaseOutcome::Exact(exact) => &exact.mask_overlay,
        }
    }

    /// Whether this frame is a proxy approximation of the exact render at all. The one word a
    /// client reads; [`ProxyOutcome::approximation`] says which of the two made it so. Never on the
    /// exact phase, which is the frame every number comes from.
    pub fn proxy_approximate(&self) -> bool {
        self.proxy()
            .is_some_and(|proxy| proxy.approximation.is_approximate())
    }

    /// Whether this is an exact phase that a newer request or [`PreviewQueue::cancel`] stopped: it
    /// carries no frame, only the fact that this generation has ended. A proxy phase is never
    /// delivered cancelled; a failed proxy is recorded on the exact outcome instead.
    pub fn cancelled(&self) -> bool {
        self.exact().is_some_and(|exact| {
            exact
                .result
                .as_ref()
                .is_err_and(|error| error.kind == ErrorKind::Cancelled)
        })
    }
}
