//! The one conversion from the core's GPU plan (`luxforge_core::GpuPlan`) to the photo surface's
//! plain data (`luxforge_ui::photo_surface::GpuPlan`), which the surface evaluates over a held
//! boundary (`docs/design/gpu-preview.md`).
//!
//! The core plans whole stages and names every program by its static WGSL text; the surface takes
//! text, words and blocks and knows no core type. This module is where the two meet. It allocates
//! only the plan's steps, each unit's words and a reference to each storage block, and reads no
//! pixel: the boundary's texels are the caller's, built off the UI thread.
//!
//! [`surface_plan`] follows the core plan's parts in order, one function each, so a step kind the
//! surface gains joins the part it belongs to:
//!
//! - [`boundary_map`]: the held boundary against the plan's boundary stage;
//! - [`operation_steps`]: each content operation's units, and where its mask's coverage joins;
//! - [`geometry_steps`]: the geometry tail, and after it the output operations;
//!
//! A part the surface cannot run yet answers the reason ([`Unrunnable`]), and the gesture keeps
//! the CPU path.
use luxforge_core::{GpuDescription, GpuGeometry, GpuOperation, GpuPosition, Stage};
use luxforge_ui::photo_surface::{
    GpuBoundary, GpuPlan, GpuProgram, GpuStep, PositionMap, TexelMap,
};
use std::{borrow::Cow, sync::Arc};

/// Why the surface cannot run a plan the core answered. Each is a stage the surface does not
/// have yet, or a boundary that does not fit the plan; the gesture takes the CPU path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unrunnable {
    /// The geometry tail is not the identity, so a resample, and output-space steps after it,
    /// follow the content pass: the surface has no geometry step yet.
    Geometry,
    /// A masked operation: the surface has no coverage step yet.
    Mask { layer: usize },
    /// The boundary held does not lie inside the stage the plan's boundary layer receives.
    Boundary { held: (u32, u32), stage: (u32, u32) },
    /// A position map with a coefficient the surface's `f32` words cannot hold exactly.
    Position { layer: usize },
}

impl Unrunnable {
    /// A stable kebab-case name for the reason, for session state and evidence.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Geometry => "surface-geometry",
            Self::Mask { .. } => "surface-mask",
            Self::Boundary { .. } => "boundary-size",
            Self::Position { .. } => "position-range",
        }
    }
}

/// The largest coordinate an `f32` holds exactly, and with it every integer a position map adds.
const EXACT_F32: i64 = 1 << 24;

/// `plan` as the surface's plain data over `boundary`, which holds the plan's whole boundary
/// stage: [`surface_plan_at`] at the stage's origin.
pub(crate) fn surface_plan(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
) -> Result<GpuPlan, Unrunnable> {
    surface_plan_at(plan, boundary, (0, 0))
}

/// `plan` as the surface's plain data over `boundary`, whose first texel is at `origin` of the
/// plan's boundary stage: the boundary's texel map, then the steps of every content operation in
/// recipe order, then the geometry tail's. A windowed proxy's boundary holds the window of the
/// stage its output reads, at that window's origin. A boundary inside a colour run
/// (`plan.boundary.continues_run`) must hold that run's unclamped value; the half floats of a
/// [`GpuBoundary`] do.
pub(crate) fn surface_plan_at(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
    origin: (u32, u32),
) -> Result<GpuPlan, Unrunnable> {
    let texels = boundary_map(plan.boundary.stage, &boundary, origin)?;
    let steps = plan_steps(plan)?;
    Ok(GpuPlan {
        boundary,
        texels,
        steps,
    })
}

/// The steps of `plan` alone, without a boundary: what the surface's pipeline for the plan is
/// keyed by, which a warm list names before any boundary exists.
pub(crate) fn plan_steps(plan: &luxforge_core::GpuPlan) -> Result<Vec<GpuStep>, Unrunnable> {
    let mut steps = Vec::with_capacity(plan.operations().map(|op| op.units.len()).sum());
    for operation in &plan.content {
        operation_steps(operation, &mut steps)?;
    }
    geometry_steps(plan, &mut steps)?;
    Ok(steps)
}

/// Where the held boundary's texels are in the plan's boundary stage: the rectangle at `origin`,
/// texel for pixel, which must lie inside the stage.
pub(crate) fn boundary_map(
    stage: Stage,
    boundary: &GpuBoundary,
    origin: (u32, u32),
) -> Result<TexelMap, Unrunnable> {
    let held = boundary.size();
    let inside =
        |at: u32, size: u32, whole: u32| at.checked_add(size).is_some_and(|end| end <= whole);
    if !inside(origin.0, held.0, stage.width) || !inside(origin.1, held.1, stage.height) {
        return Err(Unrunnable::Boundary {
            held,
            stage: (stage.width, stage.height),
        });
    }
    Ok(TexelMap {
        origin: [origin.0 as f32, origin.1 as f32],
        step: [1.0, 1.0],
    })
}

/// One colour operation's steps, appended to `steps`: one colour step per unit, in order, each at
/// the operation's position map. A masked operation's coverage, and the blend against its input,
/// would join here; the surface has no coverage step yet.
pub(crate) fn operation_steps(
    operation: &GpuOperation,
    steps: &mut Vec<GpuStep>,
) -> Result<(), Unrunnable> {
    if operation.mask.is_some() {
        return Err(Unrunnable::Mask {
            layer: operation.layer,
        });
    }
    let position = position_map(operation.position).ok_or(Unrunnable::Position {
        layer: operation.layer,
    })?;
    steps.extend(operation.units.iter().map(|unit| GpuStep::Colour {
        program: program(unit),
        position,
    }));
    Ok(())
}

/// The geometry tail's steps, appended to `steps`: none for a tail that is the identity over the
/// whole boundary stage with nothing clamped, which leaves the output stage the boundary's and no
/// output operation after it. A geometry step, and the output operations through
/// [`operation_steps`] after it, would join here.
pub(crate) fn geometry_steps(
    plan: &luxforge_core::GpuPlan,
    _steps: &mut Vec<GpuStep>,
) -> Result<(), Unrunnable> {
    if !identity(&plan.geometry, plan.boundary.stage) || !plan.output.is_empty() {
        return Err(Unrunnable::Geometry);
    }
    Ok(())
}

/// Whether `geometry` takes every pixel of `stage` to itself: an affine identity onto the same
/// stage, reading the whole of it, with nothing clamped before it.
fn identity(geometry: &GpuGeometry, stage: Stage) -> bool {
    let output = geometry.output();
    let reads = geometry.reads;
    (output.width, output.height) == (stage.width, stage.height)
        && geometry.affine() == Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
        && !geometry.clamps
        && (reads.x0, reads.y0, reads.width, reads.height) == (0, 0, stage.width, stage.height)
}

/// One unit's description as the surface's program: the core's static text, borrowed, its words
/// and its storage block, shared.
pub(crate) fn program(description: &GpuDescription) -> GpuProgram {
    GpuProgram {
        entry: Cow::Borrowed(description.program.entry),
        source: Cow::Borrowed(description.program.source),
        words: description.words.clone(),
        block: description.block.clone().unwrap_or_else(|| Arc::from([])),
    }
}

/// The core's exact map as the surface's, when every coefficient is an integer an `f32` holds.
pub(crate) fn position_map(position: GpuPosition) -> Option<PositionMap> {
    let GpuPosition { a, b, tx, c, d, ty } = position;
    let narrow = |value: i64| {
        (value.abs() <= EXACT_F32)
            .then(|| i32::try_from(value).ok())
            .flatten()
    };
    Some(PositionMap {
        a: narrow(a)?,
        b: narrow(b)?,
        tx: narrow(tx)?,
        c: narrow(c)?,
        d: narrow(d)?,
        ty: narrow(ty)?,
    })
}
