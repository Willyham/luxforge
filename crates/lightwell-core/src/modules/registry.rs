//! The provider index: descriptors validated once at registration, then hash lookups by effect,
//! action, query and task identity. Registration touches no image, catalog, settings, secret,
//! network or resource file.
use super::{
    ActionDescriptor, BasicModule, CanvasInteraction, CropModule, EffectDescriptor, EffectStage,
    MAX_COLOR_UNITS, MixerModule, ModuleDescriptor, PixelModule, PresenceModule, PresetsModule,
    Processing, RawModule, SPATIAL_TILE, Stage, ToolModule, TransformModule, VignetteModule,
};
use crate::{
    Error, ErrorKind, Layer, RECIPE_FORMAT, Recipe, artifacts,
    capabilities::descriptor::TaskDescriptor,
    render::{
        Compiled, Entry, Segment,
        spatial::{SpatialPlan, prefix_hash},
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn unavailable(effect_id: &str, layers: Vec<&str>) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!(
            "unavailable effect {effect_id} (layers {})",
            layers.join(", ")
        ),
    )
}

#[derive(Default)]
pub struct ModuleRegistry {
    modules: Vec<Arc<dyn ToolModule>>,
    module_ids: HashSet<String>,
    /// Effect identity to (module, effect) position.
    effects: HashMap<String, (usize, usize)>,
    /// Action identity to (module, action) position.
    actions: HashMap<String, (usize, usize)>,
    /// Query identity to (module, query) position. Queries have their own namespace: `query.<id>`
    /// and `edit.<id>` are different methods, so an id claimed here does not claim an action name.
    queries: HashMap<String, (usize, usize)>,
    /// Task identity to (module, task) position. A task generates the method `task.<id>`, so its
    /// identity is unique across the registry in a namespace of its own.
    tasks: HashMap<String, (usize, usize)>,
    /// Canvas mode shortcut to the module that claims it, so one letter selects one mode.
    shortcuts: HashMap<String, usize>,
}

impl std::fmt::Debug for ModuleRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleRegistry")
            .field(
                "modules",
                &self
                    .modules
                    .iter()
                    .map(|module| module.descriptor().id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The linked built-in providers. External loading is a later, separately measured step.
    /// Presets come first: the module owns no layer, and its section leads the tools panel.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for module in [
            Arc::new(PresetsModule::new()) as Arc<dyn ToolModule>,
            Arc::new(PixelModule::new()),
            Arc::new(RawModule::new()),
            Arc::new(BasicModule::new()),
            Arc::new(PresenceModule::new()),
            Arc::new(MixerModule::new()),
            Arc::new(TransformModule::new()),
            Arc::new(CropModule::new()),
            Arc::new(VignetteModule::new()),
        ] {
            registry
                .register(module)
                .expect("built-in module descriptors are valid");
        }
        registry
    }

    /// Validate a descriptor and index its effects and actions. Identities are unique across the
    /// whole registry, so discovery and dispatch can never resolve to two providers.
    pub fn register(&mut self, module: Arc<dyn ToolModule>) -> Result<(), Error> {
        let descriptor = module.descriptor();
        descriptor.validate()?;
        if self.module_ids.contains(&descriptor.id) {
            return Err(validation(format!("duplicate module {}", descriptor.id)));
        }
        for effect in &descriptor.effects {
            if let Some((existing, _)) = self.effects.get(&effect.id) {
                return Err(validation(format!(
                    "effect {} is already provided by {}",
                    effect.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for action in &descriptor.actions {
            if let Some((existing, _)) = self.actions.get(&action.id) {
                return Err(validation(format!(
                    "action {} is already provided by {}",
                    action.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for query in &descriptor.queries {
            if let Some((existing, _)) = self.queries.get(&query.id) {
                return Err(validation(format!(
                    "query {} is already provided by {}",
                    query.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for task in &descriptor.tasks {
            if let Some((existing, _)) = self.tasks.get(&task.id) {
                return Err(validation(format!(
                    "task {} of module {} is already provided by {}",
                    task.id,
                    descriptor.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        let shortcut = descriptor
            .canvas
            .as_ref()
            .and_then(CanvasInteraction::shortcut);
        if let Some(letter) = shortcut
            && let Some(existing) = self.shortcuts.get(letter)
        {
            return Err(validation(format!(
                "canvas shortcut {letter} is already claimed by {}",
                self.modules[*existing].descriptor().id
            )));
        }
        let index = self.modules.len();
        if let Some(letter) = shortcut {
            self.shortcuts.insert(letter.to_owned(), index);
        }
        self.module_ids.insert(descriptor.id.clone());
        for (position, effect) in descriptor.effects.iter().enumerate() {
            self.effects.insert(effect.id.clone(), (index, position));
        }
        for (position, action) in descriptor.actions.iter().enumerate() {
            self.actions.insert(action.id.clone(), (index, position));
        }
        for (position, query) in descriptor.queries.iter().enumerate() {
            self.queries.insert(query.id.clone(), (index, position));
        }
        for (position, task) in descriptor.tasks.iter().enumerate() {
            self.tasks.insert(task.id.clone(), (index, position));
        }
        self.modules.push(module);
        Ok(())
    }

    pub fn descriptors(&self) -> Vec<&ModuleDescriptor> {
        self.modules
            .iter()
            .map(|module| module.descriptor())
            .collect()
    }

    pub fn action(&self, id: &str) -> Option<(&dyn ToolModule, &ActionDescriptor)> {
        let (module, position) = self.actions.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().actions[*position]))
    }

    /// The module that answers this read-only query, and the query's declared parameters. An
    /// unavailable provider keeps its identity here exactly as it does for actions and effects; the
    /// caller reports that rather than silently answering nothing.
    pub fn query(&self, id: &str) -> Option<(&dyn ToolModule, &ActionDescriptor)> {
        let (module, position) = self.queries.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().queries[*position]))
    }

    /// The module that offers this worker task, and the task's declaration.
    pub fn task(&self, id: &str) -> Option<(&dyn ToolModule, &TaskDescriptor)> {
        let (module, position) = self.tasks.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().tasks[*position]))
    }

    /// The registered module with this identity. A linear scan: a registry holds a handful of
    /// modules, and the capability methods that ask are not on a per-pixel path.
    pub fn module(&self, id: &str) -> Option<&dyn ToolModule> {
        self.modules
            .iter()
            .map(AsRef::as_ref)
            .find(|module| module.descriptor().id == id)
    }

    pub fn effect(&self, id: &str) -> Option<(&dyn ToolModule, &EffectDescriptor)> {
        let (module, position) = self.effects.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().effects[*position]))
    }

    /// The stage an effect's payload addresses, or `None` when no provider declares it.
    pub fn effect_stage(&self, effect_id: &str) -> Option<EffectStage> {
        self.effect(effect_id).map(|(_, effect)| effect.stage)
    }

    /// The stage and the within-stage order an effect declares, or `None` when no provider declares
    /// the effect at all.
    fn effect_placement(&self, effect_id: &str) -> Option<(EffectStage, u16)> {
        self.effect(effect_id)
            .map(|(_, effect)| (effect.stage, effect.order))
    }

    /// Where a committed layer of this effect joins a stack, by the stage and the order its
    /// descriptor declares. A module names its own effect rather than repeating its placement.
    /// An effect no provider declares is placed as a geometry effect would be and refused by the
    /// whole-stack compile that follows; a *layer* whose effect no provider declares opens no
    /// region either, because such a stack cannot compile at all and the host reports that rather
    /// than guessing a position.
    pub fn insertion_index_for(&self, layers: &[Layer], effect_id: &str) -> usize {
        let (stage, order) = self
            .effect_placement(effect_id)
            .unwrap_or((EffectStage::Geometry, 0));
        self.insertion_index(layers, stage, order)
    }

    /// Where a committed layer of this stage and order joins a stack:
    ///
    /// | Stage | Placement |
    /// | --- | --- |
    /// | `source` | index zero |
    /// | `pixel`, `color` | before the first spatial, geometry or finish layer |
    /// | `spatial` | before the first geometry or finish layer, after every pixel and colour layer |
    /// | `geometry` | before the first finish layer |
    /// | `finish` | at the end |
    ///
    /// so the geometry tail carries every content-stage edit, a neighbourhood operation reads the
    /// finished pointwise colour, and a finish effect sees the output coordinates the tail
    /// produced. A leading source layer keeps index zero whatever is inserted.
    ///
    /// Within the region its stage chooses, the new layer goes after the last layer of the *same*
    /// stage whose order is at most `order` and before the first whose order is greater. Layers of
    /// other stages inside the region keep their positions, and no existing layer ever moves, so a
    /// stack stored in another order stays exactly as it is and renders in its stored order.
    ///
    /// Cost is `O(layers)` in descriptor lookups; it reads no pixels and allocates nothing.
    pub fn insertion_index(&self, layers: &[Layer], stage: EffectStage, order: u16) -> usize {
        if stage == EffectStage::Source {
            return 0;
        }
        // A source layer prepares the content stage and always stays at index zero.
        let mut lower =
            usize::from(layers.first().is_some_and(|layer| {
                self.effect_stage(&layer.effect_id) == Some(EffectStage::Source)
            }));
        let opens_region = |candidate: EffectStage| match stage {
            EffectStage::Pixel | EffectStage::Color => matches!(
                candidate,
                EffectStage::Spatial | EffectStage::Geometry | EffectStage::Finish
            ),
            EffectStage::Spatial => {
                matches!(candidate, EffectStage::Geometry | EffectStage::Finish)
            }
            EffectStage::Geometry => candidate == EffectStage::Finish,
            EffectStage::Source | EffectStage::Finish => false,
        };
        let mut upper = layers.len();
        // The first layer of the same stage whose order is greater: the new layer goes before it.
        let mut successor = None;
        for (index, layer) in layers.iter().enumerate() {
            let Some((layer_stage, layer_order)) = self.effect_placement(&layer.effect_id) else {
                continue;
            };
            if opens_region(layer_stage) {
                upper = index;
                break;
            }
            // A spatial layer reads what the pointwise colour run produced, so it never lands
            // before a pixel or colour layer a stored stack kept later than usual.
            if stage == EffectStage::Spatial
                && matches!(layer_stage, EffectStage::Pixel | EffectStage::Color)
            {
                lower = index + 1;
            }
            if layer_stage == stage && layer_order > order && successor.is_none() {
                successor = Some(index);
            }
        }
        let upper = upper.max(lower);
        successor.unwrap_or(upper).clamp(lower, upper)
    }

    /// Whether this stack may be rendered against a downscaled proxy source.
    ///
    /// A source-stage, colour-stage, geometry-stage or finish-stage effect is resolution
    /// independent: the source development is pointwise, a colour unit is pointwise, the geometry
    /// payloads are normalized to their own input stage and a finish unit's mask is normalized to
    /// the output stage, so the same recipe compiles unchanged against a smaller content stage and
    /// produces the same picture at display size. A spatial-stage effect is eligible too, but its
    /// neighbourhoods scale with the stage, so its proxy frame is an approximation of the exact
    /// render at display size rather than the same picture; [`Self::proxy_approximate`] says when
    /// a stack renders that way, and the exact phase still produces every number. A pixel-stage
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
                    return Err(validation(format!(
                        "layer {index} is not proxy-eligible: effect {} is at the pixel stage, \
                         whose coordinates are content pixels and cannot be rescaled",
                        layer.effect_id
                    )));
                }
                None => {
                    return Err(validation(format!(
                        "layer {index} is not proxy-eligible: no provider declares effect {}, so \
                         its stage is unknown",
                        layer.effect_id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Whether a proxy render of this stack is an approximation: a spatial-stage layer's
    /// neighbourhoods scale with the stage it is rendered at, so its display-size frame is close to
    /// the exact render but not the same picture. Cost is `O(layers)` and reads no pixels.
    pub fn proxy_approximate(&self, recipe: &Recipe) -> bool {
        recipe
            .layers
            .iter()
            .any(|layer| self.effect_stage(&layer.effect_id) == Some(EffectStage::Spatial))
    }

    /// The provider that can evaluate this effect, or `None` when none is registered or the
    /// registered one reports itself unavailable.
    fn provider(&self, effect_id: &str) -> Option<&dyn ToolModule> {
        let (module, _) = self.effect(effect_id)?;
        module.descriptor().is_available().then_some(module)
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

    pub fn validate_recipe(&self, recipe: &Recipe) -> Result<(), Error> {
        recipe.validate()?;
        for layer in &recipe.layers {
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(&recipe.layers, &layer.effect_id))?;
            self.check_artifacts(layer)?;
            module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
        }
        Ok(())
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
            Err(validation(format!(
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
        if recipe.format != RECIPE_FORMAT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unsupported recipe format {}", recipe.format),
            ));
        }
        self.compile_layers(source_width, source_height, &recipe.layers)
    }

    /// Compile an ordered layer slice whose recipe format is already known good. Asking for the
    /// stage one layer receives compiles the prefix before it through here, so it copies no part of
    /// the stack.
    pub(crate) fn compile_layers(
        &self,
        source_width: u32,
        source_height: u32,
        layers: &[Layer],
    ) -> Result<Compiled, Error> {
        let mut layer_ids = HashSet::with_capacity(layers.len());
        // The effects whose module owns exactly one layer of a stack, seen so far. A module that
        // declares this cannot say which of two layers holds its state, so the host refuses the
        // stack here as well as when the module plans against it, and rewrites nothing.
        let mut single_effects: HashSet<&str> = HashSet::new();
        let mut segments = vec![Segment::new(None, source_width, source_height)];
        // The one order the host cannot evaluate: a finish effect is defined in the output
        // coordinates of the geometry tail, so a geometry layer after it has no stage to address.
        // The stack is refused as it stands and nothing is rewritten or reordered.
        let mut finish_layer: Option<&Layer> = None;
        for (index, layer) in layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(EffectStage::Source) if index != 0 => {
                    return Err(validation("source-stage effect must be at index zero"));
                }
                Some(EffectStage::Finish) => finish_layer = finish_layer.or(Some(layer)),
                Some(EffectStage::Geometry) => {
                    if let Some(finish) = finish_layer {
                        return Err(validation(format!(
                            "finish layer precedes geometry (finish {}, geometry {})",
                            finish.id, layer.id
                        )));
                    }
                }
                _ => {}
            }
            if !layer_ids.insert(&layer.id) {
                return Err(validation("duplicate layer identity"));
            }
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(layers, &layer.effect_id))?;
            if module.single_layer(&layer.effect_id)
                && !single_effects.insert(layer.effect_id.as_str())
            {
                return Err(validation(format!(
                    "ambiguous {} layers",
                    module.descriptor().title
                )));
            }
            let segment = segments.last_mut().expect("one segment always exists");
            let stage = Stage {
                width: segment.width,
                height: segment.height,
            };
            let processing = if layer.artifacts.is_empty() {
                module.compile(&layer.effect_id, layer.effect_format, &layer.payload, stage)?
            } else {
                // The caller that planned this evaluation holds the verified bytes, so resolving
                // them is a lookup; an artifact nobody prepared is refused, never skipped.
                self.check_artifacts(layer)?;
                let bound = layer
                    .artifacts
                    .iter()
                    .map(|id| {
                        artifacts::prepared(id).ok_or_else(|| {
                            Error::new(
                                ErrorKind::SourceUnavailable,
                                format!("artifact {id} is not prepared"),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                module.compile_bound(
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
                        return Err(validation(format!(
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
                    segment.has_pixels = true;
                    segment
                        .operations
                        .push(Processing::PointReplace { x, y, rgb });
                }
                Processing::Color(operation) => {
                    if operation.len() > MAX_COLOR_UNITS {
                        return Err(validation(format!(
                            "a colour operation declares {} units, more than the {MAX_COLOR_UNITS} the host evaluates",
                            operation.len()
                        )));
                    }
                    if !operation.is_finite() {
                        return Err(validation(
                            "a colour operation declares a unit whose coefficients are not finite",
                        ));
                    }
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // segment keeps the identity byte path and the shared source buffer.
                    if !operation.is_empty() {
                        segment.has_color = true;
                        segment.operations.push(Processing::Color(operation));
                    }
                }
                Processing::Spatial(operation) => {
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // stack keeps its single pass, the identity byte path and the shared source
                    // buffer, exactly as a neutral colour payload does.
                    if operation.is_empty() {
                        continue;
                    }
                    // Everything stage-dependent the operation declares — the unit count, their
                    // finiteness and the summed halo — is checked here, before a pixel is read.
                    // Nothing is rewritten or reduced to fit, and what a tile costs in memory
                    // never refuses it.
                    SpatialPlan::new(&operation, stage, SPATIAL_TILE)?;
                    let prefix_hash = prefix_hash(&layers[..index])?;
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
                        return Err(validation("a resample declares an empty output stage"));
                    }
                    if !resample.inverse.iter().all(|value| value.is_finite()) {
                        return Err(validation(
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        AssetId, BASIC_EFFECT, CROP_EFFECT, EFFECT_FORMAT, LayerId, ORIENTATION_EFFECT,
        Orientation, PIXEL_EFFECT, RAW_EFFECT, SnapshotId, SourceImage,
        modules::{
            ActionInput, ActionPlan, Availability, CropPayload, EffectStage, ModuleDescriptor,
            StageContext,
        },
        render, sample,
    };
    use serde_json::{Map, Value, json};

    /// A minimal module used to prove registration rules and missing-provider behavior.
    pub(crate) struct TestModule(ModuleDescriptor);

    impl TestModule {
        pub(crate) fn new(
            id: &str,
            effect: &str,
            action: &str,
            availability: Availability,
        ) -> Self {
            Self(ModuleDescriptor {
                id: id.into(),
                title: "Test".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: effect.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                    order: 0,
                    artifacts: false,
                }],
                actions: vec![ActionDescriptor {
                    id: action.into(),
                    title: "Test action".into(),
                    notes: "test".into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability,
                ..ModuleDescriptor::default()
            })
        }
        /// A module whose descriptor is written by the test itself.
        pub(crate) fn from_descriptor(descriptor: ModuleDescriptor) -> Arc<dyn ToolModule> {
            Arc::new(Self(descriptor))
        }
        pub(crate) fn shared(
            id: &str,
            effect: &str,
            action: &str,
            availability: Availability,
        ) -> Arc<dyn ToolModule> {
            Arc::new(Self::new(id, effect, action, availability))
        }
    }

    impl ToolModule for TestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("test layer of {effect_id}"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Err(Error::new(ErrorKind::Internal, "test module never renders"))
        }
    }

    pub(crate) const PATCH_MODULE: &str = "test.patch";
    pub(crate) const PATCH_EFFECT: &str = "test.patch.effect";
    pub(crate) const PATCH_ACTION: &str = "set-patch";

    /// A module whose one action is a field patch, the shape Basic's sliders will take: the host
    /// hands it only the fields the caller named, it merges them over the layer it already has, and
    /// it reports an unchanged result as a no-op. Its layer replaces one pixel, so a preview, a
    /// sample and a rendered frame all show which fields are in effect.
    pub(crate) struct PatchModule(ModuleDescriptor);

    impl PatchModule {
        pub(crate) fn shared() -> Arc<dyn ToolModule> {
            let channel = |name: &str| crate::ParameterDescriptor {
                name: name.into(),
                kind: crate::ParameterKind::Number {
                    min: 0.0,
                    max: 255.0,
                },
                required: false,
                default: Some(json!(0.0)),
                unit: Some("code".into()),
                step: Some(1.0),
                precision: Some(0),
                notes: format!("the {name} channel of the replaced pixel"),
                soft_min: None,
                soft_max: None,
                fine_step: None,
                zero: None,
            };
            Arc::new(Self(ModuleDescriptor {
                id: PATCH_MODULE.into(),
                title: "Patch".into(),
                hint: Some("A patched pixel".into()),
                effects: vec![EffectDescriptor {
                    id: PATCH_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                    order: 0,
                    artifacts: false,
                }],
                actions: vec![ActionDescriptor {
                    id: PATCH_ACTION.into(),
                    title: "Set patch".into(),
                    notes: "merges the named channels into the one patch layer".into(),
                    summary: Some("Patch {red} {green}".into()),
                    patch: true,
                    parameters: vec![channel("red"), channel("green")],
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            }))
        }

        /// The channels a payload holds; a missing channel is neutral.
        pub(crate) fn channels(payload: &Value) -> [f64; 2] {
            let channel = |name: &str| payload.get(name).and_then(Value::as_f64).unwrap_or(0.0);
            [channel("red"), channel("green")]
        }

        fn merged(payload: &Value, fields: &Map<String, Value>) -> Value {
            let [red, green] = Self::channels(payload);
            let field = |name: &str, current: f64| {
                fields.get(name).and_then(Value::as_f64).unwrap_or(current)
            };
            json!({"red": field("red", red), "green": field("green", green)})
        }
    }

    impl ToolModule for PatchModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(
            &self,
            action_id: &str,
            parameters: &Map<String, Value>,
        ) -> Result<ActionInput, Error> {
            // Exactly the fields the host checked: a patch stores what was sent, not the merge.
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: parameters.clone(),
            })
        }
        fn plan(
            &self,
            input: &ActionInput,
            context: &StageContext<'_>,
        ) -> Result<ActionPlan, Error> {
            let existing = context
                .layers
                .iter()
                .find(|layer| layer.effect_id == PATCH_EFFECT);
            let current = existing.map(|layer| layer.payload.clone());
            let payload = Self::merged(current.as_ref().unwrap_or(&json!({})), &input.parameters);
            match (existing, current) {
                (Some(_), Some(current))
                    if Self::channels(&current) == Self::channels(&payload) =>
                {
                    Ok(ActionPlan::NoOp)
                }
                (Some(layer), _) => Ok(ActionPlan::Update(Layer {
                    payload,
                    ..layer.clone()
                })),
                (None, _) if Self::channels(&payload) == [0.0, 0.0] => Ok(ActionPlan::NoOp),
                (None, _) => Ok(ActionPlan::Commit(Layer {
                    id: LayerId::new(),
                    effect_id: PATCH_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload,
                    artifacts: Vec::new(),
                })),
            }
        }
        /// One changed field names itself, so a slider's history row says what moved.
        fn label(&self, input: &ActionInput) -> Option<String> {
            match input.parameters.len() {
                1 => input.parameters.iter().next().map(|(name, value)| {
                    format!("Patch {name} {}", value.as_f64().unwrap_or_default())
                }),
                _ => None,
            }
        }
        fn validate_payload(&self, _: &str, format: u32, payload: &Value) -> Result<(), Error> {
            if format != EFFECT_FORMAT {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!("unsupported effect format {format}"),
                ));
            }
            let object = payload
                .as_object()
                .ok_or_else(|| validation("patch payload must be an object"))?;
            for (name, value) in object {
                if !["red", "green"].contains(&name.as_str())
                    || !value
                        .as_f64()
                        .is_some_and(|value| (0.0..=255.0).contains(&value))
                {
                    return Err(validation(format!("invalid patch field {name}")));
                }
            }
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
            let [red, green] = Self::channels(payload);
            Ok(format!("Patch {red}, {green}"))
        }
        fn values(&self, _: &str, _: u32, payload: &Value) -> Result<Map<String, Value>, Error> {
            let [red, green] = Self::channels(payload);
            Ok(json!({"red": red, "green": green})
                .as_object()
                .expect("an object")
                .clone())
        }
        fn compile(&self, _: &str, _: u32, payload: &Value, _: Stage) -> Result<Processing, Error> {
            let [red, green] = Self::channels(payload);
            Ok(Processing::PointReplace {
                x: 0,
                y: 0,
                rgb: [red as u8, green as u8, 0],
            })
        }
    }

    pub(crate) const STAGE_EFFECT: &str = "test.stage.effect";
    pub(crate) const STAGE_ACTION: &str = "set-stage";

    /// A module whose one effect declares any stage and any order and compiles to an identity
    /// colour operation. Placement, the order within a stage and the one refused order are
    /// properties of the host, so they are proved with this rather than with a real tool: a spatial
    /// or finish effect has no processing primitive of its own yet.
    pub(crate) struct StageModule(ModuleDescriptor);

    impl StageModule {
        pub(crate) fn shared(
            id: &str,
            effect: &str,
            action: &str,
            stage: EffectStage,
            order: u16,
        ) -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: id.into(),
                title: "Stage".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: effect.into(),
                    format: EFFECT_FORMAT,
                    stage,
                    order,
                    artifacts: false,
                }],
                actions: vec![ActionDescriptor {
                    id: action.into(),
                    title: "Set stage".into(),
                    notes: "commits one layer of this module's effect".into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            }))
        }
    }

    impl ToolModule for StageModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: self.0.effects[0].id.clone(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
                artifacts: Vec::new(),
            }))
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("stage layer of {effect_id}"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::neutral()))
        }
    }

    pub(crate) fn test_layer(effect: &str) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            artifacts: Vec::new(),
        }
    }

    fn source() -> SourceImage {
        SourceImage {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255].into(),
            fingerprint: "sha256:test".into(),
            orientation: 1,
        }
    }

    #[test]
    fn registration_rejects_duplicate_and_invalid_identities_across_modules() {
        let mut registry = ModuleRegistry::builtin();
        assert!(registry.action("set-pixel").is_some());
        assert!(registry.action("transform").is_some());
        assert!(registry.effect(PIXEL_EFFECT).is_some());
        assert!(registry.effect(ORIENTATION_EFFECT).is_some());
        assert!(registry.action("crop").is_some());
        assert!(registry.effect(CROP_EFFECT).is_some());
        assert!(registry.action("set-basic").is_some());
        assert!(registry.action("reset-basic").is_some());
        assert!(registry.effect(BASIC_EFFECT).is_some());
        assert!(registry.action("set-mixer").is_some());
        assert!(registry.action("reset-mixer").is_some());
        assert!(registry.effect(crate::MIXER_EFFECT).is_some());
        assert!(registry.action("set-raw-exposure").is_some());
        assert!(registry.action("reset-raw").is_some());
        assert!(registry.effect(RAW_EFFECT).is_some());
        assert!(registry.action("set-vignette").is_some());
        assert!(registry.action("reset-vignette").is_some());
        assert!(registry.effect(crate::VIGNETTE_EFFECT).is_some());
        assert!(registry.action("set-presence").is_some());
        assert!(registry.action("reset-presence").is_some());
        assert!(registry.effect(crate::PRESENCE_EFFECT).is_some());
        assert!(registry.action("apply-preset").is_some());
        assert_eq!(registry.descriptors().len(), 9);
        assert!(registry.action("edit.set-pixel").is_none());

        for (case, module) in [
            (
                "duplicate module",
                TestModule::shared(
                    "lightwell.pixel",
                    "test.other",
                    "test-other",
                    Availability::Available,
                ),
            ),
            (
                "duplicate effect",
                TestModule::shared(
                    "test.module",
                    PIXEL_EFFECT,
                    "test-other",
                    Availability::Available,
                ),
            ),
            (
                "duplicate action",
                TestModule::shared(
                    "test.module",
                    "test.effect",
                    "set-pixel",
                    Availability::Available,
                ),
            ),
            (
                "invalid module identity",
                TestModule::shared(
                    "Test Module",
                    "test.effect",
                    "test-other",
                    Availability::Available,
                ),
            ),
        ] {
            let error = registry.register(module).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        }
        assert_eq!(
            registry.descriptors().len(),
            9,
            "nothing was half-registered"
        );
        assert!(
            registry
                .register(TestModule::shared(
                    "test.module",
                    "test.effect",
                    "test-action",
                    Availability::Available
                ))
                .is_ok()
        );
        assert_eq!(registry.descriptors().len(), 10);
    }

    /// A module that declares no effects owns no layer and claims no effect identity, so it
    /// registers like any other and its actions dispatch. The presets module is one.
    #[test]
    fn a_module_that_declares_no_effects_registers() {
        let mut registry = ModuleRegistry::builtin();
        let (presets, _) = registry.action("apply-preset").expect("the presets module");
        assert!(presets.descriptor().effects.is_empty());
        let mut descriptor = TestModule::new(
            "test.effectless",
            "test.unused",
            "test-effectless",
            Availability::Available,
        )
        .0;
        descriptor.effects.clear();
        registry
            .register(TestModule::from_descriptor(descriptor))
            .expect("a module without effects registers");
        let (module, _) = registry
            .action("test-effectless")
            .expect("its action is dispatched");
        assert_eq!(module.descriptor().id, "test.effectless");
        assert!(registry.effect("test.unused").is_none());
    }

    /// A module whose canvas claims one mode-strip letter.
    fn shortcut_module(id: &str, effect: &str, action: &str, letter: &str) -> Arc<dyn ToolModule> {
        let coordinate = |name: &str| crate::ParameterDescriptor {
            name: name.into(),
            kind: crate::ParameterKind::Integer { min: 0, max: 100 },
            required: true,
            default: None,
            unit: None,
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        };
        let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
        descriptor.actions[0].parameters = vec![coordinate("x"), coordinate("y")];
        descriptor.canvas = Some(crate::CanvasInteraction::PointPick {
            action: action.into(),
            x: "x".into(),
            y: "y".into(),
            title: "Test mode".into(),
            shortcut: Some(letter.into()),
        });
        // A pick canvas is reached from the panel, so it declares its picker control.
        descriptor.controls = vec![crate::Control::Picker {
            label: "Test mode".into(),
        }];
        TestModule::from_descriptor(descriptor)
    }

    #[test]
    fn one_canvas_shortcut_letter_selects_one_mode_across_the_registry() {
        let mut registry = ModuleRegistry::builtin();
        assert_eq!(
            registry
                .effect(CROP_EFFECT)
                .expect("the crop module")
                .0
                .descriptor()
                .canvas
                .as_ref()
                .and_then(crate::CanvasInteraction::shortcut),
            Some("R"),
            "the built-in crop mode claims R"
        );
        let error = registry
            .register(shortcut_module(
                "test.one",
                "test.one.effect",
                "test-one",
                "R",
            ))
            .expect_err("R is already claimed by the crop module");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("canvas shortcut R is already claimed"),
            "{error}"
        );
        registry
            .register(shortcut_module(
                "test.two",
                "test.two.effect",
                "test-two",
                "K",
            ))
            .expect("a free letter registers");
        let error = registry
            .register(shortcut_module(
                "test.three",
                "test.three.effect",
                "test-three",
                "K",
            ))
            .expect_err("K is now claimed too");
        assert_eq!(error.kind, ErrorKind::Validation);
    }

    /// A query identity is unique across the whole registry, so `query.<id>` can never resolve to
    /// two providers; it is its own namespace, so it does not collide with an action of that name.
    #[test]
    fn one_query_identity_resolves_to_one_provider_across_the_registry() {
        let mut registry = ModuleRegistry::builtin();
        let (module, query) = registry
            .query("neutral-sample")
            .expect("the Basic module declares the neutral picker");
        assert_eq!(module.descriptor().id, "lightwell.basic");
        assert_eq!(query.id, "neutral-sample");
        assert!(!query.patch);
        assert!(
            registry.query("set-basic").is_none(),
            "queries are separate"
        );
        assert!(registry.action("neutral-sample").is_none());

        let with_query = |id: &str, effect: &str, action: &str, query: &str| {
            let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
            descriptor.queries = vec![ActionDescriptor {
                id: query.into(),
                title: "Test query".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: Vec::new(),
            }];
            TestModule::from_descriptor(descriptor)
        };
        let error = registry
            .register(with_query(
                "test.one",
                "test.one.effect",
                "test-one",
                "neutral-sample",
            ))
            .expect_err("the Basic module already provides that query");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("query neutral-sample is already provided by lightwell.basic"),
            "{error}"
        );
        // An action may still be named after a query of another module: different namespaces.
        registry
            .register(with_query(
                "test.two",
                "test.two.effect",
                "neutral-sample",
                "test-query",
            ))
            .expect("an action named after another module's query is free");
        assert_eq!(
            registry
                .query("test-query")
                .expect("the new query")
                .0
                .descriptor()
                .id,
            "test.two"
        );
        assert_eq!(
            registry
                .query("neutral-sample")
                .expect("still the Basic module's")
                .0
                .descriptor()
                .id,
            "lightwell.basic"
        );
    }

    /// A descriptor carrying queries and a sample-apply canvas round-trips through JSON, so a
    /// client reads exactly what the registry validated.
    #[test]
    fn queries_and_the_sample_apply_canvas_survive_a_json_round_trip() {
        let registry = ModuleRegistry::builtin();
        let basic = registry
            .effect(BASIC_EFFECT)
            .expect("the Basic module")
            .0
            .descriptor();
        let encoded = serde_json::to_value(basic).expect("a serializable descriptor");
        assert_eq!(encoded["queries"][0]["id"], json!("neutral-sample"));
        assert_eq!(
            encoded["canvas"],
            json!({
                "kind": "sample-apply",
                "query": "neutral-sample",
                "x": "x",
                "y": "y",
                "action": "set-basic",
                "title": "Neutral picker",
                "shortcut": "W",
            })
        );
        assert_eq!(
            &ModuleDescriptor::parse(&encoded).expect("a valid descriptor"),
            basic
        );
        // A descriptor written before queries existed still reads, with none declared.
        let mut without = encoded.clone();
        let object = without.as_object_mut().expect("an object");
        object.remove("queries");
        object.insert("canvas".into(), Value::Null);
        assert!(
            serde_json::from_value::<ModuleDescriptor>(without)
                .expect("queries are optional")
                .queries
                .is_empty()
        );
    }

    /// The generic check in front of a patch action: the module is handed exactly the fields the
    /// caller named, with no declared default filled in and no required parameter demanded, and it
    /// merges them over the state it already holds.
    #[test]
    fn a_patch_action_is_checked_field_by_field_and_fills_no_defaults() {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(PatchModule::shared())
            .expect("a patch descriptor is valid");
        let (module, action) = registry.action(PATCH_ACTION).expect("the patch action");
        assert!(action.patch);
        let checked = crate::check_parameters(action, &json!({"red": 12})).expect("one field");
        assert_eq!(
            checked,
            json!({"red": 12}).as_object().unwrap().clone(),
            "only the field that was sent, exactly as it was sent"
        );
        assert_eq!(
            crate::check_parameters(action, &json!({}))
                .expect("an empty patch")
                .len(),
            0,
            "a patch fills no declared default"
        );
        for (case, sent, fragment) in [
            (
                "unknown field",
                json!({"blue": 1}),
                "unknown parameter blue",
            ),
            (
                "out of range",
                json!({"red": 300}),
                "parameter red must be a number within 0..=255",
            ),
            (
                "wrong kind",
                json!({"red": "12"}),
                "parameter red must be a number",
            ),
        ] {
            let error = crate::check_parameters(action, &sent).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        // The module merges what it was handed over what it already stores.
        let input = ActionInput {
            action_id: PATCH_ACTION.into(),
            parameters: checked,
        };
        let parsed = module.parse(PATCH_ACTION, &input.parameters).unwrap();
        assert_eq!(parsed.parameters, input.parameters);
        assert_eq!(
            module
                .values(PATCH_EFFECT, EFFECT_FORMAT, &json!({"red": 12.0}))
                .unwrap(),
            json!({"red": 12.0, "green": 0.0})
                .as_object()
                .unwrap()
                .clone(),
            "a stored layer reports every parameter it represents, neutral fields included"
        );
        assert_eq!(
            module.label(&parsed).as_deref(),
            Some("Patch red 12"),
            "one changed field labels its own entry"
        );
        assert_eq!(
            module.label(&ActionInput {
                action_id: PATCH_ACTION.into(),
                parameters: json!({"red": 1, "green": 2}).as_object().unwrap().clone(),
            }),
            None,
            "a module that has nothing to add leaves the label to the host"
        );
    }

    #[test]
    fn built_in_modules_describe_their_stored_layers() {
        let registry = ModuleRegistry::builtin();
        let described = |layer: &Layer| -> String {
            let (module, _) = registry.effect(&layer.effect_id).expect("a provider");
            module
                .describe_layer(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("a stored payload")
        };
        assert_eq!(
            described(&Layer::pixel(3, 4, [1, 2, 3])),
            "Pixel 3, 4 → 1,2,3"
        );
        // An orientation layer holds a composed state, so its row names the orientation it is in,
        // not the actions that reached it. All eight are named and the neutral one says so.
        let orientation = |mirror: bool, turns: u8| -> String {
            described(&Layer::orientation(Orientation { mirror, turns }))
        };
        assert_eq!(orientation(false, 0), "Upright");
        assert_eq!(orientation(false, 1), "Rotate right");
        assert_eq!(orientation(false, 2), "Rotate 180°");
        assert_eq!(orientation(false, 3), "Rotate left");
        assert_eq!(orientation(true, 0), "Mirror horizontal");
        assert_eq!(orientation(true, 1), "Mirror horizontal · Rotate right");
        assert_eq!(orientation(true, 2), "Flip vertical");
        assert_eq!(orientation(true, 3), "Mirror horizontal · Rotate left");
        assert_eq!(
            described(&Layer::crop(crate::CropPayload::NEUTRAL)),
            "Whole image"
        );
    }

    #[test]
    fn an_unavailable_provider_keeps_its_identity_and_fails_evaluation_with_its_layers() {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(TestModule::shared(
                "test.module",
                "test.effect",
                "test-action",
                Availability::Unavailable {
                    reason: "not built in this configuration".into(),
                },
            ))
            .unwrap();
        assert!(
            registry.effect("test.effect").is_some(),
            "an unavailable provider keeps its effect identity"
        );
        let first = test_layer("test.effect");
        let second = test_layer("test.effect");
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![
                first.clone(),
                Layer::orientation(Orientation {
                    mirror: false,
                    turns: 1,
                }),
                second.clone(),
            ],
        };
        let expected = format!(
            "unavailable effect test.effect (layers {}, {})",
            first.id, second.id
        );
        for error in [
            registry.validate_recipe(&recipe).unwrap_err(),
            registry.validate_layer(&first).unwrap_err(),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
        }
        assert_eq!(
            registry.validate_recipe(&recipe).unwrap_err().detail,
            expected
        );
        assert_eq!(
            render(&registry, &source(), SnapshotId::new(), &recipe)
                .unwrap_err()
                .detail,
            expected
        );
        let missing = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![test_layer("test.absent")],
        };
        assert_eq!(
            registry.validate_recipe(&missing).unwrap_err().detail,
            format!(
                "unavailable effect test.absent (layers {})",
                missing.layers[0].id
            )
        );
        let _ = AssetId::new();
    }

    /// Current shapes only: the retired per-action transform effect has no provider, so a stack
    /// holding it is refused exactly like any other unavailable effect. Nothing rewrites it, so
    /// the data survives the refusal and the owner can open it with a build that provides it.
    #[test]
    fn a_stack_holding_the_retired_transform_effect_is_refused_without_being_rewritten() {
        let registry = ModuleRegistry::builtin();
        let retired = Layer {
            id: LayerId::new(),
            effect_id: "lightwell.geometry.transform".into(),
            effect_format: EFFECT_FORMAT,
            payload: json!("rotate-right"),
            artifacts: Vec::new(),
        };
        assert!(registry.effect("lightwell.geometry.transform").is_none());
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![retired.clone()],
        };
        let expected = format!(
            "unavailable effect lightwell.geometry.transform (layers {})",
            retired.id
        );
        for error in [
            registry.validate_recipe(&recipe).unwrap_err(),
            registry.validate_layer(&retired).unwrap_err(),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, expected);
        }
        assert_eq!(recipe.layers, vec![retired], "the refused stack is kept");
    }

    #[test]
    fn payload_format_and_shape_are_validated_by_the_providing_module() {
        let registry = ModuleRegistry::builtin();
        let wrong_format = Layer {
            effect_format: 99,
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        assert_eq!(
            registry.validate_layer(&wrong_format).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let wrong_payload = Layer {
            payload: json!({"x": 1}),
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        assert_eq!(
            registry.validate_layer(&wrong_payload).unwrap_err().kind,
            ErrorKind::Validation
        );
        for wrong_orientation in [
            json!("rotate-sideways"),
            json!({"mirror": false, "turns": 4}),
            json!({"mirror": false, "turns": 0, "flip": true}),
        ] {
            let layer = Layer {
                payload: wrong_orientation.clone(),
                ..Layer::orientation(Orientation::NEUTRAL)
            };
            assert_eq!(
                registry.validate_layer(&layer).unwrap_err().kind,
                ErrorKind::Validation,
                "{wrong_orientation}"
            );
        }
        assert!(
            registry
                .validate_layer(&Layer::orientation(Orientation::NEUTRAL))
                .is_ok()
        );
        assert!(
            registry
                .validate_layer(&Layer::pixel(0, 0, [1, 2, 3]))
                .is_ok()
        );
    }

    #[test]
    fn a_pixel_layer_joins_the_stack_before_the_first_geometry_layer() {
        let registry = ModuleRegistry::builtin();
        let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
        let turn = || {
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            })
        };
        let crop = || {
            Layer::crop(CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            })
        };
        for (case, layers, expected) in [
            ("an empty stack", vec![], 0),
            ("geometry only", vec![turn(), crop()], 0),
            ("pixels only", vec![pixel(), pixel()], 2),
            ("a pixel before the tail", vec![pixel(), crop(), turn()], 1),
            (
                // Such a stack renders as it always did; a new edit still joins the content stage.
                "an interleaved pixel after geometry",
                vec![pixel(), turn(), pixel(), crop()],
                1,
            ),
            (
                "a layer no provider declares does not open the tail",
                vec![test_layer("test.absent"), turn()],
                1,
            ),
        ] {
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Pixel, 0),
                expected,
                "{case}"
            );
            // A colour-stage layer joins the stack by the same rule, so a Basic layer lands before
            // the quarter-turns, reflections and crop that carry it.
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Color, 0),
                expected,
                "{case}: colour joins where a pixel edit does"
            );
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Geometry, 0),
                layers.len(),
                "{case}: geometry extends the tail"
            );
            // The host reads the same rule from an effect's own descriptor.
            assert_eq!(
                registry.insertion_index_for(&layers, PIXEL_EFFECT),
                expected,
                "{case}: the pixel effect's own placement"
            );
        }
        assert_eq!(
            registry.effect_stage(PIXEL_EFFECT),
            Some(EffectStage::Pixel)
        );
        assert_eq!(
            registry.effect_stage(CROP_EFFECT),
            Some(EffectStage::Geometry)
        );
        assert_eq!(registry.effect_stage("test.absent"), None);
    }

    pub(crate) const MIXER_EFFECT: &str = "test.mixer.effect";
    pub(crate) const SPATIAL_EFFECT: &str = "test.spatial.effect";
    pub(crate) const FINISH_EFFECT: &str = "test.finish.effect";

    /// The built-ins plus one colour effect of order 10, one spatial effect and one finish effect,
    /// which is every stage and two orders within the colour stage.
    pub(crate) fn staged_registry() -> ModuleRegistry {
        let mut registry = ModuleRegistry::builtin();
        for (id, effect, action, stage, order) in [
            (
                "test.mixer",
                MIXER_EFFECT,
                // Distinct from the real mixer module's own "set-mixer" action, which
                // `ModuleRegistry::builtin()` now registers.
                "set-test-mixer",
                EffectStage::Color,
                10,
            ),
            (
                "test.spatial",
                SPATIAL_EFFECT,
                "set-spatial",
                EffectStage::Spatial,
                0,
            ),
            (
                "test.finish",
                FINISH_EFFECT,
                "set-finish",
                EffectStage::Finish,
                0,
            ),
        ] {
            registry
                .register(StageModule::shared(id, effect, action, stage, order))
                .expect("a valid test module");
        }
        registry
    }

    /// Every stage's region, over stacks that mix them all: a pixel or colour layer joins the
    /// content region before the first spatial, geometry or finish layer, a spatial layer follows
    /// the pointwise work and precedes the tail, a geometry layer precedes the first finish layer
    /// and a finish layer goes last, with a leading source layer always keeping index zero.
    #[test]
    fn every_stage_joins_the_region_the_placement_table_names() {
        let registry = staged_registry();
        let source = || test_layer(RAW_EFFECT);
        let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
        let basic = || test_layer(BASIC_EFFECT);
        let mixer = || test_layer(MIXER_EFFECT);
        let spatial = || test_layer(SPATIAL_EFFECT);
        let turn = || {
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            })
        };
        let finish = || test_layer(FINISH_EFFECT);
        // (case, stack, pixel, colour order 0, colour order 10, spatial, geometry, finish)
        for (case, layers, expected) in [
            ("an empty stack", vec![], [0, 0, 0, 0, 0, 0]),
            (
                "the canonical stack of every stage",
                vec![
                    source(),
                    pixel(),
                    basic(),
                    mixer(),
                    spatial(),
                    turn(),
                    finish(),
                ],
                [4, 3, 4, 5, 6, 7],
            ),
            ("a source layer alone", vec![source()], [1, 1, 1, 1, 1, 1]),
            (
                "the geometry tail only",
                vec![turn(), turn()],
                [0, 0, 0, 0, 2, 2],
            ),
            (
                "a finish layer over the tail",
                vec![turn(), finish()],
                [0, 0, 0, 0, 1, 2],
            ),
            (
                "a spatial layer before the tail",
                vec![basic(), spatial(), turn()],
                [1, 1, 1, 2, 3, 3],
            ),
            // The colour region is read in order: an order-0 layer goes before an order-10 one
            // whichever was committed first, and neither existing layer moves.
            (
                "one colour layer of order 10",
                vec![mixer()],
                [1, 0, 1, 1, 1, 1],
            ),
            (
                "one colour layer of order 0",
                vec![basic()],
                [1, 1, 1, 1, 1, 1],
            ),
            (
                "both colour orders before the tail",
                vec![basic(), mixer(), turn()],
                [2, 1, 2, 2, 3, 3],
            ),
            (
                // A stored stack the host did not build keeps every layer where it is; the region
                // still ends at the first layer of a later stage.
                "a pixel layer stored after the tail",
                vec![turn(), pixel()],
                [0, 0, 0, 0, 2, 2],
            ),
        ] {
            let placement = [
                registry.insertion_index(&layers, EffectStage::Pixel, 0),
                registry.insertion_index(&layers, EffectStage::Color, 0),
                registry.insertion_index(&layers, EffectStage::Color, 10),
                registry.insertion_index(&layers, EffectStage::Spatial, 0),
                registry.insertion_index(&layers, EffectStage::Geometry, 0),
                registry.insertion_index(&layers, EffectStage::Finish, 0),
            ];
            assert_eq!(placement, expected, "{case}");
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Source, 0),
                0,
                "{case}: a source layer prepares the content stage"
            );
            // The same answers through the effects' own descriptors.
            assert_eq!(
                [
                    registry.insertion_index_for(&layers, PIXEL_EFFECT),
                    registry.insertion_index_for(&layers, BASIC_EFFECT),
                    registry.insertion_index_for(&layers, MIXER_EFFECT),
                    registry.insertion_index_for(&layers, SPATIAL_EFFECT),
                    registry.insertion_index_for(&layers, CROP_EFFECT),
                    registry.insertion_index_for(&layers, FINISH_EFFECT),
                ],
                expected,
                "{case}: read from each effect's descriptor"
            );
        }

        // Committing the two colour orders in either sequence leaves the same stack.
        let mut committed_low_first = vec![basic()];
        committed_low_first.insert(
            registry.insertion_index_for(&committed_low_first, MIXER_EFFECT),
            mixer(),
        );
        let mut committed_high_first = vec![mixer()];
        committed_high_first.insert(
            registry.insertion_index_for(&committed_high_first, BASIC_EFFECT),
            basic(),
        );
        for (case, stack) in [
            ("order 0 first", committed_low_first),
            ("order 10 first", committed_high_first),
        ] {
            assert_eq!(
                stack
                    .iter()
                    .map(|layer| layer.effect_id.as_str())
                    .collect::<Vec<_>>(),
                vec![BASIC_EFFECT, MIXER_EFFECT],
                "{case}: the declared order decides, not the commit sequence"
            );
        }

        // `module.list` reports the stage and the order of every effect.
        let descriptors = serde_json::to_value(registry.descriptors()).expect("descriptor JSON");
        let effect_of = |module: &str| {
            descriptors
                .as_array()
                .expect("an array")
                .iter()
                .find(|descriptor| descriptor["id"] == json!(module))
                .expect("a registered module")["effects"][0]
                .clone()
        };
        assert_eq!(
            effect_of("test.mixer"),
            json!({"id": MIXER_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "order": 10})
        );
        assert_eq!(
            effect_of("test.spatial"),
            json!({"id": SPATIAL_EFFECT, "format": EFFECT_FORMAT, "stage": "spatial", "order": 0})
        );
        assert_eq!(
            effect_of("test.finish"),
            json!({"id": FINISH_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0})
        );
    }

    /// The one order the host cannot evaluate: a finish layer is defined in the output coordinates
    /// the geometry tail produced, so a geometry layer after it has no stage to address. The stack
    /// is refused as it stands, and nothing is rewritten, reordered or dropped.
    #[test]
    fn a_finish_layer_before_a_geometry_layer_is_refused_by_compilation() {
        let registry = staged_registry();
        let finish = test_layer(FINISH_EFFECT);
        let turn = Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        });
        let refused = vec![finish.clone(), turn.clone()];
        let error = registry
            .compile_layers(2, 1, &refused)
            .err()
            .expect("a finish layer before geometry never compiles");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.starts_with("finish layer precedes geometry"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains(finish.id.as_str()));
        assert!(error.detail.contains(turn.id.as_str()));
        assert_eq!(refused.len(), 2, "the refused stack is kept as it stands");
        // The order the host does build compiles, and so does a stack with no geometry at all.
        assert!(
            registry
                .compile_layers(2, 1, &[turn, finish.clone()])
                .is_ok()
        );
        assert!(registry.compile_layers(2, 1, &[finish]).is_ok());
    }

    const BOUND_EFFECT: &str = "test.bound.effect";

    /// An identity colour unit that names the artifact it was compiled with, so a compiled stack
    /// shows which artifacts its module received and in what order.
    struct Named(String);

    impl crate::PointwiseColor for Named {
        fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {}
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            self.0.clone()
        }
    }

    /// A colour effect that declares artifacts and compiles one named unit per bound artifact.
    struct BoundModule(ModuleDescriptor);

    impl ToolModule for BoundModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok("bound".into())
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::neutral()))
        }
        fn compile_bound(
            &self,
            _: &str,
            _: u32,
            _: &Value,
            _: Stage,
            artifacts: &[Arc<crate::artifacts::PreparedArtifact>],
        ) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::new(
                artifacts
                    .iter()
                    .map(|artifact| {
                        Arc::new(Named(artifact.id.to_string())) as Arc<dyn crate::PointwiseColor>
                    })
                    .collect(),
            )))
        }
    }

    #[test]
    fn compile_binds_artifacts_in_listed_order_and_refuses_unprepared_ones() {
        let descriptor = ModuleDescriptor::parse(&json!({
            "id": "test.bound",
            "title": "Bound",
            "effects": [{"id": BOUND_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "artifacts": true}],
            "actions": [],
            "controls": [],
            "availability": {"kind": "available"},
        }))
        .unwrap();
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(BoundModule(descriptor)))
            .unwrap();
        let artifact = |digit: &str| {
            let id = crate::ArtifactId::for_hash(&format!("{digit}{}", "d".repeat(63))).unwrap();
            let meta = crate::artifacts::ArtifactMeta {
                kind: "test".into(),
                width: None,
                height: None,
                colour: None,
            };
            crate::artifacts::register_prepared(Arc::new(crate::artifacts::PreparedArtifact::new(
                id,
                &meta,
                vec![0].into(),
            )))
        };
        let (first, second) = (artifact("1"), artifact("2"));
        let layer = Layer {
            artifacts: vec![second.id.clone(), first.id.clone()],
            ..test_layer(BOUND_EFFECT)
        };
        let compiled = registry
            .compile_layers(2, 1, std::slice::from_ref(&layer))
            .unwrap();
        let Processing::Color(operation) = &compiled.segments[0].operations[0] else {
            panic!("a colour operation");
        };
        let named: Vec<String> = operation
            .units()
            .iter()
            .map(|unit| unit.describe())
            .collect();
        assert_eq!(
            named,
            [second.id.to_string(), first.id.to_string()],
            "the module receives the layer's order"
        );
        // A layer without artifacts is compiled exactly as before, through `compile`.
        let plain = registry
            .compile_layers(2, 1, &[test_layer(BOUND_EFFECT)])
            .unwrap();
        assert!(plain.segments[0].operations.is_empty());
        // Bytes nobody holds are not prepared, and the stack is refused rather than evaluated
        // without them.
        let missing = second.id.clone();
        drop(second);
        let error = registry
            .compile_layers(2, 1, std::slice::from_ref(&layer))
            .err()
            .expect("an unprepared artifact never compiles");
        assert_eq!(error.kind, ErrorKind::SourceUnavailable);
        assert_eq!(error.detail, format!("artifact {missing} is not prepared"));
        // An effect that does not declare artifacts cannot be compiled with any.
        let pixel = Layer {
            artifacts: vec![first.id.clone()],
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        let error = registry
            .compile_layers(2, 1, &[pixel])
            .err()
            .expect("a pixel layer never binds an artifact");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("which its effect does not declare"));
    }
}
