//! Settings sets — a named preset or another photograph's copied settings — composed as one history
//! entry through the same planner.
//!
//! It declares no effects, so it never owns a layer and writes nothing itself. Its one action,
//! `apply-settings`, plans a [`ActionPlan::Compose`] of the other modules' settings; the host runs
//! patches first, then per-photo analysis steps, against the intermediate stack and commits once.
//! Every field those steps do not write keeps its value.
//!
//! The request carries the settings rather than a library reference, so the entry, request
//! deduplication and a copied request each describe exactly what was applied, and the module needs
//! no access to the catalog. Where the set came from is a [`SettingsOrigin`], which only labels the
//! entry.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, LayerReport,
    ModuleDescriptor, ModuleLayout, ParameterDescriptor, Processing, StageContext, ToolModule,
    descriptor::{PRESET_SETTINGS, SETTINGS_ORIGIN},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{AssetId, Error, PresetId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(crate) const APPLY_SETTINGS: &str = "apply-settings";

/// The longest preset name, in characters: the history label and the entry's provenance. The
/// library holds its names to the same bound, so every library preset can be applied by name.
pub(crate) const MAX_PRESET_NAME: usize = 128;

/// The longest source photograph name a paste carries, in characters: every desktop platform holds
/// a file name to at most 255 bytes or UTF-16 units, so to at most 255 characters.
pub const MAX_SOURCE_NAME: usize = 255;

/// The most of a source name a history label shows, in characters; a longer one keeps its start and
/// its end, extension included, around an ellipsis.
const LABEL_SOURCE: usize = 64;

/// Where a settings set came from. It labels the history entry (`Preset: <name>`, `Paste settings
/// from <source>`) and a batch job's detail; the identities it carries are provenance only, never
/// looked up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SettingsOrigin {
    /// A preset, by its name and, when it came from the library, its identity.
    Preset {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset_id: Option<PresetId>,
    },
    /// Settings copied from another photograph, by its file name and, optionally, its identity.
    Paste {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_asset: Option<AssetId>,
    },
}

impl SettingsOrigin {
    /// Read an origin as a request sends it: its shape, then a name or source that is not blank,
    /// holds no control character and fits its bound. Nothing is trimmed: the origin is stored
    /// exactly as sent.
    pub(crate) fn read(value: &Value) -> Result<Self, Error> {
        let origin = Self::deserialize(value).map_err(|error| {
            Error::validation(format!(
                "must be {{kind: preset, name, preset_id?}} or {{kind: paste, source, \
                 source_asset?}}: {error}"
            ))
        })?;
        origin.check()?;
        Ok(origin)
    }

    /// A name or source that is not blank, holds no control character and fits its bound.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let (field, text, bound) = match self {
            Self::Preset { name, .. } => ("name", name, MAX_PRESET_NAME),
            Self::Paste { source, .. } => ("source", source, MAX_SOURCE_NAME),
        };
        if text.trim().is_empty() {
            return Err(Error::validation(format!("{field} must not be empty")));
        }
        if text.chars().count() > bound {
            return Err(Error::validation(format!(
                "{field} must be at most {bound} characters"
            )));
        }
        if text.chars().any(char::is_control) {
            return Err(Error::validation(format!(
                "{field} must not contain control characters"
            )));
        }
        Ok(())
    }

    /// The preset's name or the source photograph's: what a batch report names the set by.
    pub fn name(&self) -> &str {
        match self {
            Self::Preset { name, .. } => name,
            Self::Paste { source, .. } => source,
        }
    }

    /// The history entry's label: `Preset: <name>`, or `Paste settings from <source>` with a
    /// source longer than 64 characters shortened in the middle.
    pub fn label(&self) -> String {
        match self {
            Self::Preset { name, .. } => format!("Preset: {name}"),
            Self::Paste { source, .. } => format!("Paste settings from {}", elide(source)),
        }
    }
}

/// `text` within [`LABEL_SOURCE`] characters: its start and end around an ellipsis, so a long
/// file name keeps its extension.
fn elide(text: &str) -> String {
    let count = text.chars().count();
    if count <= LABEL_SOURCE {
        return text.to_owned();
    }
    let tail = LABEL_SOURCE / 3;
    let head = LABEL_SOURCE - tail - 1;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}\u{2026}{end}")
}

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
                actions: vec![ActionDescriptor {
                    parameters: vec![
                        ParameterDescriptor::settings(PRESET_SETTINGS)
                            .required(true)
                            .notes(
                                "the settings set to apply, inline (a library preset's settings \
                                 are read with preset.read or preset.list): field-patch action \
                                 identities with non-empty field objects, or declared analysis \
                                 actions with empty objects, such as {\"auto-tone\": {}}, which \
                                 run for this photo after the patches; an analysis step and the \
                                 fields it overwrites cannot both be included",
                            ),
                        ParameterDescriptor::settings_origin(SETTINGS_ORIGIN)
                            .required(true)
                            .notes(
                                "where the set came from, which labels the history entry: \
                                 {kind: preset, name, preset_id?} or {kind: paste, source, \
                                 source_asset?}; name is 1..=128 characters and source, the \
                                 source photograph's file name, 1..=255, neither blank nor \
                                 holding a control character; preset_id and source_asset are \
                                 provenance only, never looked up",
                            ),
                    ],
                    ..ActionDescriptor::new(
                        APPLY_SETTINGS,
                        "Apply settings",
                        "applies a settings set as one history entry labelled by its origin: \
                         `Preset: <name>`, or `Paste settings from <source>` with a source over \
                         64 characters shortened in the middle. Each key of settings names a \
                         field-patch action and its fields, or a declared analysis action and an \
                         empty object. The host runs patches in key order, then analysis steps \
                         such as Auto tone separately for this photo, against the intermediate \
                         stack, as it would run that action alone, and commits the result once. \
                         Fields the set does not name keep their values, masked layers are \
                         unchanged, and a set that changes nothing is a reported no-op. A step \
                         whose module does not apply to the photo's kind, and a field another \
                         control supersedes on the photo's global target, are skipped and listed \
                         under skipped in the result. Auto without usable tonal range is also \
                         skipped. An unknown, non-presettable or unavailable action, an \
                         overlapping analysis/field set or another refused field refuses the \
                         whole set and writes nothing.",
                    )
                }],
                queries: Vec::new(),
                controls: vec![Control::presets(APPLY_SETTINGS).into()],
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

    /// The host's generic check has read the origin whole ([`SettingsOrigin::read`]), refusing a
    /// blank, over-long or control-character name or source, so there is nothing left to refuse
    /// here. The request is stored exactly as sent, untrimmed.
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != APPLY_SETTINGS {
            return Err(Error::validation(format!("unknown action {action_id}")));
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

    /// The origin's label ([`SettingsOrigin::label`]).
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        input
            .parameters
            .get(SETTINGS_ORIGIN)
            .and_then(|origin| SettingsOrigin::read(origin).ok())
            .map_or_else(|| action.title.clone(), |origin| origin.label())
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

    /// The one action declares a settings set and its origin, and one presets control submits it,
    /// serialized the way a client discovers it.
    #[test]
    fn the_descriptor_declares_apply_settings_with_one_control_and_no_effects() {
        let module = PresetsModule::new();
        let descriptor = module.descriptor();
        descriptor.validate().expect("a valid descriptor");
        assert!(descriptor.effects.is_empty());
        assert!(descriptor.queries.is_empty());
        assert!(descriptor.collapsed);
        assert_eq!(descriptor.actions.len(), 1);
        let encoded = serde_json::to_value(descriptor).expect("a serializable descriptor");
        assert_eq!(encoded["id"], json!("luxforge.presets"));
        assert_eq!(encoded["title"], json!("Presets"));
        assert_eq!(encoded["hint"], json!("Saved and imported settings"));
        assert_eq!(
            encoded["controls"],
            json!([{"kind": "presets", "action": "apply-settings"}])
        );
        let action = &encoded["actions"][0];
        assert_eq!(action["id"], json!("apply-settings"));
        assert_eq!(action["title"], json!("Apply settings"));
        assert_eq!(action["patch"], json!(false));
        let parameters = action["parameters"].as_array().expect("parameters");
        let shape = |parameter: &Value| {
            json!({
                "name": parameter["name"],
                "kind": parameter["kind"],
                "required": parameter["required"],
                "default": parameter["default"],
            })
        };
        assert_eq!(
            parameters.iter().map(shape).collect::<Vec<_>>(),
            vec![
                json!({"name": "settings", "kind": "settings", "required": true, "default": null}),
                json!({"name": "origin", "kind": "settings-origin", "required": true, "default": null}),
            ]
        );
        assert!(
            parameters[0]["notes"]
                .as_str()
                .unwrap()
                .contains("{\"auto-tone\": {}}"),
            "the settings notes name an analysis step"
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
        let (module, action) = registry
            .action(APPLY_SETTINGS)
            .expect("the settings action");
        assert_eq!(module.descriptor().id, "luxforge.presets");
        assert!(!action.patch);
        let mut alone = ModuleRegistry::new();
        alone
            .register(Arc::new(PresetsModule::new()))
            .expect("a module without effects registers");
        assert_eq!(alone.descriptors().len(), 1);
    }

    /// Both origins are stored exactly as sent and label the entry by their kind; the identities
    /// they carry are typed, and a name or source is bounded, not blank and free of control
    /// characters.
    #[test]
    fn the_origin_labels_the_entry_and_is_stored_as_sent() {
        let module = PresetsModule::new();
        let action = &module.descriptor().actions[0];
        let settings = json!({"set-basic": {"exposure": 0.35}});
        for (origin, label) in [
            (
                json!({"kind": "preset", "name": "Soft film", "preset_id": "preset-000000001"}),
                "Preset: Soft film",
            ),
            (
                json!({"kind": "preset", "name": " Soft "}),
                "Preset:  Soft ",
            ),
            (
                json!({"kind": "paste", "source": "  DSC_4471.NEF", "source_asset": "asset-000000001"}),
                "Paste settings from   DSC_4471.NEF",
            ),
            (
                json!({"kind": "paste", "source": "DSC_4471.NEF"}),
                "Paste settings from DSC_4471.NEF",
            ),
        ] {
            let sent = json!({"settings": settings, "origin": origin});
            let checked = check_parameters(action, &sent).expect("a valid request");
            let parsed = module.parse(APPLY_SETTINGS, &checked).expect("parsed");
            assert_eq!(parsed.action_id, APPLY_SETTINGS);
            assert_eq!(parsed.parameters, fields(sent), "stored as sent");
            assert_eq!(module.label(action, &parsed), label);
        }
        for (case, origin, fragment) in [
            (
                "a blank name",
                json!({"kind": "preset", "name": "\u{3000}"}),
                "parameter origin is not a usable origin: name must not be empty",
            ),
            (
                "a blank source",
                json!({"kind": "paste", "source": "  "}),
                "parameter origin is not a usable origin: source must not be empty",
            ),
            (
                "a name over 128 characters",
                json!({"kind": "preset", "name": "n".repeat(129)}),
                "name must be at most 128 characters",
            ),
            (
                "a source over 255 characters",
                json!({"kind": "paste", "source": "s".repeat(256)}),
                "source must be at most 255 characters",
            ),
            (
                "a control character",
                json!({"kind": "preset", "name": "Soft\nfilm"}),
                "name must not contain control characters",
            ),
            (
                "a preset identity that is not one",
                json!({"kind": "preset", "name": "Soft", "preset_id": "soft"}),
                "invalid PresetId",
            ),
            (
                "an asset identity that is not one",
                json!({"kind": "paste", "source": "a.jpg", "source_asset": "a"}),
                "invalid AssetId",
            ),
            (
                "a field of the other kind",
                json!({"kind": "paste", "source": "a.jpg", "preset_id": "preset-000000001"}),
                "unknown field",
            ),
            (
                "an unknown kind",
                json!({"kind": "library", "name": "Soft"}),
                "unknown variant",
            ),
        ] {
            let error = check_parameters(action, &json!({"settings": settings, "origin": origin}))
                .expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        for (case, request, fragment) in [
            (
                "missing settings",
                json!({"origin": {"kind": "preset", "name": "Soft"}}),
                "missing required parameter settings",
            ),
            (
                "missing origin",
                json!({"settings": settings}),
                "missing required parameter origin",
            ),
            (
                "an empty settings set",
                json!({"settings": {}, "origin": {"kind": "preset", "name": "Soft"}}),
                "parameter settings must name 1..=16 actions",
            ),
            (
                "an unknown parameter",
                json!({"settings": settings, "origin": {"kind": "preset", "name": "Soft"}, "name": "Soft"}),
                "unknown parameter name",
            ),
        ] {
            let error = check_parameters(action, &request).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    /// Any file name a platform allows pastes, up to 255 characters, and the label keeps a long
    /// one's start and extension.
    #[test]
    fn a_long_source_file_name_pastes_with_a_shortened_label() {
        let module = PresetsModule::new();
        let action = &module.descriptor().actions[0];
        let source = format!("{}.NEF", "a".repeat(251));
        assert_eq!(source.chars().count(), MAX_SOURCE_NAME);
        let sent = json!({
            "settings": {"set-basic": {"exposure": 1.0}},
            "origin": {"kind": "paste", "source": source},
        });
        let checked = check_parameters(action, &sent).expect("a 255-character file name");
        let parsed = module.parse(APPLY_SETTINGS, &checked).unwrap();
        assert_eq!(parsed.parameters["origin"]["source"], json!(source));
        let label = module.label(action, &parsed);
        let shown = label.strip_prefix("Paste settings from ").unwrap();
        assert_eq!(shown.chars().count(), LABEL_SOURCE);
        assert!(
            shown.starts_with("aaaa") && shown.ends_with("aaa.NEF"),
            "{label}"
        );
        assert!(shown.contains('\u{2026}'), "{label}");
        // A short name is shown whole.
        assert_eq!(elide("DSC_4471.NEF"), "DSC_4471.NEF");
        assert_eq!(elide(&"b".repeat(LABEL_SOURCE)), "b".repeat(LABEL_SOURCE));
    }

    #[test]
    fn plan_composes_one_step_per_settings_key_in_key_order() {
        let module = PresetsModule::new();
        let input = ActionInput {
            action_id: APPLY_SETTINGS.into(),
            parameters: fields(json!({
                "settings": {
                    "set-vignette": {"amount": -18},
                    "set-basic": {"exposure": 0.35, "contrast": 12},
                    "set-mixer": {"blue-saturation": -20},
                },
                "origin": {"kind": "preset", "name": "Soft film"},
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
            action_id: APPLY_SETTINGS.into(),
            parameters: fields(
                json!({"settings": {}, "origin": {"kind": "preset", "name": "Nothing"}}),
            ),
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
