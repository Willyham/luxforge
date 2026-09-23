//! The colour mixer module: one colour-stage layer holding hue, saturation and luminance for each
//! of eight colour ranges, edited by one field-patch action.
//!
//! This mirrors the Basic module's shape (`docs/design/modules-and-api.md`'s field-patch
//! contract): a payload is a JSON object whose keys are the implemented parameter names, a missing
//! key is neutral, and the canonical neutral payload is the empty object `{}`. The module owns
//! exactly one layer of `lightwell.mixer.hsl`, declared order 10 so a mixer layer always follows
//! the Basic layer in the colour run (`docs/design/presence-mixer-vignette.md`, "Placement and
//! stage order"). The one pointwise unit's equations are frozen in `docs/design/mixer-study.md`
//! and implemented in [`unit::Mixer`]; this file owns only the parameters, validation, controls and
//! API surface around it.
mod unit;

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, EffectDescriptor,
    EffectStage, ModuleDescriptor, ParameterDescriptor, ParameterKind, PointwiseColor, Processing,
    RailDecoration, ResetAction, Stage, StageContext, ToolModule,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId, MIXER_EFFECT};
use serde_json::{Map, Number, Value};
use std::sync::Arc;

pub(super) const SET_MIXER: &str = "set-mixer";
pub(super) const RESET_MIXER: &str = "reset-mixer";

const HUE: &str = "hue";
const SATURATION: &str = "saturation";
const LUMINANCE: &str = "luminance";

/// Every implemented mixer field: the hue group, then saturation, then luminance, each in range
/// order red, orange, yellow, green, aqua, blue, purple, magenta. This is the payload's declared
/// order, the controls' rendering order and the order [`unit::Mixer::new`] expects its three
/// eight-element arrays in.
///
/// Names are `<range>-<property>`, hyphen-separated: [`crate::modules::valid_name`] is the shared
/// identity rule every action and parameter name is checked against (lowercase words joined by
/// `-`), so `red-hue` is what actually registers.
const FIELDS: [&str; 24] = [
    "red-hue",
    "orange-hue",
    "yellow-hue",
    "green-hue",
    "aqua-hue",
    "blue-hue",
    "purple-hue",
    "magenta-hue",
    "red-saturation",
    "orange-saturation",
    "yellow-saturation",
    "green-saturation",
    "aqua-saturation",
    "blue-saturation",
    "purple-saturation",
    "magenta-saturation",
    "red-luminance",
    "orange-luminance",
    "yellow-luminance",
    "green-luminance",
    "aqua-luminance",
    "blue-luminance",
    "purple-luminance",
    "magenta-luminance",
];

/// The group label a patch that returns every one of a property's eight fields to neutral takes in
/// history, and the label the group's own control carries.
const HUE_GROUP: &str = "Hue";
const SATURATION_GROUP: &str = "Saturation";
const LUMINANCE_GROUP: &str = "Luminance";

/// The neutral value of every mixer field.
const NEUTRAL: f64 = 0.0;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Incompatible, detail)
}

/// A finite f64 as a JSON number. Finiteness is checked before every call, so the fallback is
/// never reached in practice and never panics if it is.
fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// A declared field's range index and property, parsed from its `<range>-<property>` name.
fn parse_field(name: &str) -> Option<(usize, &'static str)> {
    let (range_name, property) = name.split_once('-')?;
    let range = unit::RANGE_NAMES
        .iter()
        .position(|candidate| *candidate == range_name)?;
    let property = match property {
        "hue" => HUE,
        "saturation" => SATURATION,
        "luminance" => LUMINANCE,
        _ => return None,
    };
    Some((range, property))
}

/// The capitalized range name a control label and a history label both use, e.g. `Red`.
fn range_label(range: usize) -> String {
    let name = unit::RANGE_NAMES[range];
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// One field's history label: the range name, the lowercase property and the value with its sign
/// always shown, e.g. `Red hue +20`, `Aqua luminance -15`.
fn field_label(name: &str, value: f64) -> String {
    let (range, property) = parse_field(name).expect("a declared mixer field");
    format!("{} {property} {value:+.0}", range_label(range))
}

/// One field of a validated payload or request: a missing key is neutral.
fn field(source: &Map<String, Value>, name: &str) -> f64 {
    source.get(name).and_then(Value::as_f64).unwrap_or(NEUTRAL)
}

/// The canonical values a payload represents, in [`FIELDS`] order. Absent and explicitly neutral
/// keys produce the same array, which is what makes `{}` and `{"red-hue": 0}` compare equal.
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
/// unsupported format is `incompatible` and is never rewritten, and every field is a finite
/// number inside its declared range.
fn read_payload(effect_id: &str, format: u32, value: &Value) -> Result<Map<String, Value>, Error> {
    if effect_id != MIXER_EFFECT {
        return Err(incompatible(format!("unavailable effect {effect_id}")));
    }
    if format != EFFECT_FORMAT {
        return Err(incompatible(format!("unsupported effect format {format}")));
    }
    let object = value
        .as_object()
        .ok_or_else(|| validation("mixer payload must be a JSON object"))?;
    for (name, value) in object {
        if !FIELDS.contains(&name.as_str()) {
            return Err(validation(format!("unknown mixer field {name}")));
        }
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| validation(format!("mixer field {name} must be a finite number")))?;
        if !(-100.0..=100.0).contains(&number) {
            return Err(validation(format!(
                "mixer field {name} must be a number within -100..=100"
            )));
        }
    }
    Ok(object.clone())
}

/// The group a patch returns entirely to neutral, when it is one: a patch holding exactly one
/// property's eight fields, all at neutral, is that group's reset however it was sent.
fn reset_group(sent: &[(&String, f64)]) -> Option<&'static str> {
    [
        (HUE_GROUP, &FIELDS[0..8]),
        (SATURATION_GROUP, &FIELDS[8..16]),
        (LUMINANCE_GROUP, &FIELDS[16..24]),
    ]
    .into_iter()
    .find(|(_, fields)| {
        sent.len() == fields.len()
            && sent
                .iter()
                .all(|(name, value)| fields.contains(&name.as_str()) && *value == NEUTRAL)
    })
    .map(|(label, _)| label)
}

/// The stack's one Colour mixer layer. Two of them would each claim to be the mixer state, so
/// every path refuses to guess which one an action addresses rather than silently choosing one;
/// nothing is rewritten.
fn locate(layers: &[Layer]) -> Result<Option<&Layer>, Error> {
    Ok(locate_index(layers)?.map(|index| &layers[index]))
}

fn locate_index(layers: &[Layer]) -> Result<Option<usize>, Error> {
    let mut found = None;
    for (index, layer) in layers.iter().enumerate() {
        if layer.effect_id == MIXER_EFFECT {
            if found.is_some() {
                return Err(validation(AMBIGUOUS));
            }
            found = Some(index);
        }
    }
    Ok(found)
}

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check.
pub(crate) const AMBIGUOUS: &str = "ambiguous Colour mixer layers";

/// One `<range>_<property>` parameter descriptor: -100..100, step 1, no display decimals, no unit.
fn mixer_parameter(field: &str) -> ParameterDescriptor {
    let (range, property) = parse_field(field).expect("a declared mixer field");
    let label = range_label(range);
    let notes = match property {
        HUE => format!(
            "rotates hue within the {label} range toward the neighbouring range by a bounded angle; the sign chooses the direction of travel"
        ),
        SATURATION => format!(
            "scales chroma within the {label} range; -100 is exactly neutral grey for that range's own colour and +100 doubles chroma"
        ),
        LUMINANCE => format!(
            "scales Oklab L within the {label} range through a compressive response with the near-black rule"
        ),
        _ => unreachable!("parse_field returns only hue, saturation or luminance"),
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

fn group_reset(fields: &[&str]) -> ResetAction {
    ResetAction {
        action: SET_MIXER.into(),
        preset: fields
            .iter()
            .map(|name| ((*name).to_owned(), number(NEUTRAL)))
            .collect(),
    }
}

/// The gradient rail for a colour's reference sRGB code, at one quarter (dark) and 60% of the way
/// to white (light).
fn dark(colour: [u8; 3]) -> [u8; 3] {
    colour.map(|channel| channel / 4)
}

fn light(colour: [u8; 3]) -> [u8; 3] {
    colour.map(|channel| {
        let channel = f32::from(channel);
        (channel + (255.0 - channel) * 0.6)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

const MID_GREY: [u8; 3] = [128, 128, 128];

/// The previous, this and next range's reference colours: `+100` rotates toward the next centre
/// and `-100` toward the previous one, so the rail shows both destinations either side of home.
fn hue_rail(range: usize) -> RailDecoration {
    let previous = unit::RANGE_REFERENCE_CODES[(range + unit::RANGE_COUNT - 1) % unit::RANGE_COUNT];
    let this = unit::RANGE_REFERENCE_CODES[range];
    let next = unit::RANGE_REFERENCE_CODES[(range + 1) % unit::RANGE_COUNT];
    RailDecoration::Gradient {
        stops: vec![previous, this, next],
    }
}

/// Mid grey at `-100` (zero chroma) to the range's own colour at `+100` (double chroma).
fn saturation_rail(range: usize) -> RailDecoration {
    RailDecoration::Gradient {
        stops: vec![MID_GREY, unit::RANGE_REFERENCE_CODES[range]],
    }
}

/// A dark version of the range's colour at `-100`, the colour itself at `0`, a light version at
/// `+100`.
fn luminance_rail(range: usize) -> RailDecoration {
    let colour = unit::RANGE_REFERENCE_CODES[range];
    RailDecoration::Gradient {
        stops: vec![dark(colour), colour, light(colour)],
    }
}

/// The eight sliders of one property group, in range order, each labelled by its range name and
/// railed by the given decoration.
fn range_controls(fields: &[&str], rail: fn(usize) -> RailDecoration) -> Vec<Control> {
    fields
        .iter()
        .enumerate()
        .map(|(range, field)| Control::Number {
            action: SET_MIXER.into(),
            parameter: (*field).into(),
            label: range_label(range),
            style: crate::NumberStyle::Slider,
            rail: Some(rail(range)),
        })
        .collect()
}

#[derive(Debug)]
pub struct MixerModule {
    descriptor: ModuleDescriptor,
}

impl Default for MixerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl MixerModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.mixer".into(),
                title: "Colour mixer".into(),
                hint: Some("Hue, saturation and luminance by range".into()),
                effects: vec![EffectDescriptor {
                    id: MIXER_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Color,
                    order: 10,
                }],
                actions: vec![
                    ActionDescriptor {
                        id: SET_MIXER.into(),
                        title: "Set Colour mixer".into(),
                        notes: "merges the named mixer fields into the stack's one Colour mixer layer, which the host places after Basic by declared order on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op".into(),
                        summary: None,
                        patch: true,
                        parameters: FIELDS.iter().map(|field| mixer_parameter(field)).collect(),
                    },
                    ActionDescriptor {
                        id: RESET_MIXER.into(),
                        title: "Reset Colour mixer".into(),
                        notes: "returns the stack's one Colour mixer layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral".into(),
                        summary: None,
                        patch: false,
                        parameters: Vec::new(),
                    },
                ],
                queries: Vec::new(),
                controls: vec![
                    Control::Group {
                        label: HUE_GROUP.into(),
                        reset: Some(group_reset(&FIELDS[0..8])),
                        controls: range_controls(&FIELDS[0..8], hue_rail),
                        collapsed: false,
                    },
                    Control::Group {
                        label: SATURATION_GROUP.into(),
                        reset: Some(group_reset(&FIELDS[8..16])),
                        controls: range_controls(&FIELDS[8..16], saturation_rail),
                        collapsed: true,
                    },
                    Control::Group {
                        label: LUMINANCE_GROUP.into(),
                        reset: Some(group_reset(&FIELDS[16..24])),
                        controls: range_controls(&FIELDS[16..24], luminance_rail),
                        collapsed: true,
                    },
                ],
                reset: Some(ResetAction {
                    action: RESET_MIXER.into(),
                    preset: Map::new(),
                }),
                canvas: None,
                developer: false,
                // A Presence module will later be registered between Basic and the mixer.
                collapsed: true,
                availability: Availability::Available,
            },
        }
    }
}

impl ToolModule for MixerModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// At most one Colour mixer layer exists in a stack, so the host refuses to compile or plan
    /// against a stack that holds two instead of guessing which one the parameters belong to.
    fn single_layer(&self, effect_id: &str) -> bool {
        effect_id == MIXER_EFFECT
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            // A patch stores exactly the fields the caller sent: the history entry, the label and
            // request deduplication all describe the patch, not the merged payload.
            SET_MIXER => parameters.clone(),
            RESET_MIXER => Map::new(),
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
            SET_MIXER => {
                let mut merged = current;
                for (slot, name) in merged.iter_mut().zip(FIELDS) {
                    if let Some(value) = input.parameters.get(name) {
                        *slot = value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| {
                                validation(format!("mixer field {name} must be a finite number"))
                            })?;
                        if !(-100.0..=100.0).contains(slot) {
                            return Err(validation(format!(
                                "mixer field {name} must be a number within -100..=100"
                            )));
                        }
                    }
                }
                merged
            }
            RESET_MIXER => [NEUTRAL; FIELDS.len()],
            action_id => return Err(validation(format!("unknown action {action_id}"))),
        };
        match existing {
            Some(_) if current == merged => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(Layer {
                id: layer.id.clone(),
                effect_id: MIXER_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                // The mask is the host's: an update keeps whatever this layer already carries.
                mask: layer.mask.clone(),
            })),
            // The host inserts a colour-stage layer after Basic by declared order; a neutral first
            // set has nothing to store, so it adds no layer at all.
            None if is_neutral(&merged) => Ok(ActionPlan::NoOp),
            None => Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: MIXER_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: payload_of(&merged),
                mask: None,
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
            RESET_MIXER => Some("Reset Colour mixer".into()),
            SET_MIXER => {
                let sent: Vec<(&String, f64)> = input
                    .parameters
                    .iter()
                    .map(|(name, value)| (name, value.as_f64().unwrap_or(NEUTRAL)))
                    .collect();
                match (reset_group(&sent), sent.as_slice()) {
                    (Some(group), _) => Some(format!("Reset {group}")),
                    (None, [(name, value)]) => Some(field_label(name, *value)),
                    // An empty patch changes nothing and commits no entry; the host falls back to
                    // the action's own title if it ever asks.
                    (None, []) => None,
                    // Any other patch: several fields changed at once, not a declared group reset.
                    (None, fields) => Some(format!("Colour mixer ({} fields)", fields.len())),
                }
            }
            _ => None,
        }
    }

    /// The values this stored layer represents, named exactly as `set-mixer`'s parameters are, so a
    /// client seeds its sliders from the displayed entry. A neutral layer reports the neutral value
    /// of every field rather than an empty object.
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
        // A neutral payload compiles to no units, which the host drops entirely: the identity
        // byte path and the shared source buffer are kept.
        if is_neutral(&values) {
            return Ok(Processing::Color(super::ColorOperation::neutral()));
        }
        let hue: [f64; unit::RANGE_COUNT] = values[0..8].try_into().expect("eight hue fields");
        let saturation: [f64; unit::RANGE_COUNT] =
            values[8..16].try_into().expect("eight saturation fields");
        let luminance: [f64; unit::RANGE_COUNT] =
            values[16..24].try_into().expect("eight luminance fields");
        let mixer: Arc<dyn PointwiseColor> = Arc::new(unit::Mixer::new(hue, saturation, luminance));
        Ok(Processing::Color(super::ColorOperation::new(vec![mixer])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BASIC_EFFECT, modules::check_parameters};
    use serde_json::json;

    const STAGE: Stage = Stage {
        width: 4,
        height: 4,
    };

    fn mixer_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: MIXER_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
        }
    }

    /// Plan one request the way the host does: generic parameter check, module parse, then plan
    /// against a stack whose stage questions are answered from constants.
    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = MixerModule::new();
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
        }
    }

    #[test]
    fn the_descriptor_declares_one_colour_effect_at_order_ten_two_actions_and_three_groups() {
        let module = MixerModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "lightwell.mixer");
        assert_eq!(descriptor.title, "Colour mixer");
        assert_eq!(
            descriptor.hint.as_deref(),
            Some("Hue, saturation and luminance by range")
        );
        assert!(descriptor.collapsed);
        assert!(!descriptor.developer);
        assert_eq!(descriptor.effects.len(), 1);
        assert_eq!(descriptor.effects[0].id, MIXER_EFFECT);
        assert_eq!(descriptor.effects[0].format, 1);
        assert_eq!(descriptor.effects[0].stage, EffectStage::Color);
        assert_eq!(descriptor.effects[0].order, 10);
        assert_eq!(
            descriptor.reset,
            Some(ResetAction {
                action: RESET_MIXER.into(),
                preset: Map::new(),
            })
        );
        assert!(descriptor.canvas.is_none());
        assert!(descriptor.queries.is_empty());

        let set = descriptor.action(SET_MIXER).expect("set-mixer");
        assert!(set.patch);
        assert!(set.summary.is_none());
        assert_eq!(set.parameters.len(), 24);
        assert_eq!(
            set.parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>(),
            FIELDS,
            "every field is a declared parameter of the one patch action, in FIELDS order"
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

        let reset = descriptor.action(RESET_MIXER).expect("reset-mixer");
        assert!(reset.parameters.is_empty());
        assert!(!reset.patch);

        assert_eq!(descriptor.controls.len(), 3);
        let group_labels: Vec<&str> = descriptor
            .controls
            .iter()
            .map(|control| match control {
                Control::Group { label, .. } => label.as_str(),
                _ => panic!("every top-level control is a group"),
            })
            .collect();
        assert_eq!(group_labels, ["Hue", "Saturation", "Luminance"]);
        let collapsed: Vec<bool> = descriptor
            .controls
            .iter()
            .map(|control| match control {
                Control::Group { collapsed, .. } => *collapsed,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            collapsed,
            [false, true, true],
            "Hue starts expanded, Saturation and Luminance collapsed"
        );
        for control in &descriptor.controls {
            let Control::Group {
                controls, reset, ..
            } = control
            else {
                unreachable!()
            };
            assert_eq!(controls.len(), 8);
            let labels: Vec<&str> = controls
                .iter()
                .map(|control| match control {
                    Control::Number { label, .. } => label.as_str(),
                    _ => panic!("every group control is a slider"),
                })
                .collect();
            assert_eq!(
                labels,
                [
                    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"
                ]
            );
            let reset = reset.as_ref().expect("a group reset");
            assert_eq!(reset.action, SET_MIXER);
            assert_eq!(reset.preset.len(), 8);
            assert!(reset.preset.values().all(|value| *value == json!(0.0)));
        }
    }

    #[test]
    fn every_slider_declares_the_gradient_rail_hint() {
        let module = MixerModule::new();
        for control in &module.descriptor().controls {
            let Control::Group { controls, .. } = control else {
                unreachable!()
            };
            for control in controls {
                let Control::Number { rail, .. } = control else {
                    unreachable!()
                };
                match rail {
                    Some(RailDecoration::Gradient { stops }) => {
                        assert!((2..=8).contains(&stops.len()));
                    }
                    other => panic!("expected a gradient rail, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_payload_is_refused_by_shape_format_field_and_range_without_being_rewritten() {
        let module = MixerModule::new();
        let refused = |format: u32, payload: Value| {
            module
                .validate_payload(MIXER_EFFECT, format, &payload)
                .expect_err("an invalid payload")
        };
        for payload in [
            json!({}),
            json!({"red-hue": 0}),
            json!({"red-hue": -100.0}),
            json!({"red-hue": 100.0}),
            json!({"aqua-saturation": -100.0, "aqua-luminance": 30.0}),
        ] {
            module
                .validate_payload(MIXER_EFFECT, 1, &payload)
                .unwrap_or_else(|error| panic!("{payload} should be valid: {error}"));
        }

        let format = refused(2, json!({"red-hue": 1.0}));
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
                json!({"red-gamma": 10.0}),
                "unknown mixer field red-gamma",
            ),
            (
                "a string value",
                json!({"red-hue": "10"}),
                "must be a finite number",
            ),
            (
                "below the range",
                json!({"red-hue": -100.001}),
                "within -100..=100",
            ),
            (
                "above the range",
                json!({"red-hue": 100.001}),
                "within -100..=100",
            ),
        ] {
            let error = refused(1, payload);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(needle), "{case}: {}", error.detail);
        }
    }

    #[test]
    fn the_canonical_neutral_payload_is_the_empty_object_and_compares_equal_to_an_explicit_zero() {
        let committed_layer = committed(planned(SET_MIXER, json!({"red-hue": 20.0}), &[]).unwrap());
        assert_eq!(committed_layer.payload, json!({"red-hue": 20.0}));
        assert_eq!(committed_layer.effect_id, MIXER_EFFECT);
        assert_eq!(committed_layer.effect_format, 1);

        let neutralized = committed(
            planned(
                SET_MIXER,
                json!({"red-hue": 0.0}),
                &[mixer_layer(json!({"red-hue": 20.0}))],
            )
            .unwrap(),
        );
        assert_eq!(neutralized.payload, json!({}));

        for stored in [json!({}), json!({"red-hue": 0.0})] {
            assert_eq!(
                planned(
                    SET_MIXER,
                    json!({"red-hue": 0.0}),
                    &[mixer_layer(stored.clone())]
                )
                .unwrap(),
                ActionPlan::NoOp,
                "setting neutral on a {stored} layer changes nothing"
            );
            assert_eq!(
                planned(RESET_MIXER, json!({}), &[mixer_layer(stored.clone())]).unwrap(),
                ActionPlan::NoOp,
                "resetting a {stored} layer changes nothing"
            );
        }
        assert_eq!(
            planned(SET_MIXER, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "a neutral first set has nothing to commit"
        );
        assert_eq!(
            planned(RESET_MIXER, json!({}), &[]).unwrap(),
            ActionPlan::NoOp,
            "resetting with no layer at all is a no-op"
        );
    }

    #[test]
    fn plan_commits_updates_and_no_ops_exactly_like_set_basic() {
        // First non-neutral set commits.
        let commit = planned(SET_MIXER, json!({"aqua-saturation": -40.0}), &[]).unwrap();
        let layer = committed(commit);
        assert_eq!(layer.payload, json!({"aqua-saturation": -40.0}));

        // A later set on an existing layer merges and updates in place.
        let update = committed(
            planned(
                SET_MIXER,
                json!({"aqua-luminance": 15.0}),
                std::slice::from_ref(&layer),
            )
            .unwrap(),
        );
        assert_eq!(update.id, layer.id);
        assert_eq!(
            update.payload,
            json!({"aqua-saturation": -40.0, "aqua-luminance": 15.0})
        );

        // An unchanged merge is a no-op.
        assert_eq!(
            planned(
                SET_MIXER,
                json!({"aqua-saturation": -40.0}),
                std::slice::from_ref(&layer)
            )
            .unwrap(),
            ActionPlan::NoOp
        );

        // Two layers are refused before this module ever plans against them: `locate` reports the
        // ambiguity itself, matching the host's own whole-stack check.
        let error =
            planned(SET_MIXER, json!({"red-hue": 1.0}), &[layer.clone(), layer]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, AMBIGUOUS);
    }

    #[test]
    fn labels_name_one_field_a_group_reset_the_module_reset_or_the_field_count() {
        let module = MixerModule::new();
        let label = |action: &str, parameters: Value| {
            module
                .label(&ActionInput {
                    action_id: action.into(),
                    parameters: parameters.as_object().unwrap().clone(),
                })
                .unwrap_or_default()
        };
        assert_eq!(label(SET_MIXER, json!({"red-hue": 20.0})), "Red hue +20");
        assert_eq!(
            label(SET_MIXER, json!({"aqua-luminance": -15.0})),
            "Aqua luminance -15"
        );
        let hue_neutral: Value = FIELDS[0..8]
            .iter()
            .map(|name| (name.to_string(), json!(0.0)))
            .collect::<Map<_, _>>()
            .into();
        assert_eq!(label(SET_MIXER, hue_neutral), "Reset Hue");
        let saturation_neutral: Value = FIELDS[8..16]
            .iter()
            .map(|name| (name.to_string(), json!(0.0)))
            .collect::<Map<_, _>>()
            .into();
        assert_eq!(label(SET_MIXER, saturation_neutral), "Reset Saturation");
        let luminance_neutral: Value = FIELDS[16..24]
            .iter()
            .map(|name| (name.to_string(), json!(0.0)))
            .collect::<Map<_, _>>()
            .into();
        assert_eq!(label(SET_MIXER, luminance_neutral), "Reset Luminance");
        assert_eq!(
            module.label(&ActionInput {
                action_id: RESET_MIXER.into(),
                parameters: Map::new(),
            }),
            Some("Reset Colour mixer".into())
        );
        assert_eq!(
            label(SET_MIXER, json!({"red-hue": 1.0, "blue-saturation": 2.0})),
            "Colour mixer (2 fields)"
        );
        assert_eq!(
            module.label(&ActionInput {
                action_id: SET_MIXER.into(),
                parameters: Map::new(),
            }),
            None
        );
    }

    #[test]
    fn values_reports_every_field_and_describe_layer_lists_non_neutral_ones() {
        let module = MixerModule::new();
        let payload = json!({"red-hue": 20.0, "aqua-luminance": -15.0});
        let values = module.values(MIXER_EFFECT, 1, &payload).unwrap();
        assert_eq!(values.len(), 24);
        assert_eq!(values["red-hue"], json!(20.0));
        assert_eq!(values["magenta-luminance"], json!(0.0));

        assert_eq!(
            module.describe_layer(MIXER_EFFECT, 1, &payload).unwrap(),
            "Red hue +20, Aqua luminance -15"
        );
        assert_eq!(
            module.describe_layer(MIXER_EFFECT, 1, &json!({})).unwrap(),
            "Neutral"
        );
    }

    #[test]
    fn compile_is_neutral_for_the_empty_payload_and_a_unit_otherwise() {
        let module = MixerModule::new();
        let neutral = module.compile(MIXER_EFFECT, 1, &json!({}), STAGE).unwrap();
        assert_eq!(
            neutral,
            Processing::Color(super::super::ColorOperation::neutral())
        );

        let coloured = module
            .compile(MIXER_EFFECT, 1, &json!({"red-hue": 20.0}), STAGE)
            .unwrap();
        match coloured {
            Processing::Color(operation) => assert_eq!(operation.len(), 1),
            other => panic!("expected a colour operation, got {other:?}"),
        }
    }
}
