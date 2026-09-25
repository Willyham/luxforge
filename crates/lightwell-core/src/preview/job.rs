//! What one preview job renders: the source it reads, the stack, and what it asks for beside the
//! frame.

use crate::{
    Component, ComponentId, ComponentMode, Error, ErrorKind, HistoryEntry, LinearImage,
    LinearSettings, Mask, MaskId, ModuleRegistry, ProxyBounds, Recipe, RenderContext, RenderSource,
    SourceImage,
    analysis::{AnalysisIdentity, MAX_OVERLAY_CELLS},
};
#[cfg(doc)]
use crate::{ExactOutcome, analysis::Report, mask::CompiledMask};
use std::{collections::BTreeMap, sync::Arc};

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
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::Jpeg(image) => &image.fingerprint,
            Self::Raw { image, .. } => image.fingerprint(),
        }
    }

    /// Whether this source evaluates a RAW white balance its developed planes do not hold, through
    /// a [`crate::WhiteBalanceApproximation`]. Only the preview of an open draft is planned that
    /// way. Every frame rendered from such a source is approximate, at the proxy scale and at full
    /// size alike, and none is ever reduced into a report.
    pub fn approximate_white_balance(&self) -> bool {
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
    pub fn input(&self) -> RenderSource<'_> {
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

/// What a preview job asks the worker for beside the frame: the coverage grid of one mask, over the
/// frame that job renders, on a `cells_w × cells_h` display grid.
///
/// **Why a component *identity* and not an index.** Hovering a row of the component list shows that
/// row's own contribution, so one component's grid has to be obtainable on its own. The cheapest
/// honest way to ask for it is one more field on this request, because it costs nothing anywhere
/// else: the host derives a one-component mask and compiles it through the same [`CompiledMask`]
/// the whole mask goes through, so the row's overlay and the mask's overlay cannot disagree about
/// that component's field, and compiling one component is strictly cheaper than compiling all of
/// them. It is a [`ComponentId`] rather than a position because a position is not an identity: the
/// component list is reorderable, a hover and the frame that answers it are a request apart, and an
/// index that silently slid onto the neighbouring row would draw the wrong field with no way to
/// tell. A component the mask does not hold is refused by name instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskOverlayRequest {
    /// The mask to describe. It must be one this job's recipe holds.
    pub mask: MaskId,
    /// One component of that mask, on its own, or `None` for the whole composed mask.
    pub component: Option<ComponentId>,
    pub cells_w: u32,
    pub cells_h: u32,
}

/// The mask a component's own row describes: that one component alone.
///
/// Its mode is `add` because there is nothing before it to subtract from or intersect with, the
/// whole-mask amount and inversion are left out because they are the mask's modifiers and not the
/// row's, and the component's *own* inversion is kept because that is a control on the row. `None`
/// when the mask does not hold that component.
pub(super) fn one_component(mask: &Mask, component: &ComponentId) -> Option<Mask> {
    let found = mask.components.iter().find(|held| &held.id == component)?;
    let alone = Component {
        mode: ComponentMode::Add,
        ..found.clone()
    };
    Some(Mask {
        id: mask.id.clone(),
        name: mask.name.clone(),
        amount: Mask::FULL_AMOUNT,
        invert: false,
        next_ordinal: BTreeMap::new(),
        components: vec![alone],
    })
}

#[derive(Clone, Debug)]
pub struct PreviewJob {
    pub source: PreviewSource,
    /// The entry this preview shows. Its identity and snapshot correlate the frame with history;
    /// what is rendered is [`PreviewJob::recipe`], which differs from the entry's own stack while a
    /// draft is open.
    pub entry: HistoryEntry,
    /// The providers the worker evaluates this stack with; shared, never rebuilt per job.
    pub registry: Arc<ModuleRegistry>,
    /// The budgets and the estimate store this job's renders share with every other evaluation
    /// its planner runs; shared, never rebuilt per job.
    pub context: RenderContext,
    /// The stack to render: the entry's own recipe, or an open draft's effective recipe, bound
    /// with the verified bytes of every artifact it lists, so the worker compiles it whatever the
    /// owner's cache evicts meanwhile.
    pub recipe: Recipe,
    /// `Some(n)` renders only the first `n` layers of that recipe, which is how the desktop shows
    /// the input stage of the layer it is drafting. `None` renders the whole stack.
    pub layer_count: Option<usize>,
    /// The draft revision this recipe was planned from, for correlating a frame with the settings
    /// that produced it. `None` when no draft was involved.
    pub draft_revision: Option<u64>,
    /// Which evaluated image this frame is, computed exactly as an analysis job's identity is. It
    /// describes the whole planned stack, so a truncated job's identity is the stack it was planned
    /// from, not the prefix it renders — which is why a truncated job is never analysed.
    pub identity: AnalysisIdentity,
    /// Reduce the rendered raster into a [`Report`] and return it with the frame, so the displayed
    /// target needs no second render. Refused together with [`PreviewJob::layer_count`].
    ///
    /// Ignored when the source approximates its white balance
    /// ([`PreviewSource::approximate_white_balance`]): an approximate frame is never reduced into a
    /// report, whatever the job asked, so every histogram and clipping count comes from an exact
    /// render.
    pub analyse: bool,
    /// The physical pixels the display can show this frame in. `Some` asks for a proxy phase before
    /// the exact one; `None` is the exact path alone, as a percentage zoom at or above 100% takes.
    /// A proxy is only ever an offer: an ineligible stack, a scale of one or any failure building
    /// or rendering the proxy declines it in [`ExactOutcome::proxy_declined`] and the exact phase
    /// runs unchanged.
    pub proxy: Option<ProxyBounds>,
    /// Fill one mask's coverage grid beside the frame and return it with it, exactly as
    /// [`PreviewJob::analyse`] returns a [`Report`]. Set through
    /// [`PreviewJob::with_mask_overlay`], which is what validates it against this job's own stack.
    pub mask_overlay: Option<MaskOverlayRequest>,
}

impl PreviewJob {
    /// Ask this job's exact phase for one mask's coverage grid, validated against the stack this
    /// job renders.
    ///
    /// Validation happens here and not on the worker because the answer depends on the stack, and
    /// the stack is in hand: a mask or a component this recipe does not hold is a named
    /// `validation` refusal now rather than a silently absent overlay later. It costs
    /// `O(masks + components)` and reads no pixel, so the thread that plans a job may call it
    /// ([performance rule 5](../../docs/engineering/performance-rules.md#rules)).
    pub fn with_mask_overlay(mut self, request: MaskOverlayRequest) -> Result<Self, Error> {
        let mask = self
            .recipe
            .masks
            .iter()
            .find(|mask| mask.id == request.mask)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    format!(
                        "mask {} is not in the stack this preview renders",
                        request.mask
                    ),
                )
            })?;
        if let Some(component) = &request.component
            && !mask.components.iter().any(|held| &held.id == component)
        {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("mask {} holds no component {component}", mask.name),
            ));
        }
        if request.cells_w == 0 || request.cells_h == 0 {
            return Err(Error::new(
                ErrorKind::Validation,
                "a mask overlay needs a non-empty cell grid",
            ));
        }
        if request.cells_w > MAX_OVERLAY_CELLS || request.cells_h > MAX_OVERLAY_CELLS {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "a mask overlay of {}x{} cells exceeds the {MAX_OVERLAY_CELLS} cells a side the display overlay allows",
                    request.cells_w, request.cells_h
                ),
            ));
        }
        self.mask_overlay = Some(request);
        Ok(self)
    }
}
