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
    ColorOperation, EffectStage, PointwiseColor, Processing, Stage,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use std::sync::Arc;

/// The one finish-stage effect of the Vignette module: every implemented Vignette parameter of a
/// stack lives in one layer of this effect, evaluated after the geometry tail in output-stage
/// pixel coordinates. Named `postcrop` rather than `post-crop`: `valid_identity` forbids a hyphen
/// inside a dot-separated identity segment (every other built-in effect follows the same rule,
/// e.g. `luxforge.basic.adjust`), so the closest one-word form of the design's "post-crop
/// vignette" name is used instead of a literal hyphen.
pub const VIGNETTE_EFFECT: &str = "luxforge.vignette.postcrop";

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
    let field = Field::slider(name, label, notes)
        .history(format!("{GROUP_LABEL} {}", label.to_ascii_lowercase()))
        .range(min, 100.0)
        .default(default);
    match zero {
        Some(zero) => field.zero(zero),
        None => field,
    }
}

/// The Vignette module's table, neutrality rule and compilation.
#[derive(Debug, Default)]
pub(crate) struct Vignette;

/// The Vignette module: `Vignette` as a field-patch module.
pub(crate) type VignetteModule = FieldPatchModule<Vignette>;

impl FieldPatch for Vignette {
    fn spec() -> Spec {
        Spec::new(
            "luxforge.vignette",
            GROUP_LABEL,
            "Darken or lighten the corners after the crop",
            VIGNETTE_EFFECT,
            EffectStage::Finish,
        )
        .set_notes(
            "merges the named Vignette fields into the stack's one Vignette layer. A missing key \
             means that field's own default (amount 0, midpoint 50, roundness 0, feather 50), not \
             neutral 0 for every field: unlike Basic, midpoint and feather default away from 0. \
             The host places the layer at the end of the stack, after the geometry tail, on the \
             first commit whose merged amount is non-zero, and updates it there in place \
             afterwards; a patch that changes nothing is a reported no-op, and a first set whose \
             merged amount is still 0 commits no layer at all.",
        )
        .reset_notes(
            "returns the stack's one Vignette layer to its all-default payload, keeping its \
             identity and position; a no-op without one and when it is already all default.",
        )
        .fields([
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
        ])
        .group(Group::new(GROUP_LABEL, FIELDS))
        .collapsed()
    }

    /// The layer as a whole is neutral exactly when `amount` is 0, whatever the other three
    /// fields hold: the frozen mask geometry never runs when the amount equation is itself the
    /// identity, so the shared field patch compiles such a layer to no units and never asks
    /// [`FieldPatch::compile`] to build a mask for it.
    fn is_neutral(&self, values: &Values<'_>) -> bool {
        values.number(AMOUNT) == 0.0
    }

    fn compile(&self, values: &Values<'_>, stage: Stage) -> Result<Processing, Error> {
        let unit: Arc<dyn PointwiseColor> = Arc::new(unit::Vignette::new(
            values.number(AMOUNT),
            values.number(MIDPOINT),
            values.number(ROUNDNESS),
            values.number(FEATHER),
            stage,
        ));
        Ok(Processing::Color(ColorOperation::new(vec![unit])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::check_parameters;
    use crate::modules::{ActionInput, ActionPlan, FixedStage, ToolModule};
    use crate::{EFFECT_FORMAT, Layer};
    use serde_json::Value;
    use serde_json::json;

    const SET_VIGNETTE: &str = "set-vignette";
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
            let label = module.label(
                module.descriptor().action(SET_VIGNETTE).unwrap(),
                &ActionInput {
                    action_id: SET_VIGNETTE.to_owned(),
                    parameters: parameters.as_object().cloned().unwrap(),
                },
            );
            assert_eq!(label, expected, "{parameters}");
        }
    }

    // ------------------------------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------------------------------

    // ------------------------------------------------------------------------------------------
    // describe / compile
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
