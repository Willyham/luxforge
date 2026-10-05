//! What one preview job renders — the evaluation it was planned with — and how it presents it.

use crate::{
    Error, Evaluation, LinearImage, LinearSettings, ProxyBounds, Region, RenderSource, SourceImage,
    analysis::AnalysisIdentity,
};
#[cfg(doc)]
use crate::{ExactOutcome, analysis::Report};

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
    pub(crate) fn fingerprint(&self) -> &str {
        match self {
            Self::Jpeg(image) => &image.fingerprint,
            Self::Raw { image, .. } => image.fingerprint(),
        }
    }

    /// Whether this source evaluates a RAW white balance its developed planes do not hold, through
    /// a [`crate::WhiteBalanceApproximation`]. Only the preview of an open draft is planned that
    /// way. Every frame rendered from such a source is approximate, at the proxy scale and at full
    /// size alike, and none is ever reduced into a report.
    pub(crate) fn approximate_white_balance(&self) -> bool {
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
    pub(crate) fn input(&self) -> RenderSource<'_> {
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

/// How much work the one preview lane may do for this request. An interactive request — a draft's
/// tick the GPU does not draw — produces visible pixels only, from a proxy when its bounds plan
/// one; the desktop asks for settlement once its shared quiet gate opens or the gesture commits.
/// Every other request renders no proxy: its exact frame, reduced to the view's bounds when it
/// names them, is the reference frame of a stack at rest, whose picture is the GPU's. A crop
/// draft's input stage is asked for interactively whenever it has bounds, since nothing is reduced
/// from it, and without bounds as a normal exact-only job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewIntent {
    #[default]
    Immediate,
    Interactive,
    Settle,
    /// Reduce a retained exact frame to new Fit bounds without rendering its recipe again.
    Reduce,
    /// A paused draft's view at 100% and above: its exact visible region alone, the full detail a
    /// pause restores, and no whole frame, which no pause in a gesture renders.
    Refine,
}

#[derive(Clone, Debug)]
pub struct PreviewJob {
    /// Shared exact pixels for a reduce-only job; never a full-frame clone.
    pub reduce: Option<std::sync::Arc<crate::Raster>>,
    /// What this job evaluates, planned once on the catalog owner: the entry it shows, the stack it
    /// renders — the entry's own recipe or an open draft's effective recipe, bound with the verified
    /// bytes of every artifact it lists — the source, the shared registry and render context, and
    /// the stack's one compilation, which the worker renders the exact phase from instead of
    /// compiling it again. Everything after it on this job is how the frame is presented, which the
    /// desktop may change after planning.
    pub evaluation: Evaluation,
    /// `Some(n)` renders only the first `n` layers of that recipe, which is how the desktop shows
    /// the input stage of the layer it is drafting. `None` renders the whole stack.
    pub layer_count: Option<usize>,
    /// Which evaluated image this frame is: the evaluation's identity, exactly as an analysis
    /// job's is. It describes the whole planned stack, so a truncated job's identity is the stack
    /// it was planned from, not the prefix it renders — which is why a truncated job is never
    /// analysed.
    pub identity: AnalysisIdentity,
    /// Reduce the rendered raster into a [`Report`] and return it with the frame, so the displayed
    /// target needs no second render. Refused together with [`PreviewJob::layer_count`].
    ///
    /// Ignored when the source approximates its white balance
    /// (`PreviewSource::approximate_white_balance`): an approximate frame is never reduced into a
    /// report, whatever the job asked, so every histogram and clipping count comes from an exact
    /// render.
    pub analyse: bool,
    /// The physical pixels the display can show this frame in. An interactive job renders a proxy
    /// phase at them; any other job reduces its exact frame to them ([`ExactOutcome::display`]).
    /// `None` is the exact path alone, as a percentage zoom at or above 100% takes. A proxy is
    /// only ever an offer: an ineligible stack, a scale of one or any failure building or
    /// rendering the proxy declines it in [`ExactOutcome::proxy_declined`] and the exact phase runs
    /// unchanged.
    pub proxy: Option<ProxyBounds>,
    /// The visible output-stage rectangle at a percentage zoom, in full-stage pixels. `None` is
    /// the Fit path. The worker clips it against its uncut compilation and explicitly reports a
    /// region decline rather than interpreting it as a recipe crop.
    pub viewport: Option<Region>,
    /// Whether to produce only the interactive frame, or refine it and finish whole-frame
    /// analysis. The desktop sets this after the owner has planned the immutable stack.
    pub intent: PreviewIntent,
    /// Worker-only reason a region was declined before the existing proxy/exact fallback ran.
    /// Owner-planned jobs start with `None`; the worker fills it in its own owned job.
    pub viewport_declined: Option<String>,
    /// An open draft's GPU preview, planned with this job when its request asked
    /// (`PreviewRequest::gpu`): the plan a tick is drawn from, or why the gesture takes the CPU
    /// path, and the boundary it starts from. Preview state, never an API result.
    pub gpu: Option<Box<crate::GpuPreview>>,
    /// The plans a gesture on this job's stack is likely to draw, planned with a committed stack's
    /// job when its request asked (`PreviewRequest::gpu`), so the desktop can compile their
    /// program sequences before a drag begins. Preview state, never an API result.
    pub gpu_warm: Option<std::sync::Arc<[crate::GpuPlan]>>,
    /// How many of [`Self::gpu_warm`]'s plans, from the first, are drags of this job's stack
    /// itself; the rest are the first drags of the modules it does not hold, which the surface
    /// compiles after them.
    pub gpu_warm_open: usize,
    /// A displayed stack's picture at rest on the GPU at its view's bounds or region, planned with
    /// its job when its request asked (`PreviewRequest::gpu`): the plan of the stack itself from the
    /// source, and the boundary every gesture over the same source and view starts from
    /// ([`crate::GpuRest`]). Every committed stack has one, the empty stack included. Preview
    /// state, never an API result.
    pub gpu_rest: Option<Box<crate::GpuRest>>,
    /// The Fit bounds whose picture at rest's tiles the worker plans again once the exact phase
    /// has stored the global estimates the owner's plan could not read
    /// ([`crate::ExactOutcome::rest`]): set by the owner when its tiles named `region-estimate`.
    pub rest_bounds: Option<crate::ProxyBounds>,
}

impl PreviewJob {
    /// A job that renders `evaluation` whole, exactly and at once: no layer prefix, no report, no
    /// proxy phase and no viewport, which the caller sets afterwards. Its identity
    /// is the evaluation's ([`Evaluation::identity`]), which hashes the stack: `O(recipe)`.
    pub fn new(evaluation: Evaluation) -> Result<Self, Error> {
        Ok(Self {
            reduce: None,
            identity: evaluation.identity()?,
            evaluation,
            layer_count: None,
            analyse: false,
            proxy: None,
            viewport: None,
            intent: PreviewIntent::Immediate,
            viewport_declined: None,
            gpu: None,
            gpu_warm: None,
            gpu_warm_open: 0,
            gpu_rest: None,
            rest_bounds: None,
        })
    }
}
