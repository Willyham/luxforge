//! What the repair layer compiles to.
//!
//! An empty layer — before the first correction, or after a reset — changes nothing and compiles
//! to the neutral spatial operation, which the host drops. A layer holding operations compiles to
//! the repair primitive the render task delivers (`Processing::Repair`, TASK-003). Until it is in
//! this build, such a layer compiles — so the edit is admitted and kept in history like any other —
//! to one spatial unit that refuses every render, sample and export by name and has no GPU form, so
//! the GPU falls back to the reference, which refuses too. A correction is never silently omitted.
use super::payload::RepairPayload;
use crate::{
    CompileStage, Error, OperationIdentity, Processing, Stage,
    modules::{Global, Parallelism, Planes, PlanesMut, SpatialOperation, SpatialUnit},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) fn compile(payload: &RepairPayload, _at: CompileStage) -> Result<Processing, Error> {
    if payload.operations.is_empty() {
        return Ok(Processing::Spatial(SpatialOperation::neutral()));
    }
    let canonical = serde_json::to_vec(&payload.clone().stored())
        .map_err(|error| Error::internal(error.to_string()))?;
    let digest = Sha256::digest(&canonical);
    let words = digest
        .chunks_exact(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("an eight-byte chunk")));
    Ok(Processing::Spatial(SpatialOperation::new(vec![Arc::new(
        Unrendered {
            identity: OperationIdentity::new("luxforge.corrections.unrendered", words),
            count: payload.operations.len(),
        },
    )])?))
}

/// The refusal of a repair layer this build cannot draw.
pub(crate) fn unrendered(count: usize) -> Error {
    let (noun, verb) = if count == 1 {
        ("correction", "is")
    } else {
        ("corrections", "are")
    };
    Error::incompatible(format!(
        "this build cannot render Corrections yet: the photograph's {count} {noun} {verb} kept, \
         and rendering, sampling and export are refused rather than drawn without them"
    ))
}

/// A repair layer with operations, in a build without the repair primitive.
struct Unrendered {
    identity: OperationIdentity,
    count: usize,
}

impl SpatialUnit for Unrendered {
    fn halo(&self, _stage: Stage) -> u32 {
        0
    }

    fn scratch_bytes(&self, _region: Stage) -> u64 {
        0
    }

    fn apply(
        &self,
        _input: &Planes<'_>,
        _output: &mut PlanesMut<'_>,
        _global: Option<&Global>,
        _scratch: &mut [f32],
        _parallelism: Parallelism,
    ) -> Result<(), Error> {
        Err(unrendered(self.count))
    }

    fn is_finite(&self) -> bool {
        true
    }

    fn identity(&self) -> OperationIdentity {
        self.identity.clone()
    }

    fn describe(&self) -> String {
        format!("{} unrendered corrections", self.count)
    }
}
