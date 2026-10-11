//! The desktop tests' brush consumer: a module declaring the `brush-mask` canvas over an operation
//! list, built here from the public module interface exactly as a module author would, so the
//! shared capture is proved against a declaration it knows nothing about. It is the desktop's copy
//! of the core's own test-only repair brush and, like it, never a production tool.
use luxforge_core::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, BrushEdit, BrushSource,
    CanvasInteraction, ColorOperation, CompileStage, CorrectionId, EffectDescriptor,
    EffectOperations, EffectStage, Error, IdentityKind, LayerReport, LayerUpdate, ModuleDescriptor,
    NewLayer, OPERATIONS_FIELD, ParameterDescriptor, ParameterKind, Processing, StageContext,
    ToolModule,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) const MODULE: &str = "test.repairbrush";
pub(crate) const EFFECT: &str = "test.repairbrush.effect";
pub(crate) const ADD: &str = "add-repair";
pub(crate) const UPDATE: &str = "update-repair";
pub(crate) const OPERATION: &str = "operation";

fn brush_fields(required: bool) -> Vec<ParameterDescriptor> {
    vec![
        ParameterDescriptor::new(
            "points",
            ParameterKind::Points {
                points_min: 1,
                points_max: luxforge_core::path::CAPTURED_POINTS_PER_STROKE,
            },
        )
        .required(required)
        .notes("the painted path"),
        ParameterDescriptor::number("radius", 0.0005, 0.25)
            .required(required)
            .default(0.05)
            .step(0.005)
            .notes("radius in content heights"),
        ParameterDescriptor::enumeration("mode", ["heal", "clone"])
            .default("heal")
            .notes("test"),
        ParameterDescriptor::integer("feather", 0, 100)
            .default(50)
            .step(1.0)
            .notes("test"),
        ParameterDescriptor::integer("opacity", 0, 100)
            .default(100)
            .step(1.0)
            .notes("test"),
        ParameterDescriptor::number("source-x", -1.0, 2.0).notes("test"),
        ParameterDescriptor::number("source-y", -1.0, 2.0).notes("test"),
    ]
}

fn operation() -> ParameterDescriptor {
    ParameterDescriptor::new(
        OPERATION,
        ParameterKind::Identity {
            of: IdentityKind::Correction,
        },
    )
    .required(true)
    .notes("the repair this edits")
}

pub(crate) fn descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: MODULE.into(),
        title: "Repair brush".into(),
        effects: vec![EffectDescriptor {
            single: true,
            operations: Some(EffectOperations {
                identity: IdentityKind::Correction,
                max: 100,
            }),
            ..EffectDescriptor::new(EFFECT, EffectStage::Color)
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
        ],
        queries: vec![
            ActionDescriptor::new("repairs", "Repairs", "lists the drawable operations"),
            ActionDescriptor::new("repair-sources", "Repair sources", "ranks sources"),
        ],
        canvas: Some(CanvasInteraction::BrushMask {
            action: ADD.into(),
            points: "points".into(),
            radius: "radius".into(),
            feather: Some("feather".into()),
            opacity: Some("opacity".into()),
            mode: Some("mode".into()),
            source: Some(BrushSource {
                x: "source-x".into(),
                y: "source-y".into(),
                query: "repair-sources".into(),
            }),
            edit: Some(BrushEdit {
                action: UPDATE.into(),
                operation: OPERATION.into(),
                list: "repairs".into(),
            }),
            title: "Repair".into(),
            shortcut: None,
            icon: Some("heal".into()),
        }),
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    }
}

/// The consumer's layer: `{"next": n, "operations": [{id, points, radius, …}]}`, each operation
/// exactly the fields its add or update sent, so a test compares what two clients stored.
pub(crate) struct RepairBrush(ModuleDescriptor);

impl RepairBrush {
    pub(crate) fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self(descriptor()))
    }
}

impl ToolModule for RepairBrush {
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
            .and_then(|layer| layer.payload[OPERATIONS_FIELD].as_array().cloned())
            .unwrap_or_default();
        let mut next = layer
            .and_then(|layer| layer.payload["next"].as_u64())
            .unwrap_or(1);
        let mut fields = input.parameters.clone();
        if input.action_id == ADD {
            fields.insert("id".into(), json!(next));
            next += 1;
            operations.push(Value::Object(fields));
        } else {
            let wanted = CorrectionId::parse(fields[OPERATION].as_str().unwrap_or_default())?;
            let held = operations
                .iter_mut()
                .find(|held| held["id"] == wanted.number())
                .ok_or_else(|| Error::validation(format!("no repair {wanted}")))?;
            fields.remove(OPERATION);
            let held = held.as_object_mut().expect("an operation");
            let before = held.clone();
            held.extend(fields);
            if *held == before {
                return Ok(ActionPlan::NoOp);
            }
        }
        let payload = json!({"next": next, OPERATIONS_FIELD: operations});
        Ok(match layer {
            Some(layer) => ActionPlan::Update(LayerUpdate::new(layer.id.clone(), payload)),
            None => ActionPlan::Commit(NewLayer::new(EFFECT, payload)),
        })
    }

    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }

    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<LayerReport, Error> {
        Ok(LayerReport::new("Repairs"))
    }

    /// A neutral colour layer: no geometry, so the map a test captured through stays the map of
    /// every entry the consumer commits.
    fn compile(&self, _: &str, _: u32, _: &Value, _: CompileStage) -> Result<Processing, Error> {
        Ok(Processing::Color(ColorOperation::new(Vec::new())))
    }
}
