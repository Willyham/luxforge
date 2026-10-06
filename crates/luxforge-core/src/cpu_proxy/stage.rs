//! A proxy's stage: whether a stack takes a proxy at all, the whole proxy stage it is planned at,
//! the stack compiled once against it, and that compilation rendered against the proxy source.

use super::ProxyApproximation;
use crate::{
    Cancel, EffectStage, Error, ModuleRegistry, ProxyBounds, ProxyPlan, Recipe, Render,
    RenderContext, RenderOptions, RenderSource,
    mask_field::MaskSampling,
    render::{Compiled, check_source},
};

/// A job's proxy stage, planned by [`Render::proxy_stage`] and rendered by
/// [`Render::render_proxy`]: the stack's one compilation at the whole proxy stage, so the render
/// reuses what the plan compiled.
pub(crate) struct ProxyStage {
    plan: ProxyPlan,
    /// The compilation, or why the stack does not compile at the proxy stage, which the render
    /// reports as the proxy phase's reason.
    compiled: Result<Compiled, Error>,
}

impl ProxyStage {
    /// The plan the proxy source is built to and cached under: the whole proxy stage.
    pub(crate) fn plan(&self) -> ProxyPlan {
        self.plan
    }
}

impl ProxyPlan {
    /// [`Self::whole`] over a `source`-sized source, its scale lowered when need be so the whole
    /// stage holds at most [`ProxyBounds::MAX_PIXELS`]: the stage a CPU proxy renders, which is
    /// always whole. A tight crop fits a small output into the bounds, which raises the scale
    /// towards one; the whole stage then stays display-sized and the crop's part of it is drawn
    /// magnified, sharp again in the reference's frame at rest. `O(1)`.
    pub(crate) fn whole_within(self, source: (u32, u32)) -> Self {
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels <= ProxyBounds::MAX_PIXELS || source.0 == 0 || source.1 == 0 {
            return self.whole();
        }
        let scale =
            (ProxyBounds::MAX_PIXELS as f64 / (f64::from(source.0) * f64::from(source.1))).sqrt();
        let mut width = ((f64::from(source.0) * scale).floor() as u32).clamp(1, self.width);
        let mut height = ((f64::from(source.1) * scale).floor() as u32).clamp(1, self.height);
        // Flooring cannot raise the product above the limit in exact arithmetic; this settles the
        // last pixel when the square root rounded upwards.
        while u64::from(width) * u64::from(height) > ProxyBounds::MAX_PIXELS {
            if width >= height {
                width -= 1;
            } else {
                height -= 1;
            }
        }
        Self {
            width,
            height,
            bounds: self.bounds,
            window: None,
        }
    }
}

impl ModuleRegistry {
    /// Whether `recipe` may be rendered as a proxy at all.
    ///
    /// Every layer a gesture drafts is resolution independent: colour layers are pointwise, crop
    /// payloads are normalized to their own input stage and a finish unit's mask is normalized to
    /// the output stage, so the same recipe compiles unchanged against a smaller content stage and
    /// produces the same picture at display size. A spatial-stage effect is eligible too, but its
    /// neighbourhoods scale with the stage, so its proxy frame is an approximation of the exact
    /// render at display size rather than the same picture; [`Render::approximation`] says when a
    /// stack renders that way, and the exact phase still produces every number. A pixel-stage
    /// effect is not eligible: its payload addresses content pixels, which a rescaled stage no
    /// longer has. An effect no provider declares is ineligible too, because nothing can say what
    /// stage it addresses.
    ///
    /// Cost is `O(layers)` and reads no pixels. The error names the first ineligible layer's effect
    /// identity and its index, so the caller reports the reason rather than silently taking the
    /// exact path.
    pub(crate) fn proxy_eligible(&self, recipe: &Recipe) -> Result<(), Error> {
        for (index, layer) in recipe.layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(
                    EffectStage::Source
                    | EffectStage::Restoration
                    | EffectStage::Color
                    | EffectStage::Spatial
                    | EffectStage::Geometry
                    | EffectStage::Finish,
                ) => {}
                Some(EffectStage::Pixel) => {
                    return Err(Error::validation(format!(
                        "layer {index} is not proxy-eligible: effect {} is at the pixel stage, \
                         whose coordinates are content pixels and cannot be rescaled",
                        layer.effect_id
                    )));
                }
                None => {
                    return Err(Error::validation(format!(
                        "layer {index} is not proxy-eligible: no provider declares effect {}, so \
                         its stage is unknown",
                        layer.effect_id
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Compiled {
    /// Why a frame of this compilation is an approximation of the exact render at its size: a
    /// spatial operation, whose neighbourhoods scale with the stage, and a mask the proxy phase
    /// supersampled. `O(layers + components)`, no pixel read.
    pub(crate) fn approximation(&self) -> ProxyApproximation {
        ProxyApproximation {
            spatial: self.runs_spatial(EffectStage::Spatial),
            restoration: self.runs_spatial(EffectStage::Restoration),
            mask: self.supersampled_masks(),
        }
    }
}

impl<'a> Render<'a> {
    /// Why this render's frame is an approximation of the exact render at its size, read from the
    /// compilation the frame itself uses, so what is reported and what is drawn cannot disagree.
    /// `O(layers + components)`, no pixel read.
    pub(crate) fn approximation(&self) -> ProxyApproximation {
        self.compilation().approximation()
    }

    /// The proxy of this render's source that fits `bounds`, planned from this compilation's
    /// output stage, or `None` when no proxy strictly smaller than the source would fit
    /// (`ProxyPlan::fit`).
    pub fn proxy_plan(&self, bounds: ProxyBounds) -> Option<ProxyPlan> {
        ProxyPlan::fit(self.source().dimensions(), self.stage(), bounds)
    }

    /// The proxy stage of this render's stack for `plan`, fitted from this render's output stage
    /// ([`Self::proxy_plan`]): the plan over the whole proxy stage, its scale lowered so that stage
    /// holds no more than a display-sized proxy ([`ProxyPlan::whole_within`]), and the stack
    /// compiled once against it, its masks taking the thin-feature supersample.
    /// [`Self::render_proxy`] renders this compilation, so a job compiles its stack once at the
    /// proxy stage. `O(layers)`, and reads no pixel.
    pub(crate) fn proxy_stage(
        &self,
        registry: &ModuleRegistry,
        recipe: &Recipe,
        plan: ProxyPlan,
    ) -> ProxyStage {
        #[cfg(test)]
        self.render_context().note_compile();
        let (width, height) = self.source().dimensions();
        let plan = plan.whole_within((width, height));
        let compiled = registry.compile_sampled(
            plan.width,
            plan.height,
            width,
            height,
            recipe,
            MaskSampling::ThinFeature,
        );
        ProxyStage { plan, compiled }
    }

    /// The proxy phase of this render's stack: `stage`'s compilation, from [`Self::proxy_stage`],
    /// rendered at the proxy phase against `source`, the proxy source its plan built, under
    /// `cancel`. Compiles nothing: `O(layers)` and no pixel.
    pub(crate) fn render_proxy<'s>(
        &self,
        source: RenderSource<'s>,
        stage: ProxyStage,
        cancel: &Cancel,
        context: &'s RenderContext,
    ) -> Result<Render<'s>, Error> {
        let ProxyStage { plan, compiled } = stage;
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
        Render::compiled(source, compiled, RenderOptions::proxy(cancel), context)
    }
}
