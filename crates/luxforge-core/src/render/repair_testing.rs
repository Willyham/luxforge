//! A repair-stage test module whose payload is the repair table itself, for the core's tests and
//! the desktop's GPU qualification of the repair program before the Corrections module compiles
//! its own payload into the same table.
use crate::{
    Error,
    modules::{
        ActionInput, ActionPlan, Availability, EffectDescriptor, EffectStage, ModuleDescriptor,
        Processing, RepairKind, RepairOperation, RepairTable, StageContext, ToolModule,
    },
};
use serde_json::{Map, Value};
use std::sync::Arc;

pub const TEST_REPAIR_EFFECT: &str = "test.repair.table";

/// A repair-stage module whose payload is the table itself:
/// `{"operations": [{"id", "kind", "points", "radius", "feather", "opacity", "offset"}]}` in grid
/// steps. The Corrections module compiles its own payload into the same table.
pub struct RepairTestModule {
    descriptor: ModuleDescriptor,
}

impl RepairTestModule {
    pub fn shared() -> Arc<dyn ToolModule> {
        Arc::new(Self {
            descriptor: ModuleDescriptor {
                id: "test.repair.module".into(),
                title: "Test repair".into(),
                effects: vec![EffectDescriptor {
                    single: true,
                    ..EffectDescriptor::new(TEST_REPAIR_EFFECT, EffectStage::Repair)
                }],
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
        })
    }
}

pub fn table_of(payload: &Value) -> Result<RepairTable, Error> {
    let rows = payload["operations"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|row| {
            let points: Vec<[i32; 2]> = serde_json::from_value(row["points"].clone())
                .map_err(|error| Error::validation(error.to_string()))?;
            Ok(RepairOperation {
                id: row["id"].as_u64().unwrap_or(1) as u32,
                kind: match row["kind"].as_str() {
                    Some("clone") => RepairKind::Clone,
                    _ => RepairKind::Heal,
                },
                points: points.into(),
                radius: row["radius"].as_i64().unwrap_or(0) as i32,
                feather: row["feather"].as_u64().unwrap_or(50) as u8,
                opacity: row["opacity"].as_u64().unwrap_or(100) as u8,
                offset: serde_json::from_value(row["offset"].clone())
                    .map_err(|error| Error::validation(error.to_string()))?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    RepairTable::new(rows)
}

impl ToolModule for RepairTestModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::internal("no actions"))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
        table_of(payload).map(drop)
    }
    fn describe(&self, _: &str, _: u32, payload: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!("test repair {payload}")))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        payload: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        table_of(payload).map(Processing::Repair)
    }
}

