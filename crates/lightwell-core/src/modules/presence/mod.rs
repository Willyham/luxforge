//! The Presence module: one spatial-stage layer holding Texture, Clarity and Dehaze, edited by one
//! field-patch action.
//!
//! This is a field-patch module (`docs/design/modules-and-api.md`'s field-patch contract, shared in
//! [`super::field_patch`]): a payload is a JSON object whose keys are the implemented parameter
//! names, a missing key is neutral, and the canonical neutral payload is the empty object `{}`. The
//! module owns exactly one layer of `lightwell.presence.adjust`, a `spatial` effect, so the host
//! places it after the pointwise colour run and before the geometry tail
//! (`docs/design/presence-mixer-vignette.md`, "Placement and stage order").
//!
//! The three units' equations, radii, halos and tolerance are frozen in
//! `docs/design/presence-study.md` and implemented in [`dehaze`], [`texture`] and [`clarity`]; this
//! file owns only the field table and the frozen order the compiled operation runs in: **dehaze,
//! then texture, then clarity**. A unit whose amount is 0 is the exact identity, so it is omitted
//! from the operation entirely, which also saves its halo and its work; an all-neutral payload
//! compiles to no units at all, which the host drops, so the layer opens no stage boundary and the
//! render keeps the identity byte path and the shared source buffer.

mod clarity;
mod dehaze;
mod filters;
mod texture;

#[cfg(test)]
mod oracle;

use super::{
    EffectDescriptor, EffectStage, Processing, SpatialOperation, SpatialUnit, Stage,
    field_patch::{ActionText, Field, FieldPatch, FieldPatchModule, Group, Spec, Values},
};
use crate::{EFFECT_FORMAT, Error};
use std::sync::Arc;

/// The one spatial-stage effect of the Presence module: Texture, Clarity and Dehaze of a stack live
/// in one layer of this effect, evaluated after the pointwise colour run and before the geometry
/// tail as one tiled neighbourhood pass.
pub const PRESENCE_EFFECT: &str = "lightwell.presence.adjust";

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

/// One Presence field: -100..100, step 1, no display decimals, no unit, a zero hint, on a plain
/// rail, because none of the three has a colour a gradient could show.
fn presence_field(name: &'static str, label: &str, notes: &str) -> Field {
    Field {
        zero: Some(NEUTRAL),
        ..Field::slider(name, label, notes)
    }
}

/// The Presence module's table and compilation.
#[derive(Debug, Default)]
pub struct Presence;

/// The Presence module: `Presence` as a field-patch module.
pub type PresenceModule = FieldPatchModule<Presence>;

impl FieldPatch for Presence {
    fn spec() -> Spec {
        Spec {
            id: "lightwell.presence",
            title: "Presence",
            hint: "Texture, clarity and dehaze",
            noun: "presence",
            effect: EffectDescriptor {
                id: PRESENCE_EFFECT.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Spatial,
                order: 0,
                maskable: true,
                artifacts: false,
                single: true,
            },
            set: ActionText {
                id: SET_PRESENCE,
                title: "Set Presence",
                notes: "merges the named presence fields into the stack's one Presence layer, which the host places after the pointwise colour run and before the geometry tail on the first non-neutral value and updates in place afterwards; omitted fields keep their stored values and a patch that changes nothing is a reported no-op",
            },
            reset: ActionText {
                id: RESET_PRESENCE,
                title: "Reset Presence",
                notes: "returns the stack's one Presence layer to its neutral payload, keeping its identity and position; a no-op without one and when it is already neutral",
            },
            fields: vec![
                presence_field(
                    TEXTURE,
                    "Texture",
                    "scales a medium-frequency luminance band isolated between two edge-preserving \
                     smoothers; negative values attenuate it",
                ),
                presence_field(
                    CLARITY,
                    "Clarity",
                    "scales the residual of luminance against a broad edge-preserving base computed \
                     on a reduced grid; negative values soften it",
                ),
                presence_field(
                    DEHAZE,
                    "Dehaze",
                    "removes the estimated atmospheric veil by inverting I = t*J + (1 - t)*A; \
                     negative values add a veil through the same model",
                ),
            ],
            groups: vec![Group {
                label: GROUP,
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

    /// The payload as one spatial operation: dehaze, then texture, then clarity, the frozen order,
    /// with an amount-0 unit omitted because it is the exact identity.
    ///
    /// The stage decides every radius and therefore every halo, so each unit is built with the long
    /// side of the stage this layer is compiled against, which is the stage the host evaluates the
    /// operation at.
    fn compile(&self, values: &Values<'_>, stage: Stage) -> Result<Processing, Error> {
        // A neutral payload compiles to no units. The host drops an empty spatial operation
        // entirely, so the layer opens no stage boundary, the identity byte path is kept and the
        // render shares the source buffer.
        if values.all_default() {
            return Ok(Processing::Spatial(SpatialOperation::neutral()));
        }
        let (texture, clarity, dehaze) =
            (values.get(TEXTURE), values.get(CLARITY), values.get(DEHAZE));
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
    use crate::modules::{ActionInput, Control, ParameterKind, ResetAction, ToolModule};

    use serde_json::json;
    use serde_json::{Map, Value};

    const STAGE: Stage = Stage {
        width: 24,
        height: 24,
    };

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

    /// The words a history label and the recipe row use for each field, with its declared decimals,
    /// sign and unit. The rules that choose a label — a group reset, the module reset, a field count
    /// — are the shared field-patch rules the conformance suite proves for every module.
    #[test]
    fn each_field_is_named_by_its_own_history_words() {
        let module = PresenceModule::new();
        for (parameters, expected) in [
            (json!({"texture": 40.0}), "Texture +40"),
            (json!({"clarity": -20.0}), "Clarity -20"),
            (json!({"dehaze": 15.0}), "Dehaze +15"),
        ] {
            let label = module.label(&ActionInput {
                action_id: SET_PRESENCE.to_owned(),
                parameters: parameters.as_object().cloned().unwrap(),
            });
            assert_eq!(label.as_deref(), Some(expected), "{parameters}");
        }
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
