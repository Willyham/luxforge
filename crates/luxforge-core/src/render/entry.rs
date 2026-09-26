//! The one way into rendering.
//!
//! [`render`] compiles a recipe against one source for one phase and binds it to the
//! [`RenderContext`] whose budgets and estimate store it reads. What it returns, a [`Render`],
//! answers everything a caller asks of an evaluated stack from that one compilation: the whole
//! frame, one pixel, a grid of pixels, the output stage and its geometry. Every export, preview
//! phase, analysis, sample and draft evaluation enters here, whichever interpretation the source has.
//! The source's interpretation picks the pixel domain ([`super::Byte`] or [`linear::Linear`]); the
//! one pipeline ([`super::pipeline`]) evaluates either, and each domain's driver
//! ([`super::rasterize`] or [`linear::rasterize`]) writes its frame.

use super::{
    Byte, Cancel, Compiled, Evaluation, LayerInput, PixelDomain, Raster, Sample, SpatialMode,
    StageTransform, check_source, grid_centres,
    linear::{self, Linear, LinearImage, LinearSettings},
    rasterize,
    spatial::PRODUCTION_TILE,
    transform_of,
    window::WindowPlan,
};
use crate::{
    Error, ModuleRegistry, ProxyApproximation, ProxyBounds, ProxyPlan, ProxyWindow, Recipe,
    SnapshotId, SourceImage,
    mask_field::MaskSampling,
    modules::{Global, Region},
};
use std::borrow::Cow;

pub use super::context::RenderContext;

/// The pixels one render reads: a JPEG's decoded bytes, or a developed RAW's linear planes with the
/// settings its recipe asks of them. Borrowed, so entering a render copies nothing.
#[derive(Clone, Copy, Debug)]
pub enum RenderSource<'a> {
    Byte(&'a SourceImage),
    Linear {
        image: &'a LinearImage,
        settings: LinearSettings,
    },
}

impl RenderSource<'_> {
    /// The content-stage dimensions a recipe is compiled against.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Byte(image) => (image.width, image.height),
            Self::Linear { image, .. } => (image.width(), image.height()),
        }
    }

    /// The fingerprint every frame is stamped with.
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::Byte(image) => &image.fingerprint,
            Self::Linear { image, .. } => image.fingerprint(),
        }
    }
}

impl<'a> From<&'a SourceImage> for RenderSource<'a> {
    fn from(image: &'a SourceImage) -> Self {
        Self::Byte(image)
    }
}

/// Which of a preview job's phases a render is.
///
/// Every render but the proxy phase is [`RenderPhase::Exact`], which point-samples each mask
/// field. [`RenderPhase::Proxy`] is the same code and the same colour arithmetic against a
/// downscaled source, with the thin-feature rule applied to the masks: a mask drawing a feature
/// narrower than two pixels of that smaller stage has its field supersampled 2 x 2, never the
/// effect, and [`Render::approximation`] reports it. No exact render ever takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderPhase {
    #[default]
    Exact,
    Proxy,
}

impl RenderPhase {
    fn sampling(self) -> MaskSampling {
        match self {
            Self::Exact => MaskSampling::Point,
            Self::Proxy => MaskSampling::ThinFeature,
        }
    }
}

/// How one render is evaluated: its phase and the token its passes read.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub phase: RenderPhase,
    /// Read once per row or chunk by every rasterizing pass and once per batch of spatial tiles. A
    /// token already cancelled when a frame is asked for costs no frame.
    pub cancel: Cancel,
    /// The spatial tile size: [`PRODUCTION_TILE`] everywhere but in the tests that prove a frame
    /// and a sample do not depend on it.
    pub(crate) tile: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            phase: RenderPhase::Exact,
            cancel: Cancel::never(),
            tile: PRODUCTION_TILE,
        }
    }
}

impl RenderOptions {
    /// The exact phase under `cancel`.
    pub fn exact(cancel: &Cancel) -> Self {
        Self {
            cancel: cancel.clone(),
            ..Self::default()
        }
    }

    /// The proxy phase under `cancel`.
    pub fn proxy(cancel: &Cancel) -> Self {
        Self {
            phase: RenderPhase::Proxy,
            cancel: cancel.clone(),
            ..Self::default()
        }
    }

    /// The same options at another spatial tile size.
    #[cfg(test)]
    pub(crate) fn with_tile(mut self, tile: u32) -> Self {
        self.tile = tile;
        self
    }
}

/// One recipe compiled against one source for one phase, bound to the context its evaluations
/// read. Compiling costs `O(layers + components)` and reads no pixel; everything after it reuses
/// that one compilation.
pub struct Render<'a> {
    source: RenderSource<'a>,
    compiled: Compiled,
    options: RenderOptions,
    context: &'a RenderContext,
}

/// Enter rendering: compile `recipe` against `source` for the phase `options` name.
///
/// This is the one entry point. It refuses what no evaluation of the stack could accept — a source
/// buffer of the wrong length, RAW settings out of range, a stack the host cannot compile, or a
/// linear stack with more than one resample — before any pixel is read, and allocates nothing but
/// the compiled operation lists. The catalog owner may therefore call it to learn a stack's output
/// stage, and a preview job compiles its stack once here before either of its phases.
pub fn render<'a>(
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    recipe: &Recipe,
    options: RenderOptions,
    context: &'a RenderContext,
) -> Result<Render<'a>, Error> {
    let source = source.into();
    match source {
        RenderSource::Byte(image) => check_source(image)?,
        RenderSource::Linear { settings, .. } => {
            settings.multiplier()?;
        }
    }
    let (width, height) = source.dimensions();
    #[cfg(test)]
    context.note_compile();
    let compiled = registry.compile_sampled(width, height, recipe, options.phase.sampling())?;
    Render::compiled(source, compiled, options, context)
}

impl<'a> Render<'a> {
    /// A render of a stack compiled elsewhere, for a caller that compiles a prefix once and asks it
    /// many questions. `compiled` must have been compiled against `source`'s dimensions at
    /// `options`' phase.
    pub(crate) fn compiled(
        source: RenderSource<'a>,
        compiled: Compiled,
        options: RenderOptions,
        context: &'a RenderContext,
    ) -> Result<Self, Error> {
        if let RenderSource::Linear { .. } = source {
            linear::check_resamples(&compiled)?;
        }
        Ok(Self {
            source,
            compiled,
            options,
            context,
        })
    }

    /// Whether a point of this stack evaluates a spatial tile, the declared exception to a point
    /// query costing `O(layers)`. `O(segments)` and reads no pixel.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.compiled.evaluates_spatial()
    }

    /// The compilation, for a caller that plans a render on one thread and evaluates it on another
    /// through [`Self::compiled`], against the same source and at the same phase.
    pub(crate) fn into_compiled(self) -> Compiled {
        self.compiled
    }

    /// The output stage's dimensions.
    pub fn stage(&self) -> (u32, u32) {
        let stage = self.compiled.stage();
        (stage.width, stage.height)
    }

    /// The whole frame, stamped with `snapshot_id`. A cancelled token answers
    /// [`ErrorKind::Cancelled`] and never a partial frame, with every reservation released.
    pub fn frame(&self, snapshot_id: SnapshotId) -> Result<Raster, Error> {
        let cancel = &self.options.cancel;
        match self.source {
            RenderSource::Byte(image) => rasterize(
                image,
                &self.compiled,
                snapshot_id,
                cancel,
                self.options.tile,
                self.context,
            ),
            RenderSource::Linear { image, settings } => {
                cancel.check()?;
                let evaluation =
                    self.evaluation(Linear::new(image, settings)?, SpatialMode::Frames)?;
                linear::rasterize(&evaluation, snapshot_id, cancel, self.context)
            }
        }
    }

    /// One output pixel without rasterizing a frame: `O(layers)`, and through a spatial layer the
    /// one tile that contains it, which is the declared exception to point queries never
    /// rasterizing. The byte is the byte [`Self::frame`] writes there. `rgba` is `None` outside the
    /// output stage.
    pub fn sample(&self, x: u32, y: u32) -> Result<Sample, Error> {
        let (width, height) = self.stage();
        let rgba = match self.source {
            RenderSource::Byte(image) => self.sample_in(Byte(image), x, y)?,
            RenderSource::Linear { image, settings } => {
                self.sample_in(Linear::new(image, settings)?, x, y)?
            }
        };
        Ok(Sample {
            width,
            height,
            rgba,
        })
    }

    /// The output pixels at the centres of a `side` × `side` grid, row by row from the top-left,
    /// through one evaluation, so each equals the rendered byte there: `O(side² × layers)`, with
    /// each spatial segment answered as the domain's [`PixelDomain::GRID`] says — the byte path's
    /// points share one tile cache and allocate no frame, and the linear path materializes each
    /// spatial output once for all of them, since the points spread over the whole stage.
    /// `checkpoint` is asked before each point.
    pub(crate) fn grid(
        &self,
        side: u32,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<[u8; 4]>, Error> {
        match self.source {
            RenderSource::Byte(image) => self.grid_in(Byte(image), side, checkpoint),
            RenderSource::Linear { image, settings } => {
                self.grid_in(Linear::new(image, settings)?, side, checkpoint)
            }
        }
    }

    /// The whole geometry tail as one affine map between the content stage and the output stage,
    /// from this compilation: `O(layers)`, no pixel read.
    pub fn transform(&self) -> Result<StageTransform, Error> {
        let (width, height) = self.source.dimensions();
        transform_of(&self.compiled, width, height)
    }

    /// Why this render's frame is an approximation of the exact render at its size, read from the
    /// compilation the frame itself uses, so what is reported and what is drawn cannot disagree:
    /// a spatial operation, whose neighbourhoods scale with the stage, and a mask the proxy phase
    /// supersampled. `O(layers + components)`, no pixel read.
    pub fn approximation(&self) -> ProxyApproximation {
        self.compiled.approximation()
    }

    /// The proxy of this render's source that fits `bounds`, planned from this compilation's
    /// output stage, or `None` when no proxy strictly smaller than the source would fit
    /// ([`ProxyPlan::fit`]).
    pub fn proxy_plan(&self, bounds: ProxyBounds) -> Option<ProxyPlan> {
        ProxyPlan::fit(self.source.dimensions(), self.stage(), bounds)
    }

    /// `plan`, fitted from this render's output stage ([`Self::proxy_plan`]), with the window of
    /// its proxy stage that `recipe` reads, when it reads less than all of it: what a crop reads
    /// through every boundary before it, plus the margins each boundary needs
    /// ([`super::window`]). The plan comes back unchanged — a whole-stage proxy, which is always a
    /// correct answer — when the stack reads the whole stage, cannot be cut, or does not compile
    /// at the proxy stage into the segments it compiles to here. Compiles the stack once at the
    /// proxy stage, `O(layers)`, and reads no pixel.
    pub fn proxy_window(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        plan: ProxyPlan,
    ) -> ProxyPlan {
        let Ok(compiled) = registry.compile_sampled(
            plan.width,
            plan.height,
            recipe,
            RenderPhase::Proxy.sampling(),
        ) else {
            return plan;
        };
        if !same_segments(&compiled, &self.compiled) {
            return plan;
        }
        match WindowPlan::of(&compiled, (plan.width, plan.height)) {
            Some(windows) => ProxyPlan {
                window: Some(ProxyWindow {
                    x: windows.source.x0,
                    y: windows.source.y0,
                    width: windows.source.width,
                    height: windows.source.height,
                }),
                ..plan
            },
            None => plan.whole(),
        }
    }

    /// The proxy phase of this render's stack: `recipe` compiled at `plan`'s proxy stage against
    /// `source`, the proxy source `plan` built, under `cancel`. Without a window this is
    /// [`render`] at the proxy phase. With one, the compilation against the whole proxy stage is
    /// cut to the window ([`super::window`]), and a spatial operation whose stage the window cuts
    /// is handed the global estimates this render — the exact phase of the same job — resolves for
    /// it, which the store then holds for the exact frame. `O(layers)` and no pixel, besides the
    /// exact stage's one reduction per estimate the store does not hold.
    pub fn render_proxy<'s>(
        &self,
        registry: &ModuleRegistry,
        source: RenderSource<'s>,
        recipe: &Recipe,
        plan: ProxyPlan,
        cancel: &Cancel,
        context: &'s RenderContext,
    ) -> Result<Render<'s>, Error> {
        let options = RenderOptions::proxy(cancel);
        let Some(window) = plan.window else {
            return render(registry, source, recipe, options, context);
        };
        match source {
            RenderSource::Byte(image) => check_source(image)?,
            RenderSource::Linear { settings, .. } => {
                settings.multiplier()?;
            }
        }
        if source.dimensions() != (window.width, window.height) {
            return Err(Error::internal(format!(
                "a proxy window of {}x{} was handed a {}x{} source",
                window.width,
                window.height,
                source.dimensions().0,
                source.dimensions().1
            )));
        }
        #[cfg(test)]
        context.note_compile();
        let compiled =
            registry.compile_sampled(plan.width, plan.height, recipe, options.phase.sampling())?;
        let placed = Region {
            x0: window.x,
            y0: window.y,
            width: window.width,
            height: window.height,
        };
        let windows = WindowPlan::of(&compiled, (plan.width, plan.height))
            .filter(|windows| windows.source == placed && same_segments(&compiled, &self.compiled))
            .ok_or_else(|| {
                Error::internal("a proxy window no longer matches the stack it was planned for")
            })?;
        let compiled = windows.apply(compiled, (plan.width, plan.height), |index| {
            self.spatial_globals(index)
        })?;
        Render::compiled(source, compiled, options, context)
    }

    /// The global estimates the spatial operation entering segment `index` reads, resolved as a
    /// frame of this render resolves them: from the store, or from one reduction of that
    /// operation's whole input stage, which the store then keeps for the frame. A point
    /// evaluation, so no frame is materialized for it.
    pub(crate) fn spatial_globals(&self, index: usize) -> Result<Vec<Option<Global>>, Error> {
        match self.source {
            RenderSource::Byte(image) => self
                .evaluation(Byte(image), SpatialMode::Point)?
                .globals_of(index),
            RenderSource::Linear { image, settings } => self
                .evaluation(Linear::new(image, settings)?, SpatialMode::Point)?
                .globals_of(index),
        }
    }

    /// The source this render reads.
    pub(crate) fn source(&self) -> RenderSource<'a> {
        self.source
    }

    /// This compilation evaluated in `domain`, answering its spatial segments as `mode` says.
    fn evaluation<D: PixelDomain>(
        &self,
        domain: D,
        mode: SpatialMode,
    ) -> Result<Evaluation<'_, D>, Error> {
        Evaluation::new(
            domain,
            Cow::Borrowed(&self.compiled),
            self.options.tile,
            mode,
            &self.options.cancel,
            self.context,
        )
    }

    fn sample_in<D: PixelDomain>(
        &self,
        domain: D,
        x: u32,
        y: u32,
    ) -> Result<Option<[u8; 4]>, Error> {
        let (width, height) = self.stage();
        domain.check_output(width, height)?;
        self.evaluation(domain, SpatialMode::Point)?.terminal(x, y)
    }

    fn grid_in<D: PixelDomain>(
        &self,
        domain: D,
        side: u32,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<[u8; 4]>, Error> {
        let (width, height) = self.stage();
        domain.check_output(width, height)?;
        let evaluation = self.evaluation(domain, D::GRID)?;
        let outside = || Error::internal("a grid centre lies outside the stage");
        grid_centres(side, width, height)
            .into_iter()
            .map(|(x, y)| {
                checkpoint()?;
                evaluation.terminal(x, y)?.ok_or_else(outside)
            })
            .collect()
    }
}

/// Whether two compilations of one stack at two stages have the same segments with the same kinds
/// of entry, which is what lets a windowed proxy ask the exact compilation for the estimates of
/// the spatial operation at the same index.
fn same_segments(left: &Compiled, right: &Compiled) -> bool {
    left.segments.len() == right.segments.len()
        && left
            .segments
            .iter()
            .zip(&right.segments)
            .all(|(left, right)| {
                matches!(
                    (&left.entry, &right.entry),
                    (None, None)
                        | (
                            Some(super::Entry::Resample(_)),
                            Some(super::Entry::Resample(_))
                        )
                        | (
                            Some(super::Entry::Spatial { .. }),
                            Some(super::Entry::Spatial { .. })
                        )
                )
            })
}

/// The input of one layer of `recipe` as a point query over the stage that layer receives: the
/// prefix before it, compiled once, answering any number of pixels in linear light.
///
/// This is the same prefix the colour-constrained brush's seed and `mask.sample-input` read,
/// through `StageContext::sample_before`; the mask table and the stroke store travel with it for
/// the same reason they do there — a prefix layer may itself be masked, and dropping them would
/// make a valid stack look as if it named a mask that does not exist.
///
/// **A prefix holding a spatial layer is refused by name, before an evaluation exists.** One point
/// query through such a layer is the declared exception to [performance rule
/// 4](../../docs/engineering/performance-rules.md#rules) — it evaluates the stage-aligned tiles
/// its pixel needs, plus the operation's halo, each once per query. The caller here asks per
/// display cell over the whole stage, which would evaluate every tile of it on every overlay, so
/// it is refused rather than paid: the check is the prefix's own compilation, which is
/// `O(layers)` and allocates no frame, and the evaluation reuses that compilation.
pub(crate) fn layer_input<'a>(
    registry: &ModuleRegistry,
    source: RenderSource<'a>,
    recipe: &Recipe,
    layer: usize,
    context: &'a RenderContext,
) -> Result<LayerInput<'a>, Error> {
    let layers = crate::editor::prefix(&recipe.layers, layer)?;
    let (width, height) = source.dimensions();
    let compiled = registry.compile_layers(
        width,
        height,
        layers,
        &recipe.masks,
        &recipe.strokes,
        &recipe.artifacts,
    )?;
    if compiled.evaluates_spatial() {
        return Err(Error::resource_limit(format!(
            "a spatial layer before the masked one means reading the pixel it receives \
                 evaluates a {PRODUCTION_TILE} px tile per grid cell"
        )));
    }
    match source {
        RenderSource::Byte(image) => {
            check_source(image)?;
            Ok(LayerInput::Byte(point(Byte(image), compiled, context)?))
        }
        RenderSource::Linear { image, settings } => Ok(LayerInput::Linear(point(
            Linear::new(image, settings)?,
            compiled,
            context,
        )?)),
    }
}

/// A point query of `compiled` in `domain`: this prefix is read one pixel per display cell, or once
/// for a stroke's colour seed, never as a whole frame. A prefix holding a spatial layer is refused
/// before this, so the mode changes nothing admissible; it is named for what the read is.
fn point<'a, D: PixelDomain>(
    domain: D,
    compiled: Compiled,
    context: &'a RenderContext,
) -> Result<Evaluation<'a, D>, Error> {
    Evaluation::new(
        domain,
        Cow::Owned(compiled),
        PRODUCTION_TILE,
        SpatialMode::Point,
        &Cancel::never(),
        context,
    )
}
