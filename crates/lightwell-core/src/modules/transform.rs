//! The exact transform module: quarter turns and reflections as integer coordinate mappings.
use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, Control, EffectDescriptor,
    EffectStage, ExactGeometry, ModuleDescriptor, ParameterDescriptor, ParameterKind, Processing,
    Stage, StageContext, ToolModule, render_summary,
};
use crate::{EFFECT_FORMAT, Error, ErrorKind, Layer, TRANSFORM_EFFECT, Transform};
use serde_json::{Map, Value};

pub(super) const TRANSFORM_ACTION: &str = "transform";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

impl Transform {
    /// The exact input-to-output mapping of this transform at one input stage.
    pub(crate) fn geometry(self, width: u32, height: u32) -> ExactGeometry {
        match self {
            Self::RotateRight => ExactGeometry {
                a: 0,
                b: -1,
                c: 1,
                d: 0,
                tx: i64::from(height) - 1,
                ty: 0,
                output_width: height,
                output_height: width,
            },
            Self::RotateLeft => ExactGeometry {
                a: 0,
                b: 1,
                c: -1,
                d: 0,
                tx: 0,
                ty: i64::from(width) - 1,
                output_width: height,
                output_height: width,
            },
            Self::MirrorHorizontal => ExactGeometry {
                a: -1,
                b: 0,
                c: 0,
                d: 1,
                tx: i64::from(width) - 1,
                ty: 0,
                output_width: width,
                output_height: height,
            },
            Self::FlipVertical => ExactGeometry {
                a: 1,
                b: 0,
                c: 0,
                d: -1,
                tx: 0,
                ty: i64::from(height) - 1,
                output_width: width,
                output_height: height,
            },
        }
    }
}

fn control(transform: Transform, label: &str) -> Control {
    let mut preset = Map::new();
    preset.insert("transform".into(), Value::from(transform.action_id()));
    Control::Action {
        action: TRANSFORM_ACTION.into(),
        label: label.into(),
        preset,
    }
}

#[derive(Debug)]
pub struct TransformModule {
    descriptor: ModuleDescriptor,
}

impl Default for TransformModule {
    fn default() -> Self {
        Self::new()
    }
}

impl TransformModule {
    pub fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "lightwell.transform".into(),
                title: "Transforms".into(),
                hint: Some("Rotate, mirror and flip".into()),
                effects: vec![EffectDescriptor {
                    id: TRANSFORM_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                }],
                actions: vec![ActionDescriptor {
                    id: TRANSFORM_ACTION.into(),
                    title: "Transform".into(),
                    notes: "exact quarter turns and reflections; integer mappings with no interpolation".into(),
                    summary: Some("{transform}".into()),
                    parameters: vec![ParameterDescriptor {
                        name: "transform".into(),
                        kind: ParameterKind::Enum {
                            options: vec![
                                Transform::RotateLeft.action_id().into(),
                                Transform::RotateRight.action_id().into(),
                                Transform::MirrorHorizontal.action_id().into(),
                                Transform::FlipVertical.action_id().into(),
                            ],
                        },
                        required: true,
                        default: None,
                        unit: None,
                        notes: "the exact transform to append to the stack".into(),
                    }],
                }],
                controls: vec![Control::Group {
                    label: "Exact transforms".into(),
                    reset: None,
                    controls: vec![
                        control(Transform::RotateLeft, "Rotate left"),
                        control(Transform::RotateRight, "Rotate right"),
                        control(Transform::MirrorHorizontal, "Mirror horizontal"),
                        control(Transform::FlipVertical, "Flip vertical"),
                    ],
                }],
                reset: None,
                canvas: None,
                developer: false,
                availability: Availability::Available,
            },
        }
    }
}

fn transform_value(parameters: &Map<String, Value>) -> Result<Transform, Error> {
    let value = parameters
        .get("transform")
        .ok_or_else(|| validation("missing required parameter transform for action transform"))?;
    serde_json::from_value(value.clone())
        .map_err(|error| validation(format!("invalid transform: {error}")))
}

fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<Transform, Error> {
    if effect_id != TRANSFORM_EFFECT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unavailable effect {effect_id}"),
        ));
    }
    if format != EFFECT_FORMAT {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("unsupported effect format {format}"),
        ));
    }
    serde_json::from_value(payload.clone())
        .map_err(|error| validation(format!("invalid transform payload: {error}")))
}

impl ToolModule for TransformModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        if action_id != TRANSFORM_ACTION {
            return Err(validation(format!("unknown action {action_id}")));
        }
        let transform = transform_value(parameters)?;
        let mut stored = Map::new();
        stored.insert("transform".into(), Value::from(transform.action_id()));
        Ok(ActionInput {
            // The durable history identity is the transform itself, unchanged since M2.
            action_id: transform.action_id().into(),
            parameters: stored,
        })
    }

    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        // Every exact transform changes the stack: four quarter turns are an identity, one is not.
        Ok(ActionPlan::Commit(Layer::transform(transform_value(
            &input.parameters,
        )?)))
    }

    fn validate_payload(&self, effect_id: &str, format: u32, value: &Value) -> Result<(), Error> {
        payload(effect_id, format, value).map(|_| ())
    }

    fn describe_layer(&self, effect_id: &str, format: u32, value: &Value) -> Result<String, Error> {
        let transform = payload(effect_id, format, value)?;
        // The same wording the history label uses for this transform.
        let mut parameters = Map::new();
        parameters.insert("transform".into(), Value::from(transform.action_id()));
        Ok(render_summary("{transform}", &parameters))
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        value: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        Ok(Processing::ExactGeometry(
            payload(effect_id, format, value)?.geometry(stage.width, stage.height),
        ))
    }
}
