//! Named presets and copied settings, composed as one history entry through the same planner.
//!
//! It declares no effects, so it never owns a layer and writes nothing itself. Each action plans
//! a [`ActionPlan::Compose`] of the other modules' settings; the host runs patches first, then
//! per-photo analysis steps, against the intermediate stack and commits once. Every field those
//! steps do not write keeps its value.
//!
//! The request carries the settings rather than a library reference, so the entry, request
//! deduplication and a copied request each describe exactly what was applied, and the module needs
//! no access to the catalog.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, LayerReport,
    ModuleDescriptor, ModuleLayout, ParameterDescriptor, Processing, StageContext, ToolModule,
    descriptor::{PRESET_ID, PRESET_NAME, PRESET_SETTINGS},
};
use crate::Error;
#[cfg(test)]
use crate::ErrorKind;
use serde_json::{Map, Value};

pub(crate) const APPLY_PRESET: &str = "apply-preset";
pub(crate) const PASTE_SETTINGS: &str = "paste-settings";

/// The longest preset name, in characters: the history label and the entry's provenance. The
/// library holds its names to the same bound, so every library preset can be applied by name.
pub(crate) const MAX_PRESET_NAME: usize = 128;

/// The longest library identity a request may carry, in characters.
const MAX_PRESET_ID_LENGTH: usize = 96;

/// The module declares no effect, so any payload handed to it is addressed to something else.
fn no_effects(effect_id: &str) -> Error {
    Error::validation(format!(
        "the presets module declares no effects, so it has no {effect_id} layer"
    ))
}

#[derive(Debug)]
pub(crate) struct PresetsModule {
    descriptor: ModuleDescriptor,
}

impl Default for PresetsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetsModule {
    pub(crate) fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "luxforge.presets".into(),
                title: "Presets".into(),
                hint: Some("Saved and imported settings".into()),
                effects: Vec::new(),
                actions: vec![
                    ActionDescriptor {
                        parameters: vec![
                            ParameterDescriptor::settings(PRESET_SETTINGS)
                                .required(true)
                                .notes(
                                    "the settings set to apply: field-patch action identities with \
                                 non-empty field objects, or declared analysis actions with empty \
                                 objects (auto-tone: {}); an analysis step and the fields it \
                                 overwrites cannot both be included",
                                ),
                            ParameterDescriptor::string(PRESET_NAME, MAX_PRESET_NAME)
                                .required(true)
                                .notes(
                                    "the preset's name, which labels the history entry; not empty",
                                ),
                            ParameterDescriptor::string(PRESET_ID, MAX_PRESET_ID_LENGTH).notes(
                                "the library preset the settings came from; provenance only, never \
                             looked up",
                            ),
                        ],
                        ..ActionDescriptor::new(
                            APPLY_PRESET,
                            "Apply preset",
                            "applies a settings set as one history entry labelled `Preset: \
                             <name>`. Each key of settings names a field-patch action and its \
                             fields, or a declared analysis action and an empty object. The host \
                             runs patches in key order, then analysis steps such as Auto tone \
                             separately for this photo, against the intermediate stack, as it \
                             would run that action alone, and commits the result once. Fields the \
                             set does not name keep their values, and a set that changes nothing \
                             is a reported no-op. A step whose module does not apply to the \
                             photo's kind, and a field another control supersedes on the photo's \
                             global target, are skipped and listed under skipped in the result. \
                             Auto without usable tonal range is also skipped. An unknown, \
                             non-presettable or unavailable action, an overlapping analysis/field \
                             set or another refused field refuses the whole preset and writes nothing.",
                        )
                    },
                    ActionDescriptor {
                        parameters: vec![
                            ParameterDescriptor::settings(PRESET_SETTINGS).required(true),
                            ParameterDescriptor::string("source", MAX_PRESET_NAME)
                                .required(true)
                                .notes("the source photograph's display name; not empty"),
                            ParameterDescriptor::string("source-asset", MAX_PRESET_ID_LENGTH)
                                .notes("source asset provenance only; never looked up"),
                        ],
                        ..ActionDescriptor::new(
                            PASTE_SETTINGS,
                            "Paste settings",
                            "applies captured settings as one entry labelled Paste settings from <source>; uses the same field patches, skips, refusals and no-op behavior as apply-preset; fields not named keep their values and masked layers are unchanged",
                        )
                    },
                ],
                queries: Vec::new(),
                controls: vec![Control::presets(APPLY_PRESET).into()],
                reset: None,
                canvas: None,
                developer: false,
                // Collapsed, so Basic still leads the tools panel.
                collapsed: true,
                layout: ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
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
        let field = match action_id {
            APPLY_PRESET => PRESET_NAME,
            PASTE_SETTINGS => "source",
            _ => return Err(Error::validation(format!("unknown action {action_id}"))),
        };
        let name = parameters
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::validation(format!("preset {field} must be a string")))?;
        if name.trim().is_empty() {
            return Err(Error::validation(format!(
                "preset {field} must not be empty"
            )));
        }
        let mut parameters = parameters.clone();
        if action_id == PASTE_SETTINGS {
            parameters.insert(field.into(), Value::String(name.trim().to_owned()));
        }
        Ok(ActionInput {
            action_id: action_id.to_owned(),
            parameters,
        })
    }

    /// One step per settings key, in key order, so a set always applies the same way. Basic,
    /// Presence, the mixer and the vignette each update their own module's one layer, which the
    /// host places by a stage and order none of the others shares, so for them the result does not
    /// depend on the order of the steps. Planning reads the request only: the host plans each step.
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let settings = input
            .parameters
            .get(PRESET_SETTINGS)
            .and_then(Value::as_object)
            .ok_or_else(|| Error::validation("preset settings must be an object"))?;
        if settings.is_empty() {
            return Err(Error::validation("preset settings name no action"));
        }
        crate::presets::validate_composite(context.registry, settings)?;
        settings
            .iter()
            .map(|(action_id, fields)| {
                let fields = fields.as_object().ok_or_else(|| {
                    Error::validation(format!(
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

    /// `Preset: <name>`.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        if input.action_id == PASTE_SETTINGS {
            return input
                .parameters
                .get("source")
                .and_then(Value::as_str)
                .map_or_else(
                    || action.title.clone(),
                    |source| format!("Paste settings from {source}"),
                );
        }
        match input.parameters.get(PRESET_NAME).and_then(Value::as_str) {
            Some(name) => format!("Preset: {name}"),
            None => action.title.clone(),
        }
    }

    fn validate_payload(&self, effect_id: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(no_effects(effect_id))
    }

    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Err(no_effects(effect_id))
    }

    fn compile(
        &self,
        effect_id: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(no_effects(effect_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stage;
    use crate::{ModuleRegistry, check_parameters};
    use serde_json::json;
    use std::sync::Arc;

    fn fields(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    /// The preset action declares three
    /// parameters, and one presets control, serialized the way a client discovers it.
    #[test]
    fn the_descriptor_declares_presets_and_paste_with_one_control_and_no_effects() {
        let module = PresetsModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert!(descriptor.effects.is_empty());
        assert!(descriptor.queries.is_empty());
        assert!(descriptor.collapsed);
        let encoded = serde_json::to_value(descriptor).expect("a serializable descriptor");
        assert_eq!(encoded["id"], json!("luxforge.presets"));
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
            &serde_json::from_value::<ModuleDescriptor>(encoded).expect("the JSON form reads"),
            descriptor,
            "the descriptor round-trips through JSON"
        );
    }

    /// A module with no effects registers like any other and claims no effect identity.
    #[test]
    fn a_module_without_effects_registers_after_crop() {
        let registry = ModuleRegistry::builtin();
        assert_eq!(registry.descriptors()[1].id, "luxforge.presets");
        let (module, action) = registry.action(APPLY_PRESET).expect("the preset action");
        assert_eq!(module.descriptor().id, "luxforge.presets");
        assert!(!action.patch);
        let mut alone = ModuleRegistry::new();
        alone
            .register(Arc::new(PresetsModule::new()))
            .expect("a module without effects registers");
        assert_eq!(alone.descriptors().len(), 1);
    }

    #[test]
    fn paste_settings_declares_bounded_provenance_and_trims_the_source() {
        let module = PresetsModule::new();
        let descriptor = module.descriptor();
        assert_eq!(descriptor.actions.len(), 2);
        let action = &descriptor.actions[1];
        assert_eq!(action.id, PASTE_SETTINGS);
        assert!(!action.patch);
        let checked = check_parameters(
            action,
            &json!({
                "settings": {"set-basic": {"exposure": 1.0}},
                "source": "  DSC_4471.NEF  ", "source-asset": "provenance-only"
            }),
        )
        .unwrap();
        let parsed = module.parse(PASTE_SETTINGS, &checked).unwrap();
        assert_eq!(parsed.parameters["source"], "DSC_4471.NEF");
        assert_eq!(parsed.parameters["source-asset"], "provenance-only");
        assert_eq!(
            module.label(action, &parsed),
            "Paste settings from DSC_4471.NEF"
        );
        for source in ["", "   "] {
            let checked = check_parameters(
                action,
                &json!({
                    "settings": {"set-basic": {"exposure": 1.0}}, "source": source
                }),
            )
            .unwrap();
            assert!(module.parse(PASTE_SETTINGS, &checked).is_err());
        }
        for request in [
            json!({"settings": {"set-basic": {"exposure": 1.0}}, "source": "x".repeat(129)}),
            json!({"settings": {"set-basic": {"exposure": 1.0}}, "source": "photo", "source-asset": "x".repeat(97)}),
        ] {
            assert!(check_parameters(action, &request).is_err());
        }
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
        assert_eq!(module.label(action, &parsed), "Preset: Soft film");
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
        let stage = crate::modules::FixedStage::new(Stage {
            width: 1,
            height: 1,
        });
        let registry = crate::ModuleRegistry::builtin();
        let context = stage.context(&[], &registry);
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
                .validate_payload("luxforge.presets.any", 1, &json!({}))
                .unwrap_err(),
            module
                .describe("luxforge.presets.any", 1, &json!({}))
                .unwrap_err(),
            module
                .compile(
                    "luxforge.presets.any",
                    1,
                    &json!({}),
                    crate::CompileStage::exact(stage),
                )
                .unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(
                error.detail,
                "the presets module declares no effects, so it has no luxforge.presets.any layer"
            );
        }
    }
}
