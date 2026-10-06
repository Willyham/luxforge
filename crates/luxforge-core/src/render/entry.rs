//! The one way into rendering.
//!
//! [`render`] compiles a recipe against one source for one phase and binds it to the
//! [`RenderContext`] whose budgets it reads. What it returns, a [`Render`],
//! answers everything a caller asks of an evaluated stack from that one compilation: the whole
//! frame, one pixel, a grid of pixels, the output stage and its geometry. Every export, preview
//! phase, analysis, sample and draft evaluation enters here, whichever interpretation the source has:
//! the service's [`crate::Evaluation`] compiles its stack once on the catalog owner and renders that
//! compilation through [`Render::shared`], which refuses what [`render`] refuses of the source.
//! The source's interpretation picks the pixel domain ([`super::Byte`] or [`linear::Linear`]); the
//! one pipeline ([`super::pipeline`]) evaluates either, and each domain's driver
//! ([`super::rasterize`] or [`linear::rasterize`]) writes its frame.

use super::{
    Byte, Compiled, Evaluation, GeometryMap, PixelDomain, Raster, Sample, SpatialMode,
    check_source,
    linear::{self, Linear, LinearSettings},
    rasterize,
    spatial::Tiling,
    transform_of,
};
use crate::{
    Cancel, Error, LinearImage, ModuleRegistry, Recipe, SnapshotId, SourceImage,
    analysis::{MaskInputPixel, cell_pixel},
    mask_field::MaskSampling,
    modules::{Region, Stage},
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
    /// Read once per row or chunk by every rasterizing pass and once per spatial tile. A
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

#[cfg(any(test, feature = "qualification"))]
fn clipped(requested: Region, stage: super::StageSize) -> Option<Region> {
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

    /// The input of the layer that begins at `position` — a segment and an operation index of this
    /// render's compilation — held over the window of its received stage that the output stage's
    /// `rect` reads at full scale: the visible region and every margin the boundaries after it
    /// need on the GPU ([`super::window::WindowPlan::of_gpu_rect`]). It is the reference renderer's whole frame
    /// of the stages up to the layer, every spatial operation among them reducing its own whole
    /// input, kept to that window.
    ///
    /// A stack the planner cannot cut answers its reason as an error, as a region the boundary
    /// cannot hold is no frame of it. The editor renders none: every plan starts from the source,
    /// which the photo surface cuts on the GPU; this is the reference for tests and the
    /// qualification harness.
    #[cfg(any(test, feature = "qualification"))]
    pub(crate) fn region_boundary(
        &self,
        rect: Region,
        position: (usize, usize),
        format: super::BoundaryFormat,
    ) -> Result<super::BoundaryFrame, Error> {
        let (width, height) = self.stage();
        let Some(rect) = clipped(rect, super::StageSize { width, height }) else {
            return Err(Error::validation(
                "the GPU preview's region lies outside the output stage",
            ));
        };
        let source_size = self.source.dimensions();
        let windows = super::window::WindowPlan::of_gpu_rect(&self.compiled, source_size, rect)
            .map_err(|reason| {
                Error::validation(format!(
                    "the GPU preview's region boundary: {}",
                    reason.reason()
                ))
            })?;
        self.options.cancel.check()?;
        let source = Stage {
            width: source_size.0,
            height: source_size.1,
        };
        self.boundary_kept(
            &self.compiled,
            source,
            Region::whole(source),
            position,
            format,
            Some(windows.reads(position.0)),
        )
    }

    /// [`Self::region_boundary`] over the whole output stage: the input of the layer that begins at
    /// `position`, held over the window of its received stage that the whole output reads — what
    /// a crop, a straightening and a warp read, with their taps — which a Fit frame drawn at the
    /// exact stage starts from.
    #[cfg(test)]
    pub(crate) fn output_boundary(
        &self,
        position: (usize, usize),
        format: super::BoundaryFormat,
    ) -> Result<super::BoundaryFrame, Error> {
        let (width, height) = self.stage();
        self.region_boundary(
            Region {
                x0: 0,
                y0: 0,
                width,
                height,
            },
            position,
            format,
        )
    }

    /// [`Self::region_boundary`] of layer `layer`, wherever it begins in this render's
    /// compilation: the boundary a percentage zoom's GPU preview of a drag from that layer holds.
    #[cfg(feature = "qualification")]
    pub(crate) fn layer_region_boundary(
        &self,
        rect: Region,
        layer: usize,
        format: super::BoundaryFormat,
    ) -> Result<super::BoundaryFrame, Error> {
        let position = super::gpu::position(&self.compiled, layer)
            .ok_or_else(|| Error::validation(format!("layer {layer} is past the stack")))?;
        self.region_boundary(rect, position, format)
    }

    /// One output pixel, the byte [`Self::frame`] writes there: `O(layers)` without a spatial layer,
    /// and through one the reference renderer's whole frames of the spatial segments, materialized
    /// for this one pixel, as the reference answers every read. `rgba` is `None` outside the
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

    /// One rectangle of the output stage, row by row from its top-left, read in frame mode: each
    /// spatial segment's whole frame is materialized once ([`SpatialMode::Frames`]), on the pool as
    /// a render materializes it, and every pixel of `rect` is then evaluated through those frames
    /// in `O(layers)`. Each byte is therefore the byte [`Self::sample`]
    /// answers at that pixel and [`Self::frame`] writes there: the reference renderer's read of a
    /// rectangle, which a GPU tile read is held to. `rect` must lie inside the output stage, and it
    /// answers at most the pixels one evaluated frame may hold.
    pub fn read_rect(&self, rect: Region) -> Result<Vec<[u8; 4]>, Error> {
        let (width, height) = self.stage();
        if rect.x1() > width || rect.y1() > height {
            return Err(Error::validation(format!(
                "the {}x{} rectangle at ({}, {}) is not inside the {width}x{height} output stage",
                rect.width, rect.height, rect.x0, rect.y0
            )));
        }
        // A rectangle's codes are bounded as an evaluated frame of its size is.
        Raster::expected_len(rect.width, rect.height)?;
        let mut codes = Vec::with_capacity(rect.pixels() as usize);
        let pixels = Render {
            source: self.source,
            compiled: Cow::Borrowed(&*self.compiled),
            options: self.options.clone(),
            context: self.context,
        }
        .frame_pixels()?;
        for y in rect.y0..rect.y1() {
            self.options.cancel.check()?;
            for x in rect.x0..rect.x1() {
                codes.push(
                    pixels.rgba(x, y)?.ok_or_else(|| {
                        Error::render("a pixel inside the output stage is missing")
                    })?,
                );
            }
        }
        Ok(codes)
    }

    /// The output stage held for reads in frame mode: what [`Self::read_rect`] reads, kept for a
    /// caller that reads it more than once, such as the reference tile service over one call
    /// (`crate::tiles`). Its spatial frames are materialized here, once, and each pixel read from
    /// it afterwards costs `O(layers)`. It answers [`Self::sample`]'s bytes and the output pixels'
    /// own linear values, and holds its frames until it is dropped.
    pub(crate) fn frame_pixels(self) -> Result<Box<dyn StagePixels + 'a>, Error> {
        let Render {
            source,
            compiled,
            options,
            context,
        } = self;
        match source {
            RenderSource::Byte(image) => {
                check_source(image)?;
                output_pixels(Byte(image), compiled, &options, context)
            }
            RenderSource::Linear { image, settings } => {
                output_pixels(Linear::new(image, settings)?, compiled, &options, context)
            }
        }
    }

    /// The stage this render's stack produces held in frame mode as the input of the layer after
    /// it, as [`prefix_pixels`] reads it: with the boundary width `wide` the whole recipe chooses
    /// at its last spatial segment, and the linear values that layer receives as `mode` says. Its
    /// spatial frames are materialized here, once, at those widths; each pixel read from it
    /// afterwards costs `O(layers)`.
    pub(crate) fn frame_input(
        self,
        wide: bool,
        mode: MaskInputMode,
    ) -> Result<Box<dyn StagePixels + 'a>, Error> {
        fn in_domain<'a, D: PixelDomain + 'a>(
            domain: D,
            compiled: Cow<'a, Compiled>,
            options: &RenderOptions,
            context: &'a RenderContext,
            wide: bool,
            mode: MaskInputMode,
        ) -> Result<Box<dyn StagePixels + 'a>, Error> {
            Ok(Box::new(InputPixels {
                evaluation: frame_evaluation(
                    domain,
                    compiled,
                    options.tiling,
                    Some(wide),
                    &options.cancel,
                    context,
                )?,
                mode,
            }))
        }
        let Render {
            source,
            compiled,
            options,
            context,
        } = self;
        match source {
            RenderSource::Byte(image) => {
                check_source(image)?;
                in_domain(Byte(image), compiled, &options, context, wide, mode)
            }
            RenderSource::Linear { image, settings } => in_domain(
                Linear::new(image, settings)?,
                compiled,
                &options,
                context,
                wide,
                mode,
            ),
        }
    }

    /// The output pixels at the centres of a `side` × `side` grid, row by row from the top-left,
    /// through one evaluation, so each equals the rendered byte there: `O(side² × layers)` once
    /// the reference renderer has materialized the whole frame of every spatial segment, which a
    /// stack without one has none of. `checkpoint` is asked before each point.
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

    /// The global estimates the spatial operation entering segment `index` reads, resolved as a
    /// frame of this render resolves them: the ones it was handed, or one reduction of that
    /// operation's whole input stage, for which the whole frames of the spatial segments before it
    /// are materialized; with none before it, none is.
    #[cfg(any(test, feature = "qualification"))]
    pub(crate) fn spatial_globals(
        &self,
        index: usize,
    ) -> Result<Vec<Option<crate::modules::Global>>, Error> {
        fn in_domain<D: PixelDomain>(
            render: &Render<'_>,
            domain: D,
            index: usize,
        ) -> Result<Vec<Option<crate::modules::Global>>, Error> {
            let (tiling, cancel) = (render.options.tiling, &render.options.cancel);
            if !render.compiled.spatial_before(index) {
                return Evaluation::new(
                    domain,
                    Cow::Borrowed(&*render.compiled),
                    tiling,
                    SpatialMode::Point,
                    cancel,
                    render.context,
                )?
                .globals_of(index);
            }
            Evaluation::framed(
                domain,
                Cow::Borrowed(&*render.compiled),
                tiling,
                None,
                index,
                cancel,
                render.context,
            )?
            .globals_of(index)
        }
        match self.source {
            RenderSource::Byte(image) => in_domain(self, Byte(image), index),
            RenderSource::Linear { image, settings } => {
                in_domain(self, Linear::new(image, settings)?, index)
            }
        }
    }

    /// Qualification only: hand the spatial operation entering segment `index` the estimates
    /// `globals` names, one per unit in order, in place of the ones its frames would reduce: a unit
    /// that declares no estimate key is handed none, and one past `globals` none either. Every
    /// frame and read of this render from then on reads them.
    #[cfg(feature = "qualification")]
    pub(crate) fn hold_spatial_globals(
        &mut self,
        index: usize,
        globals: &[Option<crate::modules::Global>],
    ) -> Result<(), Error> {
        let Some(super::Entry::Spatial(entry)) = self
            .compiled
            .to_mut()
            .segments
            .get_mut(index)
            .and_then(|segment| segment.entry.as_mut())
        else {
            return Err(Error::internal(format!(
                "segment {index} enters through no spatial operation"
            )));
        };
        let held = entry
            .operation
            .units()
            .iter()
            .enumerate()
            .map(|(at, unit)| {
                unit.estimate_key()
                    .and_then(|_| globals.get(at).cloned().flatten())
            })
            .collect();
        entry.globals = Some(std::sync::Arc::new(held));
        Ok(())
    }

    /// Qualification only: the first spatial unit of this render's compilation that prepares a
    /// global estimate.
    #[cfg(feature = "qualification")]
    pub(crate) fn estimating_unit(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::modules::SpatialUnit>> {
        self.compiled
            .segments
            .iter()
            .find_map(|segment| match &segment.entry {
                Some(super::Entry::Spatial(entry)) => entry
                    .operation
                    .units()
                    .iter()
                    .find(|unit| unit.estimate_key().is_some())
                    .cloned(),
                _ => None,
            })
    }

    /// Qualification only: the segment whose entry is layer `layer`'s spatial operation, when it
    /// has one in this render's compilation.
    #[cfg(feature = "qualification")]
    pub(crate) fn spatial_segment_of(&self, layer: usize) -> Option<usize> {
        let (segment, operation) = super::gpu::position(&self.compiled, layer)?;
        let next = segment + 1;
        (operation == self.compiled.segments[segment].operations.len()
            && matches!(
                self.compiled
                    .segments
                    .get(next)
                    .and_then(|s| s.entry.as_ref()),
                Some(super::Entry::Spatial(_))
            ))
        .then_some(next)
    }

    /// The source this render reads.
    pub(crate) fn source(&self) -> RenderSource<'a> {
        self.source
    }

    /// The compilation this render evaluates.
    pub(crate) fn compilation(&self) -> &Compiled {
        &self.compiled
    }

    /// The context this render reads, for a test that counts what it compiles.
    #[cfg(test)]
    pub(crate) fn render_context(&self) -> &'a RenderContext {
        self.context
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
        self.evaluation(domain, SpatialMode::Frames)?.terminal(x, y)
    }

    fn grid_in<D: PixelDomain>(
        &self,
        domain: D,
        side: u32,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<[u8; 4]>, Error> {
        let (width, height) = self.stage();
        domain.check_output(width, height)?;
        let evaluation = self.evaluation(domain, SpatialMode::Frames)?;
        let outside = || Error::internal("a grid centre lies outside the stage");
        grid_centres(side, width, height)
            .map(|(x, y)| {
                checkpoint()?;
                evaluation.terminal(x, y)?.ok_or_else(outside)
            })
            .collect()
    }
}

/// The input of one layer of `recipe` as a point query over the stage that layer receives: the
/// prefix before it, compiled once, answering any number of pixels in linear light.
///
/// This is the same prefix the colour-constrained brush's seed and `mask.sample-input` read,
/// through `StageContext::sample_before`; the mask table and the stroke store travel with it for
/// the same reason they do there — a prefix layer may itself be masked, and dropping them would
/// make a valid stack look as if it named a mask that does not exist.
///
/// **A prefix holding a spatial layer is refused by name, before an evaluation exists.** A pixel
/// read through such a layer is answered from that layer's whole frame, which the overlay, asking
/// per display cell, would render on every overlay ([performance rule
/// 4](../../../../docs/engineering/performance-rules.md#rules)), so it is refused rather than paid:
/// the check is the prefix's own compilation, which is `O(layers)` and allocates no frame, and the
/// evaluation reuses that compilation.
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

/// One stage held for reads for a complete query, with the frames of its spatial segments.
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

/// An output stage held for reads: its terminal bytes exactly as [`Render::sample`] answers them,
/// and its output pixels' own linear values.
struct OutputPixels<'a, D: PixelDomain>(Evaluation<'a, D>);

impl<D: PixelDomain> MaskInputPixel for OutputPixels<'_, D> {
    fn linear(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        self.0.linear(x, y)
    }
}

impl<D: PixelDomain> StagePixels for OutputPixels<'_, D> {
    fn rgba(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.0.terminal(x, y)
    }
}

/// [`Render::frame_pixels`] in one domain: the output stage it may produce, held in frame mode.
fn output_pixels<'a, D: PixelDomain + 'a>(
    domain: D,
    compiled: Cow<'a, Compiled>,
    options: &RenderOptions,
    context: &'a RenderContext,
) -> Result<Box<dyn StagePixels + 'a>, Error> {
    let stage = compiled.stage();
    domain.check_output(stage.width, stage.height)?;
    Ok(Box::new(OutputPixels(frame_evaluation(
        domain,
        compiled,
        options.tiling,
        None,
        &options.cancel,
        context,
    )?)))
}

/// `compiled` evaluated in `domain` in frame mode: every spatial operation's output materialized
/// once over its whole stage, in stage order, as [`SpatialMode::Frames`] materializes it. With
/// `wide`, the boundary widths are the ones the whole recipe chooses for this prefix
/// ([`Evaluation::with_input_width`]), set before any frame exists.
fn frame_evaluation<'a, D: PixelDomain>(
    domain: D,
    compiled: Cow<'a, Compiled>,
    tiling: Tiling,
    wide: Option<bool>,
    cancel: &Cancel,
    context: &'a RenderContext,
) -> Result<Evaluation<'a, D>, Error> {
    let Some(wide) = wide else {
        return Evaluation::new(
            domain,
            compiled,
            tiling,
            SpatialMode::Frames,
            cancel,
            context,
        );
    };
    let segments = compiled.segments.len();
    Evaluation::framed(
        domain,
        compiled,
        tiling,
        Some(wide),
        segments,
        cancel,
        context,
    )
}

/// The stage `compiled`, a prefix of a recipe, produces, held for reads as the layer after it
/// receives it, at the boundary width `wide` the whole recipe chooses and with the linear values
/// `mode` says: the reference renderer's read of a layer's input, its spatial frames materialized
/// here once ([`frame_evaluation`]), each pixel read from it afterwards `O(layers)`.
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
            evaluation: frame_evaluation(
                domain,
                Cow::Owned(compiled),
                Tiling::Halo,
                Some(wide),
                cancel,
                context,
            )?,
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
