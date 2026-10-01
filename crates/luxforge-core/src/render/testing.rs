//! What the crate's unit tests render through: [`super::render`], through a context the test constructs
//! when it reads that context's budgets or estimate store ([`frame_in`], [`sample_in`] and the
//! evaluations), and through a new context per call otherwise. No two tests share a context, so no
//! test's budget, high-water mark or estimate store moves with what another test renders. Every
//! helper is one call to the entry point; none is a second way to render.

use super::render as enter;
use super::{
    Byte, Evaluation, LinearSettings, Raster, RenderContext, RenderOptions, RenderSource, Sample,
    SpatialMode, linear::Linear,
};
use crate::{Cancel, Error, LinearImage, ModuleRegistry, Recipe, SnapshotId, SourceImage};
use std::borrow::Cow;

/// A real frozen Nikon profile, resolved from the committed index without using its asynchronous
/// singleton. Geometry tests can therefore change profile terms without racing index fault tests.
pub(crate) fn frozen_lens(width: u32, height: u32, focal: f64) -> crate::Layer {
    use crate::modules::lens::{index::LensIndex, payload, resolve};
    let index = LensIndex::parse(include_bytes!("../../data/lensfun/index.json")).unwrap();
    let input = resolve::ResolveInput {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z 6".into(),
        lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()),
        focal_mm: Some(focal),
        focal_35mm: None,
        focal_override: None,
        stage: crate::Stage { width, height },
    };
    let row = resolve::candidates(&index, &input)
        .into_iter()
        .find(|row| row.eligible && row.matched == resolve::Match::LensModel)
        .unwrap();
    let resolved = resolve::resolve(&index, &row.key, &input).unwrap();
    let profile = payload::Profile::from_resolution(
        resolved,
        &crate::SourceOptics::jpeg(&crate::export::CaptureMetadata::default()),
        true,
    )
    .unwrap();
    crate::Layer::new(
        crate::LENS_EFFECT,
        serde_json::to_value(payload::Payload {
            profile: Some(profile),
        })
        .unwrap(),
    )
}

/// A frame of `recipe` rendered through `context`, for a test that reads what the render left
/// in it.
pub(crate) fn frame_in<'a>(
    context: &'a RenderContext,
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    options: RenderOptions,
) -> Result<Raster, Error> {
    enter(registry, source, recipe, options, context)?.frame(snapshot_id)
}

/// The sample at (`x`, `y`) of `recipe` evaluated through `context`.
pub(crate) fn sample_in<'a>(
    context: &'a RenderContext,
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    recipe: &Recipe,
    options: RenderOptions,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    enter(registry, source, recipe, options, context)?.sample(x, y)
}

fn frame<'a>(
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    options: RenderOptions,
) -> Result<Raster, Error> {
    let source: RenderSource<'_> = source.into();
    frame_in(
        &RenderContext::new(),
        registry,
        source,
        snapshot_id,
        recipe,
        options,
    )
}

pub(crate) fn linear(source: &LinearImage, settings: LinearSettings) -> RenderSource<'_> {
    RenderSource::Linear {
        image: source,
        settings,
    }
}

pub(crate) fn render_tiled(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    cancel: &Cancel,
    tile: u32,
) -> Result<Raster, Error> {
    let options = RenderOptions::exact(cancel).with_tile(tile);
    frame(registry, source, snapshot_id, recipe, options)
}

pub(crate) fn render(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
) -> Result<Raster, Error> {
    frame(
        registry,
        source,
        snapshot_id,
        recipe,
        RenderOptions::default(),
    )
}

pub(crate) fn render_linear_tiled(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
    cancel: &Cancel,
    tile: u32,
) -> Result<Raster, Error> {
    let options = RenderOptions::exact(cancel).with_tile(tile);
    frame(
        registry,
        linear(source, settings),
        snapshot_id,
        recipe,
        options,
    )
}

pub(crate) fn render_linear_cancellable(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
    cancel: &Cancel,
) -> Result<Raster, Error> {
    frame(
        registry,
        linear(source, settings),
        snapshot_id,
        recipe,
        RenderOptions::exact(cancel),
    )
}

pub(crate) fn render_linear(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
) -> Result<Raster, Error> {
    render_linear_cancellable(
        registry,
        source,
        snapshot_id,
        recipe,
        settings,
        &Cancel::never(),
    )
}

pub(crate) fn sample(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    sample_in(
        &RenderContext::new(),
        registry,
        source,
        recipe,
        RenderOptions::default(),
        x,
        y,
    )
}

pub(crate) fn sample_linear(
    registry: &ModuleRegistry,
    source: &LinearImage,
    recipe: &Recipe,
    settings: LinearSettings,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    sample_in(
        &RenderContext::new(),
        registry,
        linear(source, settings),
        recipe,
        RenderOptions::default(),
        x,
        y,
    )
}

pub(crate) fn extents(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
) -> Result<(u32, u32), Error> {
    Ok(enter(
        registry,
        source,
        recipe,
        RenderOptions::default(),
        &RenderContext::new(),
    )?
    .stage())
}

/// A byte point evaluation of `recipe` through `context`, compiled once, for a test that asks
/// it many pixels or inspects its tile cache.
pub(crate) fn evaluation<'a>(
    context: &'a RenderContext,
    registry: &ModuleRegistry,
    source: &'a SourceImage,
    recipe: &Recipe,
) -> Result<Evaluation<'a, Byte<'a>>, Error> {
    super::check_source(source)?;
    let compiled = registry.compile(source.width, source.height, recipe)?;
    Evaluation::new(
        Byte(source),
        Cow::Owned(compiled),
        super::spatial::Tiling::Halo,
        SpatialMode::Point,
        &Cancel::never(),
        context,
    )
}

/// A linear evaluation of `recipe` through `context` in either spatial mode, for a test that
/// inspects the frames or the tiles it holds.
pub(crate) fn linear_evaluation<'a>(
    context: &'a RenderContext,
    registry: &ModuleRegistry,
    source: &'a LinearImage,
    recipe: &Recipe,
    settings: LinearSettings,
    tiling: super::spatial::Tiling,
    mode: SpatialMode,
) -> Result<Evaluation<'a, Linear<'a>>, Error> {
    let domain = Linear::new(source, settings)?;
    let compiled = registry.compile(source.width(), source.height(), recipe)?;
    Evaluation::new(
        domain,
        Cow::Owned(compiled),
        tiling,
        mode,
        &Cancel::never(),
        context,
    )
}

/// Every output byte of `recipe` over `source` from the point evaluator in `mode`, row by row,
/// with the output stage's dimensions: the reference a rendered frame is compared with. It
/// resolves the segment, the replacement that wins and the view for every pixel and applies the
/// colour runs to that pixel alone, never through the rows a frame is written from; in
/// [`SpatialMode::Point`] it is the evaluation a sample reads, with one tile cache for every pixel.
pub(crate) fn point_evaluated<'a>(
    context: &'a RenderContext,
    registry: &ModuleRegistry,
    source: impl Into<RenderSource<'a>>,
    recipe: &Recipe,
    mode: SpatialMode,
) -> Result<(u32, u32, Vec<u8>), Error> {
    fn every_pixel<D: super::PixelDomain>(
        evaluation: Evaluation<'_, D>,
    ) -> Result<(u32, u32, Vec<u8>), Error> {
        let crate::modules::Stage { width, height } = evaluation.stage();
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let pixel = evaluation
                    .terminal(x, y)?
                    .ok_or_else(|| Error::internal("a reference pixel outside the stage"))?;
                rgba.extend(pixel);
            }
        }
        Ok((width, height, rgba))
    }
    let source: RenderSource<'a> = source.into();
    let (width, height) = source.dimensions();
    let compiled = Cow::Owned(registry.compile(width, height, recipe)?);
    let tiling = super::spatial::Tiling::Halo;
    let never = Cancel::never();
    match source {
        RenderSource::Byte(image) => {
            super::check_source(image)?;
            every_pixel(Evaluation::new(
                Byte(image),
                compiled,
                tiling,
                mode,
                &never,
                context,
            )?)
        }
        RenderSource::Linear { image, settings } => every_pixel(Evaluation::new(
            Linear::new(image, settings)?,
            compiled,
            tiling,
            mode,
            &never,
            context,
        )?),
    }
}

/// The unit tests' own reading of a preview source: each method is one call to the entry point
/// through a new context, so a test compares a worker's frame against the same code.
impl crate::PreviewSource {
    pub(crate) fn render(
        &self,
        registry: &ModuleRegistry,
        snapshot_id: SnapshotId,
        recipe: &Recipe,
    ) -> Result<Raster, Error> {
        frame(
            registry,
            self,
            snapshot_id,
            recipe,
            RenderOptions::default(),
        )
    }

    pub(crate) fn render_cancellable(
        &self,
        registry: &ModuleRegistry,
        snapshot_id: SnapshotId,
        recipe: &Recipe,
        cancel: &Cancel,
    ) -> Result<Raster, Error> {
        frame(
            registry,
            self,
            snapshot_id,
            recipe,
            RenderOptions::exact(cancel),
        )
    }

    pub(crate) fn render_proxy_cancellable(
        &self,
        registry: &ModuleRegistry,
        snapshot_id: SnapshotId,
        recipe: &Recipe,
        cancel: &Cancel,
    ) -> Result<Raster, Error> {
        frame(
            registry,
            self,
            snapshot_id,
            recipe,
            RenderOptions::proxy(cancel),
        )
    }

    pub(crate) fn sample(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        x: u32,
        y: u32,
    ) -> Result<Sample, Error> {
        sample_in(
            &RenderContext::new(),
            registry,
            self,
            recipe,
            RenderOptions::default(),
            x,
            y,
        )
    }
}

impl crate::PreviewSource {
    /// The proxy plan for `recipe` over this source at `bounds`, from a compile of its
    /// dimensions alone: a test may plan over a source that holds no pixels.
    pub(crate) fn proxy_plan(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        bounds: crate::ProxyBounds,
    ) -> Result<Option<crate::ProxyPlan>, Error> {
        let (width, height) = self.dimensions();
        let stage = registry.compile(width, height, recipe)?.stage();
        Ok(crate::ProxyPlan::fit(
            (width, height),
            (stage.width, stage.height),
            bounds,
        ))
    }
}

impl ModuleRegistry {
    /// What a proxy-phase render of `recipe` over a `width` × `height` source reports, from a
    /// compile of the dimensions alone; the default when the stack does not compile there.
    pub(crate) fn proxy_approximation(
        &self,
        recipe: &Recipe,
        width: u32,
        height: u32,
    ) -> crate::ProxyApproximation {
        self.compile_sampled(
            width,
            height,
            width,
            height,
            recipe,
            crate::mask_field::MaskSampling::ThinFeature,
        )
        .map(|compiled| compiled.approximation())
        .unwrap_or_default()
    }
}
