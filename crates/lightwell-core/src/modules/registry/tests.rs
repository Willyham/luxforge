//! Test modules and stacks the registry's tests share, some of which other tests in the crate use
//! too, and the registration tests.
use super::*;
use crate::{
    ActionDescriptor, BASIC_EFFECT, CROP_EFFECT, Component, ComponentMode, EFFECT_FORMAT,
    EffectDescriptor, Layer, LayerId, Mask, ORIENTATION_EFFECT, PIXEL_EFFECT, RAW_EFFECT, Recipe,
    SourceImage,
    modules::{ActionInput, ActionPlan, Availability, EffectStage, ModuleDescriptor, StageContext},
};
use serde_json::{Map, Value, json};

/// A minimal module used to prove registration rules and missing-provider behavior.
pub(crate) struct TestModule(pub(super) ModuleDescriptor);

impl TestModule {
    pub(crate) fn new(id: &str, effect: &str, action: &str, availability: Availability) -> Self {
        Self(ModuleDescriptor {
            id: id.into(),
            title: "Test".into(),
            hint: None,
            effects: vec![EffectDescriptor {
                id: effect.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Pixel,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
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
            effects: vec![EffectDescriptor {
                id: PATCH_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Pixel,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
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
                maskable: false,
                artifacts: false,
                single: false,
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
        Ok(ActionPlan::Commit(crate::NewLayer::new(
            self.0.effects[0].id.clone(),
            json!({}),
        )))
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

pub(crate) const HELD_EFFECT: &str = "test.held.effect";

pub(crate) const HELD_ACTION: &str = "hold-render";

/// A gate a test shuts to hold every render that reaches it. It is a pointwise colour unit that
/// leaves its pixels exactly as it found them, so a stack carrying one renders the image it
/// would render without it; all it changes is *when* that render finishes.
///
/// Shut it only while nothing samples a stack that holds the layer: a point sample evaluates
/// the same unit on the calling thread, so the caller would wait with it.
pub(crate) struct RenderGate {
    shut: std::sync::Mutex<bool>,
    opened: std::sync::Condvar,
    /// How many times anything has reached the gate: one per row a render evaluates through it.
    reached: std::sync::atomic::AtomicU64,
}

impl RenderGate {
    /// A gate that is open, which is how a test builds the stack before it holds anything.
    pub(crate) fn open_gate() -> Arc<Self> {
        Arc::new(Self {
            shut: std::sync::Mutex::new(false),
            opened: std::sync::Condvar::new(),
            reached: std::sync::atomic::AtomicU64::new(0),
        })
    }
    /// How many times anything has reached the gate, held or not. A render reaches it once per
    /// row, so this counts the rows a render has evaluated through the held layer.
    pub(crate) fn reached(&self) -> u64 {
        self.reached.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Hold every render that reaches this gate from now on.
    pub(crate) fn shut(&self) {
        *self.shut.lock().expect("the render gate") = true;
    }
    /// Release whatever is waiting and let every later render through.
    pub(crate) fn open(&self) {
        *self.shut.lock().expect("the render gate") = false;
        self.opened.notify_all();
    }
    /// Wait here while the gate is shut. A render reaches it through its colour unit; a test
    /// that holds other work, such as a source preparation, calls it from a hook in that work.
    pub(crate) fn pass(&self) {
        self.reached
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut shut = self.shut.lock().expect("the render gate");
        while *shut {
            shut = self.opened.wait(shut).expect("the render gate");
        }
    }
}

impl crate::PointwiseColor for RenderGate {
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

/// A module whose one colour effect compiles to a [`RenderGate`]. A test that is about the
/// analysis worker's slots commits one of these layers and shuts the gate: the job on the
/// worker then stays there until the test opens it, so what the single pending slot does is
/// decided by the queue's rule and never by how fast this machine renders a frame.
pub(crate) struct HeldModule {
    descriptor: ModuleDescriptor,
    gate: Arc<RenderGate>,
}

impl HeldModule {
    pub(crate) fn shared(gate: Arc<RenderGate>) -> Arc<dyn ToolModule> {
        Arc::new(Self {
            descriptor: ModuleDescriptor {
                id: "test.held".into(),
                title: "Held".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: HELD_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Color,
                    order: 0,
                    artifacts: false,
                    single: false,
                    maskable: false,
                }],
                actions: vec![ActionDescriptor {
                    id: HELD_ACTION.into(),
                    title: "Hold render".into(),
                    notes: "commits one colour layer whose render waits for the test's gate".into(),
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
    fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok("held render".into())
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
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
    let listed: Vec<Arc<dyn ToolModule>> = builtin_modules();
    assert_eq!(
        ids(ModuleRegistry::builtin().descriptors()),
        ids(listed.iter().map(|module| module.descriptor()).collect()),
    );

    let mut registry = ModuleRegistry::new();
    for module in builtin_modules() {
        if module.descriptor().id == "lightwell.basic" {
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
            .starts_with("unavailable effect lightwell.basic.adjust"),
        "{error}"
    );
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
