//! The Presence module: one spatial-stage layer holding Texture, Clarity and Dehaze, edited by one
//! field-patch action.
//!
//! This mirrors the Basic and colour mixer modules' shape (`docs/design/modules-and-api.md`'s
//! field-patch contract): a payload is a JSON object whose keys are the implemented parameter
//! names, a missing key is neutral, and the canonical neutral payload is the empty object `{}`. The
//! module owns exactly one layer of `lightwell.presence.adjust`, a `spatial` effect, so the host
//! places it after the pointwise colour run and before the geometry tail
//! (`docs/design/presence-mixer-vignette.md`, "Placement and stage order").
//!
//! The three units' equations, radii, halos and tolerance are frozen in
//! `docs/design/presence-study.md` and implemented in [`dehaze`], [`texture`] and [`clarity`]; this
//! file owns only the parameters, validation, controls and API surface around them, and the frozen
//! order the compiled operation runs in: **dehaze, then texture, then clarity**. A unit whose amount
//! is 0 is the exact identity, so it is omitted from the operation entirely, which also saves its
//! halo and its work; an all-neutral payload compiles to no units at all, which the host drops, so
//! the layer opens no stage boundary and the render keeps the identity byte path and the shared
//! source buffer.

mod clarity;
mod dehaze;
mod filters;
mod texture;

#[cfg(test)]
mod oracle;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, EffectDescriptor,
    EffectStage, ModuleDescriptor, ParameterDescriptor, ParameterKind, Processing, ResetAction,
    SpatialOperation, SpatialUnit, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId, PRESENCE_EFFECT};
use serde_json::{Map, Number, Value};
use std::sync::Arc;

pub(super) const SET_PRESENCE: &str = "set-presence";
pub(super) const RESET_PRESENCE: &str = "reset-presence";

const TEXTURE: &str = "texture";
const CLARITY: &str = "clarity";
const DEHAZE: &str = "dehaze";

/// Every implemented Presence field, in the payload's declared order, which is also the controls'
/// rendering order. It is deliberately not the order the units run in: the operation always
/// evaluates dehaze, then texture, then clarity.
const FIELDS: [&str; 3] = [TEXTURE, CLARITY, DEHAZE];

/// The group label the one control group carries.
const GROUP: &str = "Presence";

/// The neutral value of every Presence field.
const NEUTRAL: f64 = 0.0;

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check.
pub(crate) const AMBIGUOUS: &str = "ambiguous Presence layers";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Incompatible, detail)
}

/// A finite f64 as a JSON number. Finiteness is checked before every call, so the fallback is never
/// reached in practice and never panics if it is.
fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// One field's control and history label: `Texture`, `Clarity`, `Dehaze`.
fn field_label(name: &str) -> &'static str {
    match name {
        TEXTURE => "Texture",
        CLARITY => "Clarity",
        DEHAZE => "Dehaze",
        _ => "Presence",
    }
}

/// One field of a validated payload or request: a missing key is neutral.
fn field(source: &Map<String, Value>, name: &str) -> f64 {
    source.get(name).and_then(Value::as_f64).unwrap_or(NEUTRAL)
}

/// The canonical values a payload represents, in [`FIELDS`] order. Absent and explicitly neutral
/// keys produce the same array, which is what makes `{}` and `{"texture": 0}` compare equal.
fn canonical(payload: &Map<String, Value>) -> [f64; FIELDS.len()] {
    FIELDS.map(|name| field(payload, name))
}

fn is_neutral(values: &[f64; FIELDS.len()]) -> bool {
    values.iter().all(|value| *value == NEUTRAL)
}

/// The canonical stored form of a set of values: only the fields that are not neutral, so the
/// neutral payload is exactly `{}`.
fn payload_of(values: &[f64; FIELDS.len()]) -> Value {
    let mut payload = Map::new();
    for (name, value) in FIELDS.iter().zip(values) {
        if *value != NEUTRAL {
            payload.insert((*name).to_owned(), number(*value));
        }
    }
    Value::Object(payload)
}

/// The object a stored payload must be, with the effect identity and format checked first: an
/// unsupported format is `incompatible` and is never rewritten, and every field is a finite number
/// inside its declared range.
fn read_payload(effect_id: &str, format: u32, value: &Value) -> Result<Map<String, Value>, Error> {
    if effect_id != PRESENCE_EFFECT {
        return Err(incompatible(format!("unavailable effect {effect_id}")));
    }
    if format != EFFECT_FORMAT {
        return Err(incompatible(format!("unsupported effect format {format}")));
    }
    let object = value
        .as_object()
        .ok_or_else(|| validation("presence payload must be a JSON object"))?;
    for (name, value) in object {
        if !FIELDS.contains(&name.as_str()) {
            return Err(validation(format!("unknown presence field {name}")));
        }
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| validation(format!("presence field {name} must be a finite number")))?;
        if !(-100.0..=100.0).contains(&number) {
            return Err(validation(format!(
                "presence field {name} must be a number within -100..=100"
            )));
        }
    }
    Ok(object.clone())
}

/// The stack's one Presence layer. Two of them would each claim to be the Presence state, so every
/// path refuses to guess which one an action addresses rather than silently choosing one; nothing
/// is rewritten.
fn locate(layers: &[Layer]) -> Result<Option<&Layer>, Error> {
    let mut found = None;
    for layer in layers {
        if layer.effect_id == PRESENCE_EFFECT {
            if found.is_some() {
                return Err(validation(AMBIGUOUS));
            }
            found = Some(layer);
        }
    }
    Ok(found)
}

/// One Presence parameter descriptor: -100..100, step 1, no display decimals, no unit.
fn presence_parameter(field: &str) -> ParameterDescriptor {
    let notes = match field {
        TEXTURE => "scales a medium-frequency luminance band isolated between two edge-preserving \
                    smoothers; negative values attenuate it"
            .to_string(),
        CLARITY => {
            "scales the residual of luminance against a broad edge-preserving base computed \
                    on a reduced grid; negative values soften it"
                .to_string()
        }
        DEHAZE => "removes the estimated atmospheric veil by inverting I = t*J + (1 - t)*A; \
                   negative values add a veil through the same model"
            .to_string(),
        _ => String::new(),
    };
    ParameterDescriptor {
        name: field.into(),
        kind: ParameterKind::Number {
            min: -100.0,
            max: 100.0,
        },
        required: false,
        default: Some(number(NEUTRAL)),
        unit: None,
        step: Some(1.0),
        precision: Some(0),
        notes,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: Some(NEUTRAL),
    }
}

/// The group's reset: a `set-presence` preset holding all three fields at neutral.
fn group_reset() -> ResetAction {
    ResetAction {
        action: SET_PRESENCE.into(),
        preset: FIELDS
            .iter()
            .map(|name| ((*name).to_owned(), number(NEUTRAL)))
            .collect(),
    }
}

/// The three sliders, in [`FIELDS`] order, on plain rails: none of the three has a colour a
/// gradient could show.
fn sliders() -> Vec<Control> {
    FIELDS
        .iter()
        .map(|field| Control::Number {
            action: SET_PRESENCE.into(),
            parameter: (*field).into(),
            label: field_label(field).into(),
            style: crate::NumberStyle::Slider,
            rail: None,
            reset: None,
        })
        .collect()
}

#[derive(Debug)]
pub struct PresenceModule {
    descriptor: ModuleDescriptor,
}

impl Default for PresenceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl PresenceModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.presence".into(),
                title: "Presence".into(),
                hint: Some("Texture, clarity and dehaze".into()),
                effects: vec![EffectDescriptor {
                    id: PRESENCE_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Spatial,
                    order: 0,
                    maskable: true,
                    artifacts: false,
                }],
                actions: vec![
                    ActionDescriptor {
                        id: SET_PRESENCE.into(),
                        title: "Set Presence".into(),
                        notes: "merges the named presence fields into the stack's one Presence layer, which the host places after the pointwise colour run and before the geometry tail on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op".into(),
                        summary: None,
                        patch: true,
                        parameters: FIELDS.iter().map(|field| presence_parameter(field)).collect(),
                    },
                    ActionDescriptor {
                        id: RESET_PRESENCE.into(),
                        title: "Reset Presence".into(),
                        notes: "returns the stack's one Presence layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral".into(),
                        summary: None,
                        patch: false,
                        parameters: Vec::new(),
                    },
                ],
                queries: Vec::new(),
                controls: vec![Control::Group {
                    label: GROUP.into(),
                    reset: Some(group_reset()),
                    controls: sliders(),
                    collapsed: false,
                }],
                reset: Some(ResetAction {
                    action: RESET_PRESENCE.into(),
                    preset: Map::new(),
                }),
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

impl ToolModule for PresenceModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// At most one Presence layer exists in a stack, so the host refuses to compile or plan against
    /// a stack that holds two instead of guessing which one the parameters belong to.
    fn single_layer(&self, effect_id: &str) -> bool {
        effect_id == PRESENCE_EFFECT
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            // A patch stores exactly the fields the caller sent: the history entry, the label and
            // request deduplication all describe the patch, not the merged payload.
            SET_PRESENCE => parameters.clone(),
            RESET_PRESENCE => Map::new(),
            _ => return Err(validation(format!("unknown action {action_id}"))),
        };
        Ok(ActionInput {
            action_id: action_id.to_owned(),
            parameters,
        })
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let existing = locate(context.layers)?;
        let current = match existing {
            Some(layer) => canonical(&read_payload(
                &layer.effect_id,
                layer.effect_format,
                &layer.payload,
            )?),
            None => [NEUTRAL; FIELDS.len()],
        };
        let merged = match input.action_id.as_str() {
            SET_PRESENCE => {
                let mut merged = current;
                for (slot, name) in merged.iter_mut().zip(FIELDS) {
                    if let Some(value) = input.parameters.get(name) {
                        *slot = value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| {
                                validation(format!("presence field {name} must be a finite number"))
                            })?;
                        if !(-100.0..=100.0).contains(slot) {
                            return Err(validation(format!(
                                "presence field {name} must be a number within -100..=100"
                            )));
                        }
                    }
                }
                merged
            }
            RESET_PRESENCE => [NEUTRAL; FIELDS.len()],
            action_id => return Err(validation(format!("unknown action {action_id}"))),
        };
        match existing {
            Some(_) if current == merged => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(Layer {
                id: layer.id.clone(),
                effect_id: PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                // The mask is the host's: an update keeps whatever this layer already carries.
                mask: layer.mask.clone(),
                artifacts: Vec::new(),
            })),
            // The host inserts a spatial layer after every pixel and colour layer and before the
            // geometry tail; a neutral first set has nothing to store, so it adds no layer at all.
            None if is_neutral(&merged) => Ok(ActionPlan::NoOp),
            None => Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                mask: None,
                artifacts: Vec::new(),
            })),
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        read_payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        if is_neutral(&values) {
            return Ok("Neutral".into());
        }
        Ok(FIELDS
            .iter()
            .zip(values)
            .filter(|(_, value)| *value != NEUTRAL)
            .map(|(name, value)| format!("{} {value:+.0}", field_label(name)))
            .collect::<Vec<_>>()
            .join(", "))
    }

    /// The history label for a request the action's title cannot describe: the one field a slider
    /// moved, or the reset that cleared them all.
    fn label(&self, input: &ActionInput) -> Option<String> {
        match input.action_id.as_str() {
            RESET_PRESENCE => Some("Reset Presence".into()),
            SET_PRESENCE => {
                let sent: Vec<(&String, f64)> = input
                    .parameters
                    .iter()
                    .map(|(name, value)| (name, value.as_f64().unwrap_or(NEUTRAL)))
                    .collect();
                match sent.as_slice() {
                    [(name, value)] => Some(format!("{} {value:+.0}", field_label(name))),
                    // An empty patch changes nothing and commits no entry; the host falls back to
                    // the action's own title if it ever asks.
                    [] => None,
                    // The group's reset preset sends every field at neutral: name it as Basic and
                    // the vignette name theirs, not by its field count.
                    fields
                        if fields.len() == FIELDS.len()
                            && fields.iter().all(|(_, value)| *value == NEUTRAL) =>
                    {
                        Some("Reset Presence".into())
                    }
                    fields => Some(format!("Presence ({} fields)", fields.len())),
                }
            }
            _ => None,
        }
    }

    /// The values this stored layer represents, named exactly as `set-presence`'s parameters are,
    /// so a client seeds its sliders from the displayed entry. A neutral layer reports the neutral
    /// value of every field rather than an empty object.
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        Ok(FIELDS
            .iter()
            .zip(values)
            .map(|(name, value)| ((*name).to_owned(), number(value)))
            .collect())
    }

    /// The payload as one spatial operation: dehaze, then texture, then clarity, the frozen order,
    /// with an amount-0 unit omitted because it is the exact identity.
    ///
    /// The stage decides every radius and therefore every halo, so each unit is built with the long
    /// side of the stage this layer is compiled against, which is the stage the host evaluates the
    /// operation at.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        // A neutral payload compiles to no units. The host drops an empty spatial operation
        // entirely, so the layer opens no stage boundary, the identity byte path is kept and the
        // render shares the source buffer.
        if is_neutral(&values) {
            return Ok(Processing::Spatial(SpatialOperation::neutral()));
        }
        let [texture, clarity, dehaze] = values;
        let long_side = stage.width.max(stage.height);
        let mut units: Vec<Arc<dyn SpatialUnit>> = Vec::with_capacity(FIELDS.len());
        if dehaze != NEUTRAL {
            units.push(Arc::new(dehaze::Dehaze::new(dehaze, long_side)));
        }
        if texture != NEUTRAL {
            units.push(Arc::new(texture::Texture::new(texture, long_side)));
        }
        if clarity != NEUTRAL {
            units.push(Arc::new(clarity::Clarity::new(clarity, long_side)));
        }
        Ok(Processing::Spatial(SpatialOperation::new(units)?))
    }
}

/// The summed halo of the three units in their frozen order at a stage long side, which the host
/// bounds at 512. Only the tests read it; production asks each unit for its own.
#[cfg(test)]
pub(crate) fn presence_halo(long_side: u32) -> i64 {
    dehaze::halo(long_side) + texture::halo(long_side) + clarity::halo(long_side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BASIC_EFFECT, modules::check_parameters};
    use serde_json::json;

    const STAGE: Stage = Stage {
        width: 24,
        height: 24,
    };

    fn presence_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    /// Plan one request the way the host does: generic parameter check, module parse, then plan
    /// against a stack whose stage questions are answered from constants.
    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = PresenceModule::new();
        let declared = module
            .descriptor()
            .action(action)
            .expect("a declared action");
        let checked = check_parameters(declared, &parameters)?;
        let input = module.parse(action, &checked)?;
        let sampler = |_: u32, _: u32| Ok(Some([0, 0, 0, 255]));
        let stage_before = |_: usize| Ok(STAGE);
        let insertion_index = |_: EffectStage| 0usize;
        let insertion_index_for = |_: &str| 0usize;
        let sample_before = |_: usize, _: u32, _: u32| Ok(Some([0, 0, 0, 255]));
        module.plan(
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
    }

    fn committed(plan: ActionPlan) -> Layer {
        match plan {
            ActionPlan::Commit(layer) | ActionPlan::Update(layer) => layer,
            ActionPlan::NoOp => panic!("expected a layer, not a no-op"),
            ActionPlan::Compose(_) => panic!("expected a layer, not a composite"),
        }
    }

    #[test]
    fn the_descriptor_declares_one_spatial_effect_two_actions_and_one_group() {
        let module = PresenceModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "lightwell.presence");
        assert_eq!(descriptor.title, "Presence");
        assert_eq!(
            descriptor.hint.as_deref(),
            Some("Texture, clarity and dehaze")
        );
        assert!(descriptor.collapsed);
        assert!(!descriptor.developer);
        assert_eq!(descriptor.effects.len(), 1);
        assert_eq!(descriptor.effects[0].id, PRESENCE_EFFECT);
        assert_eq!(descriptor.effects[0].format, 1);
        assert_eq!(descriptor.effects[0].stage, EffectStage::Spatial);
        assert_eq!(descriptor.effects[0].order, 0);
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: RESET_PRESENCE.into(),
                preset: Map::new(),
            })
        );
        assert!(descriptor.canvas.is_none());
        assert!(descriptor.queries.is_empty());

        let set = descriptor.action(SET_PRESENCE).expect("set-presence");
        assert!(set.patch);
        assert!(set.summary.is_none());
        assert_eq!(
            set.parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>(),
            FIELDS,
            "the three fields are declared parameters of the one patch action, in FIELDS order"
        );
        for parameter in &set.parameters {
            assert_eq!(
                parameter.kind,
                ParameterKind::Number {
                    min: -100.0,
                    max: 100.0
                },
                "{}",
                parameter.name
            );
            assert!(!parameter.required, "{}", parameter.name);
            assert_eq!(parameter.default, Some(json!(0.0)), "{}", parameter.name);
            assert_eq!(parameter.unit, None, "{}", parameter.name);
            assert_eq!(parameter.step, Some(1.0), "{}", parameter.name);
            assert_eq!(parameter.precision, Some(0), "{}", parameter.name);
            assert_eq!(parameter.zero, Some(0.0), "{}", parameter.name);
            assert!(!parameter.notes.is_empty(), "{}", parameter.name);
        }

        let reset = descriptor.action(RESET_PRESENCE).expect("reset-presence");
        assert!(reset.parameters.is_empty());
        assert!(!reset.patch);

        assert_eq!(descriptor.controls.len(), 1);
        let Control::Group {
            label,
            reset,
            controls,
            collapsed,
        } = &descriptor.controls[0]
        else {
            panic!("the one top-level control is a group");
        };
        assert_eq!(label, GROUP);
        assert!(!collapsed, "the one group starts expanded");
        let reset = reset.as_ref().expect("a group reset");
        assert_eq!(reset.action, SET_PRESENCE);
        assert_eq!(reset.preset.len(), 3);
        assert!(reset.preset.values().all(|value| *value == json!(0.0)));
        let labels: Vec<(&str, bool)> = controls
            .iter()
            .map(|control| match control {
                Control::Number { label, rail, .. } => (label.as_str(), rail.is_some()),
                _ => panic!("every group control is a slider"),
            })
            .collect();
        assert_eq!(
            labels,
            [("Texture", false), ("Clarity", false), ("Dehaze", false)],
            "three sliders on plain rails"
        );
    }

    #[test]
    fn a_payload_is_refused_by_shape_format_field_and_range_without_being_rewritten() {
        let module = PresenceModule::new();
        let refused = |format: u32, payload: Value| {
            module
                .validate_payload(PRESENCE_EFFECT, format, &payload)
                .expect_err("an invalid payload")
        };
        for payload in [
            json!({}),
            json!({"texture": 0}),
            json!({"texture": -100.0}),
            json!({"clarity": 100.0}),
            json!({"dehaze": 40.0, "texture": -20.0}),
        ] {
            module
                .validate_payload(PRESENCE_EFFECT, 1, &payload)
                .unwrap_or_else(|error| panic!("{payload} should be valid: {error}"));
        }

        let format = refused(2, json!({"texture": 1.0}));
        assert_eq!(format.kind, ErrorKind::Incompatible);
        assert!(format.detail.contains("unsupported effect format 2"));

        let effect = module
            .validate_payload(BASIC_EFFECT, 1, &json!({}))
            .expect_err("another effect");
        assert_eq!(effect.kind, ErrorKind::Incompatible);

        for (case, payload, needle) in [
            ("a list", json!([1.0]), "must be a JSON object"),
            ("a number", json!(1.0), "must be a JSON object"),
            (
                "an unknown key",
                json!({"structure": 10.0}),
                "unknown presence field structure",
            ),
            (
                "a string value",
                json!({"texture": "10"}),
                "must be a finite number",
            ),
            (
                "below the range",
                json!({"clarity": -100.001}),
                "within -100..=100",
            ),
            (
                "above the range",
                json!({"dehaze": 100.001}),
                "within -100..=100",
            ),
        ] {
            let error = refused(1, payload);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(needle), "{case}: {}", error.detail);
        }
    }

    #[test]
    fn plan_commits_updates_and_no_ops_exactly_like_set_basic() {
        let commit = planned(SET_PRESENCE, json!({"texture": 40.0}), &[]).unwrap();
        let layer = committed(commit);
        assert_eq!(layer.payload, json!({"texture": 40.0}));
        assert_eq!(layer.effect_id, PRESENCE_EFFECT);
        assert_eq!(layer.effect_format, 1);

        let update = committed(
            planned(
                SET_PRESENCE,
                json!({"dehaze": 15.0}),
                std::slice::from_ref(&layer),
            )
            .unwrap(),
        );
        assert_eq!(update.id, layer.id);
        assert_eq!(update.payload, json!({"texture": 40.0, "dehaze": 15.0}));

        assert_eq!(
            planned(
                SET_PRESENCE,
                json!({"texture": 40.0}),
                std::slice::from_ref(&layer)
            )
            .unwrap(),
            ActionPlan::NoOp,
            "an unchanged merge is a no-op"
        );

        // The canonical neutral payload is the empty object, and `{}` and an explicit zero are the
        // same state written differently.
        let neutralized = committed(
            planned(
                SET_PRESENCE,
                json!({"texture": 0.0}),
                std::slice::from_ref(&update),
            )
            .unwrap(),
        );
        assert_eq!(neutralized.payload, json!({"dehaze": 15.0}));
        for stored in [json!({}), json!({"clarity": 0.0})] {
            assert_eq!(
                planned(
                    SET_PRESENCE,
                    json!({"clarity": 0.0}),
                    &[presence_layer(stored.clone())]
                )
                .unwrap(),
                ActionPlan::NoOp,
                "setting neutral on a {stored} layer changes nothing"
            );
            assert_eq!(
                planned(RESET_PRESENCE, json!({}), &[presence_layer(stored.clone())]).unwrap(),
                ActionPlan::NoOp,
                "resetting a {stored} layer changes nothing"
            );
        }
        assert_eq!(
            planned(SET_PRESENCE, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "a neutral first set has nothing to commit"
        );
        assert_eq!(
            planned(RESET_PRESENCE, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "resetting with no layer at all is a no-op"
        );

        // `reset-presence` updates an edited layer in place, keeping its identity.
        let reset =
            committed(planned(RESET_PRESENCE, json!({}), std::slice::from_ref(&layer)).unwrap());
        assert_eq!(reset.id, layer.id);
        assert_eq!(reset.payload, json!({}));

        // Two layers are refused before this module ever plans against them.
        let error = planned(
            SET_PRESENCE,
            json!({"texture": 1.0}),
            &[layer.clone(), layer],
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, AMBIGUOUS);
    }

    #[test]
    fn labels_name_one_field_the_module_reset_or_the_field_count() {
        let module = PresenceModule::new();
        let label = |action: &str, parameters: Value| {
            module
                .label(&ActionInput {
                    action_id: action.into(),
                    parameters: parameters.as_object().unwrap().clone(),
                })
                .unwrap_or_default()
        };
        assert_eq!(label(SET_PRESENCE, json!({"texture": 40.0})), "Texture +40");
        assert_eq!(
            label(SET_PRESENCE, json!({"clarity": -20.0})),
            "Clarity -20"
        );
        assert_eq!(label(SET_PRESENCE, json!({"dehaze": 15.0})), "Dehaze +15");
        assert_eq!(
            module.label(&ActionInput {
                action_id: RESET_PRESENCE.into(),
                parameters: Map::new(),
            }),
            Some("Reset Presence".into())
        );
        assert_eq!(
            label(SET_PRESENCE, json!({"texture": 1.0, "clarity": 2.0})),
            "Presence (2 fields)"
        );
        assert_eq!(
            label(
                SET_PRESENCE,
                json!({"texture": 0.0, "clarity": 0.0, "dehaze": 0.0})
            ),
            "Reset Presence",
            "the group's reset sends every field at neutral and is named as the module reset is"
        );
        assert_eq!(
            module.label(&ActionInput {
                action_id: SET_PRESENCE.into(),
                parameters: Map::new(),
            }),
            None
        );
    }

    #[test]
    fn values_reports_every_field_and_describe_layer_lists_non_neutral_ones() {
        let module = PresenceModule::new();
        let payload = json!({"texture": 40.0, "dehaze": -15.0});
        let values = module.values(PRESENCE_EFFECT, 1, &payload).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values["texture"], json!(40.0));
        assert_eq!(values["clarity"], json!(0.0));
        assert_eq!(values["dehaze"], json!(-15.0));

        assert_eq!(
            module.describe_layer(PRESENCE_EFFECT, 1, &payload).unwrap(),
            "Texture +40, Dehaze -15"
        );
        assert_eq!(
            module
                .describe_layer(PRESENCE_EFFECT, 1, &json!({}))
                .unwrap(),
            "Neutral"
        );
    }

    #[test]
    fn compile_is_neutral_for_the_empty_payload_and_orders_the_units_dehaze_texture_clarity() {
        let module = PresenceModule::new();
        let neutral = module
            .compile(PRESENCE_EFFECT, 1, &json!({}), STAGE)
            .unwrap();
        assert_eq!(neutral, Processing::Spatial(SpatialOperation::neutral()));

        let operation = |payload: Value| -> SpatialOperation {
            match module.compile(PRESENCE_EFFECT, 1, &payload, STAGE).unwrap() {
                Processing::Spatial(operation) => operation,
                other => panic!("expected a spatial operation, got {other:?}"),
            }
        };

        let all = operation(json!({"texture": 10.0, "clarity": -20.0, "dehaze": 30.0}));
        assert_eq!(all.len(), 3);
        let described: Vec<String> = all.units().iter().map(|unit| unit.describe()).collect();
        assert!(described[0].starts_with("presence dehaze"), "{described:?}");
        assert!(
            described[1].starts_with("presence texture"),
            "{described:?}"
        );
        assert!(
            described[2].starts_with("presence clarity"),
            "{described:?}"
        );

        // An amount-0 unit is the exact identity, so it is not in the operation at all.
        let one = operation(json!({"clarity": -20.0}));
        assert_eq!(one.len(), 1);
        assert!(one.units()[0].describe().starts_with("presence clarity"));
        let two = operation(json!({"texture": 5.0, "dehaze": 5.0}));
        assert_eq!(two.len(), 2);
        assert!(two.units()[0].describe().starts_with("presence dehaze"));
        assert!(two.units()[1].describe().starts_with("presence texture"));
    }

    /// Every halo the three units declare, against the study's own table, and the summed halo
    /// against the host's 512 px bound. The reference's `halos_at_the_documented_sizes` asserts the
    /// same numbers on the `f64` side.
    #[test]
    fn the_declared_halos_match_the_study_table_and_fit_the_host_bound() {
        let halos = |long_side: u32| {
            (
                texture::halo(long_side),
                clarity::halo(long_side),
                dehaze::halo(long_side),
            )
        };
        assert_eq!(halos(480), (4, 23, 19));
        assert_eq!(halos(6000), (8, 199, 67));
        assert_eq!(halos(10_000), (14, 327, 107));
        assert_eq!(presence_halo(480), 46);
        assert_eq!(presence_halo(6000), 274);
        assert_eq!(presence_halo(10_000), 448);
        // The scale cap holds the halo flat above 60 MP, so the largest stage the host accepts
        // still fits the bound.
        assert_eq!(presence_halo(16_384), 448);
        assert!(presence_halo(16_384) <= i64::from(crate::modules::MAX_SPATIAL_HALO));

        // And the compiled units report exactly those halos to the host.
        let module = PresenceModule::new();
        let stage = Stage {
            width: 6000,
            height: 4000,
        };
        let Processing::Spatial(operation) = module
            .compile(
                PRESENCE_EFFECT,
                1,
                &json!({"texture": 10.0, "clarity": 10.0, "dehaze": 10.0}),
                stage,
            )
            .unwrap()
        else {
            panic!("a spatial operation");
        };
        assert_eq!(operation.halos(stage), vec![67, 8, 199]);
        assert_eq!(operation.summed_halo(stage), 274);
    }
}
