//! A test-only consumer of the declared `brush-mask` canvas and the operation identity: a module
//! whose one layer holds an ordered list of painted operations, each with a minted
//! [`crate::OperationId`], a path, a radius, feather, opacity and an optional source position.
//!
//! It proves the generic descriptor, identity and draft bindings a production module (the
//! Corrections module) reuses, without being one: it is compiled only into the core's tests, it
//! renders nothing but the identity and it declares no control, so no client could ever show it as
//! a tool. Its paths are held inline in its payload rather than in the stroke store, because the
//! store's reuse by a second consumer is already proved by `path::tests::RepairStroke`.
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, EffectDescriptor,
    EffectStage, Error, ExactGeometry, IdentityKind, LayerUpdate, ModuleDescriptor, NewLayer,
    OPERATION_PARAMETER, OperationId, ParameterDescriptor, ParameterKind, Processing, StageContext,
    ToolModule,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) const MODULE: &str = "test.repairbrush";
pub(crate) const EFFECT: &str = "test.repairbrush.effect";
pub(crate) const ADD: &str = "add-repair";
pub(crate) const UPDATE: &str = "update-repair";
pub(crate) const DELETE: &str = "delete-repair";

/// The brush fields both actions declare, under the names the canvas binds. The source pair is
/// optional on the add: omitted, the module would choose one.
fn brush_fields(required: bool) -> Vec<ParameterDescriptor> {
    vec![
        ParameterDescriptor::points("points", 1, crate::path::POINTS_PER_STROKE)
            .required(required)
            .notes("the painted path in normalized content coordinates"),
        ParameterDescriptor::number("radius", 0.001, 0.5)
            .required(required)
            .default(0.05)
            .step(0.005)
            .notes("radius in units of the content height"),
        ParameterDescriptor::number("feather", 0.0, 100.0)
            .default(50.0)
            .step(1.0)
            .notes("test"),
        ParameterDescriptor::number("opacity", 0.0, 100.0)
            .default(100.0)
            .step(1.0)
            .notes("test"),
        ParameterDescriptor::number("source-x", -1.0, 2.0)
            .notes("where the path's first point samples from"),
        ParameterDescriptor::number("source-y", -1.0, 2.0)
            .notes("where the path's first point samples from"),
    ]
}

fn operation() -> ParameterDescriptor {
    ParameterDescriptor::new(
        OPERATION_PARAMETER,
        ParameterKind::Identity {
            of: IdentityKind::Operation,
        },
    )
    .required(true)
    .notes("the operation this edits")
}

/// The interaction this consumer declares: every bound name of [`brush_fields`], with the update.
pub(crate) fn canvas() -> CanvasInteraction {
    CanvasInteraction::BrushMask {
        action: ADD.into(),
        points: "points".into(),
        radius: "radius".into(),
        feather: "feather".into(),
        opacity: "opacity".into(),
        source_x: Some("source-x".into()),
        source_y: Some("source-y".into()),
        update: Some(UPDATE.into()),
        title: "Repair".into(),
        shortcut: None,
        icon: Some("heal".into()),
    }
}

pub(crate) fn descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: MODULE.into(),
        title: "Repair brush".into(),
        effects: vec![EffectDescriptor {
            single: true,
            ..EffectDescriptor::new(EFFECT, EffectStage::Geometry)
        }],
        actions: vec![
            ActionDescriptor {
                preset: false,
                parameters: brush_fields(true),
                ..ActionDescriptor::new(ADD, "Add repair", "adds one painted operation")
            },
            ActionDescriptor {
                preset: false,
                patch: true,
                parameters: std::iter::once(operation())
                    .chain(brush_fields(false))
                    .collect(),
                ..ActionDescriptor::new(UPDATE, "Update repair", "edits one operation")
            },
            ActionDescriptor {
                preset: false,
                parameters: vec![operation()],
                ..ActionDescriptor::new(DELETE, "Delete repair", "removes one operation")
            },
        ],
        canvas: Some(canvas()),
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    }
}

/// The consumer: its layer's payload is `{"operations": [{operation, points, radius, feather,
/// opacity, source}]}` in stack order.
pub(crate) struct RepairBrushModule(ModuleDescriptor);

impl RepairBrushModule {
    pub(crate) fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self(descriptor()))
    }

    fn operations(payload: &Value) -> Vec<Value> {
        payload["operations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

impl ToolModule for RepairBrushModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }

    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let layer = context.own_layer(EFFECT)?.map(|(_, layer)| layer);
        let mut operations = layer
            .map(|layer| Self::operations(&layer.payload))
            .unwrap_or_default();
        let fields = &input.parameters;
        let named = || -> Result<usize, Error> {
            let wanted = fields[OPERATION_PARAMETER].as_str().unwrap_or_default();
            operations
                .iter()
                .position(|held| held[OPERATION_PARAMETER] == wanted)
                .ok_or_else(|| Error::validation(format!("unknown operation {wanted}")))
        };
        match input.action_id.as_str() {
            ADD => {
                let mut added = fields.clone();
                added.insert(
                    OPERATION_PARAMETER.into(),
                    json!(OperationId::new().as_str()),
                );
                operations.push(Value::Object(added));
            }
            UPDATE => {
                let at = named()?;
                let held = operations[at].as_object_mut().expect("an operation object");
                let before = held.clone();
                for (name, value) in fields {
                    held.insert(name.clone(), value.clone());
                }
                if *held == before {
                    return Ok(ActionPlan::NoOp);
                }
            }
            DELETE => {
                let at = named()?;
                operations.remove(at);
            }
            other => return Err(Error::validation(format!("unknown action {other}"))),
        }
        let payload = json!({ "operations": operations });
        Ok(match layer {
            Some(layer) => ActionPlan::Update(LayerUpdate::new(layer.id.clone(), payload)),
            None => ActionPlan::Commit(NewLayer::new(EFFECT, payload)),
        })
    }

    fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
        if payload["operations"].is_array() {
            Ok(())
        } else {
            Err(Error::validation("a repair layer holds an operation list"))
        }
    }

    fn describe(&self, _: &str, _: u32, payload: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!(
            "{} repairs",
            Self::operations(payload).len()
        )))
    }

    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        at: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Ok(Processing::ExactGeometry(ExactGeometry {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: at.stage.width,
            output_height: at.stage.height,
        }))
    }
}
