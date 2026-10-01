//! The one way into rendering.
//!
//! [`render`] compiles a recipe against one source for one phase and binds it to the
//! [`RenderContext`] whose budgets and estimate store it reads. What it returns, a [`Render`],
//! answers everything a caller asks of an evaluated stack from that one compilation: the whole
//! frame, one pixel, a grid of pixels, the output stage and its geometry. Every export, preview
//! phase, analysis, sample and draft evaluation enters here, whichever interpretation the source has:
//! the service's [`crate::Evaluation`] compiles its stack once on the catalog owner and renders that
//! compilation through [`Render::shared`], which refuses what [`render`] refuses of the source.
//! The source's interpretation picks the pixel domain ([`super::Byte`] or [`linear::Linear`]); the
//! one pipeline ([`super::pipeline`]) evaluates either, and each domain's driver
//! ([`super::rasterize`] or [`linear::rasterize`]) writes its frame.

use super::{
    Byte, Compiled, Evaluation, GeometryMap, PixelDomain, Raster, Sample, SpatialMode, StageSize,
    check_source,
    linear::{self, Linear, LinearSettings},
    rasterize,
    spatial::Tiling,
    transform_of,
    window::{RegionFallback, WindowPlan},
};
use crate::{
    Cancel, Error, LinearImage, ModuleRegistry, ProxyApproximation, ProxyBounds, ProxyPlan,
    ProxyWindow, Recipe, SnapshotId, SourceImage,
    analysis::{MaskInputPixel, cell_pixel},
    mask_field::MaskSampling,
    modules::{Global, Region, Stage},
};
use std::borrow::Cow;

pub(super) use super::context::RenderContext;

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
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Byte(image) => (image.width, image.height),
            Self::Linear { image, .. } => (image.width(), image.height()),
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
    pub(super) fn sampling(self) -> MaskSampling {
        match self {
            Self::Exact => MaskSampling::Point,
            Self::Proxy => MaskSampling::ThinFeature,
        }
    }
}

/// How one render is evaluated: its phase and the token its passes read.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub(crate) phase: RenderPhase,
    /// Read once per row or chunk by every rasterizing pass and once per batch of spatial tiles. A
    /// token already cancelled when a frame is asked for costs no frame.
    pub cancel: Cancel,
    /// How each spatial operation is cut into tiles: [`Tiling::Halo`] everywhere but in the tests
    /// that prove a frame and a sample do not depend on it.
    pub(crate) tiling: Tiling,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            phase: RenderPhase::Exact,
            cancel: Cancel::never(),
            tiling: Tiling::Halo,
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
    pub(crate) fn proxy(cancel: &Cancel) -> Self {
        Self {
            phase: RenderPhase::Proxy,
            cancel: cancel.clone(),
            ..Self::default()
        }
    }

    /// The same options at another spatial tile size.
    #[cfg(test)]
    pub(crate) fn with_tile(mut self, tile: u32) -> Self {
        self.tiling = Tiling::Fixed(tile);
        self
    }
}

/// One recipe compiled against one source for one phase, bound to the context its evaluations
/// read. Compiling costs `O(layers + components)` and reads no pixel; everything after it reuses
/// that one compilation.
pub struct Render<'a> {
    pub(super) source: RenderSource<'a>,
    /// Owned when this render compiled it, borrowed when it renders a compilation an
    /// [`crate::Evaluation`] already holds.
    pub(super) compiled: Cow<'a, Compiled>,
    pub(super) options: RenderOptions,
    pub(super) context: &'a RenderContext,
}

/// A job's proxy stage, planned by [`Render::proxy_window`] and rendered by
/// [`Render::render_proxy`]: the stack's one compilation at the proxy stage, and the window of it
/// the proxy source holds, so the render reuses what the plan compiled and walked.
pub(crate) struct ProxyStage {
    plan: ProxyPlan,
    /// The compilation, or why the stack does not compile at the proxy stage, which the render
    /// reports as the proxy phase's reason.
    compiled: Result<Compiled, Error>,
    /// The window walk over `compiled`, when the plan has a window.
    windows: Option<WindowPlan>,
}

impl ProxyStage {
    /// The plan the proxy source is built to and cached under: the fitted plan, with the window
    /// the stack reads when it reads less than the whole stage.
    pub(crate) fn plan(&self) -> ProxyPlan {
        self.plan
    }
}

/// At most 32 MiB of returned RGBA8 pixels in one viewport frame. Intermediate/source windows
/// retain their existing, named frame and scratch limits; spatial halos can exceed this bound.
const REGION_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// A region raster's coordinates in its whole evaluated stage, and that stage's relationship to
/// the original full-resolution output. The raster is only the rectangle: it cannot be reduced
/// into a full-image report by this API.
#[derive(Debug)]
pub struct RegionFrame {
    pub raster: Raster,
    pub rect: Region,
    pub stage: StageSize,
    pub full_rect: Region,
    pub full_stage: StageSize,
    pub approximation: ProxyApproximation,
}

pub(crate) enum RegionRenderOutcome {
    Rendered(RegionFrame),
    Declined(RegionFallback),
}

/// A half-detail viewport proxy source and the part of its whole scaled output stage to produce.
/// The `proxy` plan, including its source window, is the existing one-entry cache's key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProxyRegionPlan {
    pub proxy: ProxyPlan,
    pub output: Region,
    pub stage: StageSize,
    pub full_rect: Region,
    pub full_stage: StageSize,
}

fn clipped(requested: Region, stage: StageSize) -> Option<Region> {
    let x0 = requested.x0.min(stage.width);
    let y0 = requested.y0.min(stage.height);
    let x1 = requested.x1().min(stage.width);
    let y1 = requested.y1().min(stage.height);
    (x1 > x0 && y1 > y0).then_some(Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

fn admitted(rect: Region) -> bool {
    rect.pixels() * 4 <= REGION_FRAME_BYTES
}

fn scaled_rect(rect: Region, from: StageSize, to: StageSize) -> Region {
    let floor =
        |value: u32, a: u32, b: u32| (u64::from(value) * u64::from(b) / u64::from(a)) as u32;
    let ceil = |value: u32, a: u32, b: u32| {
        (u64::from(value) * u64::from(b)).div_ceil(u64::from(a)) as u32
    };
    let x0 = floor(rect.x0, from.width, to.width);
    let y0 = floor(rect.y0, from.height, to.height);
    let x1 = ceil(rect.x1(), from.width, to.width).min(to.width);
    let y1 = ceil(rect.y1(), from.height, to.height).min(to.height);
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

enum RegionSource {
    Byte(SourceImage),
    Linear {
        image: LinearImage,
        settings: LinearSettings,
    },
}

impl RegionSource {
    fn input(&self) -> RenderSource<'_> {
        match self {
            Self::Byte(image) => RenderSource::Byte(image),
            Self::Linear { image, settings } => RenderSource::Linear {
                image,
                settings: *settings,
            },
        }
    }
}

/// Enter rendering: compile `recipe` against `source` for the phase `options` name.
///
/// This is the one entry point. It refuses what no evaluation of the stack could accept — a source
/// buffer of the wrong length, RAW settings out of range, a stack the host cannot compile, or a
/// linear stack with more than one resample — before any pixel is read, and allocates nothing but
/// the compiled operation lists. The catalog owner may therefore call it to learn a stack's output
/// stage. A truncated preview job compiles its layer prefix here; a whole stack is compiled once by
/// its [`crate::Evaluation`], before either phase of its job.
pub fn render<'a>(
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    recipe: &Recipe,
    options: RenderOptions,
    context: &'a RenderContext,
) -> Result<Render<'a>, Error> {
    let source = source.into();
    if let RenderSource::Byte(image) = source {
        check_source(image)?;
    }
    let (width, height) = source.dimensions();
    #[cfg(test)]
    context.note_compile();
    let compiled = registry.compile_sampled(
        width,
        height,
        width,
        height,
        recipe,
        options.phase.sampling(),
    )?;
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
        Self::of(source, Cow::Owned(compiled), options, context)
    }

    /// A render of a stack an [`crate::Evaluation`] compiled once against `source`'s dimensions at
    /// the exact phase, borrowed rather than compiled again. It refuses what [`render`] refuses of
    /// the source, a byte buffer of the wrong length, before anything else.
    pub(crate) fn shared(
        source: RenderSource<'a>,
        compiled: &'a Compiled,
        options: RenderOptions,
        context: &'a RenderContext,
    ) -> Result<Self, Error> {
        if let RenderSource::Byte(image) = source {
            check_source(image)?;
        }
        Self::of(source, Cow::Borrowed(compiled), options, context)
    }

    fn of(
        source: RenderSource<'a>,
        compiled: Cow<'a, Compiled>,
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

    /// The output stage's dimensions.
    pub(crate) fn stage(&self) -> (u32, u32) {
        let stage = self.compiled.stage();
        (stage.width, stage.height)
    }

    /// The whole frame, stamped with `snapshot_id`. A cancelled token answers
    /// [`crate::ErrorKind::Cancelled`] and never a partial frame, with every reservation released.
    pub fn frame(&self, snapshot_id: SnapshotId) -> Result<Raster, Error> {
        let cancel = &self.options.cancel;
        match self.source {
            RenderSource::Byte(image) => rasterize(
                image,
                &self.compiled,
                snapshot_id,
                cancel,
                self.options.tiling,
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

    /// Render exactly the requested, clipped full-output-stage rectangle. Its global spatial
    /// estimates are the same whole-stage estimates as `frame()`; only pixel production is cut.
    /// Unsupported stacks answer a named fallback, never an apparently exact partial image.
    pub(crate) fn region(
        &self,
        snapshot_id: SnapshotId,
        requested: Region,
    ) -> Result<RegionRenderOutcome, Error> {
        let (width, height) = self.stage();
        let stage = StageSize { width, height };
        let Some(rect) = clipped(requested, stage) else {
            return Ok(RegionRenderOutcome::Declined(RegionFallback::Empty));
        };
        if !admitted(rect) {
            return Ok(RegionRenderOutcome::Declined(RegionFallback::TooLarge));
        }
        let source_size = self.source.dimensions();
        let windows = match WindowPlan::of_rect(&self.compiled, source_size, rect) {
            Ok(windows) => windows,
            Err(reason) => return Ok(RegionRenderOutcome::Declined(reason)),
        };
        self.options.cancel.check()?;
        let source = match self.source {
            RenderSource::Byte(image) => RegionSource::Byte(
                if windows.source
                    == Region::whole(Stage {
                        width: image.width,
                        height: image.height,
                    })
                {
                    image.clone()
                } else {
                    image.window(windows.source, &self.options.cancel)?
                },
            ),
            RenderSource::Linear { image, settings } => RegionSource::Linear {
                image: image.window(windows.source)?,
                settings,
            },
        };
        let compiled = windows.apply(Compiled::clone(&self.compiled), source_size, |index| {
            self.spatial_globals(index)
        })?;
        let raster =
            Render::compiled(source.input(), compiled, self.options.clone(), self.context)?
                .frame(snapshot_id)?;
        Ok(RegionRenderOutcome::Rendered(RegionFrame {
            raster,
            rect,
            stage,
            full_rect: rect,
            full_stage: stage,
            approximation: ProxyApproximation::default(),
        }))
    }

    /// Plan the first moving viewport phase against a source stage roughly half the exact size on
    /// each side. The returned source window participates in the existing one-entry proxy key.
    pub(crate) fn plan_proxy_region(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        requested: Region,
    ) -> Result<ProxyRegionPlan, RegionFallback> {
        let (width, height) = self.stage();
        let full_stage = StageSize { width, height };
        let full_rect = clipped(requested, full_stage).ok_or(RegionFallback::Empty)?;
        registry
            .proxy_eligible(recipe)
            .map_err(|_| RegionFallback::ProxyIneligible)?;
        // A viewport builds only its source window. Applying ProxyBounds::clamped here would
        // silently lower a 60 MP half-stage again because its *uncut* area exceeds 8 MP.
        let (source_width, source_height) = self.source.dimensions();
        let proxy = ProxyPlan {
            width: source_width.div_ceil(2),
            height: source_height.div_ceil(2),
            bounds: ProxyBounds {
                width: width.div_ceil(2),
                height: height.div_ceil(2),
            },
            window: None,
        };
        if (proxy.width, proxy.height) == (source_width, source_height) {
            return Err(RegionFallback::ProxyUnavailable);
        }
        let compiled = registry
            .compile_sampled(
                proxy.width,
                proxy.height,
                source_width,
                source_height,
                recipe,
                RenderPhase::Proxy.sampling(),
            )
            .map_err(|_| RegionFallback::ProxyCompileFailed)?;
        if !same_segments(&compiled, &self.compiled) {
            return Err(RegionFallback::SegmentMismatch);
        }
        let output_stage = compiled.stage();
        let stage = StageSize {
            width: output_stage.width,
            height: output_stage.height,
        };
        let output = scaled_rect(full_rect, full_stage, stage);
        if !admitted(output) {
            return Err(RegionFallback::TooLarge);
        }
        let windows = WindowPlan::of_rect(&compiled, (proxy.width, proxy.height), output)?;
        let source = windows.source;
        let proxy = ProxyPlan {
            window: (source.x0 != 0
                || source.y0 != 0
                || source.width != proxy.width
                || source.height != proxy.height)
                .then_some(ProxyWindow {
                    x: source.x0,
                    y: source.y0,
                    width: source.width,
                    height: source.height,
                }),
            ..proxy
        };
        Ok(ProxyRegionPlan {
            proxy,
            output,
            stage,
            full_rect,
            full_stage,
        })
    }

    /// Render a planned half-detail viewport from the worker's cached/built source proxy. The
    /// whole exact `Render` supplies pan-independent exact global estimates when needed.
    #[cfg(test)]
    pub(crate) fn render_proxy_region(
        &self,
        registry: &ModuleRegistry,
        source: RenderSource<'_>,
        recipe: &Recipe,
        plan: ProxyRegionPlan,
        snapshot_id: SnapshotId,
        context: &RenderContext,
    ) -> Result<RegionRenderOutcome, Error> {
        self.options.cancel.check()?;
        if source.dimensions() != plan.proxy.source_dimensions() {
            return Ok(RegionRenderOutcome::Declined(
                RegionFallback::SegmentMismatch,
            ));
        }
        let compiled = registry.compile_sampled(
            plan.proxy.width,
            plan.proxy.height,
            self.source.dimensions().0,
            self.source.dimensions().1,
            recipe,
            RenderPhase::Proxy.sampling(),
        )?;
        if !same_segments(&compiled, &self.compiled) {
            return Ok(RegionRenderOutcome::Declined(
                RegionFallback::SegmentMismatch,
            ));
        }
        let windows = match WindowPlan::of_rect(
            &compiled,
            (plan.proxy.width, plan.proxy.height),
            plan.output,
        ) {
            Ok(windows) => windows,
            Err(reason) => return Ok(RegionRenderOutcome::Declined(reason)),
        };
        let expected = plan.proxy.window.map_or(
            Region::whole(Stage {
                width: plan.proxy.width,
                height: plan.proxy.height,
            }),
            |window| Region {
                x0: window.x,
                y0: window.y,
                width: window.width,
                height: window.height,
            },
        );
        if windows.source != expected {
            return Ok(RegionRenderOutcome::Declined(
                RegionFallback::SegmentMismatch,
            ));
        }
        let compiled = windows.apply(compiled, (plan.proxy.width, plan.proxy.height), |index| {
            self.spatial_globals(index)
        })?;
        let approximation = {
            let mut approximation = compiled.approximation();
            approximation.reduced_detail = true;
            approximation
        };
        let raster = Render::compiled(
            source,
            compiled,
            RenderOptions::proxy(&self.options.cancel),
            context,
        )?
        .frame(snapshot_id)?;
        Ok(RegionRenderOutcome::Rendered(RegionFrame {
            raster,
            rect: plan.output,
            stage: plan.stage,
            full_rect: plan.full_rect,
            full_stage: plan.full_stage,
            approximation,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_proxy_region_cached(
        &self,
        registry: &ModuleRegistry,
        source: RenderSource<'_>,
        recipe: &Recipe,
        plan: ProxyRegionPlan,
        snapshot_id: SnapshotId,
        context: &RenderContext,
        key: &crate::ProxyKey,
        cache: &mut super::RestorationPrefixCache,
    ) -> Result<(RegionRenderOutcome, Option<super::PrefixUse>), Error> {
        let result = self.proxy_region_cached(
            registry,
            source,
            recipe,
            plan,
            snapshot_id,
            context,
            key,
            cache,
        );
        if !matches!(&result, Ok((RegionRenderOutcome::Rendered(_), _))) {
            cache.clear();
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn proxy_region_cached(
        &self,
        registry: &ModuleRegistry,
        source: RenderSource<'_>,
        recipe: &Recipe,
        plan: ProxyRegionPlan,
        snapshot_id: SnapshotId,
        context: &RenderContext,
        key: &crate::ProxyKey,
        cache: &mut super::RestorationPrefixCache,
    ) -> Result<(RegionRenderOutcome, Option<super::PrefixUse>), Error> {
        self.options.cancel.check()?;
        if source.dimensions() != plan.proxy.source_dimensions() {
            return Ok((
                RegionRenderOutcome::Declined(RegionFallback::SegmentMismatch),
                None,
            ));
        }
        let compiled = registry.compile_sampled(
            plan.proxy.width,
            plan.proxy.height,
            self.source.dimensions().0,
            self.source.dimensions().1,
            recipe,
            RenderPhase::Proxy.sampling(),
        )?;
        if !same_segments(&compiled, &self.compiled) {
            return Ok((
                RegionRenderOutcome::Declined(RegionFallback::SegmentMismatch),
                None,
            ));
        }
        let windows = match WindowPlan::of_rect(
            &compiled,
            (plan.proxy.width, plan.proxy.height),
            plan.output,
        ) {
            Ok(windows) => windows,
            Err(reason) => return Ok((RegionRenderOutcome::Declined(reason), None)),
        };
        let expected = plan.proxy.window.map_or(
            Region::whole(Stage {
                width: plan.proxy.width,
                height: plan.proxy.height,
            }),
            |window| Region {
                x0: window.x,
                y0: window.y,
                width: window.width,
                height: window.height,
            },
        );
        if windows.source != expected {
            return Ok((
                RegionRenderOutcome::Declined(RegionFallback::SegmentMismatch),
                None,
            ));
        }
        let compiled = windows.apply(compiled, (plan.proxy.width, plan.proxy.height), |index| {
            self.spatial_globals(index)
        })?;
        let approximation = {
            let mut approximation = compiled.approximation();
            approximation.reduced_detail = true;
            approximation
        };
        let (raster, prefix_use) = Render::compiled(
            source,
            compiled,
            RenderOptions::proxy(&self.options.cancel),
            context,
        )?
        .frame_with_restoration_cache(snapshot_id, registry, recipe, key, cache)?;
        Ok((
            RegionRenderOutcome::Rendered(RegionFrame {
                raster,
                rect: plan.output,
                stage: plan.stage,
                full_rect: plan.full_rect,
                full_stage: plan.full_stage,
                approximation,
            }),
            prefix_use,
        ))
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
    /// through one point evaluation, so each equals the rendered byte there: `O(side² × layers)`,
    /// with each spatial segment answered through one tile cache on both pixel domains, so the
    /// points evaluate only the tiles they fall in and allocate no frame. `checkpoint` is asked
    /// before each point.
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

    /// The whole geometry tail as one bounded map between the content and output stages, from
    /// this compilation: `O(layers)`, no pixel read.
    pub(crate) fn transform(&self) -> Result<GeometryMap, Error> {
        let (width, height) = self.source.dimensions();
        transform_of(&self.compiled, width, height)
    }

    /// Why this render's frame is an approximation of the exact render at its size, read from the
    /// compilation the frame itself uses, so what is reported and what is drawn cannot disagree:
    /// a spatial operation, whose neighbourhoods scale with the stage, and a mask the proxy phase
    /// supersampled. `O(layers + components)`, no pixel read.
    pub(crate) fn settles_from_exact(&self) -> bool {
        self.compiled.settles_from_exact()
    }

    pub(crate) fn approximation(&self) -> ProxyApproximation {
        self.compiled.approximation()
    }

    /// The proxy of this render's source that fits `bounds`, planned from this compilation's
    /// output stage, or `None` when no proxy strictly smaller than the source would fit
    /// (`ProxyPlan::fit`).
    pub fn proxy_plan(&self, bounds: ProxyBounds) -> Option<ProxyPlan> {
        ProxyPlan::fit(self.source.dimensions(), self.stage(), bounds)
    }

    /// The proxy stage of this render's stack for `plan`, fitted from this render's output stage
    /// ([`Self::proxy_plan`]): the stack compiled once at the proxy stage, and the plan with the
    /// window of that stage the stack reads, when it reads less than all of it — what a crop reads
    /// through every boundary before it, plus the margins each boundary needs ([`super::window`]).
    /// The plan is the whole stage — a whole-stage proxy, which is always a correct answer — when
    /// the stack reads the whole stage, cannot be cut, or does not compile at the proxy stage into
    /// the segments it compiles to here. [`Self::render_proxy`] renders this compilation, so a job
    /// compiles its stack once at the proxy stage. `O(layers)`, and reads no pixel.
    pub(crate) fn proxy_window(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        plan: ProxyPlan,
    ) -> ProxyStage {
        #[cfg(test)]
        self.context.note_compile();
        let compiled = registry.compile_sampled(
            plan.width,
            plan.height,
            self.source.dimensions().0,
            self.source.dimensions().1,
            recipe,
            RenderPhase::Proxy.sampling(),
        );
        let windows = compiled
            .as_ref()
            .ok()
            .filter(|compiled| same_segments(compiled, &self.compiled))
            .and_then(|compiled| WindowPlan::of(compiled, (plan.width, plan.height)));
        let plan = match &windows {
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
        };
        ProxyStage {
            plan,
            compiled,
            windows,
        }
    }

    /// The proxy phase of this render's stack: `stage`'s compilation, from [`Self::proxy_window`],
    /// against `source`, the proxy source its plan built, under `cancel`. Without a window this is
    /// that compilation rendered at the proxy phase. With one, it is cut to the window
    /// ([`super::window`]), and a spatial operation whose stage the window cuts is handed the
    /// global estimates this render — the exact phase of the same job — resolves for it, which the
    /// store then holds for the exact frame. Compiles nothing: `O(layers)` and no pixel, besides
    /// the exact stage's one reduction per estimate the store does not hold.
    pub(crate) fn render_proxy<'s>(
        &self,
        source: RenderSource<'s>,
        stage: ProxyStage,
        cancel: &Cancel,
        context: &'s RenderContext,
    ) -> Result<Render<'s>, Error> {
        let ProxyStage {
            plan,
            compiled,
            windows,
        } = stage;
        let compiled = compiled?;
        if let RenderSource::Byte(image) = source {
            check_source(image)?;
        }
        if source.dimensions() != plan.source_dimensions() {
            return Err(Error::internal(format!(
                "a proxy source of {}x{} was handed a {}x{} source",
                plan.source_dimensions().0,
                plan.source_dimensions().1,
                source.dimensions().0,
                source.dimensions().1
            )));
        }
        let compiled = match windows {
            None => compiled,
            Some(windows) => windows.apply(compiled, (plan.width, plan.height), |index| {
                // A cold estimate belongs to the proxy phase: superseding the exact phase must not
                // cancel a proxy that can still be presented during an interactive sequence.
                self.spatial_globals_with_cancel(index, cancel)
            })?,
        };
        Render::compiled(source, compiled, RenderOptions::proxy(cancel), context)
    }

    /// The global estimates the spatial operation entering segment `index` reads, resolved as a
    /// frame of this render resolves them: from the store, or from one reduction of that
    /// operation's whole input stage, which the store then keeps for the frame. A point
    /// evaluation, so no frame is materialized for it.
    pub(crate) fn spatial_globals(&self, index: usize) -> Result<Vec<Option<Global>>, Error> {
        self.spatial_globals_with_cancel(index, &self.options.cancel)
    }

    /// Resolve a proxy's exact-stage estimate under the proxy token, independently of the
    /// exact frame's cancellation token, while reusing this render's one compilation.
    fn spatial_globals_with_cancel(
        &self,
        index: usize,
        cancel: &Cancel,
    ) -> Result<Vec<Option<Global>>, Error> {
        match self.source {
            RenderSource::Byte(image) => Evaluation::new(
                Byte(image),
                Cow::Borrowed(&*self.compiled),
                self.options.tiling,
                SpatialMode::Point,
                cancel,
                self.context,
            )?
            .globals_of(index),
            RenderSource::Linear { image, settings } => Evaluation::new(
                Linear::new(image, settings)?,
                Cow::Borrowed(&*self.compiled),
                self.options.tiling,
                SpatialMode::Point,
                cancel,
                self.context,
            )?
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
            Cow::Borrowed(&*self.compiled),
            self.options.tiling,
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
        let evaluation = self.evaluation(domain, SpatialMode::Point)?;
        let outside = || Error::internal("a grid centre lies outside the stage");
        grid_centres(side, width, height)
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
                left.entry.as_ref().map(std::mem::discriminant)
                    == right.entry.as_ref().map(std::mem::discriminant)
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
/// 4](../../../../docs/engineering/performance-rules.md#rules) — it evaluates the stage-aligned tiles
/// its pixel needs, plus the operation's halo, each once per query. The caller here asks per
/// display cell over the whole stage, which would evaluate every tile of it on every overlay, so
/// it is refused rather than paid: the check is the prefix's own compilation, which is
/// `O(layers)` and allocates no frame, and the evaluation reuses that compilation.
///
/// The answer is the stage the layer receives, which is the stage a mask bound to it is compiled
/// against, and the point query over it, in the domain the render's masked primitives blend in.
/// A colour layer receives the final run's unclamped `f32` input, before JPEG encoding; a spatial
/// layer receives the decoded, encoded boundary selected by the whole recipe. The overlay,
/// colour-constrained seed and `mask.sample-input` read the same value in either domain.
pub(crate) fn layer_input<'a>(
    registry: &ModuleRegistry,
    source: RenderSource<'a>,
    recipe: &Recipe,
    layer: usize,
    context: &'a RenderContext,
) -> Result<(Stage, Box<dyn MaskInputPixel + 'a>), Error> {
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
    let mode = MaskInputMode::for_layer(registry, recipe, layer);
    if compiled.evaluates_spatial() {
        return Err(Error::resource_limit(
            "a spatial layer before the masked one means reading the pixel it receives \
             evaluates a spatial tile per grid cell",
        ));
    }
    match source {
        RenderSource::Byte(image) => {
            check_source(image)?;
            point(Byte(image), compiled, context, mode)
        }
        RenderSource::Linear { image, settings } => {
            linear::check_resamples(&compiled)?;
            point(Linear::new(image, settings)?, compiled, context, mode)
        }
    }
}

/// A layer's input is one point evaluation of its prefix in either domain, read in linear light.
impl<D: PixelDomain> MaskInputPixel for Evaluation<'_, D> {
    fn linear(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        Ok(self.pixel(x, y)?.map(D::linear))
    }
}

/// The centres of the cells of a `side` × `side` grid over a `width` × `height` stage, row by row
/// from the top-left: each cell's [`cell_pixel`], the clipping and coverage overlays' own cell
/// rule, on both axes.
pub(super) fn grid_centres(side: u32, width: u32, height: u32) -> impl Iterator<Item = (u32, u32)> {
    (0..side).flat_map(move |row| {
        (0..side).map(move |column| {
            (
                cell_pixel(column, width, side),
                cell_pixel(row, height, side),
            )
        })
    })
}

/// A point query of `compiled` in `domain`: this prefix is read one pixel per display cell, or once
/// for a stroke's colour seed, never as a whole frame. A prefix holding a spatial layer is refused
/// before this, so the mode changes nothing admissible; it is named for what the read is.
fn point<'a, D: PixelDomain + 'a>(
    domain: D,
    compiled: Compiled,
    context: &'a RenderContext,
    mode: MaskInputMode,
) -> Result<(Stage, Box<dyn MaskInputPixel + 'a>), Error> {
    let evaluation = Evaluation::new(
        domain,
        Cow::Owned(compiled),
        Tiling::Halo,
        SpatialMode::Point,
        &Cancel::never(),
        context,
    )?;
    Ok((
        evaluation.stage(),
        Box::new(InputPixels { evaluation, mode }),
    ))
}

/// A spatial operation receives an encoded boundary; a pointwise colour operation receives the
/// float value inside its run, including extended values that terminal JPEG encoding clamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MaskInputMode {
    Boundary,
    ColourRun,
}
impl MaskInputMode {
    pub(crate) fn for_layer(registry: &ModuleRegistry, recipe: &Recipe, layer: usize) -> Self {
        match recipe
            .layers
            .get(layer)
            .and_then(|layer| registry.effect_stage(&layer.effect_id))
        {
            Some(crate::EffectStage::Color) => Self::ColourRun,
            _ => Self::Boundary,
        }
    }
}

struct InputPixels<'a, D: PixelDomain> {
    evaluation: Evaluation<'a, D>,
    mode: MaskInputMode,
}
impl<D: PixelDomain> MaskInputPixel for InputPixels<'_, D> {
    fn linear(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        match self.mode {
            MaskInputMode::Boundary => self.evaluation.linear(x, y),
            MaskInputMode::ColourRun => self.evaluation.colour_input(x, y),
        }
    }
}

/// One prefix point evaluation retained for a complete query, including all spatial tile caches.
pub(crate) trait StagePixels: MaskInputPixel {
    fn rgba(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error>;
}

impl<D: PixelDomain> StagePixels for Evaluation<'_, D> {
    fn rgba(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.pixel(x, y)?
            .map(|pixel| D::terminal(D::narrow(pixel)))
            .transpose()
    }
}

impl<D: PixelDomain> StagePixels for InputPixels<'_, D> {
    fn rgba(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.evaluation.rgba(x, y)
    }
}

pub(crate) fn prefix_pixels<'a>(
    source: RenderSource<'a>,
    compiled: Compiled,
    context: &'a RenderContext,
    cancel: &Cancel,
    wide: bool,
    mode: MaskInputMode,
) -> Result<Box<dyn StagePixels + 'a>, Error> {
    fn in_domain<'a, D: PixelDomain + 'a>(
        domain: D,
        compiled: Compiled,
        context: &'a RenderContext,
        cancel: &Cancel,
        wide: bool,
        mode: MaskInputMode,
    ) -> Result<Box<dyn StagePixels + 'a>, Error> {
        Ok(Box::new(InputPixels {
            evaluation: Evaluation::new(
                domain,
                Cow::Owned(compiled),
                Tiling::Halo,
                SpatialMode::Point,
                cancel,
                context,
            )?
            .with_input_width(wide),
            mode,
        }))
    }
    match source {
        RenderSource::Byte(image) => {
            check_source(image)?;
            in_domain(Byte(image), compiled, context, cancel, wide, mode)
        }
        RenderSource::Linear { image, settings } => in_domain(
            Linear::new(image, settings)?,
            compiled,
            context,
            cancel,
            wide,
            mode,
        ),
    }
}
