//! A test module whose colour effect is evaluated with an artifact's bytes, so every rendered pixel
//! of its layer proves which bytes were bound. Its action takes an `artifact` parameter and keeps
//! one layer that references that artifact; a second effect does not declare artifacts, so a layer
//! of it that lists one proves the host refuses it.
use super::{ArtifactMeta, PreparedArtifact};
use crate::{
    ActionInput, ActionPlan, ColorOperation, EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId,
    ModuleDescriptor, ModuleRegistry, PointwiseColor, Processing, Stage, StageContext, ToolModule,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) const TINT_MODULE: &str = "test.tint";
pub(crate) const TINT_EFFECT: &str = "test.tint.gains";
pub(crate) const PLAIN_EFFECT: &str = "test.tint.plain";
pub(crate) const APPLY_TINT: &str = "apply-test-tint";
pub(crate) const APPLY_PLAIN: &str = "apply-plain-tint";

/// Three linear-light gains, one per channel.
struct Gains([f32; 3]);

impl PointwiseColor for Gains {
    fn apply_row(&self, _: u32, _: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            for (channel, gain) in pixel.iter_mut().zip(self.0) {
                *channel *= gain;
            }
        }
    }
    fn is_finite(&self) -> bool {
        self.0.iter().all(|gain| gain.is_finite())
    }
    fn describe(&self) -> String {
        format!("gains {:?}", self.0)
    }
}

pub(crate) struct TintModule(ModuleDescriptor);

impl TintModule {
    /// Built from JSON, so descriptor fields added later default the way a client's would.
    pub(crate) fn shared() -> Arc<dyn ToolModule> {
        let action = |id: &str, title: &str| {
            json!({
                "id": id,
                "title": title,
                "notes": "binds the tint to the published artifact that holds its three gains",
                "parameters": [{
                    "name": "artifact",
                    "kind": "artifact",
                    "required": true,
                    "notes": "twelve bytes: three little-endian f32 linear gains",
                }],
            })
        };
        let descriptor = ModuleDescriptor::parse(&json!({
            "id": TINT_MODULE,
            "title": "Tint",
            "effects": [
                {"id": TINT_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "artifacts": true},
                {"id": PLAIN_EFFECT, "format": EFFECT_FORMAT, "stage": "color"},
            ],
            "actions": [action(APPLY_TINT, "Apply tint"), action(APPLY_PLAIN, "Apply plain")],
            "controls": [],
            "availability": {"kind": "available"},
        }))
        .expect("the tint descriptor is valid");
        Arc::new(Self(descriptor))
    }

    /// The built-in providers and this module.
    pub(crate) fn registry() -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry.register(Self::shared()).unwrap();
        Arc::new(registry)
    }

    /// The artifact bytes of three gains.
    pub(crate) fn bytes(gains: [f32; 3]) -> Vec<u8> {
        gains.iter().flat_map(|gain| gain.to_le_bytes()).collect()
    }

    pub(crate) fn meta() -> ArtifactMeta {
        ArtifactMeta {
            kind: "tint".into(),
            width: None,
            height: None,
            colour: Some("linear-srgb".into()),
        }
    }

    fn artifact(parameters: &Map<String, Value>) -> Result<super::ArtifactId, Error> {
        super::ArtifactId::parse(
            parameters
                .get("artifact")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    }
}

impl ToolModule for TintModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let artifact = Self::artifact(parameters)?;
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: json!({"artifact": artifact})
                .as_object()
                .expect("an object")
                .clone(),
        })
    }
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let artifact = Self::artifact(&input.parameters)?;
        if input.action_id == APPLY_PLAIN {
            return Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: PLAIN_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
                artifacts: vec![artifact],
            }));
        }
        match context
            .layers
            .iter()
            .find(|layer| layer.effect_id == TINT_EFFECT)
        {
            Some(layer) if layer.artifacts == [artifact.clone()] => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(Layer {
                artifacts: vec![artifact],
                ..layer.clone()
            })),
            None => Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: TINT_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
                artifacts: vec![artifact],
            })),
        }
    }
    fn validate_payload(&self, _: &str, format: u32, payload: &Value) -> Result<(), Error> {
        if format == EFFECT_FORMAT && payload.as_object().is_some_and(Map::is_empty) {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Validation, "a tint payload is {}"))
        }
    }
    fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok(if effect_id == TINT_EFFECT {
            "Tint".into()
        } else {
            "Plain".into()
        })
    }
    fn compile(&self, effect_id: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        if effect_id == TINT_EFFECT {
            Err(Error::new(
                ErrorKind::Validation,
                "a tint layer is evaluated with its gains artifact",
            ))
        } else {
            Ok(Processing::Color(ColorOperation::neutral()))
        }
    }
    fn compile_bound(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: Stage,
        artifacts: &[Arc<PreparedArtifact>],
    ) -> Result<Processing, Error> {
        let [artifact] = artifacts else {
            return Err(Error::new(
                ErrorKind::Validation,
                "a tint layer binds exactly one artifact",
            ));
        };
        if artifact.bytes.len() != 12 {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("artifact {} does not hold three gains", artifact.id),
            ));
        }
        let gain = |index: usize| {
            let bytes = &artifact.bytes[index * 4..index * 4 + 4];
            f32::from_le_bytes(bytes.try_into().expect("four bytes"))
        };
        Ok(Processing::Color(ColorOperation::new(vec![Arc::new(
            Gains([gain(0), gain(1), gain(2)]),
        )])))
    }
}
