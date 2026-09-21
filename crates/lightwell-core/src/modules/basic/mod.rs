//! The Basic adjustment module: one colour-stage layer holding every Basic parameter, edited by
//! one field-patch action.
//!
//! This slice implements Exposure, Vibrance and Saturation. A payload is a JSON object whose keys
//! are the implemented parameter names; a missing key is neutral, so the canonical neutral payload
//! is the empty object `{}` and `{"exposure": 0}` is the same state written differently. Every
//! comparison here is between canonical values, never between JSON maps, so the two forms are
//! never mistaken for a change.
//!
//! The host places the layer by its effect stage: a colour-stage commit joins the stack before the
//! geometry tail, like a pixel replacement, and stays at that position for the rest of its life.
//! Later sets update it in place at the same identity and index.
mod colour;
mod exposure;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, ColorOperation, Control,
    EffectDescriptor, EffectStage, ModuleDescriptor, ParameterDescriptor, ParameterKind,
    PointwiseColor, Processing, ResetAction, Stage, StageContext, ToolModule,
};
use crate::{BASIC_EFFECT, EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId};
use colour::{Saturation, Vibrance};
use exposure::Exposure;
use serde_json::{Map, Number, Value};
use std::sync::Arc;

pub(super) const SET_BASIC: &str = "set-basic";
pub(super) const RESET_BASIC: &str = "reset-basic";

const EXPOSURE: &str = "exposure";
const EXPOSURE_MIN: f64 = -5.0;
const EXPOSURE_MAX: f64 = 5.0;
const EXPOSURE_STEP: f64 = 0.01;
const EXPOSURE_PRECISION: u8 = 2;
const EXPOSURE_UNIT: &str = "EV";
const EXPOSURE_LABEL: &str = "Exposure";

/// Vibrance and Saturation: `docs/design/basic-colour.md`'s frozen range, step and precision. No
/// unit is declared; the design states the accepted range directly in slider units.
const VIBRANCE: &str = "vibrance";
const SATURATION: &str = "saturation";
const COLOUR_MIN: f64 = -100.0;
const COLOUR_MAX: f64 = 100.0;
const COLOUR_STEP: f64 = 1.0;
const COLOUR_PRECISION: u8 = 0;
const VIBRANCE_LABEL: &str = "Vibrance";
const SATURATION_LABEL: &str = "Saturation";

/// The group label the Tone controls share, and the label a patch that returns every one of its
/// fields to neutral takes in history.
const TONE_GROUP: &str = "Tone";

/// The group label the Vibrance/Saturation controls share, and the label a patch that returns
/// every one of its fields to neutral takes in history.
const COLOUR_GROUP: &str = "Colour";

/// Every implemented Basic field, in the payload's declared order. A later slice adds further
/// optional keys of the same format, and a neutral-defaulting key changes no existing
/// interpretation.
const FIELDS: [&str; 3] = [EXPOSURE, VIBRANCE, SATURATION];

/// The fields of the Tone group. Exposure is its only implemented member, so a patch holding
/// exactly these at neutral is the group reset.
const TONE_FIELDS: [&str; 1] = [EXPOSURE];

/// The fields of the Colour group: a patch holding exactly these at neutral is that group's reset.
const COLOUR_FIELDS: [&str; 2] = [VIBRANCE, SATURATION];

/// The neutral value of every Basic field.
const NEUTRAL: f64 = 0.0;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Incompatible, detail)
}

/// The inclusive range one field accepts, which is also the range its descriptor declares, so the
/// generic parameter check and a stored payload are refused by the same numbers.
fn range(name: &str) -> (f64, f64) {
    match name {
        EXPOSURE => (EXPOSURE_MIN, EXPOSURE_MAX),
        VIBRANCE | SATURATION => (COLOUR_MIN, COLOUR_MAX),
        // Unreachable: `FIELDS` is the closed set and every caller matched a name against it.
        _ => (0.0, 0.0),
    }
}

/// The display precision and unit a field's label uses, taken from the same numbers its descriptor
/// declares.
fn display(name: &str) -> (&'static str, u8, &'static str) {
    match name {
        EXPOSURE => (EXPOSURE_LABEL, EXPOSURE_PRECISION, EXPOSURE_UNIT),
        VIBRANCE => (VIBRANCE_LABEL, COLOUR_PRECISION, ""),
        SATURATION => (SATURATION_LABEL, COLOUR_PRECISION, ""),
        _ => ("Basic", 2, ""),
    }
}

/// One field of a validated payload or request: a missing key is neutral.
fn field(source: &Map<String, Value>, name: &str) -> f64 {
    source.get(name).and_then(Value::as_f64).unwrap_or(NEUTRAL)
}

/// The canonical values a payload represents, in `FIELDS` order. Absent and explicitly neutral keys
/// produce the same array, which is what makes `{}` and `{"exposure": 0}` compare equal.
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

/// A finite f64 as a JSON number. Finiteness is checked before every call, so the fallback is never
/// reached in practice and never panics if it is.
fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// The object a stored payload must be, with the effect identity and format checked first: an
/// unsupported format is `incompatible` and is never rewritten, and every field is a finite number
/// inside its declared range.
fn read_payload(effect_id: &str, format: u32, value: &Value) -> Result<Map<String, Value>, Error> {
    if effect_id != BASIC_EFFECT {
        return Err(incompatible(format!("unavailable effect {effect_id}")));
    }
    if format != EFFECT_FORMAT {
        return Err(incompatible(format!("unsupported effect format {format}")));
    }
    let object = value
        .as_object()
        .ok_or_else(|| validation("basic payload must be a JSON object"))?;
    for (name, value) in object {
        if !FIELDS.contains(&name.as_str()) {
            return Err(validation(format!("unknown basic field {name}")));
        }
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| validation(format!("basic field {name} must be a finite number")))?;
        let (min, max) = range(name);
        if number < min || number > max {
            return Err(validation(format!(
                "basic field {name} must be a number within {min}..={max}"
            )));
        }
    }
    Ok(object.clone())
}

/// One field's history label: the control's name, the value with its sign and declared decimals,
/// and the declared unit.
fn field_label(name: &str, value: f64) -> String {
    let (label, precision, unit) = display(name);
    let precision = usize::from(precision);
    if unit.is_empty() {
        format!("{label} {value:+.precision$}")
    } else {
        format!("{label} {value:+.precision$} {unit}")
    }
}

/// The stack's one Basic layer. Two of them would each claim to be the Basic state, so every path
/// refuses to guess which one an action addresses rather than silently choosing one; nothing is
/// rewritten.
fn locate(layers: &[Layer]) -> Result<Option<&Layer>, Error> {
    let mut found = None;
    for layer in layers {
        if layer.effect_id == BASIC_EFFECT {
            if found.is_some() {
                return Err(validation(AMBIGUOUS));
            }
            found = Some(layer);
        }
    }
    Ok(found)
}

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check.
pub(crate) const AMBIGUOUS: &str = "ambiguous Basic layers";

fn exposure_parameter() -> ParameterDescriptor {
    ParameterDescriptor {
        name: EXPOSURE.into(),
        kind: ParameterKind::Number {
            min: EXPOSURE_MIN,
            max: EXPOSURE_MAX,
        },
        required: false,
        default: Some(number(NEUTRAL)),
        unit: Some(EXPOSURE_UNIT.into()),
        step: Some(EXPOSURE_STEP),
        precision: Some(EXPOSURE_PRECISION),
        notes: "multiplies the linear-light channels by 2^EV. The input is a rendered sRGB JPEG decoded through the sRGB transfer function, not scene-linear RAW data, so this is an exposure correction of a rendered image and cannot recover detail a clipped plateau no longer holds".into(),
    }
}

fn vibrance_parameter() -> ParameterDescriptor {
    ParameterDescriptor {
        name: VIBRANCE.into(),
        kind: ParameterKind::Number {
            min: COLOUR_MIN,
            max: COLOUR_MAX,
        },
        required: false,
        default: Some(number(NEUTRAL)),
        unit: None,
        step: Some(COLOUR_STEP),
        precision: Some(COLOUR_PRECISION),
        notes: "raises chroma more for near-neutral colour than for colour already close to the sRGB gamut edge, with reduced gain in a skin-like hue band; that hue weighting is a colour heuristic, not skin detection, and is not a promise about every skin tone".into(),
    }
}

fn saturation_parameter() -> ParameterDescriptor {
    ParameterDescriptor {
        name: SATURATION.into(),
        kind: ParameterKind::Number {
            min: COLOUR_MIN,
            max: COLOUR_MAX,
        },
        required: false,
        default: Some(number(NEUTRAL)),
        unit: None,
        step: Some(COLOUR_STEP),
        precision: Some(COLOUR_PRECISION),
        notes: "scales chroma uniformly about the achromatic axis; -100 is neutral grayscale, not merely a strong desaturation".into(),
    }
}

#[derive(Debug)]
pub struct BasicModule {
    descriptor: ModuleDescriptor,
}

impl Default for BasicModule {
    fn default() -> Self {
        Self::new()
    }
}

impl BasicModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.basic".into(),
                title: "Basic".into(),
                hint: Some("Exposure, tone, white balance and colour".into()),
                effects: vec![EffectDescriptor {
                    id: BASIC_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Color,
                }],
                actions: vec![
                    ActionDescriptor {
                        id: SET_BASIC.into(),
                        title: "Set Basic".into(),
                        notes: "merges the named Basic fields into the stack's one Basic layer, which the host places before the geometry tail on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op".into(),
                        summary: None,
                        patch: true,
                        parameters: vec![
                            exposure_parameter(),
                            vibrance_parameter(),
                            saturation_parameter(),
                        ],
                    },
                    ActionDescriptor {
                        id: RESET_BASIC.into(),
                        title: "Reset Basic".into(),
                        notes: "returns the stack's one Basic layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral".into(),
                        summary: None,
                        patch: false,
                        parameters: Vec::new(),
                    },
                ],
                controls: vec![
                    Control::Group {
                        label: TONE_GROUP.into(),
                        reset: Some(ResetAction {
                            action: SET_BASIC.into(),
                            preset: [(EXPOSURE.to_owned(), number(NEUTRAL))]
                                .into_iter()
                                .collect(),
                        }),
                        controls: vec![Control::Number {
                            action: SET_BASIC.into(),
                            parameter: EXPOSURE.into(),
                            label: EXPOSURE_LABEL.into(),
                        }],
                    },
                    Control::Group {
                        label: COLOUR_GROUP.into(),
                        reset: Some(ResetAction {
                            action: SET_BASIC.into(),
                            preset: [
                                (VIBRANCE.to_owned(), number(NEUTRAL)),
                                (SATURATION.to_owned(), number(NEUTRAL)),
                            ]
                            .into_iter()
                            .collect(),
                        }),
                        controls: vec![
                            Control::Number {
                                action: SET_BASIC.into(),
                                parameter: VIBRANCE.into(),
                                label: VIBRANCE_LABEL.into(),
                            },
                            Control::Number {
                                action: SET_BASIC.into(),
                                parameter: SATURATION.into(),
                                label: SATURATION_LABEL.into(),
                            },
                        ],
                    },
                ],
                reset: Some(ResetAction {
                    action: RESET_BASIC.into(),
                    preset: Map::new(),
                }),
                canvas: None,
                developer: false,
                availability: Availability::Available,
            },
        }
    }
}

impl ToolModule for BasicModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// At most one Basic layer exists in a stack, so the host refuses to compile or plan against a
    /// stack that holds two instead of guessing which one the parameters belong to.
    fn single_layer(&self, effect_id: &str) -> bool {
        effect_id == BASIC_EFFECT
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            // A patch stores exactly the fields the caller sent, which the generic check has
            // already validated against the declared range: the history entry, the label and
            // request deduplication all describe the patch, not the merged payload.
            SET_BASIC => parameters.clone(),
            RESET_BASIC => Map::new(),
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
            // The sent fields over the stored payload, or over neutral when no layer exists.
            SET_BASIC => {
                let mut merged = current;
                for (slot, name) in merged.iter_mut().zip(FIELDS) {
                    if let Some(value) = input.parameters.get(name) {
                        *slot = value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| {
                                validation(format!("basic field {name} must be a finite number"))
                            })?;
                        let (min, max) = range(name);
                        if *slot < min || *slot > max {
                            return Err(validation(format!(
                                "basic field {name} must be a number within {min}..={max}"
                            )));
                        }
                    }
                }
                merged
            }
            RESET_BASIC => [NEUTRAL; FIELDS.len()],
            action_id => return Err(validation(format!("unknown action {action_id}"))),
        };
        match existing {
            // Canonical comparison, so returning a field to 0 on a layer that stores no key at all
            // is the no-op it looks like.
            Some(_) if current == merged => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(Layer {
                id: layer.id.clone(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
            })),
            // The host inserts a colour-stage layer before the geometry tail; a neutral first set
            // has nothing to store, so it adds no layer at all.
            None if is_neutral(&merged) => Ok(ActionPlan::NoOp),
            None => Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
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
            .map(|(name, value)| field_label(name, value))
            .collect::<Vec<_>>()
            .join(", "))
    }

    /// The history label for a request the action's title cannot describe: the one field a slider
    /// moved, or the group a reset cleared.
    fn label(&self, input: &ActionInput) -> Option<String> {
        match input.action_id.as_str() {
            RESET_BASIC => Some("Reset Basic".into()),
            SET_BASIC => {
                let sent: Vec<(&String, f64)> = input
                    .parameters
                    .iter()
                    .map(|(name, value)| (name, value.as_f64().unwrap_or(NEUTRAL)))
                    .collect();
                // A patch holding exactly one group's fields, all at neutral, is that group's reset
                // however it was sent: from the header button, a keyboard reset or an API call.
                let tone_reset = sent.len() == TONE_FIELDS.len()
                    && sent.iter().all(|(name, value)| {
                        TONE_FIELDS.contains(&name.as_str()) && *value == NEUTRAL
                    });
                let colour_reset = sent.len() == COLOUR_FIELDS.len()
                    && sent.iter().all(|(name, value)| {
                        COLOUR_FIELDS.contains(&name.as_str()) && *value == NEUTRAL
                    });
                match (tone_reset, colour_reset, sent.as_slice()) {
                    (true, _, _) => Some(format!("Reset {TONE_GROUP}")),
                    (_, true, _) => Some(format!("Reset {COLOUR_GROUP}")),
                    // An empty patch changes nothing and commits no entry; the host falls back to
                    // the action's own title if it ever asks.
                    (false, false, []) => None,
                    (false, false, [(name, value)]) => Some(field_label(name, *value)),
                    (false, false, sent) => Some(format!("Basic ({} fields)", sent.len())),
                }
            }
            _ => None,
        }
    }

    /// The values this stored layer represents, named exactly as `set-basic`'s parameters are, so a
    /// client seeds its sliders from the displayed entry. A neutral layer reports the neutral value
    /// of every implemented field rather than an empty object.
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
        _: Stage,
    ) -> Result<Processing, Error> {
        let values = canonical(&read_payload(effect_id, format, value)?);
        // A neutral payload compiles to no units, which the host drops entirely: the identity byte
        // path and the shared source buffer are kept.
        if is_neutral(&values) {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
        let [exposure, vibrance, saturation] = values;
        if exposure != NEUTRAL {
            units.push(Arc::new(Exposure::new(exposure)));
        }
        // Colour units run last, in the frozen internal order: vibrance, then saturation.
        if vibrance != NEUTRAL {
            units.push(Arc::new(Vibrance::new(vibrance)));
        }
        if saturation != NEUTRAL {
            units.push(Arc::new(Saturation::new(saturation)));
        }
        Ok(Processing::Color(ColorOperation::new(units)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, modules::check_parameters};
    use serde_json::json;

    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn basic_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
        }
    }

    /// Plan one request the way the host does: generic parameter check, module parse, then plan
    /// against a stack whose stage questions are answered from constants.
    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = BasicModule::new();
        let declared = module
            .descriptor()
            .action(action)
            .expect("a declared action");
        let checked = check_parameters(declared, &parameters)?;
        let input = module.parse(action, &checked)?;
        let sampler = |_: u32, _: u32| Ok(Some([0, 0, 0, 255]));
        let stage_before = |_: usize| Ok(STAGE);
        let insertion_index = |_: EffectStage| 0usize;
        let sample_before = |_: usize, _: u32, _: u32| Ok(Some([0, 0, 0, 255]));
        module.plan(
            &input,
            &StageContext {
                stage: STAGE,
                layers,
                sampler: &sampler,
                stage_before: &stage_before,
                insertion_index: &insertion_index,
                sample_before: &sample_before,
            },
        )
    }

    fn committed(plan: ActionPlan) -> Layer {
        match plan {
            ActionPlan::Commit(layer) | ActionPlan::Update(layer) => layer,
            ActionPlan::NoOp => panic!("expected a layer, not a no-op"),
        }
    }

    #[test]
    fn the_descriptor_declares_one_colour_effect_two_actions_and_the_tone_group() {
        let module = BasicModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "lightwell.basic");
        assert_eq!(descriptor.title, "Basic");
        assert_eq!(
            descriptor.hint.as_deref(),
            Some("Exposure, tone, white balance and colour")
        );
        assert!(!descriptor.developer);
        assert_eq!(descriptor.effects.len(), 1);
        assert_eq!(descriptor.effects[0].id, BASIC_EFFECT);
        assert_eq!(descriptor.effects[0].format, 1);
        assert_eq!(descriptor.effects[0].stage, EffectStage::Color);
        assert!(descriptor.canvas.is_none(), "Basic drives no canvas mode");
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: RESET_BASIC.into(),
                preset: Map::new(),
            })
        );

        let set = descriptor.action(SET_BASIC).expect("set-basic");
        assert!(set.patch, "every slider sends one field");
        assert!(set.summary.is_none());
        assert_eq!(
            set.parameters.len(),
            3,
            "exposure, vibrance and saturation are implemented"
        );
        let exposure = set.parameter(EXPOSURE).expect("the exposure parameter");
        assert_eq!(
            exposure.kind,
            ParameterKind::Number {
                min: -5.0,
                max: 5.0
            }
        );
        assert!(!exposure.required);
        assert_eq!(exposure.default, Some(json!(0.0)));
        assert_eq!(exposure.unit.as_deref(), Some("EV"));
        assert_eq!(exposure.step, Some(0.01));
        assert_eq!(exposure.precision, Some(2));
        assert!(
            exposure.notes.contains("2^EV") && exposure.notes.contains("not scene-linear RAW"),
            "{}",
            exposure.notes
        );

        for (name, label, note_needle) in [
            (VIBRANCE, "Vibrance", "colour heuristic"),
            (SATURATION, "Saturation", "neutral grayscale"),
        ] {
            let parameter = set.parameter(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                parameter.kind,
                ParameterKind::Number {
                    min: -100.0,
                    max: 100.0
                },
                "{name}"
            );
            assert!(!parameter.required, "{name}");
            assert_eq!(parameter.default, Some(json!(0.0)), "{name}");
            assert_eq!(parameter.unit, None, "{name}: no unit is declared");
            assert_eq!(parameter.step, Some(1.0), "{name}");
            assert_eq!(parameter.precision, Some(0), "{name}");
            assert!(
                parameter.notes.contains(note_needle),
                "{name}: {}",
                parameter.notes
            );
            let _ = label;
        }

        let reset = descriptor.action(RESET_BASIC).expect("reset-basic");
        assert!(reset.parameters.is_empty());
        assert!(!reset.patch);

        assert_eq!(
            descriptor.controls,
            vec![
                Control::Group {
                    label: "Tone".into(),
                    reset: Some(ResetAction {
                        action: SET_BASIC.into(),
                        preset: [("exposure".to_owned(), json!(0.0))].into_iter().collect(),
                    }),
                    controls: vec![Control::Number {
                        action: SET_BASIC.into(),
                        parameter: "exposure".into(),
                        label: "Exposure".into(),
                    }],
                },
                Control::Group {
                    label: "Colour".into(),
                    reset: Some(ResetAction {
                        action: SET_BASIC.into(),
                        preset: [
                            ("vibrance".to_owned(), json!(0.0)),
                            ("saturation".to_owned(), json!(0.0)),
                        ]
                        .into_iter()
                        .collect(),
                    }),
                    controls: vec![
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "vibrance".into(),
                            label: "Vibrance".into(),
                        },
                        Control::Number {
                            action: SET_BASIC.into(),
                            parameter: "saturation".into(),
                            label: "Saturation".into(),
                        },
                    ],
                },
            ],
            "the Tone group, then the Colour group, each with its slider(s) and group reset"
        );
    }

    #[test]
    fn a_payload_is_refused_by_shape_format_field_and_range_without_being_rewritten() {
        let module = BasicModule::new();
        let refused = |format: u32, payload: Value| {
            module
                .validate_payload(BASIC_EFFECT, format, &payload)
                .expect_err("an invalid payload")
        };
        for payload in [
            json!({}),
            json!({"exposure": 0}),
            json!({"exposure": -5.0}),
            json!({"exposure": 5.0}),
            json!({"vibrance": -100.0}),
            json!({"vibrance": 100.0}),
            json!({"saturation": -100.0}),
            json!({"saturation": 100.0}),
            json!({"vibrance": 50.0, "saturation": -20.0}),
        ] {
            module
                .validate_payload(BASIC_EFFECT, 1, &payload)
                .unwrap_or_else(|error| panic!("{payload} should be valid: {error}"));
        }

        let format = refused(2, json!({"exposure": 1.0}));
        assert_eq!(format.kind, ErrorKind::Incompatible);
        assert!(format.detail.contains("unsupported effect format 2"));

        let effect = module
            .validate_payload("lightwell.other", 1, &json!({}))
            .expect_err("another effect");
        assert_eq!(effect.kind, ErrorKind::Incompatible);

        for (case, payload, needle) in [
            ("a list", json!([1.0]), "must be a JSON object"),
            ("a number", json!(1.0), "must be a JSON object"),
            (
                "an unknown key",
                json!({"contrast": 10.0}),
                "unknown basic field contrast",
            ),
            (
                "a string value",
                json!({"exposure": "1.0"}),
                "must be a finite number",
            ),
            (
                "below the range",
                json!({"exposure": -5.001}),
                "within -5..=5",
            ),
            (
                "above the range",
                json!({"exposure": 5.001}),
                "within -5..=5",
            ),
        ] {
            let error = refused(1, payload);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(needle), "{case}: {}", error.detail);
        }
    }

    #[test]
    fn the_canonical_neutral_payload_is_the_empty_object_and_compares_equal_to_an_explicit_zero() {
        // First non-neutral set commits a layer holding only the field it set.
        let committed_layer = committed(planned(SET_BASIC, json!({"exposure": 0.5}), &[]).unwrap());
        assert_eq!(committed_layer.payload, json!({"exposure": 0.5}));
        assert_eq!(committed_layer.effect_id, BASIC_EFFECT);
        assert_eq!(committed_layer.effect_format, 1);

        // A set back to zero stores the canonical neutral form, keeping the layer.
        let neutralized = committed(
            planned(
                SET_BASIC,
                json!({"exposure": 0.0}),
                &[basic_layer(json!({"exposure": 0.5}))],
            )
            .unwrap(),
        );
        assert_eq!(neutralized.payload, json!({}));

        // Both spellings of neutral are the same state, in both directions.
        for stored in [json!({}), json!({"exposure": 0.0})] {
            assert_eq!(
                planned(
                    SET_BASIC,
                    json!({"exposure": 0.0}),
                    &[basic_layer(stored.clone())]
                )
                .unwrap(),
                ActionPlan::NoOp,
                "setting neutral on a {stored} layer changes nothing"
            );
            assert_eq!(
                planned(RESET_BASIC, json!({}), &[basic_layer(stored.clone())]).unwrap(),
                ActionPlan::NoOp,
                "resetting a {stored} layer changes nothing"
            );
            assert_eq!(
                BasicModule::new()
                    .describe_layer(BASIC_EFFECT, 1, &stored)
                    .unwrap(),
                "Neutral"
            );
        }
    }

    #[test]
    fn a_set_commits_updates_or_reports_a_no_op_against_the_stack_it_finds() {
        // No layer and a neutral result adds nothing.
        assert_eq!(
            planned(SET_BASIC, json!({"exposure": 0.0}), &[]).unwrap(),
            ActionPlan::NoOp
        );
        assert_eq!(
            planned(SET_BASIC, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "an empty patch sets nothing"
        );

        // An existing layer is updated in place, keeping its identity.
        let existing = basic_layer(json!({"exposure": -1.0}));
        let plan = planned(
            SET_BASIC,
            json!({"exposure": 2.25}),
            std::slice::from_ref(&existing),
        )
        .unwrap();
        match plan {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, existing.id, "the layer keeps its identity");
                assert_eq!(layer.payload, json!({"exposure": 2.25}));
            }
            other => panic!("expected an update, got {other:?}"),
        }

        // The same value again is a no-op, whichever way the number was written.
        for same in [json!({"exposure": -1.0}), json!({"exposure": -1})] {
            assert_eq!(
                planned(SET_BASIC, same, std::slice::from_ref(&existing)).unwrap(),
                ActionPlan::NoOp
            );
        }

        // A reset keeps the layer's identity and stores the neutral payload.
        match planned(RESET_BASIC, json!({}), std::slice::from_ref(&existing)).unwrap() {
            ActionPlan::Update(layer) => {
                assert_eq!(layer.id, existing.id);
                assert_eq!(layer.payload, json!({}));
            }
            other => panic!("expected an update, got {other:?}"),
        }
        assert_eq!(
            planned(RESET_BASIC, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "a reset without a layer is a no-op"
        );
    }

    #[test]
    fn a_patch_merges_over_the_stored_payload_and_leaves_omitted_fields_alone() {
        // The generic check fills no default for a patch action, so an omitted field never arrives
        // as a neutral value that would silently clear it.
        let module = BasicModule::new();
        let declared = module.descriptor().action(SET_BASIC).expect("set-basic");
        assert_eq!(check_parameters(declared, &json!({})).unwrap(), Map::new());
        assert_eq!(
            check_parameters(declared, &json!({"exposure": 1.5}))
                .unwrap()
                .get(EXPOSURE),
            Some(&json!(1.5))
        );
        assert!(
            check_parameters(declared, &json!({"highlights": 1.0})).is_err(),
            "an unknown field is still rejected"
        );
        assert!(
            check_parameters(declared, &json!({"exposure": 6.0})).is_err(),
            "the declared range still applies"
        );

        // An empty patch over a stored value preserves it.
        let stored = basic_layer(json!({"exposure": 1.5}));
        assert_eq!(
            planned(SET_BASIC, json!({}), std::slice::from_ref(&stored)).unwrap(),
            ActionPlan::NoOp
        );
        // The request stores the patch as sent, not the merged payload.
        let input = module
            .parse(
                SET_BASIC,
                &check_parameters(declared, &json!({"exposure": 1.5})).unwrap(),
            )
            .unwrap();
        assert_eq!(input.action_id, SET_BASIC);
        assert_eq!(
            input.parameters,
            json!({"exposure": 1.5}).as_object().cloned().unwrap()
        );
        assert_eq!(
            module.parse(RESET_BASIC, &Map::new()).unwrap().parameters,
            Map::new()
        );
        assert!(module.parse("set-crop", &Map::new()).is_err());
    }

    #[test]
    fn two_basic_layers_are_ambiguous_rather_than_silently_resolved() {
        let stack = [
            basic_layer(json!({"exposure": 1.0})),
            basic_layer(json!({"exposure": -1.0})),
        ];
        for action in [SET_BASIC, RESET_BASIC] {
            let error = planned(action, json!({}), &stack).expect_err("an ambiguous stack");
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(error.detail, "ambiguous Basic layers");
        }
        assert!(
            BasicModule::new().single_layer(BASIC_EFFECT),
            "the host refuses to compile the same stack"
        );
        assert!(!BasicModule::new().single_layer(PIXEL_EFFECT));
    }

    #[test]
    fn labels_name_the_moved_field_the_reset_group_and_the_module_reset() {
        let module = BasicModule::new();
        let label = |action: &str, parameters: Value| {
            module.label(&ActionInput {
                action_id: action.to_owned(),
                parameters: parameters.as_object().cloned().unwrap(),
            })
        };
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 0.5})).as_deref(),
            Some("Exposure +0.50 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"exposure": -1.0})).as_deref(),
            Some("Exposure -1.00 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 5.0})).as_deref(),
            Some("Exposure +5.00 EV")
        );
        assert_eq!(
            label(SET_BASIC, json!({"exposure": 0.0})).as_deref(),
            Some("Reset Tone"),
            "the Tone group's only implemented field at neutral is that group's reset"
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 30.0})).as_deref(),
            Some("Vibrance +30")
        );
        assert_eq!(
            label(SET_BASIC, json!({"saturation": -100.0})).as_deref(),
            Some("Saturation -100")
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 0.0, "saturation": 0.0})).as_deref(),
            Some("Reset Colour"),
            "the Colour group's fields at neutral, together, is that group's reset"
        );
        assert_eq!(
            label(SET_BASIC, json!({"vibrance": 50.0, "saturation": 20.0})).as_deref(),
            Some("Basic (2 fields)"),
            "a mixed non-reset patch names how many fields it touched"
        );
        assert_eq!(
            label(RESET_BASIC, json!({})).as_deref(),
            Some("Reset Basic")
        );
        assert_eq!(label(SET_BASIC, json!({})), None);
    }

    #[test]
    fn a_layer_describes_and_reports_its_values() {
        let module = BasicModule::new();
        assert_eq!(
            module
                .describe_layer(BASIC_EFFECT, 1, &json!({"exposure": 0.5}))
                .unwrap(),
            "Exposure +0.50 EV"
        );
        assert_eq!(
            module
                .values(BASIC_EFFECT, 1, &json!({"exposure": 0.5}))
                .unwrap(),
            json!({"exposure": 0.5, "vibrance": 0.0, "saturation": 0.0})
                .as_object()
                .cloned()
                .unwrap()
        );
        assert_eq!(
            module.values(BASIC_EFFECT, 1, &json!({})).unwrap(),
            json!({"exposure": 0.0, "vibrance": 0.0, "saturation": 0.0})
                .as_object()
                .cloned()
                .unwrap(),
            "a neutral layer reports the neutral value of every implemented field"
        );
        assert_eq!(
            module
                .values(
                    BASIC_EFFECT,
                    1,
                    &json!({"vibrance": 50.0, "saturation": -20.0})
                )
                .unwrap(),
            json!({"exposure": 0.0, "vibrance": 50.0, "saturation": -20.0})
                .as_object()
                .cloned()
                .unwrap()
        );
        assert!(module.values(BASIC_EFFECT, 2, &json!({})).is_err());
        assert!(module.describe_layer(BASIC_EFFECT, 2, &json!({})).is_err());
    }

    #[test]
    fn compilation_produces_one_exposure_unit_or_nothing_at_all() {
        let module = BasicModule::new();
        let compiled = |payload: Value| module.compile(BASIC_EFFECT, 1, &payload, STAGE).unwrap();
        match compiled(json!({})) {
            Processing::Color(operation) => {
                assert!(
                    operation.is_empty(),
                    "a neutral payload compiles to nothing"
                );
                assert_eq!(operation, ColorOperation::neutral());
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        match compiled(json!({"exposure": 0.5})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "exposure(+0.50)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        match compiled(json!({"vibrance": 30.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "vibrance(+30)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        match compiled(json!({"saturation": -100.0})) {
            Processing::Color(operation) => {
                assert_eq!(operation.len(), 1);
                assert!(operation.is_finite());
                assert_eq!(operation.units()[0].describe(), "saturation(-100)");
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        // The frozen internal order: exposure, then vibrance, then saturation, whichever fields
        // the payload set.
        match compiled(json!({"exposure": 1.0, "vibrance": 20.0, "saturation": 10.0})) {
            Processing::Color(operation) => {
                assert_eq!(
                    operation
                        .units()
                        .iter()
                        .map(|unit| unit.describe())
                        .collect::<Vec<_>>(),
                    vec!["exposure(+1.00)", "vibrance(+20)", "saturation(+10)"]
                );
            }
            other => panic!("expected a colour operation, got {other:?}"),
        }
        assert!(
            module.compile(BASIC_EFFECT, 2, &json!({}), STAGE).is_err(),
            "an unsupported format never compiles"
        );
        assert!(
            module
                .compile(BASIC_EFFECT, 1, &json!({"exposure": 99.0}), STAGE)
                .is_err(),
            "a stored value outside the declared range never compiles"
        );
        assert!(
            module
                .compile(BASIC_EFFECT, 1, &json!({"vibrance": 101.0}), STAGE)
                .is_err(),
            "a vibrance value outside the declared range never compiles"
        );
    }

    /// The module finds its own layer wherever the host placed it, including a stack that already
    /// holds a pixel replacement and a geometry tail.
    #[test]
    fn the_basic_layer_is_found_among_pixel_and_geometry_layers() {
        let pixel = Layer::pixel(1, 1, [1, 2, 3]);
        let orientation = Layer::orientation(Orientation::NEUTRAL);
        let basic = basic_layer(json!({"exposure": 1.0}));
        let stack = [pixel.clone(), basic.clone(), orientation.clone()];
        assert_eq!(
            locate(&stack).unwrap().map(|layer| &layer.id),
            Some(&basic.id)
        );
        assert_eq!(
            locate(&[pixel, orientation])
                .unwrap()
                .map(|layer| &layer.id),
            None
        );
        assert!(PIXEL_EFFECT != BASIC_EFFECT && ORIENTATION_EFFECT != BASIC_EFFECT);
    }
}
