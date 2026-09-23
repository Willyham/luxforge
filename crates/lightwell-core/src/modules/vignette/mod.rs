//! The Vignette module: one finish-stage layer holding every Vignette parameter, edited by one
//! field-patch action.
//!
//! A payload is a JSON object whose keys are the four implemented parameter names (`amount`,
//! `midpoint`, `roundness`, `feather`); a **missing key means that parameter's own default**
//! (amount 0, midpoint 50, roundness 0, feather 50) rather than 0, unlike the Basic module, where a
//! missing key means neutral 0 for every field because every Basic field's neutral value happens to
//! be 0. The canonical all-default payload is `{}`, and `{"midpoint": 50}` is the same state written
//! differently.
//!
//! The layer as a whole is neutral — compiles to no processing at all — exactly when `amount` is
//! `0`, whatever `midpoint`, `roundness` and `feather` hold: the frozen mask geometry
//! (`docs/design/vignette-study.md`) never matters when the amount equation is the identity, so the
//! module never builds a mask table for a layer that changes nothing.
//!
//! The host places the layer at the end of the stack, after the geometry tail, because
//! `lightwell.vignette.postcrop` declares the `finish` stage: a later crop update moves the crop
//! layer in place before this one, so the vignette recentres on the new stage exactly.
mod unit;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, ColorOperation, Control,
    EffectDescriptor, EffectStage, ModuleDescriptor, ParameterDescriptor, ParameterKind,
    PointwiseColor, Processing, ResetAction, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId, VIGNETTE_EFFECT};
use serde_json::{Map, Number, Value};
use std::sync::Arc;
use unit::Vignette;

pub(super) const SET_VIGNETTE: &str = "set-vignette";
pub(super) const RESET_VIGNETTE: &str = "reset-vignette";

const AMOUNT: &str = "amount";
const MIDPOINT: &str = "midpoint";
const ROUNDNESS: &str = "roundness";
const FEATHER: &str = "feather";

const AMOUNT_LABEL: &str = "Amount";
const MIDPOINT_LABEL: &str = "Midpoint";
const ROUNDNESS_LABEL: &str = "Roundness";
const FEATHER_LABEL: &str = "Feather";

const AMOUNT_MIN: f64 = -100.0;
const AMOUNT_MAX: f64 = 100.0;
const MIDPOINT_MIN: f64 = 0.0;
const MIDPOINT_MAX: f64 = 100.0;
const ROUNDNESS_MIN: f64 = -100.0;
const ROUNDNESS_MAX: f64 = 100.0;
const FEATHER_MIN: f64 = 0.0;
const FEATHER_MAX: f64 = 100.0;

const STEP: f64 = 1.0;
const PRECISION: u8 = 0;

/// Every implemented Vignette field, in the payload's declared order, which is also the order the
/// group's four sliders render in.
const FIELDS: [&str; 4] = [AMOUNT, MIDPOINT, ROUNDNESS, FEATHER];

/// Each field's own default, read when its key is missing from a payload — **not** a shared
/// neutral value the way Basic's fields are: `midpoint` and `feather` default to `50`, not `0`.
const DEFAULTS: [f64; 4] = [0.0, 50.0, 0.0, 50.0];

/// The label the one group and the module section share (`"Vignette"` in both places, since there
/// is only one group).
const GROUP_LABEL: &str = "Vignette";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Incompatible, detail)
}

fn index_of(name: &str) -> usize {
    FIELDS
        .iter()
        .position(|field| *field == name)
        .expect("index_of is only ever called with a name from FIELDS")
}

fn default_of(name: &str) -> f64 {
    DEFAULTS[index_of(name)]
}

fn range(name: &str) -> (f64, f64) {
    match name {
        AMOUNT => (AMOUNT_MIN, AMOUNT_MAX),
        MIDPOINT => (MIDPOINT_MIN, MIDPOINT_MAX),
        ROUNDNESS => (ROUNDNESS_MIN, ROUNDNESS_MAX),
        FEATHER => (FEATHER_MIN, FEATHER_MAX),
        // Unreachable: FIELDS is the closed set and every caller matched a name against it.
        _ => (0.0, 0.0),
    }
}

fn label_of(name: &str) -> &'static str {
    match name {
        AMOUNT => AMOUNT_LABEL,
        MIDPOINT => MIDPOINT_LABEL,
        ROUNDNESS => ROUNDNESS_LABEL,
        FEATHER => FEATHER_LABEL,
        _ => "Vignette",
    }
}

/// `amount` and `roundness` are signed in their history label (`+20`/`-35`); `midpoint` and
/// `feather` are unsigned (`60`/`40`), matching the study's ranges: the first two are bipolar
/// about 0, the second two are one-sided magnitudes.
fn signed(name: &str) -> bool {
    matches!(name, AMOUNT | ROUNDNESS)
}

/// A history label names the module and the field, `Vignette amount -35`, because `Amount` alone
/// says nothing in a history list shared with every other module.
fn field_label(name: &str, value: f64) -> String {
    let label = label_of(name).to_ascii_lowercase();
    if signed(name) {
        format!("{GROUP_LABEL} {label} {value:+.0}")
    } else {
        format!("{GROUP_LABEL} {label} {value:.0}")
    }
}

/// One named field of a canonical value array.
fn value_of(values: &[f64; FIELDS.len()], name: &str) -> f64 {
    values[index_of(name)]
}

/// One field of a validated payload or request: a missing key is that field's own default.
fn field(source: &Map<String, Value>, name: &str) -> f64 {
    source
        .get(name)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| default_of(name))
}

/// The canonical values a payload represents, in `FIELDS` order. Absent and explicitly
/// default-valued keys produce the same array, which is what makes `{}` and
/// `{"midpoint": 50}` compare equal.
fn canonical(payload: &Map<String, Value>) -> [f64; FIELDS.len()] {
    FIELDS.map(|name| field(payload, name))
}

/// Whether the layer as a whole is neutral: `amount == 0`, whatever the other three fields hold.
/// The frozen mask geometry never runs when the amount equation is itself the identity.
fn is_amount_neutral(values: &[f64; FIELDS.len()]) -> bool {
    value_of(values, AMOUNT) == 0.0
}

fn is_all_default(values: &[f64; FIELDS.len()]) -> bool {
    *values == DEFAULTS
}

/// The canonical stored form of a set of values: only the fields that differ from their own
/// default, so the all-default payload is exactly `{}`.
fn payload_of(values: &[f64; FIELDS.len()]) -> Value {
    let mut payload = Map::new();
    for (name, value) in FIELDS.iter().zip(values) {
        if *value != default_of(name) {
            payload.insert((*name).to_owned(), number(*value));
        }
    }
    Value::Object(payload)
}

/// A finite f64 as a JSON number. Finiteness is checked before every call, so the fallback is
/// never reached in practice and never panics if it is.
fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// The object a stored payload must be, with the effect identity and format checked first: an
/// unsupported format is `incompatible` and is never rewritten, and every present field is a
/// finite number inside its declared range.
fn read_payload(effect_id: &str, format: u32, value: &Value) -> Result<Map<String, Value>, Error> {
    if effect_id != VIGNETTE_EFFECT {
        return Err(incompatible(format!("unavailable effect {effect_id}")));
    }
    if format != EFFECT_FORMAT {
        return Err(incompatible(format!("unsupported effect format {format}")));
    }
    let object = value
        .as_object()
        .ok_or_else(|| validation("vignette payload must be a JSON object"))?;
    for (name, value) in object {
        if !FIELDS.contains(&name.as_str()) {
            return Err(validation(format!("unknown vignette field {name}")));
        }
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| validation(format!("vignette field {name} must be a finite number")))?;
        let (min, max) = range(name);
        if number < min || number > max {
            return Err(validation(format!(
                "vignette field {name} must be a number within {min}..={max}"
            )));
        }
    }
    Ok(object.clone())
}

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check
/// (which builds the same text from the module's own title).
pub(crate) const AMBIGUOUS: &str = "ambiguous Vignette layers";

/// The stack's one Vignette layer. Two of them would each claim to be the Vignette state, so every
/// path refuses to guess which one an action addresses; nothing is rewritten.
fn locate(layers: &[Layer]) -> Result<Option<&Layer>, Error> {
    let mut found = None;
    for layer in layers {
        if layer.effect_id == VIGNETTE_EFFECT {
            if found.is_some() {
                return Err(validation(AMBIGUOUS));
            }
            found = Some(layer);
        }
    }
    Ok(found)
}

fn number_parameter(
    name: &str,
    min: f64,
    max: f64,
    default: f64,
    zero: Option<f64>,
    notes: &str,
) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number { min, max },
        required: false,
        default: Some(number(default)),
        unit: None,
        step: Some(STEP),
        precision: Some(PRECISION),
        notes: notes.into(),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero,
    }
}

fn amount_parameter() -> ParameterDescriptor {
    number_parameter(
        AMOUNT,
        AMOUNT_MIN,
        AMOUNT_MAX,
        default_of(AMOUNT),
        Some(0.0),
        "post-crop vignette strength: negative darkens toward black in linear light with gain \
         1 - |amount|*mask, positive lightens toward encoded white through a compressive mapping \
         that never pushes a below-white channel past it. 0 is the exact identity whatever \
         midpoint, roundness and feather hold, and a missing key defaults to 0.",
    )
}

fn midpoint_parameter() -> ParameterDescriptor {
    number_parameter(
        MIDPOINT,
        MIDPOINT_MIN,
        MIDPOINT_MAX,
        default_of(MIDPOINT),
        None,
        "where the falloff begins, as a fraction of the shape radius from the centre; a missing \
         key defaults to 50.",
    )
}

fn roundness_parameter() -> ParameterDescriptor {
    number_parameter(
        ROUNDNESS,
        ROUNDNESS_MIN,
        ROUNDNESS_MAX,
        default_of(ROUNDNESS),
        Some(0.0),
        "morphs the mask shape from a rounded rectangle (-100) through an ellipse (0) to a circle \
         (100); a missing key defaults to 0.",
    )
}

fn feather_parameter() -> ParameterDescriptor {
    number_parameter(
        FEATHER,
        FEATHER_MIN,
        FEATHER_MAX,
        default_of(FEATHER),
        None,
        "the width of the falloff transition, as a fraction of the shape radius; a missing key \
         defaults to 50.",
    )
}

/// The all-default preset a reset writes, keyed by field name.
fn default_preset() -> Map<String, Value> {
    FIELDS
        .iter()
        .map(|name| ((*name).to_owned(), number(default_of(name))))
        .collect()
}

/// Whether a sent `set-vignette` patch holds exactly every field, each at its own default: the
/// group's own reset button sends this patch, so it deserves the same "Reset Vignette" label the
/// non-patch `reset-vignette` action gets.
fn is_reset_patch(sent: &[(&String, f64)]) -> bool {
    sent.len() == FIELDS.len()
        && sent
            .iter()
            .all(|(name, value)| FIELDS.contains(&name.as_str()) && *value == default_of(name))
}

#[derive(Debug)]
pub struct VignetteModule {
    descriptor: ModuleDescriptor,
}

impl Default for VignetteModule {
    fn default() -> Self {
        Self::new()
    }
}

impl VignetteModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.vignette".into(),
                title: GROUP_LABEL.into(),
                hint: Some("Darken or lighten the corners after the crop".into()),
                effects: vec![EffectDescriptor {
                    id: VIGNETTE_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Finish,
                    order: 0,
                    artifacts: false,
                }],
                actions: vec![
                    ActionDescriptor {
                        id: SET_VIGNETTE.into(),
                        title: "Set Vignette".into(),
                        notes: "merges the named Vignette fields into the stack's one Vignette \
                                 layer. A missing key means that field's own default (amount 0, \
                                 midpoint 50, roundness 0, feather 50), not neutral 0 for every \
                                 field: unlike Basic, midpoint and feather default away from 0. \
                                 The host places the layer at the end of the stack, after the \
                                 geometry tail, on the first commit whose merged amount is \
                                 non-zero, and updates it there in place afterwards; a patch that \
                                 changes nothing is a reported no-op, and a first set whose \
                                 merged amount is still 0 commits no layer at all."
                            .into(),
                        summary: None,
                        patch: true,
                        parameters: vec![
                            amount_parameter(),
                            midpoint_parameter(),
                            roundness_parameter(),
                            feather_parameter(),
                        ],
                    },
                    ActionDescriptor {
                        id: RESET_VIGNETTE.into(),
                        title: "Reset Vignette".into(),
                        notes: "returns the stack's one Vignette layer to its all-default \
                                 payload, keeping its identity and position; a no-op without one \
                                 and when it is already all default."
                            .into(),
                        summary: None,
                        patch: false,
                        parameters: Vec::new(),
                    },
                ],
                queries: Vec::new(),
                controls: vec![Control::Group {
                    label: GROUP_LABEL.into(),
                    reset: Some(ResetAction {
                        action: SET_VIGNETTE.into(),
                        preset: default_preset(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_VIGNETTE.into(),
                            parameter: AMOUNT.into(),
                            label: AMOUNT_LABEL.into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                        },
                        Control::Number {
                            action: SET_VIGNETTE.into(),
                            parameter: MIDPOINT.into(),
                            label: MIDPOINT_LABEL.into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                        },
                        Control::Number {
                            action: SET_VIGNETTE.into(),
                            parameter: ROUNDNESS.into(),
                            label: ROUNDNESS_LABEL.into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                        },
                        Control::Number {
                            action: SET_VIGNETTE.into(),
                            parameter: FEATHER.into(),
                            label: FEATHER_LABEL.into(),
                            style: crate::NumberStyle::Slider,
                            rail: None,
                        },
                    ],
                    collapsed: false,
                }],
                reset: Some(ResetAction {
                    action: RESET_VIGNETTE.into(),
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

impl ToolModule for VignetteModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// At most one Vignette layer exists in a stack, so the host refuses to compile or plan
    /// against a stack that holds two instead of guessing which one an action addresses.
    fn single_layer(&self, effect_id: &str) -> bool {
        effect_id == VIGNETTE_EFFECT
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            SET_VIGNETTE => parameters.clone(),
            RESET_VIGNETTE => Map::new(),
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
            None => DEFAULTS,
        };
        let merged = match input.action_id.as_str() {
            SET_VIGNETTE => {
                let mut merged = current;
                for (slot, name) in merged.iter_mut().zip(FIELDS) {
                    if let Some(value) = input.parameters.get(name) {
                        *slot = value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| {
                                validation(format!("vignette field {name} must be a finite number"))
                            })?;
                        let (min, max) = range(name);
                        if *slot < min || *slot > max {
                            return Err(validation(format!(
                                "vignette field {name} must be a number within {min}..={max}"
                            )));
                        }
                    }
                }
                merged
            }
            RESET_VIGNETTE => DEFAULTS,
            action_id => return Err(validation(format!("unknown action {action_id}"))),
        };
        match existing {
            Some(_) if current == merged => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(Layer {
                id: layer.id.clone(),
                effect_id: VIGNETTE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                artifacts: Vec::new(),
            })),
            // The host inserts a finish-stage layer at the end of the stack; a first set whose
            // merged amount is still 0 has nothing visible to store, so it adds no layer at all.
            None if is_amount_neutral(&merged) => Ok(ActionPlan::NoOp),
            None => Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: VIGNETTE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                artifacts: Vec::new(),
            })),
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        read_payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        if is_all_default(&values) {
            return Ok("Neutral".into());
        }
        Ok(FIELDS
            .iter()
            .zip(values)
            .filter(|(name, value)| *value != default_of(name))
            .map(|(name, value)| field_label(name, value))
            .collect::<Vec<_>>()
            .join(", "))
    }

    /// The history label for a request the action's title cannot describe: the one field a
    /// slider moved, the group's reset sent as a full-default patch, or a reset action.
    fn label(&self, input: &ActionInput) -> Option<String> {
        match input.action_id.as_str() {
            RESET_VIGNETTE => Some(format!("Reset {GROUP_LABEL}")),
            SET_VIGNETTE => {
                let sent: Vec<(&String, f64)> = input
                    .parameters
                    .iter()
                    .map(|(name, value)| (name, value.as_f64().unwrap_or_else(|| default_of(name))))
                    .collect();
                if is_reset_patch(&sent) {
                    Some(format!("Reset {GROUP_LABEL}"))
                } else {
                    match sent.as_slice() {
                        [(name, value)] => Some(field_label(name, *value)),
                        [] => None,
                        fields => Some(format!("{GROUP_LABEL} ({} fields)", fields.len())),
                    }
                }
            }
            _ => None,
        }
    }

    /// The four effective values this stored layer represents, with defaults filled: named
    /// exactly as `set-vignette`'s parameters are, so a client seeds its sliders from the
    /// displayed entry.
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

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        // The layer as a whole is neutral whenever amount is 0, whatever the other three fields
        // hold: the mask never has to be built, and the identity byte path and shared source
        // buffer are kept.
        if is_amount_neutral(&values) {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        let unit: Arc<dyn PointwiseColor> = Arc::new(Vignette::new(
            value_of(&values, AMOUNT),
            value_of(&values, MIDPOINT),
            value_of(&values, ROUNDNESS),
            value_of(&values, FEATHER),
            stage,
        ));
        Ok(Processing::Color(ColorOperation::new(vec![unit])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::check_parameters;
    use serde_json::json;

    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn vignette_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: VIGNETTE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            artifacts: Vec::new(),
        }
    }

    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = VignetteModule::new();
        let declared = module
            .descriptor()
            .action(action)
            .expect("a declared action");
        let checked = check_parameters(declared, &parameters)?;
        let input = module.parse(action, &checked)?;
        let sampler = |_: u32, _: u32| Ok(Some([0, 0, 0, 255]));
        let stage_before = |_: usize| Ok(STAGE);
        let insertion_index = |_: EffectStage| layers.len();
        let insertion_index_for = |_: &str| layers.len();
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
    fn the_descriptor_declares_one_finish_effect_collapsed_group_and_two_actions() {
        let module = VignetteModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "lightwell.vignette");
        assert_eq!(descriptor.title, "Vignette");
        assert_eq!(
            descriptor.hint.as_deref(),
            Some("Darken or lighten the corners after the crop")
        );
        assert!(!descriptor.developer);
        assert!(descriptor.collapsed, "the section starts collapsed");
        assert_eq!(descriptor.effects.len(), 1);
        assert_eq!(descriptor.effects[0].id, VIGNETTE_EFFECT);
        assert_eq!(descriptor.effects[0].format, EFFECT_FORMAT);
        assert_eq!(descriptor.effects[0].stage, EffectStage::Finish);
        assert_eq!(descriptor.effects[0].order, 0);
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: RESET_VIGNETTE.into(),
                preset: Map::new(),
            })
        );
        assert!(descriptor.canvas.is_none());
        assert!(descriptor.queries.is_empty());

        let set = descriptor.action(SET_VIGNETTE).expect("set-vignette");
        assert!(set.patch);
        assert_eq!(set.parameters.len(), 4);
        assert_eq!(
            set.parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>(),
            FIELDS,
        );
        for (name, (min, max), default) in [
            (AMOUNT, (-100.0, 100.0), 0.0),
            (MIDPOINT, (0.0, 100.0), 50.0),
            (ROUNDNESS, (-100.0, 100.0), 0.0),
            (FEATHER, (0.0, 100.0), 50.0),
        ] {
            let parameter = set.parameter(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(parameter.kind, ParameterKind::Number { min, max }, "{name}");
            assert!(!parameter.required, "{name}");
            assert_eq!(parameter.default, Some(json!(default)), "{name}");
            assert_eq!(parameter.unit, None, "{name}");
            assert_eq!(parameter.step, Some(1.0), "{name}");
            assert_eq!(parameter.precision, Some(0), "{name}");
            assert!(!parameter.notes.is_empty(), "{name}");
        }
        assert_eq!(set.parameter(AMOUNT).unwrap().zero, Some(0.0));
        assert_eq!(set.parameter(ROUNDNESS).unwrap().zero, Some(0.0));

        let reset = descriptor.action(RESET_VIGNETTE).expect("reset-vignette");
        assert!(!reset.patch);
        assert!(reset.parameters.is_empty());

        assert_eq!(descriptor.controls.len(), 1);
        match &descriptor.controls[0] {
            Control::Group {
                label,
                controls,
                reset,
                collapsed,
            } => {
                assert_eq!(label, "Vignette");
                assert!(!collapsed, "the one group itself is not collapsed");
                assert_eq!(controls.len(), 4);
                let names: Vec<&str> = controls
                    .iter()
                    .map(|control| match control {
                        Control::Number { parameter, .. } => parameter.as_str(),
                        other => panic!("expected a Number control, got {other:?}"),
                    })
                    .collect();
                assert_eq!(
                    names, FIELDS,
                    "Amount, Midpoint, Roundness, Feather in order"
                );
                for control in controls {
                    if let Control::Number { style, rail, .. } = control {
                        assert_eq!(*style, crate::NumberStyle::Slider);
                        assert!(rail.is_none(), "rails are plain");
                    }
                }
                let reset = reset.as_ref().expect("the group has a reset");
                assert_eq!(reset.action, SET_VIGNETTE);
                assert_eq!(
                    reset.preset,
                    json!({"amount": 0.0, "midpoint": 50.0, "roundness": 0.0, "feather": 50.0})
                        .as_object()
                        .cloned()
                        .unwrap()
                );
            }
            other => panic!("expected a Group control, got {other:?}"),
        }
    }

    #[test]
    fn module_list_and_schema_list_report_the_vignette_effect_and_both_actions() {
        use crate::modules::ModuleRegistry;
        let registry = ModuleRegistry::builtin();
        let descriptors = serde_json::to_value(registry.descriptors()).expect("descriptor JSON");
        let vignette = descriptors
            .as_array()
            .expect("an array")
            .iter()
            .find(|descriptor| descriptor["id"] == json!("lightwell.vignette"))
            .expect("the vignette module is registered");
        assert_eq!(vignette["collapsed"], json!(true));
        assert_eq!(
            vignette["effects"][0],
            json!({"id": VIGNETTE_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0})
        );
        assert!(registry.action(SET_VIGNETTE).is_some());
        assert!(registry.action(RESET_VIGNETTE).is_some());
    }

    // ------------------------------------------------------------------------------------------
    // Plan semantics
    // ------------------------------------------------------------------------------------------

    #[test]
    fn a_first_set_with_zero_amount_commits_nothing_even_with_other_fields_set() {
        let plan = planned(
            SET_VIGNETTE,
            json!({"midpoint": 80.0, "roundness": -40.0, "feather": 10.0}),
            &[],
        )
        .expect("a plan");
        assert_eq!(plan, ActionPlan::NoOp);
    }

    #[test]
    fn a_first_set_with_non_zero_amount_commits_at_the_end_of_the_stack() {
        let plan = planned(SET_VIGNETTE, json!({"amount": -35.0}), &[]).expect("a plan");
        let layer = committed(plan);
        assert_eq!(layer.effect_id, VIGNETTE_EFFECT);
        assert_eq!(
            layer.payload,
            json!({"amount": -35.0}),
            "midpoint/roundness/feather are omitted at their own default"
        );
    }

    #[test]
    fn a_later_set_updates_the_existing_layer_in_place() {
        let existing = vignette_layer(json!({"amount": -35.0}));
        let id = existing.id.clone();
        let plan = planned(
            SET_VIGNETTE,
            json!({"amount": -35.0, "feather": 80.0}),
            &[existing],
        )
        .expect("a plan");
        match plan {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, id);
                assert_eq!(layer.payload, json!({"amount": -35.0, "feather": 80.0}));
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn an_equal_merged_payload_is_a_no_op() {
        let existing = vignette_layer(json!({"amount": -35.0, "feather": 80.0}));
        // Sending the field already at its stored value, and sending feather at its default (80
        // is already stored so this checks the "sent equals stored" case, not a default fill).
        let plan = planned(SET_VIGNETTE, json!({"amount": -35.0}), &[existing]).expect("a plan");
        assert_eq!(plan, ActionPlan::NoOp);
    }

    #[test]
    fn missing_keys_default_away_from_zero_for_midpoint_and_feather() {
        // A payload with only amount set must still describe midpoint=50 and feather=50, not 0.
        let existing = vignette_layer(json!({"amount": -20.0}));
        let values = VignetteModule::new()
            .values(VIGNETTE_EFFECT, EFFECT_FORMAT, &existing.payload)
            .expect("values");
        assert_eq!(values["amount"], json!(-20.0));
        assert_eq!(values["midpoint"], json!(50.0));
        assert_eq!(values["roundness"], json!(0.0));
        assert_eq!(values["feather"], json!(50.0));
    }

    #[test]
    fn reset_returns_an_existing_layer_to_all_defaults_keeping_identity() {
        let existing = vignette_layer(json!({"amount": -35.0, "midpoint": 80.0}));
        let id = existing.id.clone();
        let plan = planned(RESET_VIGNETTE, json!({}), &[existing]).expect("a plan");
        match plan {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, id);
                assert_eq!(layer.payload, json!({}));
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn reset_without_a_layer_or_already_default_is_a_no_op() {
        assert_eq!(
            planned(RESET_VIGNETTE, json!({}), &[]).expect("a plan"),
            ActionPlan::NoOp
        );
        let already_default = vignette_layer(json!({}));
        assert_eq!(
            planned(RESET_VIGNETTE, json!({}), &[already_default]).expect("a plan"),
            ActionPlan::NoOp
        );
    }

    #[test]
    fn two_vignette_layers_are_refused_as_ambiguous() {
        let layers = [
            vignette_layer(json!({"amount": 10.0})),
            vignette_layer(json!({"amount": 20.0})),
        ];
        let error = planned(SET_VIGNETTE, json!({"amount": 30.0}), &layers).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, AMBIGUOUS);
    }

    // ------------------------------------------------------------------------------------------
    // Labels
    // ------------------------------------------------------------------------------------------

    #[test]
    fn labels_match_the_declared_forms() {
        let module = VignetteModule::new();
        let label = |action: &str, parameters: Value| {
            module.label(&ActionInput {
                action_id: action.into(),
                parameters: parameters.as_object().cloned().unwrap_or_default(),
            })
        };
        assert_eq!(
            label(SET_VIGNETTE, json!({"amount": -35.0})),
            Some("Vignette amount -35".into())
        );
        assert_eq!(
            label(SET_VIGNETTE, json!({"midpoint": 60.0})),
            Some("Vignette midpoint 60".into())
        );
        assert_eq!(
            label(SET_VIGNETTE, json!({"roundness": 20.0})),
            Some("Vignette roundness +20".into())
        );
        assert_eq!(
            label(SET_VIGNETTE, json!({"feather": 40.0})),
            Some("Vignette feather 40".into())
        );
        assert_eq!(
            label(RESET_VIGNETTE, json!({})),
            Some("Reset Vignette".into())
        );
        assert_eq!(
            label(
                SET_VIGNETTE,
                json!({"amount": 0.0, "midpoint": 50.0, "roundness": 0.0, "feather": 50.0})
            ),
            Some("Reset Vignette".into()),
            "the group's own reset sends a full-default patch"
        );
        assert_eq!(
            label(SET_VIGNETTE, json!({"amount": -10.0, "midpoint": 60.0})),
            Some("Vignette (2 fields)".into())
        );
        assert_eq!(label(SET_VIGNETTE, json!({})), None);
    }

    // ------------------------------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------------------------------

    #[test]
    fn validate_payload_refuses_unknown_keys_and_out_of_range_values() {
        let module = VignetteModule::new();
        let unknown = module.validate_payload(VIGNETTE_EFFECT, EFFECT_FORMAT, &json!({"foo": 1}));
        assert_eq!(unknown.unwrap_err().kind, ErrorKind::Validation);
        for (name, value) in [
            (AMOUNT, 101.0),
            (AMOUNT, -101.0),
            (MIDPOINT, -1.0),
            (MIDPOINT, 101.0),
            (ROUNDNESS, 101.0),
            (FEATHER, -1.0),
        ] {
            let error = module
                .validate_payload(VIGNETTE_EFFECT, EFFECT_FORMAT, &json!({ name: value }))
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{name}={value}");
        }
        assert!(
            module
                .validate_payload(VIGNETTE_EFFECT, EFFECT_FORMAT, &json!({"amount": 50.0}))
                .is_ok()
        );
        let wrong_format = module.validate_payload(VIGNETTE_EFFECT, 2, &json!({}));
        assert_eq!(wrong_format.unwrap_err().kind, ErrorKind::Incompatible);
    }

    // ------------------------------------------------------------------------------------------
    // describe_layer / compile
    // ------------------------------------------------------------------------------------------

    #[test]
    fn describe_layer_reports_neutral_or_the_non_default_fields() {
        let module = VignetteModule::new();
        assert_eq!(
            module
                .describe_layer(VIGNETTE_EFFECT, EFFECT_FORMAT, &json!({}))
                .unwrap(),
            "Neutral"
        );
        assert_eq!(
            module
                .describe_layer(VIGNETTE_EFFECT, EFFECT_FORMAT, &json!({"amount": -35.0}))
                .unwrap(),
            "Vignette amount -35"
        );
    }

    #[test]
    fn compile_of_a_zero_amount_payload_is_the_neutral_colour_operation() {
        let module = VignetteModule::new();
        let processing = module
            .compile(
                VIGNETTE_EFFECT,
                EFFECT_FORMAT,
                &json!({"midpoint": 80.0, "roundness": -50.0, "feather": 90.0}),
                STAGE,
            )
            .expect("compiles");
        match processing {
            Processing::Color(operation) => {
                assert!(operation.is_empty(), "amount=0 must compile to no units");
            }
            other => panic!("expected Processing::Color, got {other:?}"),
        }
    }

    #[test]
    fn compile_of_a_non_zero_amount_payload_produces_exactly_one_unit() {
        let module = VignetteModule::new();
        let processing = module
            .compile(
                VIGNETTE_EFFECT,
                EFFECT_FORMAT,
                &json!({"amount": -35.0}),
                STAGE,
            )
            .expect("compiles");
        match processing {
            Processing::Color(operation) => assert_eq!(operation.len(), 1),
            other => panic!("expected Processing::Color, got {other:?}"),
        }
    }
}
