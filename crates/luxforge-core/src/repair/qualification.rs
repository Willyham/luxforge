//! Qualification only: the repair units over a whole frame and one group's GPU description, for the
//! desktop's `gpu_repair` readback tests, which hold `repair.wgsl` to these units on synthetic
//! frames and measure its tick. Built only with the core's `qualification` feature, which only a
//! `[dev-dependencies]` table may turn on, so no binary has it.
use super::{RepairFrame, RepairKind, RepairOp, apply, gpu};
use crate::{
    Cancel,
    modules::{Region, Stage},
    path,
};

pub use super::gpu::{RepairGpu, RepairPass};

/// The repair program's WGSL.
pub fn program_source() -> &'static str {
    super::REPAIR_PROGRAM.source
}

/// One operation: a stored path (normalized positions, `x` a fraction of the width), its radius in
/// content heights, feather and opacity `0..=100`, and its offset in grid steps.
#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub heal: bool,
    pub points: Vec<[f64; 2]>,
    pub radius: f64,
    pub feather: f64,
    pub opacity: f64,
    pub offset: [i32; 2],
}

impl Operation {
    /// A Heal or Clone over the path `points_px` in pixels of a `width x height` stage, `radius_px`
    /// pixels, its source `source_px` pixels away, every value put on the stored grid.
    pub fn pixels(
        heal: bool,
        (width, height): (u32, u32),
        points_px: &[[f64; 2]],
        radius_px: f64,
        source_px: [f64; 2],
    ) -> Self {
        let (w, h) = (f64::from(width), f64::from(height));
        let grid = |v: f64| path::steps_to_units(path::quantize(v));
        Self {
            heal,
            points: points_px
                .iter()
                .map(|[x, y]| [grid(x / w), grid(y / h)])
                .collect(),
            radius: grid(radius_px / h),
            feather: 50.0,
            opacity: 100.0,
            offset: [
                path::quantize(source_px[0] / w),
                path::quantize(source_px[1] / h),
            ],
        }
    }

    fn compile(&self, stage: Stage) -> RepairOp {
        RepairOp::new(
            if self.heal {
                RepairKind::Heal
            } else {
                RepairKind::Clone
            },
            &self.points,
            self.radius,
            self.feather,
            self.opacity,
            self.offset,
            stage,
        )
    }
}

fn stage((width, height): (u32, u32)) -> Stage {
    Stage { width, height }
}

/// `ops` in order over a whole `size` frame of planar `f32` RGB (R plane, then G, then B), in
/// place: the reference renderer's units.
pub fn cpu(size: (u32, u32), planes: &mut [f32], ops: &[Operation]) -> Result<(), String> {
    let stage = stage(size);
    let compiled: Vec<RepairOp> = ops.iter().map(|op| op.compile(stage)).collect();
    let mut frame =
        RepairFrame::new(stage, Region::whole(stage), planes).map_err(|e| e.to_string())?;
    apply(&mut frame, &compiled, &Cancel::never()).map_err(|e| e.to_string())
}

/// The GPU description of the group `ops` on a `size` stage, each level's tile capacity at least
/// `capacity`'s.
pub fn describe(size: (u32, u32), ops: &[Operation], capacity: Option<&[u32]>) -> RepairGpu {
    let stage = stage(size);
    let compiled: Vec<RepairOp> = ops.iter().map(|op| op.compile(stage)).collect();
    gpu::describe(&compiled, stage, capacity)
}

/// The tiles each level of the group `ops` places on a `size` stage.
pub fn tile_counts(size: (u32, u32), ops: &[Operation]) -> Vec<u32> {
    let stage = stage(size);
    let compiled: Vec<RepairOp> = ops.iter().map(|op| op.compile(stage)).collect();
    gpu::tile_counts(&compiled)
}
