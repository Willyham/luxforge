//! The CPU proxy: the drag path of a session the GPU does not draw at all — no adapter, a lost
//! device, `--no-gpu-render` (owner, 2026-10-06). Everything only that path uses lives here, and
//! this module is the one place to remove it from the core.
//!
//! A proxy source is the prepared source reduced once to the size the display can show, by the
//! view's area average (`crate::proxy`). Every layer a gesture can draft is resolution
//! independent — the orientation layer is a mapping, the crop payload is normalized to its own
//! input stage, and the Basic and RAW development layers are pointwise — so the same recipe
//! compiles unchanged against the smaller content stage and produces the same picture at display
//! size, through the same code and the same colour arithmetic. A spatial layer's neighbourhoods and
//! a thin mask are the exceptions, which a frame reports ([`ProxyApproximation`]).
//!
//! The preview worker reaches it through one call, [`CpuProxy::frame`], for a job that asks for it
//! (`PreviewIntent::Interactive` with view bounds), and the result carries one outcome type,
//! [`ProxyOutcome`]. The rest of the module:
//!
//! - `source`: building a proxy source, a JPEG's and a RAW's.
//! - `cache`: the worker's one cached proxy source, keyed by the source's identity and the plan.
//! - `stage`: whether a stack takes a proxy, the whole proxy stage it is planned at, and the stack
//!   compiled and rendered there.
//!
//! `cargo xtask check-repository` refuses this module's items outside it and its dispatch sites
//! (`cpu-proxy`).

mod cache;
mod source;
mod stage;
#[cfg(test)]
mod tests;

use crate::{Cancel, Error, PreviewJob, Raster, Recipe, Render, SnapshotId, activity::Activity};
pub(crate) use cache::{ProxyCache, ProxyKey};
use serde::{Deserialize, Serialize};

/// Why a proxy frame is an approximation of the exact render at display size, rather than the same
/// picture.
///
/// A proxy frame is normally the exact recipe at proxy size: every layer a gesture can draft is
/// resolution independent and a mask's geometry is normalized, so the same equations produce the
/// same picture at display size. Two things break that, and both are reported rather than assumed.
/// Neither one is a reason to decline the proxy: the frame is still what a drag presents, and the
/// exact phase still produces every number, the overlays and the 100% view.
///
/// One word, `approximate`, reaches the client; this is what it means in each case, so a person can
/// tell a thin mask from a spatial layer when both are present.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyApproximation {
    /// Full-resolution restoration filters are evaluated on averaged proxy pixels.
    #[serde(default)]
    pub restoration: bool,
    /// The stack compiles to a spatial operation at the proxy stage. Its neighbourhoods scale with
    /// the stage it is rendered at, so a proxy frame is close to the exact render at display size
    /// rather than equal to it. A neutral spatial layer compiles to no operation and does not set
    /// this.
    pub spatial: bool,
    /// A mask in the stack draws a feature narrower than two pixels of the proxy stage, so its
    /// field — never the effect — is evaluated with a 2 x 2 supersample per pixel
    /// ([masking](../../../docs/design/masking.md#point-queries-and-proxies), proposal P5). Without
    /// that rule a hard edge would alias differently on every frame of a drag.
    pub mask: bool,
}

impl ProxyApproximation {
    /// Whether this frame is approximate at all. `false` is the ordinary case: the proxy render is
    /// the exact recipe at proxy size, byte for byte with the exact recipe over the exact
    /// downscale of the source.
    pub fn is_approximate(self) -> bool {
        self.restoration || self.spatial || self.mask
    }

    /// Why, in one sentence, or `None` when the frame is not approximate. Both reasons are named
    /// when both are present.
    pub fn reason(self) -> Option<String> {
        const SPATIAL: &str =
            "a spatial-stage layer's neighbourhoods scale with the stage it is rendered at";
        const MASK: &str = "a mask draws a feature narrower than two proxy pixels, so its field is \
             evaluated with a 2x2 supersample per pixel";
        let mut reasons = Vec::new();
        if self.restoration {
            reasons
                .push("a restoration layer's full-resolution filters run on averaged proxy pixels");
        }
        if self.spatial {
            reasons.push(SPATIAL);
        }
        if self.mask {
            reasons.push(MASK);
        }
        (!reasons.is_empty()).then(|| reasons.join("; "))
    }
}

/// The display-size frame an interactive job presents: its one phase.
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
}

/// What the proxy phase of one job did.
pub(crate) enum ProxyPhase {
    /// The job asked for no proxy: it is not interactive, or names no view bounds.
    NotAsked,
    /// The job asked, and this is why it has none: an ineligible layer, a scale of one, or the
    /// failure — a cancel included — that planning, building or rendering it returned. Nothing in
    /// the proxy phase is fatal: the job's exact phase runs as it always does.
    Declined(String),
    /// The frame, the job's one phase.
    Drawn(ProxyOutcome),
}

/// The preview worker's CPU proxy: the one cached proxy source, held by the worker alone and
/// bounded by construction, since a new plan replaces the old entry rather than accumulating
/// beside it.
#[derive(Default)]
pub(crate) struct CpuProxy {
    cache: ProxyCache,
}

impl CpuProxy {
    /// The proxy phase of `job`, whose stack is `recipe` and whose one compilation at the exact
    /// stage is `exact`: the whole stack, or a truncated job's layer prefix, planned exactly as a
    /// whole stack of those layers would be. Read under the job's `abandoned` token, `cancel`, so a
    /// drag keeps presenting proxy frames while the full-resolution renders behind them are
    /// abandoned.
    ///
    /// The cache key is the source's identity and the plan: it holds downscaled source pixels,
    /// never a rendered stack, so a prefix and a whole stack that plan the same proxy share its
    /// pixels correctly, and ones that plan different proxies have different keys. Planning costs
    /// `O(layers)`: eligibility reads stages, the plan reads the output stage of `exact`, and the
    /// proxy stage compiles the stack once, which the frame then renders. The build, on a miss,
    /// and the render are frame work on the preview worker.
    pub(crate) fn frame(
        &mut self,
        job: &PreviewJob,
        recipe: &Recipe,
        exact: &Result<Render<'_>, Error>,
        cancel: &Cancel,
        snapshot_id: SnapshotId,
        activity: Option<&Activity>,
    ) -> ProxyPhase {
        if job.intent != crate::PreviewIntent::Interactive {
            return ProxyPhase::NotAsked;
        }
        let Some(bounds) = job.proxy else {
            return ProxyPhase::NotAsked;
        };
        let evaluation = &job.evaluation;
        if let Err(error) = evaluation.registry().proxy_eligible(recipe) {
            return ProxyPhase::Declined(error.detail);
        }
        let exact = match exact {
            Ok(exact) => exact,
            Err(error) => return ProxyPhase::Declined(error.detail.clone()),
        };
        let Some(plan) = exact.proxy_plan(bounds) else {
            return ProxyPhase::Declined(
                "the proxy scale is 1: the stage already fits the display bounds".into(),
            );
        };
        let stage = exact.proxy_stage(evaluation.registry(), recipe, plan);
        let key = ProxyKey {
            identity: evaluation.source().identity(),
            plan: stage.plan(),
        };
        if let Some(activity) = activity {
            activity.phase("proxy");
        }
        // The cache holds pixels; the settings a RAW development layer asks for come from this
        // job's recipe, so a drafted exposure renders against the cached planes. The proxy this
        // job builds belongs to the worker whether or not its frame is still wanted: the next job
        // at the same bounds is a hit either way.
        let built = self.cache.source_for(&key, evaluation.source(), || {
            evaluation.source().proxy_cancellable(key.plan, cancel)
        });
        let (source, fresh) = match built {
            Ok(built) => built,
            Err(error) => return ProxyPhase::Declined(error.detail),
        };
        let dimensions = source.dimensions();
        // The proxy stage's one compilation, the one the plan made: the frame and the reason it is
        // approximate both come from it, so what is reported and what is drawn cannot disagree.
        // Whether a mask draws a feature the proxy's pixel grid can resolve is a fact about that
        // grid, so it is read at exactly the dimensions this frame is rendered against.
        let rendered = exact
            .render_proxy(source.input(), stage, cancel, evaluation.context())
            .and_then(|proxy| Ok((proxy.frame(snapshot_id)?, proxy.approximation())));
        match rendered {
            Err(error) => ProxyPhase::Declined(error.detail),
            Ok((raster, approximation)) => ProxyPhase::Drawn(ProxyOutcome {
                raster,
                dimensions,
                built: fresh,
                approximation,
            }),
        }
    }
}
