//! The presets module: applies a settings set, named field-patch actions and the fields each one
//! sets, as one history entry labelled `Preset: <name>`.
//!
//! It declares no effects, so it never owns a layer and writes nothing itself. Its one action plans
//! a [`ActionPlan::Compose`] of the other modules' field patches; the host runs each step through
//! the registry against the stack the steps before it produced and commits the result once. A
//! field the set does not name keeps its value, because each step is a patch.
//!
//! The request carries the settings rather than a library reference, so the entry, request
//! deduplication and a copied request each describe exactly what was applied, and the module needs
//! no access to the catalog.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, ModuleDescriptor,
    ModuleLayout, ParameterDescriptor, ParameterKind, Processing, Stage, StageContext, ToolModule,
    descriptor::{PRESET_ID, PRESET_NAME, PRESET_SETTINGS},
};
use crate::{Error, ErrorKind};
use serde_json::{Map, Value};

pub const APPLY_PRESET: &str = "apply-preset";

/// The longest preset name, in characters: the history label and the entry's provenance. The
/// library holds its names to the same bound, so every library preset can be applied by name.
pub const MAX_PRESET_NAME: usize = 128;

/// The longest library identity a request may carry, in characters.
const MAX_PRESET_ID_LENGTH: usize = 96;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// The module declares no effect, so any payload handed to it is addressed to something else.
fn no_effects(effect_id: &str) -> Error {
    validation(format!(
        "the presets module declares no effects, so it has no {effect_id} layer"
    ))
}

fn parameter(name: &str, kind: ParameterKind, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind,
        required,
        default: None,
        unit: None,
        step: None,
        precision: None,
        notes: notes.into(),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
    }
}

#[derive(Debug)]
pub struct PresetsModule {
    descriptor: ModuleDescriptor,
}

impl Default for PresetsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetsModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.presets".into(),
                title: "Presets".into(),
                hint: Some("Saved and imported settings".into()),
                effects: Vec::new(),
                actions: vec![ActionDescriptor {
                    id: APPLY_PRESET.into(),
                    title: "Apply preset".into(),
                    notes: "applies a settings set as one history entry labelled `Preset: \
                             <name>`. Each key of settings names a field-patch action and its \
                             value the fields to send it; the host runs the actions in key order, \
                             each against the stack the ones before it produced, exactly as it \
                             would run that action alone, and commits the result once. Fields the \
                             set does not name keep their values, and a set that changes nothing \
                             is a reported no-op. An unknown, non-patch or unavailable action, or \
                             a field its action refuses, refuses the whole preset and writes \
                             nothing."
                        .into(),
                    summary: None,
                    patch: false,
                    parameters: vec![
                        parameter(
                            PRESET_SETTINGS,
                            ParameterKind::Settings,
                            true,
                            "the settings set to apply: field-patch action identities, each \
                             with a non-empty object of that action's fields",
                        ),
                        parameter(
                            PRESET_NAME,
                            ParameterKind::String {
                                max_length: MAX_PRESET_NAME,
                            },
                            true,
                            "the preset's name, which labels the history entry; not empty",
                        ),
                        parameter(
                            PRESET_ID,
                            ParameterKind::String {
                                max_length: MAX_PRESET_ID_LENGTH,
                            },
                            false,
                            "the library preset the settings came from; provenance only, never \
                             looked up",
                        ),
                    ],
                }],
                queries: Vec::new(),
                controls: vec![Control::Presets {
                    action: APPLY_PRESET.into(),
                }],
                reset: None,
                canvas: None,
                developer: false,
                // Collapsed, so Basic still leads the tools panel.
                collapsed: true,
                layout: ModuleLayout::Stacked,
                availability: Availability::Available,
            },
        }
    }
}

impl ToolModule for PresetsModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    /// The host has checked every field's kind; a name that is only whitespace would label an
    /// entry with nothing, so it is refused here. The request is stored exactly as sent.
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != APPLY_PRESET {
            return Err(validation(format!("unknown action {action_id}")));
        }
        let name = parameters
            .get(PRESET_NAME)
            .and_then(Value::as_str)
            .ok_or_else(|| validation("preset name must be a string"))?;
        if name.trim().is_empty() {
            return Err(validation("preset name must not be empty"));
        }
        Ok(ActionInput {
            action_id: action_id.to_owned(),
            parameters: parameters.clone(),
        })
    }

    /// One step per settings key, in key order, so a set always applies the same way. Basic,
    /// Presence, the mixer and the vignette each update their own module's one layer, which the
    /// host places by a stage and order none of the others shares, so for them the result does not
    /// depend on the order of the steps. Planning reads the request only: the host plans each step.
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let settings = input
            .parameters
            .get(PRESET_SETTINGS)
            .and_then(Value::as_object)
            .ok_or_else(|| validation("preset settings must be an object"))?;
        if settings.is_empty() {
            return Err(validation("preset settings name no action"));
        }
        settings
            .iter()
            .map(|(action_id, fields)| {
                let fields = fields.as_object().ok_or_else(|| {
                    validation(format!(
                        "preset settings for {action_id} must be an object of fields"
                    ))
                })?;
                Ok(ActionInput {
                    action_id: action_id.clone(),
                    parameters: fields.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()
            .map(ActionPlan::Compose)
    }

    fn label(&self, input: &ActionInput) -> Option<String> {
        input
            .parameters
            .get(PRESET_NAME)
            .and_then(Value::as_str)
            .map(|name| format!("Preset: {name}"))
    }

    fn validate_payload(&self, effect_id: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(no_effects(effect_id))
    }

    fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
        Err(no_effects(effect_id))
    }

    fn compile(&self, effect_id: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        Err(no_effects(effect_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleRegistry, check_parameters};
    use serde_json::json;
    use std::sync::Arc;

    fn fields(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    /// The descriptor is exactly the design's: no effects, one non-patch action with three
    /// parameters, and one presets control, serialized the way a client discovers it.
    #[test]
    fn the_descriptor_declares_one_action_one_control_and_no_effects() {
        let module = PresetsModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert!(descriptor.effects.is_empty());
        assert!(descriptor.queries.is_empty());
        assert!(descriptor.collapsed);
        let encoded = serde_json::to_value(descriptor).expect("a serializable descriptor");
        assert_eq!(encoded["id"], json!("lightwell.presets"));
        assert_eq!(encoded["title"], json!("Presets"));
        assert_eq!(encoded["hint"], json!("Saved and imported settings"));
        assert_eq!(
            encoded["controls"],
            json!([{"kind": "presets", "action": "apply-preset"}])
        );
        let action = &encoded["actions"][0];
        assert_eq!(action["id"], json!("apply-preset"));
        assert_eq!(action["title"], json!("Apply preset"));
        assert_eq!(action["patch"], json!(false));
        assert_eq!(action["summary"], json!(null));
        let parameters = action["parameters"].as_array().expect("parameters");
        let shape = |parameter: &Value| {
            json!({
                "name": parameter["name"],
                "kind": parameter["kind"],
                "max_length": parameter.get("max_length").cloned().unwrap_or(Value::Null),
                "required": parameter["required"],
                "default": parameter["default"],
            })
        };
        assert_eq!(
            parameters.iter().map(shape).collect::<Vec<_>>(),
            vec![
                json!({"name": "settings", "kind": "settings", "max_length": null, "required": true, "default": null}),
                json!({"name": "name", "kind": "string", "max_length": 128, "required": true, "default": null}),
                json!({"name": "preset-id", "kind": "string", "max_length": 96, "required": false, "default": null}),
            ]
        );
        assert_eq!(
            &ModuleDescriptor::parse(&encoded).expect("the JSON form is valid"),
            descriptor,
            "the descriptor round-trips through JSON"
        );
    }

    /// A module with no effects registers like any other and claims no effect identity.
    #[test]
    fn a_module_without_effects_registers_first() {
        let registry = ModuleRegistry::builtin();
        assert_eq!(registry.descriptors()[0].id, "lightwell.presets");
        let (module, action) = registry.action(APPLY_PRESET).expect("the preset action");
        assert_eq!(module.descriptor().id, "lightwell.presets");
        assert!(!action.patch);
        let mut alone = ModuleRegistry::new();
        alone
            .register(Arc::new(PresetsModule::new()))
            .expect("a module without effects registers");
        assert_eq!(alone.descriptors().len(), 1);
    }

    #[test]
    fn parse_stores_the_request_as_sent_and_refuses_a_blank_name() {
        let module = PresetsModule::new();
        let action = &module.descriptor().actions[0];
        let sent = json!({
            "settings": {"set-basic": {"exposure": 0.35}},
            "name": "Soft film",
            "preset-id": "preset-1",
        });
        let checked = check_parameters(action, &sent).expect("a valid request");
        let parsed = module.parse(APPLY_PRESET, &checked).expect("parsed");
        assert_eq!(parsed.action_id, APPLY_PRESET);
        assert_eq!(parsed.parameters, fields(sent));
        assert_eq!(module.label(&parsed).as_deref(), Some("Preset: Soft film"));
        // The library identity is optional and nothing fills it in.
        let without = json!({"settings": {"set-basic": {"exposure": 0.35}}, "name": "Soft"});
        let checked = check_parameters(action, &without).expect("no preset-id");
        assert_eq!(
            module.parse(APPLY_PRESET, &checked).unwrap().parameters,
            fields(without)
        );
        for name in ["", "   ", "\u{3000}"] {
            let checked = check_parameters(
                action,
                &json!({"settings": {"set-basic": {"exposure": 1}}, "name": name}),
            )
            .expect("the generic check accepts an empty string");
            let error = module
                .parse(APPLY_PRESET, &checked)
                .expect_err("a blank name");
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(error.detail, "preset name must not be empty", "{name:?}");
        }
        for (case, request, fragment) in [
            (
                "missing settings",
                json!({"name": "Soft"}),
                "missing required parameter settings",
            ),
            (
                "missing name",
                json!({"settings": {"set-basic": {"exposure": 1}}}),
                "missing required parameter name",
            ),
            (
                "a control character in the name",
                json!({"settings": {"set-basic": {"exposure": 1}}, "name": "Soft\nfilm"}),
                "parameter name must not contain control characters",
            ),
            (
                "an empty settings set",
                json!({"settings": {}, "name": "Soft"}),
                "parameter settings must name 1..=16 actions",
            ),
            (
                "an unknown parameter",
                json!({"settings": {"set-basic": {"exposure": 1}}, "name": "Soft", "amount": 1}),
                "unknown parameter amount",
            ),
        ] {
            let error = check_parameters(action, &request).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    #[test]
    fn plan_composes_one_step_per_settings_key_in_key_order() {
        let module = PresetsModule::new();
        let input = ActionInput {
            action_id: APPLY_PRESET.into(),
            parameters: fields(json!({
                "settings": {
                    "set-vignette": {"amount": -18},
                    "set-basic": {"exposure": 0.35, "contrast": 12},
                    "set-mixer": {"blue-saturation": -20},
                },
                "name": "Soft film",
            })),
        };
        let unused = |_: u32, _: u32| -> Result<Option<[u8; 4]>, Error> { Ok(None) };
        let stage_before = |_: usize| -> Result<Stage, Error> {
            Ok(Stage {
                width: 1,
                height: 1,
            })
        };
        let insertion_index = |_: crate::EffectStage| 0;
        let insertion_index_for = |_: &str| 0;
        let sample_before =
            |_: usize, _: u32, _: u32| -> Result<Option<[u8; 4]>, Error> { Ok(None) };
        let context = StageContext {
            stage: Stage {
                width: 1,
                height: 1,
            },
            layers: &[],
            sampler: &unused,
            stage_before: &stage_before,
            insertion_index: &insertion_index,
            insertion_index_for: &insertion_index_for,
            sample_before: &sample_before,
            sensor_neutral: None,
        };
        let step = |action_id: &str, parameters: Value| ActionInput {
            action_id: action_id.into(),
            parameters: fields(parameters),
        };
        assert_eq!(
            module.plan(&input, &context).expect("a composite"),
            ActionPlan::Compose(vec![
                step("set-basic", json!({"exposure": 0.35, "contrast": 12})),
                step("set-mixer", json!({"blue-saturation": -20})),
                step("set-vignette", json!({"amount": -18})),
            ])
        );
        let empty = ActionInput {
            action_id: APPLY_PRESET.into(),
            parameters: fields(json!({"settings": {}, "name": "Nothing"})),
        };
        assert_eq!(
            module.plan(&empty, &context).unwrap_err().detail,
            "preset settings name no action"
        );
    }

    #[test]
    fn every_payload_path_refuses_because_the_module_owns_no_layer() {
        let module = PresetsModule::new();
        let stage = Stage {
            width: 1,
            height: 1,
        };
        for error in [
            module
                .validate_payload("lightwell.presets.any", 1, &json!({}))
                .unwrap_err(),
            module
                .describe_layer("lightwell.presets.any", 1, &json!({}))
                .unwrap_err(),
            module
                .compile("lightwell.presets.any", 1, &json!({}), stage)
                .unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(
                error.detail,
                "the presets module declares no effects, so it has no lightwell.presets.any layer"
            );
        }
    }
}
