//! Test modules and stacks the registry's tests share, some of which other tests in the crate use
//! too, and the registration tests.
use super::*;
use crate::modules::raw::RAW_EFFECT;
use crate::{
    ActionDescriptor, BASIC_EFFECT, CROP_EFFECT, Component, ComponentMode, EFFECT_FORMAT,
    EffectDescriptor, Layer, LayerId, Mask, PIXEL_EFFECT, Recipe, SourceImage,
    modules::{
        ActionInput, ActionPlan, Availability, CapabilityModule, EffectStage, ModuleDescriptor,
        Processing, StageContext,
    },
};
use luxforge_testbase::Gate;
use serde_json::{Map, Value, json};

/// A minimal module used to prove registration rules and missing-provider behavior.
pub(crate) struct TestModule(pub(super) ModuleDescriptor);

impl TestModule {
    pub(crate) fn new(id: &str, effect: &str, action: &str, availability: Availability) -> Self {
        Self(ModuleDescriptor {
            id: id.into(),
            title: "Test".into(),
            hint: None,
            effects: vec![EffectDescriptor::new(effect, EffectStage::Pixel)],
            actions: vec![ActionDescriptor::new(action, "Test action", "test")],
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
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!(
            "test layer of {effect_id}"
        )))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::internal("test module never renders"))
    }
    /// A test writes any descriptor, capability declarations included, so the module offers
    /// the default hooks for whatever it declares.
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for TestModule {}

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
        let channel = |name: &str| {
            crate::ParameterDescriptor::number(name, 0.0, 255.0)
                .default(json!(0.0))
                .unit("code")
                .step(1.0)
                .precision(0)
                .notes(format!("the {name} channel of the replaced pixel"))
        };
        Arc::new(Self(ModuleDescriptor {
            id: PATCH_MODULE.into(),
            title: "Patch".into(),
            hint: Some("A patched pixel".into()),
            effects: vec![EffectDescriptor::new(PATCH_EFFECT, EffectStage::Pixel)],
            actions: vec![ActionDescriptor {
                patch: true,
                parameters: vec![channel("red"), channel("green")],
                ..ActionDescriptor::new(
                    PATCH_ACTION,
                    "Set patch",
                    "merges the named channels into the one patch layer",
                )
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
        let field =
            |name: &str, current: f64| fields.get(name).and_then(Value::as_f64).unwrap_or(current);
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
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let existing = context
            .layers
            .iter()
            .find(|layer| layer.effect_id == PATCH_EFFECT);
        let current = existing.map(|layer| layer.payload.clone());
        let payload = Self::merged(current.as_ref().unwrap_or(&json!({})), &input.parameters);
        match (existing, current) {
            (Some(_), Some(current)) if Self::channels(&current) == Self::channels(&payload) => {
                Ok(ActionPlan::NoOp)
            }
            (Some(layer), _) => Ok(ActionPlan::Update(crate::LayerUpdate::new(
                layer.id.clone(),
                payload,
            ))),
            (None, _) if Self::channels(&payload) == [0.0, 0.0] => Ok(ActionPlan::NoOp),
            (None, _) => Ok(ActionPlan::Commit(crate::NewLayer::new(
                PATCH_EFFECT,
                payload,
            ))),
        }
    }
    /// One changed field names itself, so a slider's history row says what moved.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        match input.parameters.iter().next() {
            Some((name, value)) if input.parameters.len() == 1 => {
                format!("Patch {name} {}", value.as_f64().unwrap_or_default())
            }
            _ => action.title.clone(),
        }
    }
    fn validate_payload(&self, _: &str, format: u32, payload: &Value) -> Result<(), Error> {
        if format != EFFECT_FORMAT {
            return Err(Error::incompatible(format!(
                "unsupported effect format {format}"
            )));
        }
        let object = payload
            .as_object()
            .ok_or_else(|| Error::validation("patch payload must be an object"))?;
        for (name, value) in object {
            if !["red", "green"].contains(&name.as_str())
                || !value
                    .as_f64()
                    .is_some_and(|value| (0.0..=255.0).contains(&value))
            {
                return Err(Error::validation(format!("invalid patch field {name}")));
            }
        }
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, payload: &Value) -> Result<crate::LayerReport, Error> {
        let [red, green] = Self::channels(payload);
        Ok(crate::LayerReport {
            values: json!({"red": red, "green": green})
                .as_object()
                .expect("an object")
                .clone(),
            ..crate::LayerReport::new(format!("Patch {red}, {green}"))
        })
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        payload: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
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
                order,
                ..EffectDescriptor::new(effect, stage)
            }],
            actions: vec![ActionDescriptor::new(
                action,
                "Set stage",
                "commits one layer of this module's effect",
            )],
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
        Ok(ActionPlan::Commit(crate::NewLayer::new(
            self.0.effects[0].id.clone(),
            json!({}),
        )))
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!(
            "stage layer of {effect_id}"
        )))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Ok(Processing::Color(crate::ColorOperation::neutral()))
    }
}

pub(crate) const HELD_EFFECT: &str = "test.held.effect";

pub(crate) const HELD_ACTION: &str = "hold-render";

/// The shared test gate as a pointwise colour unit that leaves its pixels exactly as it found
/// them, so a stack carrying one renders the image it would render without it; all it changes is
/// *when* that render finishes. A render reaches it once per row, so [`Gate::reached`] counts the
/// rows a render has evaluated through the held layer. A test that holds other work, such as a
/// source preparation, passes the same gate from a hook in that work.
///
/// Shut it only while nothing samples a stack that holds the layer: a point sample evaluates
/// the same unit on the calling thread, so the caller would wait with it.
impl crate::PointwiseColor for Gate {
    fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {
        self.pass();
    }
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "held render".into()
    }
}

/// A module whose one colour effect compiles to the shared test [`Gate`]. A test that is about the
/// analysis worker's slots commits one of these layers and shuts the gate: the job on the
/// worker then stays there until the test opens it, so what the single pending slot does is
/// decided by the queue's rule and never by how fast this machine renders a frame.
pub(crate) struct HeldModule {
    descriptor: ModuleDescriptor,
    gate: Arc<Gate>,
}

impl HeldModule {
    pub(crate) fn shared(gate: Arc<Gate>) -> Arc<dyn ToolModule> {
        Arc::new(Self {
            descriptor: ModuleDescriptor {
                id: "test.held".into(),
                title: "Held".into(),
                hint: None,
                effects: vec![EffectDescriptor::new(HELD_EFFECT, EffectStage::Color)],
                actions: vec![ActionDescriptor::new(
                    HELD_ACTION,
                    "Hold render",
                    "commits one colour layer whose render waits for the test's gate",
                )],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
            gate,
        })
    }
}

impl ToolModule for HeldModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: Map::new(),
        })
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::Commit(crate::NewLayer::new(
            HELD_EFFECT,
            json!({}),
        )))
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new("held render"))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Ok(Processing::Color(crate::ColorOperation::new(vec![
            self.gate.clone(),
        ])))
    }
}

pub(crate) fn test_layer(effect: &str) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: effect.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        mask: None,
        artifacts: Vec::new(),
    }
}

pub(super) fn source() -> SourceImage {
    SourceImage {
        width: 2,
        height: 1,
        rgba: vec![1, 2, 3, 255, 4, 5, 6, 255].into(),
        fingerprint: "sha256:test".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

#[test]
fn registration_rejects_duplicate_and_invalid_identities_across_modules() {
    // Every linked module, the test modules included, registers with every action and effect it
    // declares; which ones those are is the committed descriptor snapshot's
    // (`tests/modules/descriptors.rs`) and the developer registry's.
    let mut registry = ModuleRegistry::developer();
    let builtin = linked_modules(true);
    for module in &builtin {
        let descriptor = module.descriptor();
        for action in &descriptor.actions {
            assert!(registry.action(&action.id).is_some(), "{}", action.id);
        }
        for effect in &descriptor.effects {
            assert!(registry.effect(&effect.id).is_some(), "{}", effect.id);
        }
    }
    assert_eq!(registry.descriptors().len(), builtin.len());
    assert!(registry.action("edit.set-pixel").is_none());

    for (case, module) in [
        (
            "duplicate module",
            TestModule::shared(
                "luxforge.pixel",
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
        builtin.len(),
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
    assert_eq!(registry.descriptors().len(), builtin.len() + 1);
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

/// An effect's `sources` names the kinds a layer of it may exist on, by the tags a photo's source
/// reports, and none is every kind. A module applies to a kind when any of its effects does or it
/// declares none; the built-in RAW development alone is restricted, to RAW. A kind listed twice
/// and a kind no source reports are refused.
#[test]
fn registry_resolves_applicability_from_the_declared_sources() {
    use crate::{SourceKind, SourceTag};
    // The tags are the ones a source serializes in `kind`; a RAW source's needs its camera
    // interpretation, so its spelling is the variant's, `raw`.
    assert_eq!(
        serde_json::to_value(SourceKind::Jpeg).unwrap()["kind"],
        serde_json::to_value(SourceKind::Jpeg.tag()).unwrap()
    );
    assert_eq!(serde_json::to_value(SourceTag::Raw).unwrap(), json!("raw"));
    let module = |effects: Vec<EffectDescriptor>| {
        let descriptor = ModuleDescriptor {
            id: "test.kinds".into(),
            title: "Kinds".into(),
            effects,
            ..ModuleDescriptor::default()
        };
        descriptor.validate().map(|()| descriptor)
    };
    let effect = |id: &str, sources: &[SourceTag]| EffectDescriptor {
        sources: sources.to_vec(),
        ..EffectDescriptor::new(id, EffectStage::Color)
    };
    for (case, effects, jpeg, raw) in [
        ("no effect", vec![], true, true),
        (
            "an effect that lists no kind",
            vec![effect("test.a", &[])],
            true,
            true,
        ),
        (
            "a RAW effect",
            vec![effect("test.a", &[SourceTag::Raw])],
            false,
            true,
        ),
        (
            "a JPEG effect beside a RAW one",
            vec![
                effect("test.a", &[SourceTag::Raw]),
                effect("test.b", &[SourceTag::Jpeg]),
            ],
            true,
            true,
        ),
        (
            "both kinds listed",
            vec![effect("test.a", &[SourceTag::Jpeg, SourceTag::Raw])],
            true,
            true,
        ),
    ] {
        let descriptor = module(effects).unwrap_or_else(|error| panic!("{case}: {error:?}"));
        assert_eq!(descriptor.applies_to(SourceTag::Jpeg), jpeg, "{case}");
        assert_eq!(descriptor.applies_to(SourceTag::Raw), raw, "{case}");
        let refusal = descriptor.check_applies_to(SourceTag::Jpeg);
        if jpeg {
            refusal.unwrap();
        } else {
            let error = refusal.unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Validation);
            assert_eq!(error.detail, "Kinds does not apply to a JPEG photo");
            assert_eq!(
                error.data.as_deref(),
                Some(&json!({"source": "jpeg", "module_id": descriptor.id})),
                "{case}"
            );
        }
    }
    let error = module(vec![effect("test.a", &[SourceTag::Raw, SourceTag::Raw])]).unwrap_err();
    assert!(
        error.detail.contains("lists source kind RAW twice"),
        "a repeated kind: {}",
        error.detail
    );

    let registry = ModuleRegistry::builtin();
    for module in registry.descriptors() {
        for effect in &module.effects {
            let expected: &[SourceTag] =
                if [RAW_EFFECT, crate::LOOK_EFFECT].contains(&effect.id.as_str()) {
                    &[SourceTag::Raw]
                } else {
                    &[]
                };
            assert_eq!(effect.sources, expected, "{}", effect.id);
        }
    }
}

/// A module whose canvas claims one mode-strip letter.
fn shortcut_module(id: &str, effect: &str, action: &str, letter: &str) -> Arc<dyn ToolModule> {
    let coordinate = |name: &str| {
        crate::ParameterDescriptor::integer(name, 0, 100)
            .required(true)
            .notes("test")
    };
    let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
    descriptor.actions[0].parameters = vec![coordinate("x"), coordinate("y")];
    descriptor.canvas = Some(crate::CanvasInteraction::PointPick {
        action: action.into(),
        x: "x".into(),
        y: "y".into(),
        title: "Test mode".into(),
        shortcut: Some(letter.into()),
        icon: None,
        commit: false,
    });
    // A pick canvas is reached from the panel, so it declares its picker control.
    descriptor.controls = vec![crate::Control::Picker(crate::PickerControl {
        label: "Test mode".into(),
        variants: Vec::new(),
    })];
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

/// The one assembly both binaries use: the test modules — the pixel and controls proofs, whose
/// descriptors declare `developer` — join only a developer run, in their linked places, and the
/// capability proof only a developer run that names a proof endpoint. `--disable-module` registers
/// a served module unavailable and refuses one the run does not serve, and a proof endpoint outside
/// developer mode is refused in the words both binaries print.
#[test]
fn the_one_assembly_serves_test_modules_only_in_developer_mode() {
    let assemble = |disabled: &[&str], developer: bool, proof_endpoint: Option<&str>| {
        let disabled: Vec<String> = disabled.iter().map(|id| (*id).to_owned()).collect();
        ModuleRegistry::assemble(&RegistryOptions {
            disabled: &disabled,
            developer,
            proof_endpoint,
        })
    };
    let ids = |registry: &ModuleRegistry| {
        registry
            .descriptors()
            .iter()
            .map(|module| module.id.clone())
            .collect::<Vec<_>>()
    };
    let available = |registry: &ModuleRegistry, id: &str| {
        registry
            .descriptors()
            .iter()
            .find(|module| module.id == id)
            .unwrap_or_else(|| panic!("{id} is registered"))
            .is_available()
    };

    let ordinary = assemble(&[], false, None).unwrap();
    assert_eq!(ids(&ordinary), ids(&ModuleRegistry::builtin()));
    assert!(
        ordinary
            .descriptors()
            .iter()
            .all(|module| !module.developer)
    );
    assert!(ordinary.action("set-pixel").is_none());
    assert!(ordinary.effect(PIXEL_EFFECT).is_none());

    let developer = assemble(&[], true, None).unwrap();
    assert_eq!(ids(&developer), ids(&ModuleRegistry::developer()));
    assert_eq!(
        ids(&developer),
        [
            "luxforge.crop",
            "luxforge.presets",
            "luxforge.pixel",
            "luxforge.raw",
            "luxforge.look",
            "luxforge.basic",
            "luxforge.curve",
            "luxforge.detail",
            "luxforge.presence",
            "luxforge.mixer",
            "luxforge.lens",
            "luxforge.perspective",
            "luxforge.vignette",
            "luxforge.controls",
        ]
    );
    let tests: Vec<String> = ids(&developer)
        .into_iter()
        .filter(|id| !ids(&ordinary).contains(id))
        .collect();
    assert_eq!(tests, ["luxforge.pixel", "luxforge.controls"]);
    for id in &tests {
        assert!(developer.module(id).unwrap().descriptor().developer, "{id}");
    }
    assert!(!ids(&developer).contains(&"luxforge.capabilities".to_owned()));

    assert_eq!(
        assemble(&["luxforge.controls"], false, None).unwrap_err(),
        "--disable-module names no registered module: luxforge.controls"
    );
    assert!(!available(
        &assemble(&["luxforge.controls"], true, None).unwrap(),
        "luxforge.controls"
    ));
    let disabled = assemble(&["luxforge.raw"], false, None).unwrap();
    assert!(!available(&disabled, "luxforge.raw"));
    assert_eq!(
        disabled
            .module("luxforge.raw")
            .unwrap()
            .descriptor()
            .availability,
        Availability::Unavailable {
            reason: DISABLED_REASON.into()
        }
    );

    let combined = assemble(&["luxforge.crop"], false, None).unwrap();
    for effect in [crate::CROP_EFFECT, crate::ORIENTATION_EFFECT] {
        let (provider, _) = combined
            .effect(effect)
            .expect("both geometry effects remain declared");
        assert_eq!(provider.descriptor().id, "luxforge.crop");
        assert!(
            !provider.descriptor().is_available(),
            "disabling the combined module refuses both effects"
        );
    }
    assert!(combined.module("luxforge.transform").is_none());

    // The capability proof joins a developer run that names a proof endpoint, and no other.
    let proof = assemble(&[], true, Some("http://127.0.0.1:9")).unwrap();
    let proof_module = proof
        .descriptors()
        .into_iter()
        .find(|module| module.id == "luxforge.capabilities")
        .expect("the capability proof is registered");
    assert!(proof_module.developer);
    assert_eq!(
        proof_module.resources[0].url,
        "http://127.0.0.1:9/proof-palette.bin"
    );
    assert_eq!(
        assemble(&[], false, Some("http://127.0.0.1:9")).unwrap_err(),
        "--proof-endpoint requires developer mode (--developer)"
    );
    let refused = assemble(&[], true, Some("http://example.com")).unwrap_err();
    assert!(refused.contains("proof-palette"), "{refused}");
}

/// One list of built-in modules serves every registry, and registering one of them unavailable
/// keeps everything it declares, with the reason on its availability: its action is refused by
/// name and a stack holding its effect is reported rather than rendered without it.
#[test]
fn a_built_in_registered_unavailable_keeps_its_declarations_and_reports_why() {
    let ids = |descriptors: Vec<&ModuleDescriptor>| {
        descriptors
            .iter()
            .map(|descriptor| descriptor.id.clone())
            .collect::<Vec<_>>()
    };
    let listed: Vec<Arc<dyn ToolModule>> = linked_modules(false);
    assert_eq!(
        ids(ModuleRegistry::builtin().descriptors()),
        ids(listed.iter().map(|module| module.descriptor()).collect()),
    );

    let mut registry = ModuleRegistry::new();
    for module in linked_modules(false) {
        if module.descriptor().id == "luxforge.basic" {
            registry.register_unavailable(module, "switched off")
        } else {
            registry.register(module)
        }
        .unwrap();
    }
    let (basic, _) = registry
        .action("set-basic")
        .expect("the action stays declared");
    assert_eq!(
        basic.descriptor().availability,
        Availability::Unavailable {
            reason: "switched off".into()
        }
    );
    let mut expected = serde_json::to_value(super::BasicModule::new().descriptor()).unwrap();
    expected["availability"] = json!({"kind": "unavailable", "reason": "switched off"});
    assert_eq!(
        serde_json::to_value(basic.descriptor()).unwrap(),
        expected,
        "every declaration but availability is the module's own"
    );
    let error = registry
        .compile(
            64,
            48,
            &Recipe {
                layers: vec![basic_layer()],
                ..Recipe::default()
            },
        )
        .err()
        .expect("an unavailable effect never compiles");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        error
            .detail
            .starts_with("unavailable effect luxforge.basic.adjust"),
        "{error}"
    );
    assert_eq!(error.unavailable_effect_id(), Some(BASIC_EFFECT), "{error}");
}

/// Every module that checks a stored layer's effect against its own refuses a foreign one as the
/// one unavailable-effect refusal, whose data names the effect: the field-patch modules, the look,
/// pixel,
/// transform, crop and the capability proof. The RAW module refuses a foreign layer as an invalid
/// RAW source layer and the presets module as one it has no effect for; neither is this refusal.
#[test]
fn every_payload_check_names_a_foreign_effect_in_its_data() {
    let registry = ModuleRegistry::developer();
    let proof = crate::CapabilitiesProofModule::new("http://127.0.0.1:9/");
    let mut modules: Vec<&dyn ToolModule> = registry
        .descriptors()
        .into_iter()
        .filter(|descriptor| {
            !descriptor.effects.is_empty() && descriptor.effects.iter().all(|e| e.id != RAW_EFFECT)
        })
        .map(|descriptor| registry.module(&descriptor.id).unwrap().module())
        .collect();
    assert_eq!(
        modules.len(),
        12,
        "basic, look, curve, detail, presence, mixer, lens, perspective, vignette, pixel, crop, \
         controls"
    );
    modules.push(&proof);
    for module in modules {
        let error = module
            .validate_payload("test.nobody", 1, &json!({}))
            .unwrap_err();
        let id = &module.descriptor().id;
        assert_eq!(error.kind, ErrorKind::Incompatible, "{id}");
        assert_eq!(error.detail, "unavailable effect test.nobody", "{id}");
        assert_eq!(
            error.data.as_deref(),
            Some(&json!({"effect_id": "test.nobody"})),
            "{id}"
        );
    }
}

/// One `add` linear gradient at full amount, over the whole frame: the mask every test below
/// attaches to a layer.
pub(super) fn gradient_mask(name: &str) -> Mask {
    let mut mask = Mask::new(name);
    let component = mask.next_component_name("linear");
    mask.components.push(Component::new(
        component,
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
    ));
    mask
}

pub(super) fn basic_layer() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 1.0}),
        mask: None,
        artifacts: Vec::new(),
    }
}

pub(super) fn bound(layer: Layer, mask: &Mask) -> Layer {
    Layer {
        mask: Some(mask.id.clone()),
        ..layer
    }
}

pub(crate) const MIXER_EFFECT: &str = "test.mixer.effect";

pub(crate) const SPATIAL_EFFECT: &str = "test.spatial.effect";

pub(crate) const FINISH_EFFECT: &str = "test.finish.effect";

/// The developer registry, whose pixel proof is the pixel stage, plus one colour effect of order
/// 10, one spatial effect and one finish effect, which is every stage and two orders within the
/// colour stage.
pub(crate) fn staged_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::developer();
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
