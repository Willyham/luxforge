//! Qualification only: Corrections stacks holding frozen patches, built without a catalog, for the
//! desktop's GPU readback tests and the core's plan tests of the patch composite. A patch is its
//! verified bytes bound into the recipe's artifact table, exactly as the catalog's binding step
//! leaves a stack; a Clone or Heal is its stroke in the recipe's stroke table. Built only with the
//! core's `qualification` feature or in its own tests, so no binary has it.
use super::{
    CORRECTIONS_EFFECT,
    input::RepairInput,
    payload::{Brush, Operation, OperationKind, Origin, Patch, PatchKind, RepairPayload},
    stroke::CorrectionStroke,
};
use crate::{
    ArtifactId, Layer, Recipe,
    artifacts::{ArtifactTable, PreparedArtifact},
    path::{StrokeTable, quantize},
};
use serde_json::Map;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// One frozen patch: its rectangle in content pixels, its opacity, and each pixel's linear RGB
/// and coverage, `(column, row)` within the patch.
pub struct QualPatch {
    pub rect: [u32; 4],
    pub opacity: u8,
    pub pixel: Box<dyn Fn(u32, u32) -> ([f32; 3], f32)>,
}

/// One Clone or Heal: a path in normalized positions, its radius in content heights, its absolute
/// source anchor, feather and opacity.
pub struct QualBrush {
    pub heal: bool,
    pub points: Vec<[f64; 2]>,
    pub radius: f64,
    pub source: [f64; 2],
    pub feather: u8,
    pub opacity: u8,
}

/// A step of the repair layer, in application order.
pub enum QualStep {
    Brush(QualBrush),
    Patch(QualPatch),
}

/// The stored `repair-patch` bytes of `patch`: RGBA half floats, little-endian.
pub fn patch_bytes(patch: &QualPatch) -> Vec<u8> {
    let [_, _, width, height] = patch.rect;
    let mut bytes = Vec::with_capacity((width * height * 8) as usize);
    for row in 0..height {
        for column in 0..width {
            let (rgb, alpha) = (patch.pixel)(column, row);
            for value in [rgb[0], rgb[1], rgb[2], alpha] {
                bytes.extend_from_slice(&half::f16::from_f32(value).to_le_bytes());
            }
        }
    }
    bytes
}

/// A recipe of one repair layer holding `steps` in order, followed by `after`, with its strokes and
/// its patches' verified bytes bound: what the catalog hands a render or a GPU plan.
pub fn repair_stack(steps: Vec<QualStep>, after: Vec<Layer>) -> Recipe {
    let input = RepairInput {
        source: "0".repeat(64),
        dimensions: [1, 1],
        upstream: "0".repeat(64),
        earlier: "0".repeat(64),
    };
    let mut strokes = StrokeTable::default();
    let mut operations = Vec::new();
    let mut prepared = Vec::new();
    for (index, step) in steps.into_iter().enumerate() {
        let id = index as u32 + 1;
        match step {
            QualStep::Brush(brush) => {
                let stroke = CorrectionStroke::capture(&brush.points, brush.radius)
                    .expect("a stroke within its bounds");
                let anchor = stroke.anchor();
                let offset = [
                    quantize(brush.source[0]) - anchor[0],
                    quantize(brush.source[1]) - anchor[1],
                ];
                operations.push(Operation::Brush(Brush {
                    id,
                    kind: if brush.heal {
                        OperationKind::Heal
                    } else {
                        OperationKind::Clone
                    },
                    stroke: strokes.insert(stroke),
                    feather: brush.feather,
                    opacity: brush.opacity,
                    offset,
                    origin: Origin::Set,
                }));
            }
            QualStep::Patch(patch) => {
                let bytes = patch_bytes(&patch);
                let artifact =
                    ArtifactId::for_hash(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
                let meta = crate::artifacts::ArtifactMeta {
                    kind: crate::artifacts::candidates::PATCH_KIND.into(),
                    width: Some(patch.rect[2]),
                    height: Some(patch.rect[3]),
                    colour: Some("linear-srgb".into()),
                };
                if !prepared
                    .iter()
                    .any(|held: &Arc<PreparedArtifact>| held.id == artifact)
                {
                    prepared.push(Arc::new(PreparedArtifact::new(
                        artifact.clone(),
                        &meta,
                        bytes.into(),
                    )));
                }
                operations.push(Operation::Patch(Patch {
                    id,
                    kind: PatchKind::Patch,
                    label: "Patch".into(),
                    artifact,
                    rect: patch.rect,
                    opacity: patch.opacity,
                    input: input.clone(),
                    provenance: Map::new(),
                }));
            }
        }
    }
    let payload = RepairPayload {
        next: operations.len() as u64 + 1,
        strokes: Vec::new(),
        operations,
    };
    let artifacts = payload.artifacts();
    let mut layer = Layer::new(CORRECTIONS_EFFECT, payload.stored());
    layer.artifacts = artifacts;
    let mut layers = vec![layer];
    layers.extend(after);
    Recipe {
        layers,
        strokes,
        artifacts: prepared.into_iter().collect::<ArtifactTable>(),
        ..Recipe::default()
    }
}

/// The CPU reference of `stack`'s repair layer, its first, drawn at a reduced `stage` of a `full`
/// content stage, as a GPU Fit frame draws the repair stage smaller than its patches were placed
/// (`docs/design/corrections.md`, "The composite at a reduced stage"): the layer compiled at that
/// stage against the full one, as the host compiles a proxy, and every operation run in order over
/// `pixels`, the reduced frame's linear values row by row. The CPU's output, row by row.
pub fn reduced(
    stack: &Recipe,
    stage: (u32, u32),
    full: (u32, u32),
    pixels: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    use crate::{
        Cancel, Processing,
        modules::{CapabilityModule, Region, Stage},
    };
    let layer = &stack.layers[0];
    let bound: Vec<_> = layer
        .artifacts
        .iter()
        .map(|id| stack.artifacts.get(id).cloned().expect("a bound artifact"))
        .collect();
    let at = Stage {
        width: stage.0,
        height: stage.1,
    };
    let compiled = super::CorrectionsModule::new()
        .compile_bound(
            &layer.effect_id,
            layer.effect_format,
            &layer.payload,
            crate::CompileStage::sampled(
                at,
                Stage {
                    width: full.0,
                    height: full.1,
                },
            ),
            &bound,
            &stack.strokes,
        )
        .expect("a repair layer");
    let Processing::Repair(table) = compiled else {
        panic!("a repair layer compiles to a repair table");
    };
    let entry = crate::render::repair_entry::RepairEntry::new(table, at);
    let len = pixels.len();
    let mut values: Vec<f32> = (0..3)
        .flat_map(|channel| pixels.iter().map(move |pixel| pixel[channel]))
        .collect();
    let mut frame = crate::repair::RepairFrame::new(at, Region::whole(at), &mut values)
        .expect("the whole frame");
    crate::repair::apply(&mut frame, entry.bound(), &Cancel::never()).expect("the CPU repair");
    (0..len)
        .map(|at| [values[at], values[len + at], values[2 * len + at]])
        .collect()
}
