//! A test-only consumer of the declared `brush-mask` canvas and the correction identity: a module
//! whose one layer holds an ordered operation list (`EffectDescriptor::operations`), each operation
//! a minted number spelled `correction-<n>`, a path, a radius, feather, opacity and an optional
//! absolute source position.
//!
//! It proves the generic descriptor, identity and draft bindings the Corrections module reuses,
//! without being it: it is compiled only into the core's tests, renders nothing but the identity and
//! declares no control, so no client could show it as a tool. Its paths are held inline in its
//! payload rather than in the stroke store, whose reuse by a second consumer is already proved by
//! `path::tests::RepairStroke`.
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, BrushEdit, BrushSource,
    CanvasInteraction, CorrectionId, EffectDescriptor, EffectOperations, EffectStage, Error,
    ExactGeometry, IdentityKind, LayerUpdate, ModuleDescriptor, NewLayer, OPERATIONS_FIELD,
    ParameterDescriptor, ParameterKind, Processing, StageContext, ToolModule,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) const MODULE: &str = "test.repairbrush";
pub(crate) const EFFECT: &str = "test.repairbrush.effect";
pub(crate) const ADD: &str = "add-repair";
pub(crate) const UPDATE: &str = "update-repair";
pub(crate) const DELETE: &str = "delete-repair";
pub(crate) const LIST: &str = "repairs";
pub(crate) const SOURCES: &str = "repair-sources";
/// The identity parameter the update and delete name a repair by.
pub(crate) const OPERATION: &str = "operation";
/// A small limit, so a test reaches it in a few adds.
pub(crate) const LIMIT: u32 = 3;

/// The brush fields both actions declare, under the names the canvas binds. The source pair is
/// optional on the add: omitted, the module would choose one.
fn brush_fields(required: bool) -> Vec<ParameterDescriptor> {
    vec![
        ParameterDescriptor::points("points", 1, crate::path::POSTED_POINTS_PER_STROKE)
            .required(required)
            .notes("the painted path in normalized content coordinates"),
        ParameterDescriptor::number("radius", 0.0005, 0.25)
            .required(required)
            .step(0.005)
            .notes("radius in units of the content height"),
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
        ParameterDescriptor::number("source-x", -1.0, 2.0)
            .notes("where the path's first point samples from"),
        ParameterDescriptor::number("source-y", -1.0, 2.0)
            .notes("where the path's first point samples from"),
    ]
}

pub(crate) fn operation() -> ParameterDescriptor {
    ParameterDescriptor::new(
        OPERATION,
        ParameterKind::Identity {
            of: IdentityKind::Correction,
        },
    )
    .required(true)
    .notes("the repair this edits")
}

/// The interaction this consumer declares: every bound name of [`brush_fields`], the source with
/// its ranking query and the edit with its list.
pub(crate) fn canvas() -> CanvasInteraction {
    CanvasInteraction::BrushMask {
        action: ADD.into(),
        points: "points".into(),
        radius: "radius".into(),
        feather: Some("feather".into()),
        opacity: Some("opacity".into()),
        mode: Some("mode".into()),
        source: Some(BrushSource {
            x: "source-x".into(),
            y: "source-y".into(),
            query: SOURCES.into(),
        }),
        edit: Some(BrushEdit {
            action: UPDATE.into(),
            operation: OPERATION.into(),
            list: LIST.into(),
        }),
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
            operations: Some(EffectOperations {
                identity: IdentityKind::Correction,
                max: LIMIT,
            }),
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
        queries: vec![
            ActionDescriptor::new(LIST, "Repairs", "lists the drawable operations"),
            ActionDescriptor {
                parameters: vec![operation().required(false)],
                ..ActionDescriptor::new(SOURCES, "Repair sources", "ranks automatic sources")
            },
        ],
        canvas: Some(canvas()),
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    }
}

/// The consumer: its layer's payload is `{"next": n, "operations": [{id, points, radius, mode,
/// feather, opacity, source-x?, source-y?}]}` in application order, as the frozen Corrections
/// payload counts its operations.
pub(crate) struct RepairBrushModule(ModuleDescriptor);

impl RepairBrushModule {
    pub(crate) fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self(descriptor()))
    }

    fn operations(payload: &Value) -> Vec<Value> {
        payload[OPERATIONS_FIELD]
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
        let mut next = layer
            .and_then(|layer| layer.payload["next"].as_u64())
            .unwrap_or(1);
        let mut fields = input.parameters.clone();
        let named = |operations: &[Value], fields: &Map<String, Value>| {
            let wanted = CorrectionId::parse(fields[OPERATION].as_str().unwrap_or_default())?;
            operations
                .iter()
                .position(|held| held["id"] == wanted.number())
                .ok_or_else(|| Error::validation(format!("no repair {wanted} in this photograph")))
        };
        match input.action_id.as_str() {
            ADD => {
                if operations.len() >= LIMIT as usize {
                    return Err(crate::operations_limit(EffectOperations {
                        identity: IdentityKind::Correction,
                        max: LIMIT,
                    }));
                }
                fields.insert("id".into(), json!(next));
                next += 1;
                operations.push(Value::Object(fields));
            }
            UPDATE => {
                let at = named(&operations, &fields)?;
                fields.remove(OPERATION);
                let held = operations[at].as_object_mut().expect("an operation object");
                let before = held.clone();
                held.extend(fields);
                if *held == before {
                    return Ok(ActionPlan::NoOp);
                }
            }
            DELETE => {
                let at = named(&operations, &fields)?;
                operations.remove(at);
            }
            other => return Err(Error::validation(format!("unknown action {other}"))),
        }
        let payload = json!({"next": next, OPERATIONS_FIELD: operations});
        Ok(match layer {
            Some(layer) => ActionPlan::Update(LayerUpdate::new(layer.id.clone(), payload)),
            None => ActionPlan::Commit(NewLayer::new(EFFECT, payload)),
        })
    }

    fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
        if payload[OPERATIONS_FIELD].is_array() {
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
