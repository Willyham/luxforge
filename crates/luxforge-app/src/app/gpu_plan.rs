//! The one conversion from the core's GPU plan (`luxforge_core::GpuPlan`) to the photo surface's
//! plain data (`luxforge_ui::photo_surface::GpuPlan`), which the surface evaluates over a held
//! boundary (`docs/design/gpu-preview.md`).
//!
//! The core plans whole stages and names every program by its static WGSL text; the surface takes
//! text, words and blocks and knows no core type. This module is where the two meet. It allocates
//! only the plan's steps, each unit's words and a reference to each storage block, and reads no
//! pixel: the boundary's texels are the caller's, built off the UI thread.
//!
//! The surface runs one pass of colour steps over the boundary today. A plan that needs more — a
//! geometry tail that is not the identity, a mask, a boundary that is not the plan's stage — is
//! answered with the reason the surface cannot run it yet ([`Unrunnable`]), and its gesture keeps
//! the CPU path.
use luxforge_core::{GpuDescription, GpuPosition};
use luxforge_ui::photo_surface::{
    GpuBoundary, GpuPlan, GpuProgram, GpuStep, PositionMap, TexelMap,
};
use std::{borrow::Cow, sync::Arc};

/// Why the surface cannot run a plan the core answered. Each is a stage the surface does not
/// have yet, or a boundary that does not fit the plan; the gesture takes the CPU path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unrunnable {
    /// The geometry tail is not the identity, so output-space steps and a resample would follow
    /// the content pass: the surface has no geometry step yet.
    Geometry,
    /// A masked operation: the surface has no coverage step yet.
    Mask { layer: usize },
    /// The boundary held is not the stage the plan's boundary layer receives.
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

/// `plan` as the surface's plain data over `boundary`, the texels of the plan's whole boundary
/// stage: one colour step per unit of each content operation, in recipe order, each with its
/// operation's position map. A boundary inside a colour run (`plan.boundary.continues_run`) must
/// hold that run's unclamped value; the half floats of a [`GpuBoundary`] do.
pub(crate) fn surface_plan(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
) -> Result<GpuPlan, Unrunnable> {
    let stage = plan.boundary.stage;
    let held = boundary.size();
    if held != (stage.width, stage.height) {
        return Err(Unrunnable::Boundary {
            held,
            stage: (stage.width, stage.height),
        });
    }
    let geometry = &plan.geometry;
    let output = geometry.output();
    let whole = geometry.reads.x0 == 0
        && geometry.reads.y0 == 0
        && (geometry.reads.width, geometry.reads.height) == (stage.width, stage.height);
    if (output.width, output.height) != (stage.width, stage.height)
        || geometry.affine() != Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
        || geometry.clamps
        || !whole
        || !plan.output.is_empty()
    {
        return Err(Unrunnable::Geometry);
    }
    let mut steps = Vec::with_capacity(plan.content.iter().map(|op| op.units.len()).sum());
    for operation in &plan.content {
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
    }
    Ok(GpuPlan {
        boundary,
        texels: TexelMap::IDENTITY,
        steps,
    })
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
fn position_map(position: GpuPosition) -> Option<PositionMap> {
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
