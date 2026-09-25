//! The colour mixer module: one colour-stage layer holding hue, saturation and luminance for each
//! of eight colour ranges, edited by one field-patch action.
//!
//! This is a field-patch module (`docs/design/modules-and-api.md`'s field-patch contract, shared in
//! [`super::field_patch`]): a payload is a JSON object whose keys are the implemented parameter
//! names, a missing key is neutral, and the canonical neutral payload is the empty object `{}`. The
//! module owns exactly one layer of `luxforge.mixer.hsl`, declared order 10 so a mixer layer
//! always follows the Basic layer in the colour run (`docs/design/presence-mixer-vignette.md`,
//! "Placement and stage order"). The one pointwise unit's equations are frozen in
//! `docs/design/mixer-study.md` and implemented in [`unit::Mixer`]; this file owns only the field
//! table, the controls' rails and the compilation into that unit.
mod unit;

use super::{
    ColorOperation, EffectDescriptor, EffectStage, PointwiseColor, Processing, RailDecoration,
    Stage,
    field_patch::{ActionText, Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::{EFFECT_FORMAT, Error};
use std::sync::Arc;

/// The colour mixer's one pointwise unit: hue, saturation and luminance for the eight colour
/// ranges, declared order 10 so a mixer layer always follows the Basic layer in the colour run.
pub const MIXER_EFFECT: &str = "luxforge.mixer.hsl";

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

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check.
#[cfg(test)]
pub(crate) const AMBIGUOUS: &str = "ambiguous Colour mixer layers";

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

/// One `<range>-<property>` field: -100..100, step 1, no display decimals, no unit, a zero hint,
/// the range's name on its slider and the range's colours on its rail. Its history label names
/// the range and the property, e.g. `Red hue +20`.
fn mixer_field(name: &'static str) -> Field {
    let (range, property) = parse_field(name).expect("a declared mixer field");
    let label = range_label(range);
    let (notes, rail) = match property {
        HUE => (
            format!(
                "moves the {label} range's hues toward a neighbouring range: +100 carries its centre colour 85% of the way to the next range's centre and -100 85% of the way to the previous one; two neighbours driven at each other share one slider's travel"
            ),
            hue_rail(range),
        ),
        SATURATION => (
            format!(
                "scales chroma within the {label} range; -100 is exactly neutral grey for that range's own colour and +100 doubles chroma; every saturation slider at -100 makes the whole photo exactly grey"
            ),
            saturation_rail(range),
        ),
        LUMINANCE => (
            format!(
                "scales Oklab L within the {label} range through a compressive response with the near-black rule"
            ),
            luminance_rail(range),
        ),
        _ => unreachable!("parse_field returns only hue, saturation or luminance"),
    };
    Field {
        history: format!("{label} {property}"),
        zero: Some(NEUTRAL),
        rail: Some(rail),
        ..Field::slider(name, label, notes)
    }
}

/// The colour mixer's table and compilation.
#[derive(Debug, Default)]
pub struct Mixer;

/// The colour mixer module: `Mixer` as a field-patch module.
pub type MixerModule = FieldPatchModule<Mixer>;

impl FieldPatch for Mixer {
    fn spec() -> Spec {
        Spec {
            id: "luxforge.mixer",
            title: "Colour mixer",
            hint: "Hue, saturation and luminance by range",
            noun: "mixer",
            effect: EffectDescriptor {
                id: MIXER_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Color,
                order: 10,
                maskable: true,
                artifacts: false,
                single: true,
            },
            set: ActionText {
                id: SET_MIXER,
                title: "Set Colour mixer",
                notes: "merges the named mixer fields into the stack's one Colour mixer layer, which the host places after Basic by declared order on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op",
            },
            reset: ActionText {
                id: RESET_MIXER,
                title: "Reset Colour mixer",
                notes: "returns the stack's one Colour mixer layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral",
            },
            fields: FIELDS.iter().map(|name| mixer_field(name)).collect(),
            groups: vec![
                Group {
                    label: HUE_GROUP,
                    fields: FIELDS[0..8].to_vec(),
                    collapsed: false,
                    extra: Vec::new(),
                },
                Group {
                    label: SATURATION_GROUP,
                    fields: FIELDS[8..16].to_vec(),
                    collapsed: true,
                    extra: Vec::new(),
                },
                Group {
                    label: LUMINANCE_GROUP,
                    fields: FIELDS[16..24].to_vec(),
                    collapsed: true,
                    extra: Vec::new(),
                },
            ],
            queries: Vec::new(),
            canvas: None,
            collapsed: true,
            // The three groups are parallel views of the same eight ranges, so the desktop draws
            // them as one segmented row instead of stacked sections.
            layout: crate::ModuleLayout::Tabs,
        }
    }

    fn compile(&self, values: &Values<'_>, _: Stage) -> Result<Processing, Error> {
        // A neutral payload compiles to no units, which the host drops entirely: the identity
        // byte path and the shared source buffer are kept.
        if values.all_default() {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        let values = values.as_slice();
        let hue: [f64; unit::RANGE_COUNT] = values[0..8].try_into().expect("eight hue fields");
        let saturation: [f64; unit::RANGE_COUNT] =
            values[8..16].try_into().expect("eight saturation fields");
        let luminance: [f64; unit::RANGE_COUNT] =
            values[16..24].try_into().expect("eight luminance fields");
        let mixer: Arc<dyn PointwiseColor> = Arc::new(unit::Mixer::new(hue, saturation, luminance));
        Ok(Processing::Color(ColorOperation::new(vec![mixer])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{
        ActionInput, ActionPlan, Control, FixedStage, ParameterKind, ResetAction, ToolModule,
    };
    use crate::{BASIC_EFFECT, modules::check_parameters};
    use crate::{ErrorKind, Layer, LayerId};
    use serde_json::json;
    use serde_json::{Map, Value};

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
            artifacts: Vec::new(),
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
        module.plan(
            &input,
            &FixedStage::new(STAGE)
                .reading([0, 0, 0, 255])
                .context(layers, &crate::ModuleRegistry::builtin()),
        )
    }

    fn committed(plan: ActionPlan) -> Layer {
        match plan {
            // The layer the host stores for the plan: a commit's effect and payload, or the
            // updated layer's identity with its new payload.
            ActionPlan::Commit(new) => Layer::new(new.effect_id, new.payload),
            ActionPlan::Update(update) => Layer {
                id: update.id,
                ..Layer::new(MIXER_EFFECT, update.payload)
            },
            ActionPlan::NoOp => panic!("expected a layer, not a no-op"),
            ActionPlan::Compose(_) => panic!("expected a layer, not a composite"),
            ActionPlan::Edits(_) => panic!("expected a layer, not several edits"),
        }
    }

    #[test]
    fn the_descriptor_declares_one_colour_effect_at_order_ten_two_actions_and_three_groups() {
        let module = MixerModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "luxforge.mixer");
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
