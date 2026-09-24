//! The exact transform module: quarter turns and reflections as integer coordinate mappings.
//!
//! The four actions are the vocabulary; the stack holds the orientation they compose into. While
//! the orientation layer is the last layer of the stack the next action updates it in place, so a
//! stage carries one layer however many times it is turned or reflected.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, EffectDescriptor,
    EffectStage, ExactGeometry, ModuleDescriptor, ParameterDescriptor, ParameterKind, Processing,
    Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, ORIENTATION_EFFECT, Orientation, Transform};
use serde_json::{Map, Value};

pub(super) const TRANSFORM_ACTION: &str = "transform";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

impl Transform {
    /// The exact input-to-output mapping of this transform at one input stage.
    pub(crate) fn geometry(self, width: u32, height: u32) -> ExactGeometry {
        match self {
            Self::RotateRight => ExactGeometry {
                a: 0,
                b: -1,
                c: 1,
                d: 0,
                tx: i64::from(height) - 1,
                ty: 0,
                output_width: height,
                output_height: width,
            },
            Self::RotateLeft => ExactGeometry {
                a: 0,
                b: 1,
                c: -1,
                d: 0,
                tx: 0,
                ty: i64::from(width) - 1,
                output_width: height,
                output_height: width,
            },
            Self::MirrorHorizontal => ExactGeometry {
                a: -1,
                b: 0,
                c: 0,
                d: 1,
                tx: i64::from(width) - 1,
                ty: 0,
                output_width: width,
                output_height: height,
            },
            Self::FlipVertical => ExactGeometry {
                a: 1,
                b: 0,
                c: 0,
                d: -1,
                tx: 0,
                ty: i64::from(height) - 1,
                output_width: width,
                output_height: height,
            },
        }
    }
}

impl Orientation {
    /// The orientation reached by applying `transform` to this one.
    ///
    /// Mirror-first is a normal form because `M ∘ R^k = R^(-k) ∘ M`, and flip vertical is
    /// `R^2 ∘ M`: a quarter turn only moves `turns`, and a reflection also reverses their
    /// direction. Integer arithmetic on two small fields; nothing is allocated.
    pub fn then(self, transform: Transform) -> Self {
        let turns = u32::from(self.turns);
        match transform {
            Transform::RotateRight => Self {
                mirror: self.mirror,
                turns: ((turns + 1) % 4) as u8,
            },
            Transform::RotateLeft => Self {
                mirror: self.mirror,
                turns: ((turns + 3) % 4) as u8,
            },
            Transform::MirrorHorizontal => Self {
                mirror: !self.mirror,
                turns: ((4 - turns) % 4) as u8,
            },
            Transform::FlipVertical => Self {
                mirror: !self.mirror,
                turns: ((6 - turns) % 4) as u8,
            },
        }
    }

    /// The orientation one action reaches from the neutral one: what a new layer holds.
    pub fn of(transform: Transform) -> Self {
        Self::NEUTRAL.then(transform)
    }

    /// The exact input-to-output mapping of this orientation at one input stage: the mirror, then
    /// the quarter turns, composed by the host into a single integer mapping. The neutral
    /// orientation compiles to the identity, which leaves the stage and its buffer untouched.
    pub(crate) fn geometry(self, width: u32, height: u32) -> ExactGeometry {
        let mut geometry = ExactGeometry::identity(width, height);
        if self.mirror {
            geometry = geometry.then(Transform::MirrorHorizontal.geometry(width, height));
        }
        for _ in 0..self.turns {
            let turn =
                Transform::RotateRight.geometry(geometry.output_width, geometry.output_height);
            geometry = geometry.then(turn);
        }
        geometry
    }
}

/// The icon a client may draw for one exact transform's control, from the shared icon vocabulary.
/// Each transform is a single exact operation whose label is its icon, so a client that knows the
/// names can draw the four as a row of icon buttons with the labels as tooltips.
fn icon_name(transform: Transform) -> &'static str {
    match transform {
        Transform::RotateLeft => "rotate-left",
        Transform::RotateRight => "rotate-right",
        Transform::MirrorHorizontal => "mirror",
        Transform::FlipVertical => "flip",
    }
}

fn control(transform: Transform, label: &str) -> Control {
    let mut preset = Map::new();
    preset.insert("transform".into(), Value::from(transform.action_id()));
    Control::Action {
        action: TRANSFORM_ACTION.into(),
        label: label.into(),
        preset,
        style: crate::ActionStyle::Default,
        icon: Some(icon_name(transform).into()),
    }
}

#[derive(Debug)]
pub struct TransformModule {
    descriptor: ModuleDescriptor,
}

impl Default for TransformModule {
    fn default() -> Self {
        Self::new()
    }
}

impl TransformModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.transform".into(),
                title: "Transforms".into(),
                hint: Some("Rotate, mirror and flip".into()),
                effects: vec![EffectDescriptor {
                    id: ORIENTATION_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                }],
                actions: vec![ActionDescriptor {
                    id: TRANSFORM_ACTION.into(),
                    title: "Transform".into(),
                    notes: "exact quarter turns and reflections; integer mappings with no interpolation".into(),
                    summary: Some("{transform}".into()),
                    patch: false,
parameters: vec![ParameterDescriptor {
                        name: "transform".into(),
                        kind: ParameterKind::Enum {
                            options: vec![
                                Transform::RotateLeft.action_id().into(),
                                Transform::RotateRight.action_id().into(),
                                Transform::MirrorHorizontal.action_id().into(),
                                Transform::FlipVertical.action_id().into(),
                            ],
                        },
                        required: true,
                        default: None,
                        unit: None,
                        step: None, precision: None,
notes: "the exact transform to compose into the stack's orientation".into(),
                        soft_min: None,
                        soft_max: None,
                        fine_step: None,
                        zero: None,
                    }],
                }],
                queries: Vec::new(),
                controls: vec![Control::Group {
                    label: "Exact transforms".into(),
                    reset: None,
                    controls: vec![
                        control(Transform::RotateLeft, "Rotate left"),
                        control(Transform::RotateRight, "Rotate right"),
                        control(Transform::MirrorHorizontal, "Mirror horizontal"),
                        control(Transform::FlipVertical, "Flip vertical"),
                    ],
                    collapsed: false,
                }],
                reset: None,
                canvas: None,
                developer: false,
                collapsed: true,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
        }
    }
}

fn transform_value(parameters: &Map<String, Value>) -> Result<Transform, Error> {
    let value = parameters
        .get("transform")
        .ok_or_else(|| validation("missing required parameter transform for action transform"))?;
    serde_json::from_value(value.clone())
        .map_err(|error| validation(format!("invalid transform: {error}")))
}

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<Orientation, Error> {
    if effect_id != ORIENTATION_EFFECT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unavailable effect {effect_id}"),
        ));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unsupported effect format {format}"),
        ));
    }
    let orientation: Orientation = serde_json::from_value(payload.clone())
        .map_err(|error| validation(format!("invalid orientation payload: {error}")))?;
    // Only the four quarter turns exist; a larger count is a payload this module cannot mean.
    if orientation.turns > 3 {
        return Err(validation(format!(
            "invalid orientation payload: turns {} is outside 0..3",
            orientation.turns
        )));
    }
    Ok(orientation)
}

/// The stack's orientation layer when the next action composes into it: the last layer of the
/// stack, and nothing else. An orientation layer with a crop or any other layer after it stays
/// where it is, because that order is what a later quarter turn carrying the visible crop means.
fn updatable(layers: &[Layer]) -> Option<&Layer> {
    layers
        .last()
        .filter(|layer| layer.effect_id == ORIENTATION_EFFECT)
}

/// What one orientation layer says it does, for the recipe row. The payload is a composed state,
/// not the gestures that reached it, so the row names the resulting orientation: the reflection the
/// payload applies first, then the quarter turns, joined with a separator. Two of the eight have a
/// declared action of their own and are named by it, and the neutral orientation says so rather
/// than reading as an empty row.
fn describe(orientation: Orientation) -> String {
    let turn = match orientation.turns {
        1 => Some("Rotate right"),
        2 => Some("Rotate 180°"),
        3 => Some("Rotate left"),
        _ => None,
    };
    match (orientation.mirror, turn) {
        (false, None) => "Upright".into(),
        (false, Some(turn)) => turn.into(),
        // Mirror horizontally then turn a half circle is exactly the flip vertical action.
        (true, Some("Rotate 180°")) => "Flip vertical".into(),
        (true, None) => "Mirror horizontal".into(),
        (true, Some(turn)) => format!("Mirror horizontal · {turn}"),
    }
}

impl ToolModule for TransformModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != TRANSFORM_ACTION {
            return Err(validation(format!("unknown action {action_id}")));
        }
        let transform = transform_value(parameters)?;
        let mut stored = Map::new();
        stored.insert("transform".into(), Value::from(transform.action_id()));
        Ok(ActionInput {
            // The durable history identity is the transform itself, unchanged since M2.
            action_id: transform.action_id().into(),
            parameters: stored,
        })
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let transform = transform_value(&input.parameters)?;
        // Every exact transform changes the orientation: four quarter turns are an identity, one is
        // not, so a transform is never a no-op. Reaching the neutral orientation leaves a neutral
        // layer, as a crop reset leaves a neutral crop.
        if let Some(layer) = updatable(context.layers) {
            let composed =
                payload(&layer.effect_id, layer.effect_format, &layer.payload)?.then(transform);
            return Ok(ActionPlan::Update(Layer {
                payload: serde_json::to_value(composed).expect("orientation is serializable"),
                ..layer.clone()
            }));
        }
        Ok(ActionPlan::Commit(Layer::orientation(Orientation::of(
            transform,
        ))))
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        Ok(describe(payload(effect_id, format, value)?))
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        Ok(Processing::ExactGeometry(
            payload(effect_id, format, value)?.geometry(stage.width, stage.height),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CROP_EFFECT, LayerId, PIXEL_EFFECT, modules::check_parameters};
    use serde_json::json;

    /// A non-square stage, so a quarter turn that went the wrong way or was dropped shows up in
    /// the output dimensions as well as in the mapping.
    const STAGE: Stage = Stage {
        width: 7,
        height: 5,
    };

    const ACTIONS: [Transform; 4] = [
        Transform::RotateLeft,
        Transform::RotateRight,
        Transform::MirrorHorizontal,
        Transform::FlipVertical,
    ];

    /// The eight exact orientations, each of which is one payload.
    fn orientations() -> Vec<Orientation> {
        [false, true]
            .into_iter()
            .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }))
            .collect()
    }

    fn planned(transform: Transform, layers: &[Layer]) -> ActionPlan {
        let module = TransformModule::new();
        let declared = module
            .descriptor()
            .action(TRANSFORM_ACTION)
            .expect("a declared action");
        let checked =
            check_parameters(declared, &json!({"transform": transform.action_id()})).unwrap();
        let input = module.parse(TRANSFORM_ACTION, &checked).unwrap();
        let sampler = |_: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
            panic!("planning a transform never samples a pixel")
        };
        let stage_before =
            |_: usize| -> Result<Stage, Error> { panic!("a transform plans no input stage") };
        let insertion_index = |_: EffectStage| layers.len();
        let insertion_index_for = |_: &str| layers.len();
        let sample_before = |_: usize, _: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
            panic!("planning a transform never samples a pixel")
        };
        module
            .plan(
                &input,
                &StageContext {
                    stage: STAGE,
                    layers,
                    sampler: &sampler,
                    stage_before: &stage_before,
                    insertion_index: &insertion_index,
                    insertion_index_for: &insertion_index_for,
                    sample_before: &sample_before,
                    sensor_neutral: None,
                },
            )
            .expect("a transform always plans")
    }

    fn layer_of(plan: &ActionPlan) -> &Layer {
        match plan {
            ActionPlan::Commit(layer) | ActionPlan::Update(layer) => layer,
            ActionPlan::NoOp => panic!("a transform is never a no-op"),
            ActionPlan::Compose(_) => panic!("a transform is never a composite"),
        }
    }

    fn orientation_of(plan: &ActionPlan) -> Orientation {
        let layer = layer_of(plan);
        assert_eq!(layer.effect_id, ORIENTATION_EFFECT);
        assert_eq!(layer.effect_format, EFFECT_FORMAT);
        serde_json::from_value(layer.payload.clone()).expect("an orientation payload")
    }

    fn other_layer(effect_id: &str) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn the_descriptor_keeps_the_four_actions_controls_and_one_geometry_effect() {
        let module = TransformModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid transform descriptor");
        assert_eq!(descriptor.id, "lightwell.transform");
        assert!(descriptor.collapsed, "the section starts collapsed");
        assert_eq!(
            descriptor.effects,
            vec![EffectDescriptor {
                id: ORIENTATION_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Geometry,
                order: 0,
                maskable: false,
                artifacts: false,
            }]
        );
        let action = descriptor
            .action(TRANSFORM_ACTION)
            .expect("the one transform action");
        let ParameterKind::Enum { options } = &action.parameters[0].kind else {
            panic!("the transform parameter is an enum")
        };
        assert_eq!(
            options,
            &[
                "rotate-left",
                "rotate-right",
                "mirror-horizontal",
                "flip-vertical"
            ]
        );
        let [Control::Group { controls, .. }] = &descriptor.controls[..] else {
            panic!("the transform controls are one group")
        };
        let invoked: Vec<Value> = controls
            .iter()
            .map(|control| match control {
                Control::Action { action, preset, .. } => {
                    assert_eq!(action, TRANSFORM_ACTION);
                    preset["transform"].clone()
                }
                other => panic!("a transform control invokes an action, not {other:?}"),
            })
            .collect();
        // Every control names its icon, so a client can draw the group as one row of icon buttons.
        let icons: Vec<Option<&str>> = controls
            .iter()
            .map(|control| match control {
                Control::Action { icon, .. } => icon.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(
            icons,
            [
                Some("rotate-left"),
                Some("rotate-right"),
                Some("mirror"),
                Some("flip")
            ]
        );
        assert_eq!(
            invoked,
            options
                .iter()
                .map(|option| Value::from(option.as_str()))
                .collect::<Vec<_>>(),
            "one control per action identity"
        );
    }

    /// The composition table, exhaustively: for every one of the eight orientations and every one
    /// of the four actions, the composed payload's exact mapping is the orientation's mapping
    /// followed by that action's own step at the stage the orientation produces.
    #[test]
    fn the_composition_table_matches_the_stepwise_exact_geometry() {
        for state in orientations() {
            let before = state.geometry(STAGE.width, STAGE.height);
            assert_eq!(
                (before.output_width, before.output_height),
                if state.turns % 2 == 0 {
                    (STAGE.width, STAGE.height)
                } else {
                    (STAGE.height, STAGE.width)
                },
                "{state:?} swaps the stage for an odd quarter turn"
            );
            for transform in ACTIONS {
                let stepwise =
                    before.then(transform.geometry(before.output_width, before.output_height));
                let composed = state.then(transform);
                assert!(composed.turns <= 3, "{state:?} then {transform:?}");
                assert_eq!(
                    composed.geometry(STAGE.width, STAGE.height),
                    stepwise,
                    "{state:?} then {transform:?}"
                );
            }
        }
    }

    /// The table in the design, read back from the implementation.
    #[test]
    fn each_action_moves_the_payload_the_way_the_contract_says() {
        for state in orientations() {
            let k = u32::from(state.turns);
            for (transform, expected) in [
                (
                    Transform::RotateRight,
                    Orientation {
                        mirror: state.mirror,
                        turns: ((k + 1) % 4) as u8,
                    },
                ),
                (
                    Transform::RotateLeft,
                    Orientation {
                        mirror: state.mirror,
                        turns: ((k + 3) % 4) as u8,
                    },
                ),
                (
                    Transform::MirrorHorizontal,
                    Orientation {
                        mirror: !state.mirror,
                        turns: ((4 - k) % 4) as u8,
                    },
                ),
                (
                    Transform::FlipVertical,
                    Orientation {
                        mirror: !state.mirror,
                        turns: ((6 - k) % 4) as u8,
                    },
                ),
            ] {
                assert_eq!(state.then(transform), expected, "{state:?} {transform:?}");
            }
        }
        // Four quarter turns and two matching reflections return to where they started, and one
        // action never does: a transform is never a no-op.
        for state in orientations() {
            for (transform, count) in [
                (Transform::RotateRight, 4),
                (Transform::RotateLeft, 4),
                (Transform::MirrorHorizontal, 2),
                (Transform::FlipVertical, 2),
            ] {
                let mut composed = state;
                for step in 1..count {
                    composed = composed.then(transform);
                    assert_ne!(composed, state, "{transform:?} step {step} of {state:?}");
                }
                assert_eq!(
                    composed.then(transform),
                    state,
                    "{transform:?} of {state:?}"
                );
            }
        }
        assert_eq!(Orientation::NEUTRAL, Orientation::default());
        for transform in ACTIONS {
            assert_eq!(
                Orientation::of(transform),
                Orientation::NEUTRAL.then(transform)
            );
        }
    }

    /// A single-action layer compiles to exactly the mapping M2 declared for that action.
    #[test]
    fn a_single_action_orientation_compiles_to_that_actions_own_mapping() {
        let module = TransformModule::new();
        for transform in ACTIONS {
            let layer = Layer::orientation(Orientation::of(transform));
            assert_eq!(
                module
                    .compile(&layer.effect_id, layer.effect_format, &layer.payload, STAGE)
                    .unwrap(),
                Processing::ExactGeometry(transform.geometry(STAGE.width, STAGE.height)),
                "{transform:?}"
            );
        }
        // The neutral orientation is the identity of its own stage: unchanged size and no motion.
        assert_eq!(
            module
                .compile(
                    ORIENTATION_EFFECT,
                    EFFECT_FORMAT,
                    &json!({"mirror":false,"turns":0}),
                    STAGE
                )
                .unwrap(),
            Processing::ExactGeometry(ExactGeometry::identity(STAGE.width, STAGE.height))
        );
    }

    #[test]
    fn a_payload_is_validated_against_its_declared_effect_format_and_range() {
        let module = TransformModule::new();
        let valid = json!({"mirror":true,"turns":3});
        assert!(
            module
                .validate_payload(ORIENTATION_EFFECT, EFFECT_FORMAT, &valid)
                .is_ok()
        );
        for (case, effect, format, kind) in [
            (
                "wrong effect",
                CROP_EFFECT,
                EFFECT_FORMAT,
                ErrorKind::Incompatible,
            ),
            (
                "the retired transform effect",
                "lightwell.geometry.transform",
                EFFECT_FORMAT,
                ErrorKind::Incompatible,
            ),
            (
                "wrong format",
                ORIENTATION_EFFECT,
                99,
                ErrorKind::Incompatible,
            ),
        ] {
            assert_eq!(
                module
                    .validate_payload(effect, format, &valid)
                    .unwrap_err()
                    .kind,
                kind,
                "{case}"
            );
        }
        for (case, malformed) in [
            ("the retired payload shape", json!("rotate-right")),
            ("a missing field", json!({"turns":1})),
            (
                "an unknown field",
                json!({"mirror":false,"turns":1,"skew":2}),
            ),
            (
                "a turn count outside 0..3",
                json!({"mirror":false,"turns":4}),
            ),
            ("a negative turn count", json!({"mirror":false,"turns":-1})),
        ] {
            let error = module
                .validate_payload(ORIENTATION_EFFECT, EFFECT_FORMAT, &malformed)
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(
                error.detail.starts_with("invalid orientation payload"),
                "{case}: {error}"
            );
        }
    }

    /// The planning rule: the last layer of the stack, and only that, is composed into.
    #[test]
    fn a_transform_updates_the_last_orientation_layer_and_otherwise_commits_a_new_one() {
        let existing = Layer::orientation(Orientation {
            mirror: true,
            turns: 1,
        });
        for (case, layers) in [
            ("an empty stack", vec![]),
            ("a pixel layer", vec![other_layer(PIXEL_EFFECT)]),
            ("a crop layer", vec![other_layer(CROP_EFFECT)]),
            (
                "an orientation layer that is not the last one",
                vec![existing.clone(), other_layer(CROP_EFFECT)],
            ),
        ] {
            for transform in ACTIONS {
                let plan = planned(transform, &layers);
                assert!(
                    matches!(plan, ActionPlan::Commit(_)),
                    "{case}: {transform:?} appends a new orientation layer"
                );
                assert_eq!(
                    orientation_of(&plan),
                    Orientation::of(transform),
                    "{case}: a new layer holds the single action's state"
                );
            }
        }
        for prefix in [vec![], vec![other_layer(PIXEL_EFFECT)]] {
            for transform in ACTIONS {
                let mut layers = prefix.clone();
                layers.push(existing.clone());
                let plan = planned(transform, &layers);
                assert!(
                    matches!(plan, ActionPlan::Update(_)),
                    "the last layer is an orientation layer"
                );
                assert_eq!(layer_of(&plan).id, existing.id, "the identity is kept");
                assert_eq!(
                    orientation_of(&plan),
                    serde_json::from_value::<Orientation>(existing.payload.clone())
                        .unwrap()
                        .then(transform)
                );
            }
        }
        // Reaching the neutral orientation leaves a neutral layer; the plan model has no removal.
        let mut layers = vec![Layer::orientation(Orientation::of(Transform::RotateRight))];
        for step in 1..4 {
            let plan = planned(Transform::RotateRight, &layers);
            assert_eq!(layer_of(&plan).id, layers[0].id, "turn {step}");
            layers = vec![layer_of(&plan).clone()];
        }
        assert_eq!(
            serde_json::from_value::<Orientation>(layers[0].payload.clone()).unwrap(),
            Orientation::NEUTRAL,
            "four quarter turns leave one neutral layer"
        );
    }

    #[test]
    fn an_unknown_action_and_a_missing_or_invalid_parameter_are_refused() {
        let module = TransformModule::new();
        let mut parameters = Map::new();
        assert_eq!(
            module
                .parse("rotate-right", &parameters)
                .unwrap_err()
                .detail,
            "unknown action rotate-right"
        );
        assert_eq!(
            module
                .parse(TRANSFORM_ACTION, &parameters)
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
        parameters.insert("transform".into(), Value::from("rotate-sideways"));
        assert_eq!(
            module
                .parse(TRANSFORM_ACTION, &parameters)
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
        // The durable history identity is the action, whatever the stack does with it.
        for transform in ACTIONS {
            parameters.insert("transform".into(), Value::from(transform.action_id()));
            let input = module.parse(TRANSFORM_ACTION, &parameters).unwrap();
            assert_eq!(input.action_id, transform.action_id());
            assert_eq!(input.parameters["transform"], json!(transform.action_id()));
        }
    }
}
