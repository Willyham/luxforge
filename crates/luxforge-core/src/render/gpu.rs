//! GPU previews, the core's half (`docs/design/gpu-preview.md`): the WGSL programs modules own
//! beside their CPU units, and the plan the compiled evaluation answers for a gesture.
//!
//! - [`program`]: what a module's GPU program is, and the description a unit answers.
//! - [`plan`]: the ordered plan from a draft's boundary to the terminal output, or why there is
//!   none.
//! - [`grid`]: a lens or perspective warp's coordinate grid.
//! - [`preview`]: a draft's GPU preview, planned with its preview job, and the boundary it starts
//!   from.
//!
//! The core names no GPU crate: a program is WGSL text, a plan plain data the desktop hands the
//! photo surface. The CPU stays the only reference, and nothing here changes a CPU byte.
mod grid;
mod plan;
mod preview;
mod program;
mod spatial;

#[cfg(test)]
mod grid_tests;
#[cfg(test)]
mod interpret;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod wgsl_tests;

pub use grid::{CoordinateGrid, GRID_MAX_NODES, GRID_TOLERANCE_PX};
pub use plan::{
    EstimateSource, GpuAnswer, GpuBoundary, GpuClipping, GpuComponent, GpuEstimates, GpuFallback,
    GpuGeometry, GpuMask, GpuOperation, GpuPlan, GpuPlanRequest, GpuPosition, gpu_plan,
    gpu_plan_with,
};
pub use preview::{BoundaryKey, BoundaryRequest, GpuPreview};
pub(crate) use preview::{plan_preview, plan_warm};
#[cfg(test)]
pub(crate) use program::testing;
pub use program::{GpuDescription, GpuProgram, GpuProgramKind};
pub use spatial::{
    GPU_PASS_INPUTS, GPU_SHARED_VALUES, GPU_WORKGROUP_LANES, GpuApply, GpuPass, GpuPassShape,
    GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatial, GpuSpatialUnit,
};
pub(crate) use spatial::{Word, Words};
