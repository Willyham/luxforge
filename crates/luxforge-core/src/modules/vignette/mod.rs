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
//! `luxforge.vignette.postcrop` declares the `finish` stage: a later crop update moves the crop
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
/// e.g. `luxforge.basic.adjust`), so the closest one-word form of the design's "post-crop
/// vignette" name is used instead of a literal hyphen.
pub const VIGNETTE_EFFECT: &str = "luxforge.vignette.postcrop";

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

/// The Vignette module: `Vignette` as a field-patch module.
pub type VignetteModule = FieldPatchModule<Vignette>;

impl FieldPatch for Vignette {
    fn spec() -> Spec {
        Spec {
            id: "luxforge.vignette",
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
                single: true,
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
    use crate::Layer;
    use crate::modules::check_parameters;
    use crate::modules::{
        ActionInput, ActionPlan, Control, FixedStage, ParameterKind, ResetAction, ToolModule,
    };
    use serde_json::json;
    use serde_json::{Map, Value};

    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = VignetteModule::new();
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

    #[test]
    fn the_descriptor_declares_one_finish_effect_collapsed_group_and_two_actions() {
        let module = VignetteModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert_eq!(descriptor.id, "luxforge.vignette");
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
            .find(|descriptor| descriptor["id"] == json!("luxforge.vignette"))
            .expect("the vignette module is registered");
        assert_eq!(vignette["collapsed"], json!(true));
        assert_eq!(
            vignette["effects"][0],
            json!({"id": VIGNETTE_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0, "single": true})
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

    // ------------------------------------------------------------------------------------------
    // Labels
    // ------------------------------------------------------------------------------------------

    /// The words a history label and the recipe row use for each field, with its declared decimals,
    /// sign and unit. The rules that choose a label — a group reset, the module reset, a field count
    /// — are the shared field-patch rules the conformance suite proves for every module.
    #[test]
    fn each_field_is_named_by_its_own_history_words() {
        let module = VignetteModule::new();
        for (parameters, expected) in [
            (json!({"amount": -35.0}), "Vignette amount -35"),
            (json!({"midpoint": 60.0}), "Vignette midpoint 60"),
            (json!({"roundness": 20.0}), "Vignette roundness +20"),
            (json!({"feather": 40.0}), "Vignette feather 40"),
        ] {
            let label = module.label(&ActionInput {
                action_id: SET_VIGNETTE.to_owned(),
                parameters: parameters.as_object().cloned().unwrap(),
            });
            assert_eq!(label.as_deref(), Some(expected), "{parameters}");
        }
    }

    // ------------------------------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------------------------------

    // ------------------------------------------------------------------------------------------
    // describe_layer / compile
    // ------------------------------------------------------------------------------------------

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
