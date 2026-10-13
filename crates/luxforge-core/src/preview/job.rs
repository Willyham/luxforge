//! What one preview job renders — the evaluation it was planned with — and how it presents it.

use super::paged::PagedPreview;
use crate::{
    Error, Evaluation, LinearImage, LinearSettings, ProxyBounds, RenderSource, SourceImage,
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
    /// A paged derived source (`docs/design/panorama-merge.md`, "Rendering, samples and export"):
    /// no whole frame of level 0, its resident level and windows of level 0 read through it.
    Paged(PagedPreview),
}

impl PreviewSource {
    pub fn orientation(&self) -> u8 {
        match self {
            Self::Jpeg(image) => image.orientation,
            Self::Raw { image, .. } => image.view().1,
            Self::Paged(_) => 1,
        }
    }

    /// The source fingerprint every frame and every report is stamped with.
    pub(crate) fn fingerprint(&self) -> &str {
        match self {
            Self::Jpeg(image) => &image.fingerprint,
            Self::Raw { image, .. } => image.fingerprint(),
            Self::Paged(paged) => paged.fingerprint(),
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
            Self::Paged(paged) => paged.dimensions(),
        }
    }

    /// Whether frames of this source take the linear path: a RAW development's planes and a
    /// derived source's, resident or paged, unclamped between segments.
    pub(crate) fn linear(&self) -> bool {
        matches!(self, Self::Raw { .. } | Self::Paged(_))
    }

    /// The paged source this is, if it is one.
    pub fn paged(&self) -> Option<&PagedPreview> {
        match self {
            Self::Paged(paged) => Some(paged),
            _ => None,
        }
    }

    /// The pixels a whole-frame render of this source reads, borrowed: refused for a paged source,
    /// which has no whole frame (`resource-limit`, naming it), never allocated in its place.
    pub fn whole(&self) -> Result<RenderSource<'_>, Error> {
        match self {
            Self::Jpeg(image) => Ok(RenderSource::Byte(image)),
            Self::Raw { image, settings } => Ok(RenderSource::Linear {
                image,
                settings: *settings,
            }),
            Self::Paged(paged) => Err(
                crate::artifacts::derived_source::paged_source::no_whole_frame(
                    "a whole-frame render",
                    paged.source().id(),
                ),
            ),
        }
    }

    /// The pixels a test's whole-frame render reads: never a paged source's.
    #[cfg(test)]
    pub(crate) fn input(&self) -> RenderSource<'_> {
        self.whole().expect("a test renders no paged source whole")
    }
}

#[cfg(test)]
impl<'a> From<&'a PreviewSource> for RenderSource<'a> {
    fn from(source: &'a PreviewSource) -> Self {
        source.input()
    }
}

/// How much work the one preview lane may do for this request. An interactive request — a drag
/// tick in a session the GPU does not draw at all, the one request that asks for one
/// ([`crate::cpu_proxy`]) — produces visible pixels only, from a display-sized proxy when its
/// bounds plan one, and ends there. Every other request renders no proxy: its exact whole frame,
/// reduced to the view's bounds when it names them, is the reference frame of a stack at rest, a
/// crop draft's input stage the reference draws among them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewIntent {
    #[default]
    Immediate,
    Interactive,
    /// Reduce a retained exact frame to new Fit bounds without rendering its recipe again.
    Reduce,
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
    /// Whether to produce only the interactive frame, or the exact whole frame and its analysis.
    /// The desktop sets this after the owner has planned the immutable stack.
    pub intent: PreviewIntent,
    /// An open draft's GPU preview, planned with this job when its request asked
    /// (`PreviewRequest::gpu`): the plan a tick is drawn from, or why the gesture takes the CPU
    /// path, and the boundary it starts from. Preview state, never an API result.
    pub gpu: Option<Box<crate::GpuPreview>>,
    /// The plans a gesture on this job's stack is likely to draw, the light links their ticks
    /// compute and how many of the plans are the open stack's, planned with a committed stack's
    /// job when its request asked (`PreviewRequest::gpu`), so the desktop can compile their
    /// program sequences before a drag begins. Preview state, never an API result.
    pub gpu_warm: Option<std::sync::Arc<crate::GpuWarmList>>,
    /// A displayed stack's picture at rest on the GPU at its view's bounds or region, planned with
    /// its job when its request asked (`PreviewRequest::gpu`): the plan of the stack itself from the
    /// source, and the boundary every gesture over the same source and view starts from
    /// ([`crate::GpuRest`]). Every committed stack has one, the empty stack included. Preview
    /// state, never an API result.
    pub gpu_rest: Option<Box<crate::GpuRest>>,
}

impl PreviewJob {
    /// A job that renders `evaluation` whole, exactly and at once: no layer prefix, no report, no
    /// proxy phase, which the caller sets afterwards. Its identity
    /// is the evaluation's ([`Evaluation::identity`]), which hashes the stack: `O(recipe)`.
    pub fn new(evaluation: Evaluation) -> Result<Self, Error> {
        Ok(Self {
            reduce: None,
            identity: evaluation.identity()?,
            evaluation,
            layer_count: None,
            analyse: false,
            proxy: None,
            intent: PreviewIntent::Immediate,
            gpu: None,
            gpu_warm: None,
            gpu_rest: None,
        })
    }
}
