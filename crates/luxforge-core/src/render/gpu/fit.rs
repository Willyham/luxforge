//! The reduced stage a GPU frame is drawn at, at Fit and at a percentage zoom below 100%
//! (`docs/design/gpu-preview.md`, "The GPU source"): the source reduced to the view's size, the
//! stack compiled against it, and the window of it the output reads, planned by the GPU's own
//! window walk ([`WindowPlan::of_gpu_rect`] from the source). Nothing here reads a pixel
//! or plans a CPU render: the photo surface derives the reduced source from the source it holds,
//! by the area average this plan's coverage names ([`ProxyPlan::coverage`]).
use super::preview::output_window;
use crate::{
    EffectStage, Error, ModuleRegistry, ProxyBounds, ProxyPlan, ProxyWindow, Recipe,
    mask_field::MaskSampling,
    modules::{Region, Stage},
    render::Compiled,
};

impl ProxyPlan {
    /// `Some(plan)` when a reduced stage strictly smaller than a `source`-sized source fits
    /// `bounds` for a recipe whose full-resolution output stage is `stage`, and `None` when the
    /// scale would be one or more, where the stage is drawn at its own size.
    ///
    /// The scale is `min(bounds.width / stage.width, bounds.height / stage.height, 1)`, so a rotated
    /// crop's output — not the source rectangle it was cut from — is what gets fitted into the
    /// bounds. Pure arithmetic: the stage comes from the compilation the caller already holds.
    pub(crate) fn fit(source: (u32, u32), stage: (u32, u32), bounds: ProxyBounds) -> Option<Self> {
        let bounds = bounds.clamped();
        let (source_width, source_height) = source;
        let (stage_width, stage_height) = stage;
        if stage_width == 0 || stage_height == 0 || source_width == 0 || source_height == 0 {
            return None;
        }
        let scale = (f64::from(bounds.width) / f64::from(stage_width))
            .min(f64::from(bounds.height) / f64::from(stage_height))
            .min(1.0);
        // A non-finite scale declines too: there is no reduced stage to describe, and the stage at
        // its own size is always a correct answer.
        if !scale.is_finite() || scale >= 1.0 {
            return None;
        }
        let width = ((f64::from(source_width) * scale).round() as u32).clamp(1, source_width);
        let height = ((f64::from(source_height) * scale).round() as u32).clamp(1, source_height);
        if width == source_width && height == source_height {
            return None;
        }
        Some(Self {
            width,
            height,
            bounds,
            window: None,
        })
    }
}

/// A stack's reduced stage at a view's bounds: the plan, with the window of the reduced stage the
/// whole output reads, and the stack compiled against the whole reduced stage, uncut.
pub(crate) struct ReducedStage {
    pub(crate) plan: ProxyPlan,
    pub(crate) compiled: Compiled,
}

/// Whether every layer of `recipe` scales with the stage it is compiled against: a pixel-stage
/// layer addresses content pixels, which no reduced stage has, and a layer whose stage is unknown
/// cannot be judged. `O(layers)`.
fn scalable(registry: &ModuleRegistry, recipe: &Recipe) -> bool {
    recipe.layers.iter().all(|layer| {
        registry
            .effect_stage(&layer.effect_id)
            .is_some_and(|stage| stage != EffectStage::Pixel)
    })
}

/// The reduced stage `recipe`, over a `source`-sized source whose full-resolution output stage is
/// `output`, is drawn at within `bounds` ([`ReducedStage`]): `None` when the output fits the bounds
/// at its own size, or a layer does not scale, where the stage is drawn at its own size. The stack
/// is compiled once against the reduced stage, its masks taking the thin-feature supersample, and
/// the window is the part of the reduced stage the whole output reads through every boundary
/// after the source, each grown by its halo alone ([`output_window`]), or the whole stage when it
/// reads all of it or cannot be cut. `O(layers + segments)`, and reads no pixel; a stack that does
/// not compile at the reduced stage is that compile's error.
pub(crate) fn reduced_stage(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    source: (u32, u32),
    output: Stage,
    bounds: ProxyBounds,
) -> Result<Option<ReducedStage>, Error> {
    if !scalable(registry, recipe) {
        return Ok(None);
    }
    let Some(plan) = ProxyPlan::fit(source, (output.width, output.height), bounds) else {
        return Ok(None);
    };
    let compiled = registry.compile_sampled(
        plan.width,
        plan.height,
        source.0,
        source.1,
        recipe,
        MaskSampling::ThinFeature,
    )?;
    let stage = Stage {
        width: plan.width,
        height: plan.height,
    };
    let window = output_window(&compiled, stage, 0).map(|window| ProxyWindow {
        x: window.x0,
        y: window.y0,
        width: window.width,
        height: window.height,
    });
    Ok(Some(ReducedStage {
        plan: ProxyPlan { window, ..plan },
        compiled,
    }))
}

/// `plan`'s window grown to start at a multiple of `anchor`, so each texel the boundary holds is
/// the whole reduced stage's, bit for bit, whatever window holds it ([`super::GpuPlan::anchor`]).
pub(crate) fn anchored_plan(plan: ProxyPlan, anchor: super::GpuAnchor) -> ProxyPlan {
    let window = plan.window.map(|window| {
        let region = super::anchored(
            Region {
                x0: window.x,
                y0: window.y,
                width: window.width,
                height: window.height,
            },
            anchor,
        );
        ProxyWindow {
            x: region.x0,
            y: region.y0,
            width: region.width,
            height: region.height,
        }
    });
    ProxyPlan { window, ..plan }
}

/// The reduced-stage plan a GPU frame of `recipe` over a `source`-sized source is drawn at within
/// `bounds`, with the window of the reduced stage the output reads ([`reduced_stage`]): `None`
/// where the output is drawn at its own size. For a test's reference, which reduces the source to
/// it as the photo surface does. `O(layers + segments)`, and reads no pixel.
pub fn gpu_fit_plan(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    source: (u32, u32),
    bounds: ProxyBounds,
) -> Result<Option<ProxyPlan>, Error> {
    let output = registry
        .compile_sampled(
            source.0,
            source.1,
            source.0,
            source.1,
            recipe,
            MaskSampling::Point,
        )?
        .stage();
    Ok(reduced_stage(registry, recipe, source, output, bounds)?.map(|stage| stage.plan))
}
