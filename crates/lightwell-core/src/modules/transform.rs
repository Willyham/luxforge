//! The exact transform module: quarter turns and reflections as integer coordinate mappings.
//!
//! The four actions are the vocabulary; the stack holds the orientation they compose into. The
//! orientation goes ahead of the crop, and the next action updates it in place, so a stage carries
//! one layer however many times it is turned or reflected and the crop always frames the turned
//! photograph.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, EffectDescriptor,
    EffectStage, ExactGeometry, LayerEdit, LayerUpdate, ModuleDescriptor, NewLayer,
    ParameterDescriptor, Processing, Stage, StageContext, ToolModule, crop::stored_payload,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{CROP_EFFECT, EFFECT_FORMAT, Error, Layer, Orientation, Transform};
use serde_json::{Map, Value};

/// The transform module's one geometry effect: the composed exact orientation of the stage ahead of
/// the crop.
pub const ORIENTATION_EFFECT: &str = "lightwell.geometry.orientation";

pub(super) const TRANSFORM_ACTION: &str = "transform";

impl Layer {
    /// The one orientation layer of a stage: the composed quarter turns and reflections that every
    /// exact transform action applied there reaches. For a stack assembled directly.
    pub fn orientation(orientation: Orientation) -> Self {
        Self::new(ORIENTATION_EFFECT, orientation_value(orientation))
    }
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

    /// This orientation and then `next`: `next`'s reflection and quarter turns applied to what this
    /// one produced.
    pub fn followed_by(self, next: Self) -> Self {
        let reflected = if next.mirror {
            self.then(Transform::MirrorHorizontal)
        } else {
            self
        };
        (0..next.turns).fold(reflected, |state, _| state.then(Transform::RotateRight))
    }

    /// The orientation that undoes this one: its quarter turns back, then its reflection.
    pub fn inverse(self) -> Self {
        let unturned =
            (0..self.turns).fold(Self::NEUTRAL, |state, _| state.then(Transform::RotateLeft));
        if self.mirror {
            unturned.then(Transform::MirrorHorizontal)
        } else {
            unturned
        }
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
    Control::action(TRANSFORM_ACTION, label)
        .preset(preset)
        .icon(icon_name(transform))
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
                    single: false,
                }],
                actions: vec![ActionDescriptor {
                    id: TRANSFORM_ACTION.into(),
                    title: "Transform".into(),
                    notes: "exact quarter turns and reflections; integer mappings with no interpolation".into(),
                    summary: Some("{transform}".into()),
                    patch: false,
parameters: vec![
                        ParameterDescriptor::enumeration(
                            "transform",
                            [
                                Transform::RotateLeft.action_id(),
                                Transform::RotateRight.action_id(),
                                Transform::MirrorHorizontal.action_id(),
                                Transform::FlipVertical.action_id(),
                            ],
                        )
                        .required(true)
                        .notes("the exact transform to compose into the stack's orientation"),
                    ],
                }],
                queries: Vec::new(),
                controls: vec![Control::group(
                    "Exact transforms",
                    vec![
                        control(Transform::RotateLeft, "Rotate left"),
                        control(Transform::RotateRight, "Rotate right"),
                        control(Transform::MirrorHorizontal, "Mirror horizontal"),
                        control(Transform::FlipVertical, "Flip vertical"),
                    ],
                )],
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
    let value = parameters.get("transform").ok_or_else(|| {
        Error::validation("missing required parameter transform for action transform")
    })?;
    serde_json::from_value(value.clone())
        .map_err(|error| Error::validation(format!("invalid transform: {error}")))
}

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<Orientation, Error> {
    if effect_id != ORIENTATION_EFFECT {
        return Err(Error::incompatible(format!(
            "unavailable effect {effect_id}"
        )));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::incompatible(format!(
            "unsupported effect format {format}"
        )));
    }
    let orientation: Orientation = serde_json::from_value(payload.clone())
        .map_err(|error| Error::validation(format!("invalid orientation payload: {error}")))?;
    // Only the four quarter turns exist; a larger count is a payload this module cannot mean.
    if orientation.turns > 3 {
        return Err(Error::validation(format!(
            "invalid orientation payload: turns {} is outside 0..3",
            orientation.turns
        )));
    }
    Ok(orientation)
}

/// What one transform does to a stack, as the layer edits that do it.
///
/// A new orientation layer would go at `at`, where the host places the orientation effect: after
/// the pixel, colour and spatial work, and ahead of the crop, whose geometry order is later. The
/// transform composes into the orientation layer just before that position when there is one, and
/// otherwise commits a new layer there. Every geometry layer from `at` on is re-expressed so the
/// output is this transform applied to what the stack produced: the crop is carried through it,
/// selecting the same content in the turned stage, and an orientation layer after the crop, which
/// the host never places there but a stored stack may hold, is folded into the layer ahead of it
/// and left neutral, the crop carried through it too. The crop's input stage therefore includes
/// every transform once this has run. Finish layers after the tail follow the output as they
/// always did, and nothing else is placed there.
fn edits(transform: Transform, context: &StageContext<'_>) -> Result<Vec<LayerEdit>, Error> {
    let layers = context.layers;
    let at = context
        .insertion_index_for(ORIENTATION_EFFECT)
        .min(layers.len());
    let mut crop = None;
    let mut folded = Orientation::NEUTRAL;
    let mut edits = Vec::new();
    for (index, layer) in layers.iter().enumerate().skip(at) {
        if layer.effect_id == CROP_EFFECT {
            crop = Some((index, layer));
        } else if layer.effect_id == ORIENTATION_EFFECT {
            let trailing = payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
            folded = folded.followed_by(trailing);
            if trailing != Orientation::NEUTRAL {
                edits.push(LayerEdit::Update(LayerUpdate::new(
                    layer.id.clone(),
                    orientation_value(Orientation::NEUTRAL),
                )));
            }
        }
    }
    let turned = folded.then(transform);
    if let Some((index, layer)) = crop {
        let stored = stored_payload(layer)?;
        let input = context.stage_before(index)?;
        let carried = stored.carried((input.width, input.height), turned)?;
        if carried != stored {
            edits.push(LayerEdit::Update(LayerUpdate::new(
                layer.id.clone(),
                serde_json::to_value(carried).expect("a crop payload is serializable"),
            )));
        }
    }
    let ahead = at
        .checked_sub(1)
        .map(|index| &layers[index])
        .filter(|layer| layer.effect_id == ORIENTATION_EFFECT);
    // Reaching the neutral orientation leaves a neutral layer, as a crop reset leaves a neutral
    // crop; only folding a trailing layer back to neutral commits nothing new ahead of the crop.
    match ahead {
        Some(layer) => {
            let current = payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
            let composed = current.followed_by(turned);
            if composed != current {
                edits.insert(
                    0,
                    LayerEdit::Update(LayerUpdate::new(
                        layer.id.clone(),
                        orientation_value(composed),
                    )),
                );
            }
        }
        None if turned != Orientation::NEUTRAL => {
            edits.insert(
                0,
                LayerEdit::Commit(NewLayer::new(ORIENTATION_EFFECT, orientation_value(turned))),
            );
        }
        None => {}
    }
    Ok(edits)
}

fn orientation_value(orientation: Orientation) -> Value {
    serde_json::to_value(orientation).expect("orientation is serializable")
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
            return Err(Error::validation(format!("unknown action {action_id}")));
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
        // Every exact transform changes the output: four quarter turns are an identity, one is
        // not, so a transform is never a no-op and always has at least one edit.
        Ok(
            match <[LayerEdit; 1]>::try_from(edits(transform, context)?) {
                Ok([LayerEdit::Commit(layer)]) => ActionPlan::Commit(layer),
                Ok([LayerEdit::Update(layer)]) => ActionPlan::Update(layer),
                Err(edits) => ActionPlan::Edits(edits),
            },
        )
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect_id, format, value).map(|_| ())
    }

    /// The identity orientation, which is what four quarter turns leave behind.
    fn is_neutral(&self, effect_id: &str, format: u32, value: &Value) -> Result<bool, Error> {
        Ok(payload(effect_id, format, value)? == Orientation::NEUTRAL)
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
    use crate::{
        CropPayload, LayerId, ModuleRegistry, PIXEL_EFFECT, VIGNETTE_EFFECT,
        modules::{ParameterKind, StageQuestions, check_parameters},
    };
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

    /// Plan one transform against `layers` on [`STAGE`], with the host's own placement rule. Only
    /// orientation layers change a stage here, so the stage before any index folds them; a
    /// transform asks for the stage before the crop and nothing else.
    fn planned(transform: Transform, layers: &[Layer]) -> ActionPlan {
        let module = TransformModule::new();
        let declared = module
            .descriptor()
            .action(TRANSFORM_ACTION)
            .expect("a declared action");
        let checked =
            check_parameters(declared, &json!({"transform": transform.action_id()})).unwrap();
        let input = module.parse(TRANSFORM_ACTION, &checked).unwrap();
        module
            .plan(
                &input,
                &StageContext {
                    stage: STAGE,
                    layers,
                    registry: &ModuleRegistry::builtin(),
                    target: None,
                    questions: &Oriented(layers),
                },
            )
            .expect("a transform always plans")
    }

    /// A stack on [`STAGE`] whose only stage-changing layers are orientations: the stage before a
    /// layer folds the quarter turns ahead of it. A transform asks for the crop's input stage and
    /// never samples a pixel.
    struct Oriented<'a>(&'a [Layer]);

    impl StageQuestions for Oriented<'_> {
        fn stage_before(&self, index: usize) -> Result<Stage, Error> {
            let layers = self.0;
            assert_eq!(
                layers[index].effect_id, CROP_EFFECT,
                "a transform plans only the crop's input stage"
            );
            Ok(layers[..index].iter().fold(STAGE, |stage, layer| {
                match serde_json::from_value::<Orientation>(layer.payload.clone()) {
                    Ok(orientation) if orientation.turns % 2 == 1 => Stage {
                        width: stage.height,
                        height: stage.width,
                    },
                    _ => stage,
                }
            }))
        }
        fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
            panic!("planning a transform never samples a pixel")
        }
    }

    /// The orientation layer the host stores for a one-layer plan: a commit's payload under a new
    /// identity, or the updated layer's identity with its new payload.
    fn layer_of(plan: &ActionPlan) -> Layer {
        match plan {
            ActionPlan::Commit(new) => Layer::new(new.effect_id.clone(), new.payload.clone()),
            ActionPlan::Update(update) => Layer {
                id: update.id.clone(),
                ..Layer::new(ORIENTATION_EFFECT, update.payload.clone())
            },
            ActionPlan::NoOp => panic!("a transform is never a no-op"),
            ActionPlan::Edits(edits) => panic!("expected one layer, not {edits:?}"),
            ActionPlan::Compose(_) => panic!("a transform is never a composite"),
        }
    }

    /// The layer edits of a plan that changes several layers.
    fn edits_of(plan: ActionPlan) -> Vec<LayerEdit> {
        match plan {
            ActionPlan::Edits(edits) => edits,
            other => panic!("expected several layer edits, not {other:?}"),
        }
    }

    /// A 7 × 5 crop of the whole stage but its last column and bottom row, unstraightened.
    fn inset_crop() -> Layer {
        Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 0.0,
            width: 6.0 / 7.0,
            height: 4.0 / 5.0,
        })
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
                single: false,
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

    /// The planning rule: a transform composes into the orientation layer just ahead of where the
    /// host would place a new one, which is ahead of the crop and of any finish layer, and commits a
    /// new layer there otherwise.
    #[test]
    fn a_transform_composes_ahead_of_the_crop_and_otherwise_commits_a_new_layer_there() {
        let existing = Layer::orientation(Orientation {
            mirror: true,
            turns: 1,
        });
        let neutral_crop = Layer::crop(CropPayload::NEUTRAL);
        for (case, layers) in [
            ("an empty stack", vec![]),
            ("a pixel layer", vec![other_layer(PIXEL_EFFECT)]),
            // A neutral crop selects the whole turned stage, so only the orientation changes.
            ("a neutral crop layer", vec![neutral_crop.clone()]),
            (
                "an orientation layer behind a pixel layer",
                vec![existing.clone(), other_layer(PIXEL_EFFECT)],
            ),
        ] {
            for transform in ACTIONS {
                let plan = planned(transform, &layers);
                assert!(
                    matches!(plan, ActionPlan::Commit(_)),
                    "{case}: {transform:?} commits a new orientation layer"
                );
                assert_eq!(
                    orientation_of(&plan),
                    Orientation::of(transform),
                    "{case}: a new layer holds the single action's state"
                );
            }
        }
        for (case, layers) in [
            ("the last layer", vec![existing.clone()]),
            (
                "after a pixel layer",
                vec![other_layer(PIXEL_EFFECT), existing.clone()],
            ),
            (
                "ahead of a finish layer",
                vec![existing.clone(), other_layer(VIGNETTE_EFFECT)],
            ),
            (
                "ahead of a neutral crop",
                vec![existing.clone(), neutral_crop.clone()],
            ),
        ] {
            for transform in ACTIONS {
                let plan = planned(transform, &layers);
                assert!(
                    matches!(plan, ActionPlan::Update(_)),
                    "{case}: {transform:?} updates the orientation layer"
                );
                assert_eq!(
                    layer_of(&plan).id,
                    existing.id,
                    "{case}: the identity is kept"
                );
                assert_eq!(
                    orientation_of(&plan),
                    serde_json::from_value::<Orientation>(existing.payload.clone())
                        .unwrap()
                        .then(transform),
                    "{case}"
                );
            }
        }
        // Reaching the neutral orientation leaves a neutral layer; the plan model has no removal.
        let mut layers = vec![Layer::orientation(Orientation::of(Transform::RotateRight))];
        for step in 1..4 {
            let plan = planned(Transform::RotateRight, &layers);
            assert_eq!(layer_of(&plan).id, layers[0].id, "turn {step}");
            layers = vec![layer_of(&plan)];
        }
        assert_eq!(
            serde_json::from_value::<Orientation>(layers[0].payload.clone()).unwrap(),
            Orientation::NEUTRAL,
            "four quarter turns leave one neutral layer"
        );
    }

    /// A transform over a crop changes two layers in one action: the orientation ahead of the crop,
    /// and the crop, re-expressed so it selects the same content in the turned stage.
    #[test]
    fn a_transform_over_a_crop_carries_the_crop_through_it() {
        let crop = inset_crop();
        let stored: CropPayload = serde_json::from_value(crop.payload.clone()).unwrap();
        // The 6 × 4 rectangle at the top-left of the 7 × 5 stage, turned clockwise: the top-left of
        // the stage goes to its top-right, so the rectangle starts one column in and is 4 × 6.
        assert_eq!(
            stored
                .carried((7, 5), Orientation::of(Transform::RotateRight))
                .unwrap(),
            CropPayload {
                angle: 0.0,
                x: 1.0 / 5.0,
                y: 0.0,
                width: 4.0 / 5.0,
                height: 6.0 / 7.0,
            }
        );
        for transform in ACTIONS {
            let carried = stored.carried((7, 5), Orientation::of(transform)).unwrap();
            let edits = edits_of(planned(transform, std::slice::from_ref(&crop)));
            assert_eq!(edits.len(), 2, "{transform:?}: {edits:?}");
            let LayerEdit::Commit(orientation) = &edits[0] else {
                panic!("{transform:?} commits the orientation: {edits:?}")
            };
            assert_eq!(orientation.effect_id, ORIENTATION_EFFECT);
            assert_eq!(
                serde_json::from_value::<Orientation>(orientation.payload.clone()).unwrap(),
                Orientation::of(transform)
            );
            let LayerEdit::Update(updated) = &edits[1] else {
                panic!("{transform:?} updates the crop: {edits:?}")
            };
            assert_eq!(
                updated.id, crop.id,
                "{transform:?}: the crop keeps its identity"
            );
            assert_eq!(
                serde_json::from_value::<CropPayload>(updated.payload.clone()).unwrap(),
                carried,
                "{transform:?}"
            );

            // With an orientation ahead of the crop, that layer composes and the crop's input
            // stage is the one the orientation produced.
            let ahead = Layer::orientation(Orientation::of(Transform::RotateLeft));
            let edits = edits_of(planned(transform, &[ahead.clone(), crop.clone()]));
            let [LayerEdit::Update(composed), LayerEdit::Update(updated)] = &edits[..] else {
                panic!("{transform:?} updates both layers: {edits:?}")
            };
            assert_eq!(composed.id, ahead.id);
            assert_eq!(
                serde_json::from_value::<Orientation>(composed.payload.clone()).unwrap(),
                Orientation::of(Transform::RotateLeft).then(transform)
            );
            assert_eq!(updated.id, crop.id);
            assert_eq!(
                serde_json::from_value::<CropPayload>(updated.payload.clone()).unwrap(),
                stored.carried((5, 7), Orientation::of(transform)).unwrap(),
                "{transform:?}: carried on the 5 × 7 stage the left turn produced"
            );
        }
    }

    /// A stored stack may hold an orientation layer after the crop, where the host never places
    /// one. The next transform folds that layer, with itself, into the orientation ahead of the
    /// crop and carries the crop through both, so the output is this transform applied to what the
    /// stack showed and the crop's input stage holds every transform from then on.
    #[test]
    fn a_transform_folds_an_orientation_stored_after_the_crop_ahead_of_it() {
        let crop = inset_crop();
        let stored: CropPayload = serde_json::from_value(crop.payload.clone()).unwrap();
        let trailing = Layer::orientation(Orientation::of(Transform::RotateRight));
        for transform in ACTIONS {
            let turned = Orientation::of(Transform::RotateRight).then(transform);
            let plan = planned(transform, &[crop.clone(), trailing.clone()]);
            if turned == Orientation::NEUTRAL {
                // A left turn undoes the stored right turn: the trailing layer goes neutral and
                // nothing else changes, because the crop is already where it was made.
                assert_eq!(transform, Transform::RotateLeft);
                let ActionPlan::Update(layer) = plan else {
                    panic!("only the trailing layer changes: {plan:?}")
                };
                assert_eq!(layer.id, trailing.id);
                assert_eq!(
                    orientation_of(&ActionPlan::Update(layer)),
                    Orientation::NEUTRAL
                );
                continue;
            }
            let edits = edits_of(plan);
            let [
                LayerEdit::Commit(ahead),
                LayerEdit::Update(folded),
                LayerEdit::Update(carried),
            ] = &edits[..]
            else {
                panic!("{transform:?}: {edits:?}")
            };
            assert_eq!(
                serde_json::from_value::<Orientation>(ahead.payload.clone()).unwrap(),
                turned,
                "{transform:?}: the stored turn, then this transform"
            );
            assert_eq!(folded.id, trailing.id);
            assert_eq!(
                serde_json::from_value::<Orientation>(folded.payload.clone()).unwrap(),
                Orientation::NEUTRAL
            );
            assert_eq!(carried.id, crop.id);
            assert_eq!(
                serde_json::from_value::<CropPayload>(carried.payload.clone()).unwrap(),
                stored.carried((7, 5), turned).unwrap(),
                "{transform:?}"
            );
        }
    }

    /// Composing orientations and undoing one agree with the stepwise exact geometry.
    #[test]
    fn orientations_compose_and_invert_as_their_exact_mappings_do() {
        for first in orientations() {
            assert_eq!(
                first.followed_by(first.inverse()),
                Orientation::NEUTRAL,
                "{first:?}"
            );
            assert_eq!(
                first.inverse().followed_by(first),
                Orientation::NEUTRAL,
                "{first:?}"
            );
            let before = first.geometry(STAGE.width, STAGE.height);
            for next in orientations() {
                assert_eq!(
                    first.followed_by(next).geometry(STAGE.width, STAGE.height),
                    before.then(next.geometry(before.output_width, before.output_height)),
                    "{first:?} then {next:?}"
                );
            }
        }
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
