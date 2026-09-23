//! A module declaring every kind of capability, shared by the descriptor, settings and host tests.
use super::{
    descriptor::{
        ActivationDescriptor, AdapterAuth, AdapterCost, AdapterDescriptor, CapabilityDescriptor,
        CapabilityKind, DataClass, ProfilesDescriptor, ResourceDescriptor, SettingDescriptor,
        SettingKind, SettingsDescriptor, TaskApply, TaskDescriptor,
    },
    files::FileMode,
    transport::EndpointClass,
};
use crate::{
    ActionDescriptor, Control, EffectDescriptor, EffectStage, ModuleDescriptor,
    ParameterDescriptor, ParameterKind,
};
use serde_json::{Value, json};

pub(crate) const MODULE: &str = "test.capabilities";
pub(crate) const TASK: &str = "generate-test-tint";
pub(crate) const ADAPTER: &str = "echo-adapter";

pub(crate) fn setting(id: &str, kind: SettingKind, default: Option<Value>) -> SettingDescriptor {
    SettingDescriptor {
        id: id.into(),
        label: id.replace('-', " "),
        help: None,
        kind,
        required: false,
        default,
        invalidates_activation: false,
    }
}

fn parameter(name: &str, kind: ParameterKind, required: bool) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind,
        required,
        default: None,
        unit: None,
        step: None,
        precision: None,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
        notes: "test".into(),
    }
}

pub(crate) fn adapter() -> AdapterDescriptor {
    AdapterDescriptor {
        id: ADAPTER.into(),
        title: "Echo".into(),
        auth: AdapterAuth::Bearer,
        data: vec![DataClass::SampleGrid8],
        max_request_bytes: 4096,
        max_response_bytes: 65536,
        timeout_ms: 5000,
        retention: Some("The echo service keeps nothing.".into()),
        cost: AdapterCost::Free,
    }
}

/// Every setting kind at module level, a bearer adapter whose profiles hold an endpoint, a secret
/// and a choice, the three implemented capabilities, one resource, an activation that needs the
/// file and the resource, and one task that uses all three capabilities and applies its artifact.
pub(crate) fn capability_descriptor() -> ModuleDescriptor {
    let required = |setting: SettingDescriptor| SettingDescriptor {
        required: true,
        ..setting
    };
    ModuleDescriptor {
        id: MODULE.into(),
        title: "Capabilities test".into(),
        effects: vec![EffectDescriptor {
            id: "test.capabilities.tint".into(),
            format: 1,
            stage: EffectStage::Color,
            order: 0,
            artifacts: true,
        }],
        actions: vec![
            ActionDescriptor {
                id: "apply-test-tint".into(),
                title: "Apply tint".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: vec![parameter("tint", ParameterKind::Artifact, true)],
            },
            ActionDescriptor {
                id: "reset-test-tint".into(),
                title: "Reset tint".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: Vec::new(),
            },
        ],
        controls: vec![Control::Task {
            task: TASK.into(),
            label: "Generate tint".into(),
        }],
        settings: Some(SettingsDescriptor {
            schema: 1,
            fields: vec![
                setting(
                    "strength",
                    SettingKind::Number {
                        min: 0.0,
                        max: 1.0,
                        step: Some(0.01),
                        precision: Some(2),
                    },
                    Some(json!(0.5)),
                ),
                setting(
                    "mode",
                    SettingKind::Enum {
                        options: vec!["fast".into(), "exact".into()],
                    },
                    Some(json!("exact")),
                ),
                setting(
                    "count",
                    SettingKind::Integer { min: 1, max: 8 },
                    Some(json!(2)),
                ),
                setting("enabled", SettingKind::Boolean, Some(json!(true))),
                setting("note", SettingKind::Text { max_length: 16 }, None),
                SettingDescriptor {
                    invalidates_activation: true,
                    ..required(setting(
                        "input-file",
                        SettingKind::File {
                            mode: FileMode::File,
                        },
                        None,
                    ))
                },
                setting(
                    "local-service",
                    SettingKind::Endpoint {
                        classes: vec![EndpointClass::Loopback],
                    },
                    None,
                ),
                setting("token", SettingKind::Secret { max_length: 64 }, None),
            ],
            profiles: Some(ProfilesDescriptor {
                label: "Providers".into(),
                max: 2,
                adapters: vec![adapter()],
                fields: vec![
                    SettingDescriptor {
                        invalidates_activation: true,
                        ..required(setting(
                            "endpoint",
                            SettingKind::Endpoint {
                                classes: vec![EndpointClass::Remote, EndpointClass::Loopback],
                            },
                            None,
                        ))
                    },
                    required(setting(
                        "api-key",
                        SettingKind::Secret { max_length: 128 },
                        None,
                    )),
                    setting(
                        "model",
                        SettingKind::Enum {
                            options: vec!["small".into(), "large".into()],
                        },
                        Some(json!("small")),
                    ),
                ],
            }),
        }),
        capabilities: vec![
            CapabilityDescriptor {
                id: "input".into(),
                kind: CapabilityKind::ReadUserFile {
                    setting: "input-file".into(),
                },
                purpose: "Read the tint table you chose.".into(),
            },
            CapabilityDescriptor {
                id: "echo".into(),
                kind: CapabilityKind::RemoteImageRequest {
                    adapter: ADAPTER.into(),
                    data: DataClass::SampleGrid8,
                },
                purpose: "Ask the echo service for a tint.".into(),
            },
            CapabilityDescriptor {
                id: "palette".into(),
                kind: CapabilityKind::DownloadArtifact {
                    resource: "palette".into(),
                },
                purpose: "Install the tint palette.".into(),
            },
        ],
        resources: vec![ResourceDescriptor {
            id: "palette".into(),
            title: "Tint palette".into(),
            version: "1.0.0".into(),
            url: "https://example.com/palette.bin".into(),
            bytes: 12,
            sha256: "a".repeat(64),
            format: "rgb-gains".into(),
            license: "CC0-1.0".into(),
            provenance: "Generated for tests".into(),
            redirect_origins: vec!["https://cdn.example.com".into()],
        }],
        activation: Some(ActivationDescriptor {
            requires_settings: vec!["input-file".into()],
            requires_resources: vec!["palette".into()],
            notes: "Loads the palette.".into(),
        }),
        tasks: vec![TaskDescriptor {
            id: TASK.into(),
            title: "Generate tint".into(),
            notes: "test".into(),
            asset: true,
            profile: true,
            requires_active: true,
            uses: vec!["input".into(), "echo".into(), "palette".into()],
            parameters: vec![parameter(
                "gain",
                ParameterKind::Number { min: 0.0, max: 2.0 },
                false,
            )],
            apply: Some(TaskApply {
                action: "apply-test-tint".into(),
                parameter: "tint".into(),
            }),
        }],
        ..ModuleDescriptor::default()
    }
}
