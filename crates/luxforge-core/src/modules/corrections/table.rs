//! What the repair layer compiles to: the host's repair table ([`crate::Processing::Repair`]), one
//! row per Clone or Heal operation in application order, each with its stroke's stored positions
//! and radius. An empty layer — before the first correction, or after a reset — compiles to the
//! empty table, which the host drops.
use super::{
    actions::stroke_of,
    payload::{OperationKind, RepairPayload},
};
use crate::{
    Error, Processing,
    modules::{RepairKind, RepairOperation, RepairTable},
    path::StrokeTable,
};

/// The table of `payload`, its strokes resolved from `strokes`: a missing, corrupt or wrong-type
/// stroke is refused naming its operation, never compiled as an empty row. `O(operations)`; each
/// row shares its stroke's positions.
pub(super) fn compile(payload: &RepairPayload, strokes: &StrokeTable) -> Result<Processing, Error> {
    let rows = payload
        .operations
        .iter()
        .map(|operation| {
            let stroke = stroke_of(strokes, operation)?;
            Ok(RepairOperation {
                id: operation.id,
                kind: match operation.kind {
                    OperationKind::Heal => RepairKind::Heal,
                    OperationKind::Clone => RepairKind::Clone,
                },
                points: stroke.grid(),
                radius: stroke.radius_steps(),
                feather: operation.feather,
                opacity: operation.opacity,
                offset: operation.offset,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Processing::Repair(RepairTable::new(rows)?))
}
