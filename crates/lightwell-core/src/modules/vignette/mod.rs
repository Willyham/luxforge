//! The Vignette module: one finish-stage layer holding every Vignette parameter, edited by one
//! field-patch action.
//!
//! A payload is a JSON object whose keys are the four implemented parameter names (`amount`,
//! `midpoint`, `roundness`, `feather`); a **missing key means that parameter's own default**
//! (amount 0, midpoint 50, roundness 0, feather 50) rather than 0, unlike the Basic module, where a
//! missing key means neutral 0 for every field because every Basic field's neutral value happens to
//! be 0. The canonical all-default payload is `{}`, and `{"midpoint": 50}` is the same state written
//! differently. The field-patch behaviour lives in [`super::field_patch`]; this file is the field
//! table, the neutrality rule and the compilation.
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
    ColorOperation, EffectDescriptor, EffectStage, PointwiseColor, Processing, Stage,
    field_patch::{ActionText, Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::{EFFECT_FORMAT, Error};
use std::sync::Arc;

/// The one finish-stage effect of the Vignette module: every implemented Vignette parameter of a
/// stack lives in one layer of this effect, evaluated after the geometry tail in output-stage
/// pixel coordinates. Named `postcrop` rather than `post-crop`: `valid_identity` forbids a hyphen
/// inside a dot-separated identity segment (every other built-in effect follows the same rule,
/// e.g. `lightwell.basic.adjust`), so the closest one-word form of the design's "post-crop
/// vignette" name is used instead of a literal hyphen.
pub const VIGNETTE_EFFECT: &str = "lightwell.vignette.postcrop";

pub(super) const SET_VIGNETTE: &str = "set-vignette";
pub(super) const RESET_VIGNETTE: &str = "reset-vignette";

const AMOUNT: &str = "amount";
const MIDPOINT: &str = "midpoint";
const ROUNDNESS: &str = "roundness";
const FEATHER: &str = "feather";

/// Every implemented Vignette field, in the payload's declared order, which is also the order the
/// group's four sliders render in.
const FIELDS: [&str; 4] = [AMOUNT, MIDPOINT, ROUNDNESS, FEATHER];

/// The label the one group and the module section share (`"Vignette"` in both places, since there
/// is only one group).
const GROUP_LABEL: &str = "Vignette";

/// The one ambiguity message, shared by planning and by the host's whole-stack compile check
/// (which builds the same text from the module's own title).
#[cfg(test)]
pub(crate) const AMBIGUOUS: &str = "ambiguous Vignette layers";

/// One Vignette field. Its default is its own — `midpoint` and `feather` default to 50, not 0 —
/// and a history label names the module and the field, `Vignette amount -35`, because `Amount`
/// alone says nothing in a history list shared with every other module. `amount` and `roundness`
/// are bipolar about 0 and show a sign; `midpoint` and `feather` are one-sided magnitudes and do
/// not.
fn vignette_field(
    name: &'static str,
    label: &str,
    (min, default): (f64, f64),
    zero: Option<f64>,
    notes: &str,
) -> Field {
    Field {
        history: format!("{GROUP_LABEL} {}", label.to_ascii_lowercase()),
        min,
        default,
        zero,
        ..Field::slider(name, label, notes)
    }
}

/// The Vignette module's table, neutrality rule and compilation.
#[derive(Debug, Default)]
pub struct Vignette;

/// The Vignette module: [`Vignette`] as a field-patch module.
pub type VignetteModule = FieldPatchModule<Vignette>;

impl FieldPatch for Vignette {
    fn spec() -> Spec {
        Spec {
            id: "lightwell.vignette",
            title: GROUP_LABEL,
            hint: "Darken or lighten the corners after the crop",
            noun: "vignette",
            effect: EffectDescriptor {
                id: VIGNETTE_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Finish,
                order: 0,
                maskable: false,
                artifacts: false,
            },
            set: ActionText {
                id: SET_VIGNETTE,
                title: "Set Vignette",
                notes: "merges the named Vignette fields into the stack's one Vignette \
                         layer. A missing key means that field's own default (amount 0, \
                         midpoint 50, roundness 0, feather 50), not neutral 0 for every \
                         field: unlike Basic, midpoint and feather default away from 0. \
                         The host places the layer at the end of the stack, after the \
                         geometry tail, on the first commit whose merged amount is \
                         non-zero, and updates it there in place afterwards; a patch that \
                         changes nothing is a reported no-op, and a first set whose \
                         merged amount is still 0 commits no layer at all.",
            },
            reset: ActionText {
                id: RESET_VIGNETTE,
                title: "Reset Vignette",
                notes: "returns the stack's one Vignette layer to its all-default \
                         payload, keeping its identity and position; a no-op without one \
                         and when it is already all default.",
            },
            fields: vec![
                vignette_field(
                    AMOUNT,
                    "Amount",
                    (-100.0, 0.0),
                    Some(0.0),
                    "post-crop vignette strength: negative darkens toward black in linear light with gain \
                     1 - |amount|*mask, positive lightens toward encoded white through a compressive mapping \
                     that never pushes a below-white channel past it. 0 is the exact identity whatever \
                     midpoint, roundness and feather hold, and a missing key defaults to 0.",
                ),
                vignette_field(
                    MIDPOINT,
                    "Midpoint",
                    (0.0, 50.0),
                    None,
                    "where the falloff begins, as a fraction of the shape radius from the centre; a missing \
                     key defaults to 50.",
                ),
                vignette_field(
                    ROUNDNESS,
                    "Roundness",
                    (-100.0, 0.0),
                    Some(0.0),
                    "morphs the mask shape from a rounded rectangle (-100) through an ellipse (0) to a circle \
                     (100); a missing key defaults to 0.",
                ),
                vignette_field(
                    FEATHER,
                    "Feather",
                    (0.0, 50.0),
                    None,
                    "the width of the falloff transition, as a fraction of the shape radius; a missing key \
                     defaults to 50.",
                ),
            ],
            groups: vec![Group {
                label: GROUP_LABEL,
                fields: FIELDS.to_vec(),
                collapsed: false,
                extra: Vec::new(),
            }],
            queries: Vec::new(),
            canvas: None,
            collapsed: true,
            layout: crate::ModuleLayout::Stacked,
        }
    }

    /// The layer as a whole is neutral exactly when `amount` is 0, whatever the other three
    /// fields hold: the frozen mask geometry never runs when the amount equation is itself the
    /// identity.
    fn is_neutral(&self, values: &Values<'_>) -> bool {
        values.get(AMOUNT) == 0.0
    }

    fn compile(&self, values: &Values<'_>, stage: Stage) -> Result<Processing, Error> {
        // The mask never has to be built for a neutral layer, and the identity byte path and
        // shared source buffer are kept.
        if self.is_neutral(values) {
            return Ok(Processing::Color(ColorOperation::neutral()));
        }
        let unit: Arc<dyn PointwiseColor> = Arc::new(unit::Vignette::new(
            values.get(AMOUNT),
            values.get(MIDPOINT),
            values.get(ROUNDNESS),
            values.get(FEATHER),
            stage,
        ));
        Ok(Processing::Color(ColorOperation::new(vec![unit])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::check_parameters;
    use crate::modules::{
        ActionInput, ActionPlan, Control, ParameterKind, ResetAction, StageContext, ToolModule,
    };
    use crate::{ErrorKind, Layer, LayerId};
    use serde_json::json;
    use serde_json::{Map, Value};

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
            mask: None,
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
            // The layer the host stores for the plan: a commit's effect and payload, or the
            // updated layer's identity with its new payload.
            ActionPlan::Commit(new) => Layer::new(new.effect_id, new.payload),
            ActionPlan::Update(update) => Layer {
                id: update.id,
                ..Layer::new(VIGNETTE_EFFECT, update.payload)
            },
            ActionPlan::NoOp => panic!("expected a layer, not a no-op"),
            ActionPlan::Compose(_) => panic!("expected a layer, not a composite"),
            ActionPlan::Edits(_) => panic!("expected a layer, not several edits"),
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
