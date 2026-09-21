//! The provider index: descriptors validated once at registration, then hash lookups by effect
//! and action identity. Registration touches no image or catalog resource.
use super::{
    ActionDescriptor, BasicModule, CanvasInteraction, CropModule, EffectDescriptor, EffectStage,
    MAX_COLOR_UNITS, ModuleDescriptor, PixelModule, Processing, Stage, ToolModule, TransformModule,
};
use crate::{
    Error, ErrorKind, Layer, RECIPE_FORMAT, Recipe,
    render::{Compiled, Segment},
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
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for module in [
            Arc::new(PixelModule::new()) as Arc<dyn ToolModule>,
            Arc::new(BasicModule::new()),
            Arc::new(TransformModule::new()),
            Arc::new(CropModule::new()),
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

    pub fn effect(&self, id: &str) -> Option<(&dyn ToolModule, &EffectDescriptor)> {
        let (module, position) = self.effects.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().effects[*position]))
    }

    /// The stage an effect's payload addresses, or `None` when no provider declares it.
    pub fn effect_stage(&self, effect_id: &str) -> Option<EffectStage> {
        self.effect(effect_id).map(|(_, effect)| effect.stage)
    }

    /// Where a committed layer of this stage joins a stack. A pixel-stage or colour-stage layer is
    /// inserted immediately before the first geometry-stage layer, so the quarter-turns, reflections
    /// and crop that form the geometry tail carry it and no later geometry change moves or
    /// invalidates it; a geometry-stage layer appends, extending that tail. A layer whose effect no provider
    /// declares does not open the tail: such a stack cannot compile at all, and the host reports
    /// that rather than guessing a position. Cost is `O(layers)` and reads no pixels.
    pub fn insertion_index(&self, layers: &[Layer], stage: EffectStage) -> usize {
        if stage == EffectStage::Geometry {
            return layers.len();
        }
        layers
            .iter()
            .position(|layer| self.effect_stage(&layer.effect_id) == Some(EffectStage::Geometry))
            .unwrap_or(layers.len())
    }

    /// The provider that can evaluate this effect, or `None` when none is registered or the
    /// registered one reports itself unavailable.
    fn provider(&self, effect_id: &str) -> Option<&dyn ToolModule> {
        let (module, _) = self.effect(effect_id)?;
        module.descriptor().is_available().then_some(module)
    }

    /// Structural validation stays in the model; effect availability and payload validation are
    /// the registry's.
    pub fn validate_layer(&self, layer: &Layer) -> Result<(), Error> {
        layer.validate()?;
        let module = self
            .provider(&layer.effect_id)
            .ok_or_else(|| unavailable(&layer.effect_id, vec![layer.id.as_str()]))?;
        module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)
    }

    pub fn validate_recipe(&self, recipe: &Recipe) -> Result<(), Error> {
        recipe.validate()?;
        for layer in &recipe.layers {
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(&recipe.layers, &layer.effect_id))?;
            module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
        }
        Ok(())
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
        for layer in layers {
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
            let processing = module.compile(
                &layer.effect_id,
                layer.effect_format,
                &layer.payload,
                Stage {
                    width: segment.width,
                    height: segment.height,
                },
            )?;
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
                        Some(resample),
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
        Orientation, PIXEL_EFFECT, SnapshotId, SourceImage,
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
                }],
                actions: vec![ActionDescriptor {
                    id: action.into(),
                    title: "Test action".into(),
                    notes: "test".into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                availability,
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
            };
            Arc::new(Self(ModuleDescriptor {
                id: PATCH_MODULE.into(),
                title: "Patch".into(),
                hint: Some("A patched pixel".into()),
                effects: vec![EffectDescriptor {
                    id: PATCH_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                }],
                actions: vec![ActionDescriptor {
                    id: PATCH_ACTION.into(),
                    title: "Set patch".into(),
                    notes: "merges the named channels into the one patch layer".into(),
                    summary: Some("Patch {red} {green}".into()),
                    patch: true,
                    parameters: vec![channel("red"), channel("green")],
                }],
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                availability: Availability::Available,
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

    pub(crate) fn test_layer(effect: &str) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
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
        assert_eq!(registry.descriptors().len(), 4);
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
            4,
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
        assert_eq!(registry.descriptors().len(), 5);
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
                registry.insertion_index(&layers, EffectStage::Pixel),
                expected,
                "{case}"
            );
            // A colour-stage layer joins the stack by the same rule, so a Basic layer lands before
            // the quarter-turns, reflections and crop that carry it.
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Color),
                expected,
                "{case}: colour joins where a pixel edit does"
            );
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Geometry),
                layers.len(),
                "{case}: geometry extends the tail"
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
}
