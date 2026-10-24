//! The Grain module: one finish-stage layer holding Amount, Size and Roughness, edited by one
//! field-patch action (`docs/design/grain.md`).
//!
//! A payload is a JSON object whose keys are the three parameter names (`amount`, `size`,
//! `roughness`); a **missing key means that parameter's own default** (amount 0, size 25,
//! roughness 50), as in the vignette, so the canonical all-default payload is `{}`. The field-patch
//! behaviour lives in [`super::field_patch`]; this file is the field table, the neutrality rule and
//! the compilation.
//!
//! The layer as a whole is neutral — compiles to no processing at all — exactly when `amount` is
//! `0`, whatever `size` and `roughness` hold: the study's application is the exact identity at
//! Amount 0 (`docs/design/grain-study.md`), so no field is ever evaluated for a layer that changes
//! nothing, and the identity byte path and the shared source buffer are kept.
//!
//! The host places the layer at the end of the stack because `luxforge.grain.film` declares the
//! `finish` stage, with declared order 10, so a grain layer follows the vignette (order 0)
//! whichever was committed first, and a later crop update moves the crop in place before both. The
//! grain is a function of the output pixel's position in the full-resolution output stage, so a
//! crop re-lays the pattern without changing its size.
mod unit;

use super::{
    ColorOperation, EffectStage, PointwiseColor, Processing,
    field_patch::{Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::Error;
use std::sync::Arc;

/// The one finish-stage effect of the Grain module: the stack's one grain layer, evaluated after
/// the geometry tail and the vignette in output-stage pixel coordinates.
pub const GRAIN_EFFECT: &str = "luxforge.grain.film";

/// The grain effect's current payload format, listed among [`super::BUILTIN_FORMATS`]. A stored
/// grain layer at any other format holds values of another meaning and is refused, never
/// rewritten.
pub(super) const GRAIN_EFFECT_FORMAT: u32 = 1;

/// The effect's declared order within the finish stage: after the vignette's 0, as Lightroom
/// applies grain after its post-crop vignette.
const GRAIN_ORDER: u16 = 10;

const AMOUNT: &str = "amount";
const SIZE: &str = "size";
const ROUGHNESS: &str = "roughness";

/// Every Grain field, in the payload's declared order, which is also the order the group's three
/// sliders render in.
const FIELDS: [&str; 3] = [AMOUNT, SIZE, ROUGHNESS];

/// The label the one group and the module section share.
const GROUP_LABEL: &str = "Grain";

/// One Grain field: a 0..100 slider with its own default, named in history by the module and the
/// field, `Grain amount 40`. Every range starts at 0, so no value shows a sign.
fn grain_field(
    name: &'static str,
    label: &str,
    default: f64,
    zero: Option<f64>,
    notes: &str,
) -> Field {
    let field = Field::slider(name, label, notes)
        .history(format!("{GROUP_LABEL} {}", label.to_ascii_lowercase()))
        .range(0.0, 100.0)
        .default(default);
    match zero {
        Some(zero) => field.zero(zero),
        None => field,
    }
}

/// The Grain module's table, neutrality rule and compilation.
#[derive(Debug, Default)]
pub(crate) struct Grain;

/// The Grain module: `Grain` as a field-patch module.
pub(crate) type GrainModule = FieldPatchModule<Grain>;

impl FieldPatch for Grain {
    fn spec() -> Spec {
        Spec::new(
            "luxforge.grain",
            GROUP_LABEL,
            "Add film grain after the crop and the vignette",
            GRAIN_EFFECT,
            EffectStage::Finish,
        )
        .format(GRAIN_EFFECT_FORMAT)
        .order(GRAIN_ORDER)
        .set_notes(
            "merges the named Grain fields into the stack's one Grain layer. A missing key means \
             that field's own default (amount 0, size 25, roughness 50). The host places the \
             layer at the end of the stack, after the geometry tail and the vignette, on the \
             first commit whose merged amount is non-zero, and updates it there in place \
             afterwards; a patch that changes nothing is a reported no-op, and a first set whose \
             merged amount is still 0 commits no layer at all.",
        )
        .reset_notes(
            "returns the stack's one Grain layer to its all-default payload, keeping its identity \
             and position; a no-op without one and when it is already all default.",
        )
        .fields([
            grain_field(
                AMOUNT,
                "Amount",
                0.0,
                Some(0.0),
                "the grain's strength: luminance grain weighted by tone, vanishing at black and \
                 at each pixel's white, mean-preserving and colour-preserving. 0 is the exact \
                 identity whatever size and roughness hold, and a missing key defaults to 0.",
            ),
            grain_field(
                SIZE,
                "Size",
                25.0,
                None,
                "the grain's particle size, in pixels of the full-resolution output stage: about \
                 one pixel at 0, six at 100, unchanged by a crop; a missing key defaults to 25.",
            ),
            grain_field(
                ROUGHNESS,
                "Roughness",
                50.0,
                None,
                "the grain's regularity at fixed strength: fine and even at 0, uneven and clumped \
                 at 100; a missing key defaults to 50.",
            ),
        ])
        .group(Group::new(GROUP_LABEL, FIELDS))
        .collapsed()
    }

    /// The layer as a whole is neutral exactly when `amount` is 0, whatever the other two fields
    /// hold: the application is then the exact identity, so the shared field patch compiles such a
    /// layer to no units.
    fn is_neutral(&self, values: &Values<'_>) -> bool {
        values.number(AMOUNT) == 0.0
    }

    /// One grain unit compiled against the stage the layer receives. Its footprint, the
    /// full-resolution output pixels per stage pixel, is `max(1, full / stage)` per axis: 1 at full
    /// resolution, where the grain at a stage pixel is the full-resolution field at that output
    /// pixel, and the study's reduced-stage rule below it.
    fn compile(&self, values: &Values<'_>, at: crate::CompileStage) -> Result<Processing, Error> {
        let strength = |name: &str| RESPONSES.strength(name, values.number(name));
        let unit: Arc<dyn PointwiseColor> = Arc::new(unit::Grain::new(
            strength(AMOUNT),
            strength(SIZE),
            strength(ROUGHNESS),
            unit::Footprint::of(at.stage, at.full),
        ));
        Ok(Processing::Color(ColorOperation::new(vec![unit])))
    }
}

/// This module's frozen slider responses ([`super::response`]): the identity until the alignment
/// round folds Lightroom's.
static RESPONSES: std::sync::LazyLock<super::response::Responses> =
    std::sync::LazyLock::new(|| {
        super::response::Responses::parse(include_str!("response.json"), "grain")
    });

/// The response of this module's `field`.
pub(super) fn response(field: &str) -> Option<&'static super::response::Response> {
    RESPONSES.get(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::check_parameters;
    use crate::modules::{ActionInput, ActionPlan, FixedStage, ToolModule};
    use crate::{CompileStage, Layer, Stage};
    use serde_json::{Value, json};

    const SET_GRAIN: &str = "set-grain";
    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn planned(action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let module = GrainModule::new();
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

    fn compiled(payload: Value, at: CompileStage) -> Processing {
        GrainModule::new()
            .compile(
                GRAIN_EFFECT,
                crate::current_effect_format(GRAIN_EFFECT),
                &payload,
                at,
            )
            .expect("compiles")
    }

    fn units(processing: &Processing) -> &[Arc<dyn PointwiseColor>] {
        match processing {
            Processing::Color(operation) => operation.units(),
            other => panic!("expected Processing::Color, got {other:?}"),
        }
    }

    /// The descriptor the design names: one finish effect at order 10 and format 1, one layer per
    /// stack, not maskable, presettable, a collapsed section of one group, and three 0..100 sliders
    /// with their own defaults and Amount's zero.
    #[test]
    fn the_descriptor_declares_a_single_finish_effect_after_the_vignette() {
        let module = GrainModule::new();
        let descriptor = module.descriptor();
        assert_eq!(descriptor.id, "luxforge.grain");
        assert_eq!(descriptor.title, "Grain");
        let [effect] = descriptor.effects.as_slice() else {
            panic!("one effect: {:?}", descriptor.effects);
        };
        assert_eq!(effect.id, GRAIN_EFFECT);
        assert_eq!(effect.stage, EffectStage::Finish);
        assert_eq!(effect.order, 10);
        assert_eq!(effect.format, 1);
        assert!(effect.single);
        assert!(!effect.maskable);
        assert!(descriptor.collapsed);
        let set = descriptor.action(SET_GRAIN).expect("set-grain");
        assert!(set.patch);
        assert!(descriptor.action("reset-grain").is_some());
        let fields: Vec<(&str, Value, Option<f64>)> = set
            .parameters
            .iter()
            .map(|parameter| {
                assert_eq!(
                    parameter.kind,
                    crate::ParameterKind::Number {
                        min: 0.0,
                        max: 100.0
                    },
                    "{}",
                    parameter.name
                );
                (
                    parameter.name.as_str(),
                    parameter.default.clone().expect("a default"),
                    parameter.zero,
                )
            })
            .collect();
        assert_eq!(
            fields,
            [
                ("amount", json!(0.0), Some(0.0)),
                ("size", json!(25.0), None),
                ("roughness", json!(50.0), None),
            ]
        );
    }

    #[test]
    fn a_first_set_with_zero_amount_commits_nothing_even_with_other_fields_set() {
        let plan =
            planned(SET_GRAIN, json!({"size": 80.0, "roughness": 10.0}), &[]).expect("a plan");
        assert_eq!(plan, ActionPlan::NoOp);
    }

    /// The words a history label uses for each field: one-sided ranges show no sign.
    #[test]
    fn each_field_is_named_by_its_own_history_words() {
        let module = GrainModule::new();
        for (parameters, expected) in [
            (json!({"amount": 40.0}), "Grain amount 40"),
            (json!({"size": 25.0}), "Grain size 25"),
            (json!({"roughness": 50.0}), "Grain roughness 50"),
        ] {
            let label = module.label(
                module.descriptor().action(SET_GRAIN).unwrap(),
                &ActionInput {
                    action_id: SET_GRAIN.to_owned(),
                    parameters: parameters.as_object().cloned().unwrap(),
                },
            );
            assert_eq!(label, expected, "{parameters}");
        }
        let reset = module.descriptor().action("reset-grain").unwrap();
        assert_eq!(
            module.label(
                reset,
                &ActionInput {
                    action_id: "reset-grain".to_owned(),
                    parameters: serde_json::Map::new(),
                }
            ),
            "Reset Grain"
        );
    }

    /// Amount 0 compiles to no unit whatever Size and Roughness hold; a non-zero Amount compiles
    /// to exactly one.
    #[test]
    fn amount_zero_compiles_to_nothing_and_a_non_zero_amount_to_one_unit() {
        let at = CompileStage::exact(STAGE);
        for payload in [
            json!({}),
            json!({"amount": 0.0, "size": 100.0, "roughness": 0.0}),
            json!({"amount": -0.0}),
        ] {
            assert!(
                units(&compiled(payload.clone(), at)).is_empty(),
                "{payload}"
            );
        }
        assert_eq!(units(&compiled(json!({"amount": 40.0}), at)).len(), 1);
    }

    /// A GPU-shaped compile of a neutral layer holds the unit, and that unit is the exact identity
    /// on every pixel.
    #[test]
    fn a_neutral_gpu_shaped_unit_is_the_exact_identity() {
        let at = CompileStage::exact(STAGE).shaped(true);
        let processing = compiled(json!({"size": 100.0, "roughness": 100.0}), at);
        let [unit] = units(&processing) else {
            panic!("the GPU shape holds the unit");
        };
        let mut row = [[0.2f32, 0.4, 0.1], [0.5, 0.5, 0.5], [0.9, 0.01, 0.3]];
        let before = row;
        unit.apply_row(7, 3, &mut row);
        assert_eq!(row, before);
    }

    /// A stage compiled at full resolution has footprint 1; one compiled for a larger full stage
    /// takes `full / stage` on each axis, and the compile differs, while two stages of one
    /// footprint and payload process identically whatever their size.
    #[test]
    fn the_footprint_comes_from_the_compile_stage() {
        let payload = json!({"amount": 60.0, "size": 50.0});
        let exact = compiled(payload.clone(), CompileStage::exact(STAGE));
        let larger = compiled(
            payload.clone(),
            CompileStage::exact(Stage {
                width: 6000,
                height: 4000,
            }),
        );
        assert_eq!(
            exact, larger,
            "the field does not depend on the stage's size"
        );
        let reduced = compiled(
            payload,
            CompileStage::sampled(
                Stage {
                    width: 240,
                    height: 160,
                },
                STAGE,
            ),
        );
        assert_ne!(exact, reduced);
        assert!(units(&reduced)[0].describe().contains("footprint=2x2"));
        assert!(units(&exact)[0].describe().contains("footprint=1x1"));
    }

    /// The response table is the identity on every field for now: each strength is its value.
    #[test]
    fn the_responses_are_the_identity() {
        for field in FIELDS {
            let response = response(field).expect("a response");
            for value in [0.0, 12.5, 25.0, 50.0, 99.0, 100.0] {
                assert_eq!(response.strength(value), value, "{field}");
            }
        }
    }
}
