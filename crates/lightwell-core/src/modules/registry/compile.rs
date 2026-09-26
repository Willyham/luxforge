//! Admitting and compiling a stack against the registered providers: proxy eligibility, the layer
//! and recipe checks every write passes, and the compile that folds a stack into rasterizing
//! segments with each layer's mask and bound artifacts attached. `O(layers)`; reads no pixels.
use super::ModuleRegistry;
use crate::{
    Error, Layer, Mask, MaskId, Recipe,
    artifacts::ArtifactTable,
    mask_field::{MaskField, MaskSampling},
    modules::{
        EffectStage, MAX_COLOR_UNITS, MAX_MASKED_SPATIAL_LAYERS, Processing, SPATIAL_TILE, Stage,
    },
    render::{
        Compiled, Entry, Segment,
        spatial::{SpatialPlan, prefix_hash},
    },
};
use std::collections::HashSet;

fn unavailable(effect_id: &str, layers: Vec<&str>) -> Error {
    Error::incompatible(format!(
        "unavailable effect {effect_id} (layers {})",
        layers.join(", ")
    ))
}

impl ModuleRegistry {
    /// Whether this stack may be rendered against a downscaled proxy source.
    ///
    /// A source-stage, colour-stage, geometry-stage or finish-stage effect is resolution
    /// independent: the source development is pointwise, a colour unit is pointwise, the geometry
    /// payloads are normalized to their own input stage and a finish unit's mask is normalized to
    /// the output stage, so the same recipe compiles unchanged against a smaller content stage and
    /// produces the same picture at display size. A spatial-stage effect is eligible too, but its
    /// neighbourhoods scale with the stage, so its proxy frame is an approximation of the exact
    /// render at display size rather than the same picture; [`crate::Render::approximation`] says
    /// when a stack renders that way, and the exact phase still produces every number. A pixel-stage
    /// effect is not eligible: its payload addresses content pixels, which a rescaled stage no
    /// longer has. An effect no provider declares is ineligible too, because nothing can say what
    /// stage it addresses.
    ///
    /// Cost is `O(layers)` and reads no pixels. The error names the first ineligible layer's effect
    /// identity and its index, so the caller reports the reason rather than silently taking the
    /// exact path.
    pub fn proxy_eligible(&self, recipe: &Recipe) -> Result<(), Error> {
        for (index, layer) in recipe.layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(
                    EffectStage::Source
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

    /// Whether this stored layer changes nothing, by its available provider's own rule
    /// ([`ToolModule::is_neutral`]). A layer whose provider is missing or unavailable, or whose
    /// payload the provider cannot read, is not neutral: nothing can say it changes nothing.
    /// Reading the payload only.
    pub fn layer_neutral(&self, layer: &Layer) -> bool {
        self.provider(&layer.effect_id).is_some_and(|module| {
            module
                .is_neutral(&layer.effect_id, layer.effect_format, &layer.payload)
                .unwrap_or(false)
        })
    }

    /// Structural validation stays in the model; effect availability, whether the effect may
    /// reference artifacts and payload validation are the registry's.
    pub fn validate_layer(&self, layer: &Layer) -> Result<(), Error> {
        layer.validate()?;
        let module = self
            .provider(&layer.effect_id)
            .ok_or_else(|| unavailable(&layer.effect_id, vec![layer.id.as_str()]))?;
        self.check_artifacts(layer)?;
        module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)
    }

    /// Everything a stack must satisfy to be written, checked once where it enters the service
    /// ([`crate::EditorService`]'s admission): the layers' structure and mask references, the whole
    /// mask table ([`Recipe::validate_mask_table`]), masks only on stages that can carry one, and
    /// every layer's effect available, its artifacts declared and its payload accepted by its
    /// provider. `O(layers + components + strokes)`; it reads no pixels.
    pub fn validate_recipe(&self, recipe: &Recipe) -> Result<(), Error> {
        #[cfg(test)]
        crate::editor::validations::validated();
        recipe.validate()?;
        recipe.validate_mask_table()?;
        self.validate_masked_stages(recipe)?;
        for layer in &recipe.layers {
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(&recipe.layers, &layer.effect_id))?;
            self.check_artifacts(layer)?;
            module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
        }
        Ok(())
    }

    /// A mask's geometry is stored in content-stage coordinates, so only a layer whose input is that
    /// content stage may carry one: a geometry layer changes the stage and a finish layer is defined
    /// in the output coordinates the geometry tail produced, and neither has a content stage to read
    /// a mask in. The rule lives here rather than in the model because the stage is declared by the
    /// effect's provider, not by the recipe. `O(layers)` descriptor lookups, no pixels. An effect no
    /// provider declares is left to the unavailable report that follows, which names it already.
    fn validate_masked_stages(&self, recipe: &Recipe) -> Result<(), Error> {
        for layer in &recipe.layers {
            if layer.mask.is_none() {
                continue;
            }
            if let Some(stage @ (EffectStage::Geometry | EffectStage::Finish)) =
                self.effect_stage(&layer.effect_id)
            {
                return Err(Error::validation(format!(
                    "layer {} carries a mask, which a {} effect cannot: a mask is stored in \
                     content-stage coordinates",
                    layer.id,
                    stage.as_str()
                )));
            }
        }
        Ok(())
    }

    /// Refuse a layer whose mask this build cannot evaluate.
    ///
    /// A mask reaches a colour operation and a spatial operation, and nothing else: a point
    /// replacement writes one stored pixel and has no blend to perform, so there is nothing for a
    /// coverage to modulate. (A geometry or finish layer cannot carry a mask at all, for the earlier
    /// reason that it has no content stage to read one in; that is
    /// [`Self::validate_masked_stages`].) A mask this build cannot evaluate must never be silently
    /// omitted from a frame or an export, so such a stack is refused by name wherever it would be
    /// drawn — and because the host compiles a stack before it persists one, the refusal is also
    /// what keeps such a layer from being committed at all. It reads the stack and rewrites nothing.
    fn refuse_unevaluated_mask(&self, layer: &Layer) -> Result<(), Error> {
        if layer.mask.is_none() {
            return Ok(());
        }
        Err(Error::incompatible(format!(
            "layer {} carries a mask on the {} effect {}, and this build evaluates a mask only \
                 on a colour-stage or spatial-stage effect",
            layer.id,
            self.effect_stage(&layer.effect_id)
                .map_or("unknown", EffectStage::as_str),
            layer.effect_id
        )))
    }

    /// The compiled mask one layer is modulated by, against the stage that layer receives, or
    /// `None` for a global layer.
    ///
    /// Compiling is `O(components)` and reads no pixels, so a masked layer costs the same to compile
    /// as an unmasked one plus a handful of closed-form terms per component. Two layers bound to one
    /// mask compile it twice rather than sharing one compilation: the cost is bounded by the
    /// components-per-mask limit and a cache would have to be keyed by stage as well as identity, so
    /// it is not worth the machinery until a measurement says otherwise.
    ///
    /// A reference the mask table does not hold is refused here as well as by [`Recipe::validate`],
    /// because a prefix compile is reached without the recipe.
    fn compiled_mask(
        layer: &Layer,
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        stage: Stage,
        sampling: MaskSampling,
    ) -> Result<Option<MaskField>, Error> {
        let Some(id) = &layer.mask else {
            return Ok(None);
        };
        let mask = masks.iter().find(|mask| &mask.id == id).ok_or_else(|| {
            Error::incompatible(format!(
                "layer {} names mask {id}, which this recipe does not hold",
                layer.id
            ))
        })?;
        Ok(Some(MaskField::compile(mask, stage, strokes, sampling)?))
    }

    /// Only a layer of an effect that declares `artifacts` may reference any. The host owns the
    /// list, so this is the host's rule, checked before the module sees the payload.
    fn check_artifacts(&self, layer: &Layer) -> Result<(), Error> {
        let declared = self
            .effect(&layer.effect_id)
            .is_some_and(|(_, effect)| effect.artifacts);
        if layer.artifacts.is_empty() || declared {
            Ok(())
        } else {
            Err(Error::validation(format!(
                "layer {} of effect {} references artifacts, which its effect does not declare",
                layer.id, layer.effect_id
            )))
        }
    }

    fn unavailable_in(&self, layers: &[Layer], effect_id: &str) -> Error {
        unavailable(
            effect_id,
            layers
                .iter()
                .filter(|layer| layer.effect_id == effect_id)
                .map(|layer| layer.id.as_str())
                .collect(),
        )
    }

    /// Validate a recipe against the source dimensions and fold its exact geometry into one mapping
    /// per rasterizing pass. A resample is a stage boundary, so it closes the current pass and opens
    /// the next one. Cost is linear in the layer count and allocates only the operation lists.
    pub(crate) fn compile(
        &self,
        source_width: u32,
        source_height: u32,
        recipe: &Recipe,
    ) -> Result<Compiled, Error> {
        self.compile_sampled(source_width, source_height, recipe, MaskSampling::Point)
    }

    /// [`Self::compile`] with the way this render samples its masks as a parameter.
    ///
    /// Every exact render point samples, which is the frozen field; only the proxy phase passes
    /// [`MaskSampling::ThinFeature`], and only a mask that draws a feature narrower than two pixels
    /// *at the stage compiled here* is affected by it. Nothing else about compiling changes, which
    /// is what keeps a proxy frame byte for byte the exact recipe over the exact downscale wherever
    /// the rule does not fire.
    pub(crate) fn compile_sampled(
        &self,
        source_width: u32,
        source_height: u32,
        recipe: &Recipe,
        sampling: MaskSampling,
    ) -> Result<Compiled, Error> {
        // The layer checks every evaluation path shares: the format marker, the layers' structure
        // and each layer's mask reference, and a mask only where a stage can carry one. They cost
        // `O(layers)` and read no pixels, so compiling here is what makes a stack that names a mask
        // it does not carry, or attaches one to the geometry tail, fail rendering, sampling, proxy
        // planning and module planning alike.
        //
        // The mask table itself is not checked again: it was checked once when the recipe entered
        // the service (`Recipe::validate_mask_table`, from admission and from a drafted mask
        // gesture), and a stored recipe was admitted when it was written. What a masked layer needs
        // drawn is refused where it is drawn: compiling its mask parses every component through the
        // kind table and resolves every stroke, so a mask this build cannot evaluate, or a stroke
        // the store has lost, still refuses every path that would draw it by name, and nothing is
        // rewritten or resolved to an empty stroke. A mask no layer draws changes no pixel, so
        // rendering its stack draws exactly what the stack says.
        recipe.validate()?;
        self.validate_masked_stages(recipe)?;
        self.compile_layers_sampled(
            source_width,
            source_height,
            &recipe.layers,
            &recipe.masks,
            &recipe.strokes,
            &recipe.artifacts,
            sampling,
        )
    }

    /// Compile an ordered layer slice whose recipe format is already known good, against the mask
    /// table its layers reference. Asking for the stage one layer receives compiles the prefix
    /// before it through here, so it copies no part of the stack; a prefix carries the whole mask
    /// table, because the masks a prefix layer names are the recipe's and not the prefix's, and
    /// the recipe's bound artifacts for the same reason.
    pub(crate) fn compile_layers(
        &self,
        source_width: u32,
        source_height: u32,
        layers: &[Layer],
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        artifacts: &ArtifactTable,
    ) -> Result<Compiled, Error> {
        self.compile_layers_sampled(
            source_width,
            source_height,
            layers,
            masks,
            strokes,
            artifacts,
            MaskSampling::Point,
        )
    }

    /// [`Self::compile_layers`] with the mask sampling of the render being compiled.
    #[allow(clippy::too_many_arguments)]
    fn compile_layers_sampled(
        &self,
        source_width: u32,
        source_height: u32,
        layers: &[Layer],
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        artifacts: &ArtifactTable,
        sampling: MaskSampling,
    ) -> Result<Compiled, Error> {
        let mut layer_ids = HashSet::with_capacity(layers.len());
        // The effects whose module owns exactly one layer of a stack, seen so far, **per target**:
        // the global layer and each mask are distinct targets, so one effect may hold a layer in
        // each mask and still hold one global layer. Two layers of one effect with the same target
        // are what a module cannot resolve, so the host refuses that stack here as well as when the
        // module plans against it, and rewrites nothing.
        let mut single_effects: HashSet<(&str, Option<&MaskId>)> = HashSet::new();
        let mut segments = vec![Segment::new(None, source_width, source_height)];
        // The one order the host cannot evaluate: a finish effect is defined in the output
        // coordinates of the geometry tail, so a geometry layer after it has no stage to address.
        // The stack is refused as it stands and nothing is rewritten or reordered.
        let mut finish_layer: Option<&Layer> = None;
        // Masked spatial layers seen so far, against the declared cap. Each one is a stage boundary
        // and therefore a sequential full frame, which is the whole reason there is a cap.
        let mut masked_spatial = 0_usize;
        for (index, layer) in layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(EffectStage::Source) if index != 0 => {
                    return Err(Error::validation(
                        "source-stage effect must be at index zero",
                    ));
                }
                Some(EffectStage::Finish) => finish_layer = finish_layer.or(Some(layer)),
                Some(EffectStage::Geometry) => {
                    if let Some(finish) = finish_layer {
                        return Err(Error::validation(format!(
                            "finish layer precedes geometry (finish {}, geometry {})",
                            finish.id, layer.id
                        )));
                    }
                }
                _ => {}
            }
            if !layer_ids.insert(&layer.id) {
                return Err(Error::validation("duplicate layer identity"));
            }
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(layers, &layer.effect_id))?;
            if self.effect_single(&layer.effect_id)
                && !single_effects.insert((layer.effect_id.as_str(), layer.mask.as_ref()))
            {
                return Err(self.ambiguous(&layer.effect_id));
            }
            let segment = segments.last_mut().expect("one segment always exists");
            let stage = Stage {
                width: segment.width,
                height: segment.height,
            };
            let processing = if layer.artifacts.is_empty() {
                module.compile(&layer.effect_id, layer.effect_format, &layer.payload, stage)?
            } else {
                // The recipe carries the verified bytes it was bound with, so resolving them is a
                // lookup; an artifact the recipe was not bound with is refused, never skipped.
                self.check_artifacts(layer)?;
                let bound = layer
                    .artifacts
                    .iter()
                    .map(|id| {
                        artifacts.get(id).cloned().ok_or_else(|| {
                            Error::source_unavailable(format!(
                                "artifact {id} of layer {} is not bound to this recipe",
                                layer.id
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                // Registration refused a module that declares an artifact effect without its
                // capability hooks, so this is only a guard.
                let capabilities = module.capabilities().ok_or_else(|| {
                    Error::internal(format!(
                        "module {} compiles artifact layers without capability hooks",
                        module.descriptor().id
                    ))
                })?;
                capabilities.compile_bound(
                    &layer.effect_id,
                    layer.effect_format,
                    &layer.payload,
                    stage,
                    &bound,
                )?
            };
            match processing {
                Processing::ExactGeometry(step) => {
                    if !step.reads_inside(segment.width, segment.height) {
                        return Err(Error::validation(format!(
                            "an exact mapping to {}x{} reads outside its {}x{} input stage",
                            step.output_width, step.output_height, segment.width, segment.height
                        )));
                    }
                    segment.geometry = segment.geometry.then(step);
                    segment.width = step.output_width;
                    segment.height = step.output_height;
                    segment.operations.push(Processing::ExactGeometry(step));
                }
                Processing::PointReplace { x, y, rgb } => {
                    // A point replacement has no blend to perform, so a mask on one would be a mask
                    // this build cannot evaluate. No delivered pixel-stage effect declares itself
                    // maskable; refusing by name is what keeps a later one from silently rendering
                    // its replacement everywhere instead of through the selection.
                    self.refuse_unevaluated_mask(layer)?;
                    segment.has_pixels = true;
                    segment
                        .operations
                        .push(Processing::PointReplace { x, y, rgb });
                }
                Processing::Color(operation) => {
                    if operation.len() > MAX_COLOR_UNITS {
                        return Err(Error::validation(format!(
                            "a colour operation declares {} units, more than the {MAX_COLOR_UNITS} the host evaluates",
                            operation.len()
                        )));
                    }
                    if !operation.is_finite() {
                        return Err(Error::validation(
                            "a colour operation declares a unit whose coefficients are not finite",
                        ));
                    }
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // segment keeps the identity byte path and the shared source buffer, mask or no
                    // mask. Masking nothing is nothing, so no mask is compiled for it either.
                    if !operation.is_empty() {
                        // The mask is the host's, attached here — where a compiled layer becomes
                        // `Processing` — against the stage this layer receives, which for a
                        // content-stage layer is the content stage its geometry is normalized to. A
                        // module returned a plain operation and never saw the reference.
                        let operation =
                            match Self::compiled_mask(layer, masks, strokes, stage, sampling)? {
                                Some(mask) => operation.with_mask(mask),
                                None => operation,
                            };
                        segment.has_color = true;
                        segment.operations.push(Processing::Color(operation));
                    }
                }
                Processing::Spatial(operation) => {
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // stack keeps its single pass, the identity byte path and the shared source
                    // buffer, exactly as a neutral colour payload does. Masking nothing is nothing,
                    // so no mask is compiled for it either.
                    if operation.is_empty() {
                        continue;
                    }
                    // The mask is the host's, attached here — where a compiled layer becomes
                    // `Processing` — against the stage this layer receives. A spatial layer is a
                    // stage boundary, so that stage is also the frame the operation reads and
                    // writes, which is what lets the tile loop read the mask at a tile's own
                    // coordinates. The module returned a plain operation and never saw the
                    // reference.
                    let operation = match Self::compiled_mask(
                        layer, masks, strokes, stage, sampling,
                    )? {
                        Some(mask) => {
                            masked_spatial += 1;
                            if masked_spatial > MAX_MASKED_SPATIAL_LAYERS {
                                return Err(Error::resource_limit(format!(
                                    "this recipe holds {masked_spatial} masked spatial layers, more than the \
                                         {MAX_MASKED_SPATIAL_LAYERS} the host evaluates: each one is a stage \
                                         boundary and therefore a sequential full frame"
                                )));
                            }
                            operation.with_mask(mask)
                        }
                        None => operation,
                    };
                    // Everything stage-dependent the operation declares — the unit count, their
                    // finiteness and the summed halo — is checked here, before a pixel is read.
                    // Nothing is rewritten or reduced to fit, and what a tile costs in memory,
                    // which a mask adds two tile planes to, never refuses it.
                    SpatialPlan::new(&operation, stage, SPATIAL_TILE)?;
                    let prefix_hash = prefix_hash(&layers[..index], masks, sampling)?;
                    segments.push(Segment::new(
                        Some(Entry::Spatial {
                            operation,
                            prefix_hash,
                        }),
                        stage.width,
                        stage.height,
                    ));
                }
                Processing::Resample(resample) => {
                    if resample.output_width == 0 || resample.output_height == 0 {
                        return Err(Error::validation(
                            "a resample declares an empty output stage",
                        ));
                    }
                    if !resample.inverse.iter().all(|value| value.is_finite()) {
                        return Err(Error::validation(
                            "a resample declares a mapping that is not finite",
                        ));
                    }
                    segments.push(Segment::new(
                        Some(Entry::Resample(resample)),
                        resample.output_width,
                        resample.output_height,
                    ));
                }
            }
        }
        Ok(Compiled { segments })
    }
}
