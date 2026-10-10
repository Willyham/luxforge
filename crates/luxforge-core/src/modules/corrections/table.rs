//! What the repair layer compiles to: the host's repair table ([`crate::Processing::Repair`]), one
//! row per operation in application order — a Clone or Heal with its stroke's stored positions and
//! radius, a frozen patch with its verified bytes and rectangle. An empty layer — before the first
//! correction, or after a reset — compiles to the empty table, which the host drops.
use super::{
    actions::stroke_of,
    payload::{Operation, OperationKind, RepairPayload},
};
use crate::{
    CompileStage, Error, Processing,
    artifacts::{PreparedArtifact, candidates::PATCH_KIND},
    modules::{RepairKind, RepairOperation, RepairTable},
    path::StrokeTable,
    repair::patch::PatchSource,
};
use std::sync::Arc;

/// The table of `payload`, its strokes resolved from `strokes` and its patches' bytes from
/// `artifacts`, the verified bytes the layer lists: a missing, corrupt or wrong-type stroke, or a
/// patch whose artifact is missing or not a `repair-patch` of its rectangle's size, is refused
/// naming its operation, never compiled as an empty row. `O(operations)`; each row shares its
/// stroke's positions or its artifact's bytes.
pub(super) fn compile(
    payload: &RepairPayload,
    strokes: &StrokeTable,
    artifacts: &[Arc<PreparedArtifact>],
    at: CompileStage,
) -> Result<Processing, Error> {
    let listed: Vec<&crate::ArtifactId> = artifacts.iter().map(|artifact| &artifact.id).collect();
    if listed != payload.artifacts().iter().collect::<Vec<_>>() {
        return Err(Error::incompatible(
            "the Corrections layer must list exactly its patches' artifacts, in operation order",
        ));
    }
    if payload.operations.is_empty() {
        return Ok(Processing::Repair(RepairTable::neutral()));
    }
    let rows = payload
        .operations
        .iter()
        .map(|operation| match operation {
            Operation::Brush(brush) => {
                let stroke = stroke_of(strokes, brush)?;
                Ok(RepairOperation {
                    id: brush.id,
                    kind: match brush.kind {
                        OperationKind::Heal => RepairKind::Heal,
                        OperationKind::Clone => RepairKind::Clone,
                    },
                    points: stroke.grid(),
                    radius: stroke.radius_steps(),
                    feather: brush.feather,
                    opacity: brush.opacity,
                    offset: brush.offset,
                    patch: None,
                })
            }
            Operation::Patch(patch) => {
                let name = operation.name();
                let artifact = artifacts
                    .iter()
                    .find(|artifact| artifact.id == patch.artifact)
                    .ok_or_else(|| {
                        Error::source_unavailable(format!(
                            "artifact {} of operation {} of Corrections is not bound",
                            patch.artifact,
                            operation.identity()
                        ))
                    })?;
                let [_, _, width, height] = patch.rect;
                if artifact.kind != PATCH_KIND
                    || artifact.width != Some(width)
                    || artifact.height != Some(height)
                {
                    return Err(Error::incompatible(format!(
                        "artifact {} of {name} is not a {width}x{height} {PATCH_KIND}",
                        patch.artifact
                    )));
                }
                Ok(RepairOperation::patch(
                    patch.id,
                    &patch.label,
                    patch.opacity,
                    PatchSource {
                        artifact: artifact.clone(),
                        rect: patch.rect,
                        full: at.full,
                    },
                ))
            }
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Processing::Repair(RepairTable::new(rows)?))
}
