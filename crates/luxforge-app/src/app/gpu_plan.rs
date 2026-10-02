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
//! - [`operation_steps`]: each content operation's units, and a masked one's coverage;
//! - [`spatial_step`]: the spatial operation the content enters, its planes, passes and applies;
//! - [`geometry_steps`]: the geometry tail, through its affine matrix or a warp's coordinate grid,
//!   and after it the output operations, at the output pixel;
//!
//! A part the surface cannot run yet answers the reason ([`Unrunnable`]), and the gesture keeps
//! the CPU path.
//!
//! Every pass the surface draws ends in the CPU's output encoding, whose tables are the core's:
//! [`install_output_encoding`] hands them over once at start.
use luxforge_core::{
    ComponentMode, CoordinateGrid, GpuDescription, GpuGeometry, GpuMask, GpuOperation,
    GpuPassShape, GpuPlaneFormat, GpuPlaneSize, GpuPosition, GpuSpatial, Stage,
};
use luxforge_ui::photo_surface::{
    Coverage, CoverageComponent, CoverageMode, GpuBoundary, GpuPlan, GpuProgram, GpuStep, GpuTail,
    MaskedColour, PositionMap, TexelMap,
    gpu_preview::{self, PassShape, PlaneFormat, PlaneSize},
};
use std::{borrow::Cow, sync::Arc};

/// The core's output quantizer and decode table, which the surface encodes its output and
/// quantizes a JPEG's segment boundaries with.
pub(crate) fn output_encoding() -> luxforge_ui::photo_surface::OutputEncoding {
    luxforge_ui::photo_surface::OutputEncoding {
        thresholds: *luxforge_core::colour::srgb::output_thresholds(),
        decoded: *luxforge_core::colour::srgb::decode_table(),
    }
}

/// Hand the surface [`output_encoding`], once for the process; until it has, no GPU preview pass
/// compiles. Whether the surface holds the core's tables.
pub(crate) fn install_output_encoding() -> bool {
    luxforge_ui::photo_surface::install_output_encoding(output_encoding())
}

/// The core's boundary format as the surface's: half floats on the byte path, `f32` on the linear.
pub(crate) fn boundary_format(
    format: luxforge_core::BoundaryFormat,
) -> luxforge_ui::photo_surface::BoundaryFormat {
    match format {
        luxforge_core::BoundaryFormat::Half => luxforge_ui::photo_surface::BoundaryFormat::Half,
        luxforge_core::BoundaryFormat::Float => luxforge_ui::photo_surface::BoundaryFormat::Float,
    }
}

/// Why the surface cannot run a plan the core answered. Each is a stage the surface does not
/// have yet, or a boundary that does not fit the plan; the gesture takes the CPU path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unrunnable {
    /// A lens or perspective warp's tail with no coordinate grid held for it: the grid is computed
    /// with the boundary, and a warp that needs more nodes than a grid holds has none.
    Grid,
    /// The boundary held does not lie inside the stage the plan's boundary layer receives.
    Boundary { held: (u32, u32), stage: (u32, u32) },
    /// A position map with a coefficient the surface's `f32` words cannot hold exactly.
    Position { layer: usize },
}

impl Unrunnable {
    /// A stable kebab-case name for the reason, for session state and evidence.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Grid => "warp-grid",
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
    surface_plan_at(plan, boundary, (0, 0), None)
}

/// `plan` as the surface's plain data over `boundary`, whose first texel is at `origin` of the
/// plan's boundary stage: the boundary's texel map, then the steps of every content operation in
/// recipe order, the spatial operation's, then the geometry tail's. A windowed proxy's boundary holds the window of the
/// stage its output reads, at that window's origin. A boundary inside a colour run
/// (`plan.boundary.continues_run`) must hold that run's unclamped value; the half floats of a
/// [`GpuBoundary`] do. A warp's tail is drawn through `grid`, the coordinate grid the boundary's
/// job computed for the plan's geometry.
pub(crate) fn surface_plan_at(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
    origin: (u32, u32),
    grid: Option<&CoordinateGrid>,
) -> Result<GpuPlan, Unrunnable> {
    let texels = boundary_map(plan.boundary.stage, &boundary, origin)?;
    let steps = steps_with(plan, Grid::Held(grid))?;
    Ok(GpuPlan {
        boundary,
        texels,
        steps,
        region: None,
    })
}

/// Where a warp's tail takes its coordinate grid from.
#[derive(Clone, Copy)]
pub(crate) enum Grid<'a> {
    /// The grid the boundary's job computed, when it computed one.
    Held(Option<&'a CoordinateGrid>),
    /// No grid at all: the steps only name the pipeline, whose key the grid's nodes are not part
    /// of, as a warm list does.
    Sequence,
}

/// The steps of `plan` alone, without a boundary, a warp's tail through `grid`.
pub(crate) fn steps_with(
    plan: &luxforge_core::GpuPlan,
    grid: Grid<'_>,
) -> Result<Vec<GpuStep>, Unrunnable> {
    let mut steps =
        Vec::with_capacity(plan.operations().map(|op| op.units.len()).sum::<usize>() + 1);
    for operation in &plan.content {
        operation_steps(operation, &mut steps)?;
    }
    if let Some(spatial) = &plan.spatial {
        steps.push(spatial_step(spatial)?);
    }
    geometry_steps(plan, &mut steps, grid)?;
    Ok(steps)
}

/// What the surface's pipeline for `plan` is keyed by — its steps with no grid's nodes — which a
/// warm list names before any boundary or grid exists.
pub(crate) fn plan_steps(plan: &luxforge_core::GpuPlan) -> Result<Vec<GpuStep>, Unrunnable> {
    steps_with(plan, Grid::Sequence)
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
/// the operation's position map; or, for a masked operation, one masked step holding its units and
/// its mask's coverage, blended against the operation's own input.
pub(crate) fn operation_steps(
    operation: &GpuOperation,
    steps: &mut Vec<GpuStep>,
) -> Result<(), Unrunnable> {
    let unrunnable = || Unrunnable::Position {
        layer: operation.layer,
    };
    let position = position_map(operation.position).ok_or_else(unrunnable)?;
    match &operation.mask {
        None => steps.extend(operation.units.iter().map(|unit| GpuStep::Colour {
            program: program(unit),
            position,
        })),
        Some(mask) => steps.push(GpuStep::Masked(MaskedColour {
            units: operation.units.iter().map(program).collect(),
            position,
            mask: coverage(mask).ok_or_else(unrunnable)?,
        })),
    }
    Ok(())
}

/// The spatial operation as the surface's spatial step: the core's static program text, borrowed,
/// with the operation's words, its planes, passes and applies as they are, and a masked
/// operation's coverage, which the step blends its output by against its input.
pub(crate) fn spatial_step(spatial: &GpuSpatial) -> Result<GpuStep, Unrunnable> {
    let mask = match &spatial.mask {
        None => None,
        Some(mask) => Some(coverage(mask).ok_or(Unrunnable::Position {
            layer: spatial.layer,
        })?),
    };
    let index = |value: usize| u32::try_from(value).expect("a plane, word or apply index");
    Ok(GpuStep::Spatial(Box::new(gpu_preview::GpuSpatial {
        program: GpuProgram {
            entry: Cow::Borrowed(spatial.program.entry),
            source: Cow::Borrowed(spatial.program.source),
            words: spatial.words.clone(),
            block: Arc::from([]),
        },
        planes: spatial
            .planes
            .iter()
            .map(|plane| gpu_preview::GpuPlane {
                format: match plane.format {
                    GpuPlaneFormat::Colour => PlaneFormat::Colour,
                    GpuPlaneFormat::Scalar => PlaneFormat::Scalar,
                    GpuPlaneFormat::Pair => PlaneFormat::Pair,
                    GpuPlaneFormat::Quad => PlaneFormat::Quad,
                },
                size: match plane.size {
                    GpuPlaneSize::Reduced(s) => PlaneSize::Reduced(s),
                    GpuPlaneSize::Fixed { width, height } => PlaneSize::Fixed { width, height },
                },
            })
            .collect(),
        passes: spatial
            .passes
            .iter()
            .map(|pass| gpu_preview::GpuPass {
                kernel: Cow::Borrowed(pass.kernel),
                inputs: pass.inputs.iter().map(|&plane| index(plane)).collect(),
                output: index(pass.output),
                words: index(pass.words),
                source: index(pass.source),
                shape: match pass.shape {
                    GpuPassShape::Texels { span } => PassShape::Texels { span },
                    GpuPassShape::Workgroup => PassShape::Workgroup,
                },
            })
            .collect(),
        applies: spatial
            .applies
            .iter()
            .map(|apply| gpu_preview::GpuApply {
                function: Cow::Borrowed(apply.function),
                planes: apply.planes.iter().map(|&plane| index(plane)).collect(),
                words: index(apply.words),
            })
            .collect(),
        clamps: spatial.clamps,
        mask,
    })))
}

/// The core's mask as the surface's coverage: its position map, its bounds as the half-open
/// rectangle they are, the supersample, each component's program with its mode and inversion, the
/// mask's inversion and its amount. `None` for a position map an `f32` cannot hold exactly, its
/// doubled stage under the supersample included.
pub(crate) fn coverage(mask: &GpuMask) -> Option<Coverage> {
    let position = position_map(mask.position)?;
    let bounds = mask.bounds;
    let corners = [
        bounds.x0,
        bounds.y0,
        bounds.x0 + bounds.width,
        bounds.y0 + bounds.height,
    ];
    // A supersampled mask's components address the doubled stage, whose last pixel is twice the
    // bounds' far corner.
    if corners
        .iter()
        .any(|corner| 2 * i64::from(*corner) > EXACT_F32)
    {
        return None;
    }
    Some(Coverage {
        position,
        bounds: corners,
        supersample: mask.supersample,
        components: mask
            .components
            .iter()
            .map(|component| CoverageComponent {
                mode: match component.mode {
                    ComponentMode::Add => CoverageMode::Add,
                    ComponentMode::Subtract => CoverageMode::Subtract,
                    ComponentMode::Intersect => CoverageMode::Intersect,
                },
                invert: component.invert,
                program: program(&component.program),
            })
            .collect(),
        invert: mask.invert,
        scale: mask.scale,
    })
}

/// The geometry tail's steps, appended to `steps`: none for a tail that is the identity over the
/// whole boundary stage with nothing clamped, which leaves the output stage the boundary's and no
/// output operation after it; otherwise the tail ([`GpuTail`]), through the plan's affine matrix
/// or a warp's coordinate `grid`, quantizing where the CPU's segment boundary does, then each
/// output operation's steps at the output pixel ([`operation_steps`]).
pub(crate) fn geometry_steps(
    plan: &luxforge_core::GpuPlan,
    steps: &mut Vec<GpuStep>,
    grid: Grid<'_>,
) -> Result<(), Unrunnable> {
    let geometry = &plan.geometry;
    if identity(geometry, plan.boundary.stage) && plan.output.is_empty() {
        return Ok(());
    }
    let stage = geometry.output();
    let output = (stage.width, stage.height);
    let reads = geometry.reads;
    let reads = [
        reads.x0,
        reads.y0,
        reads.x0 + reads.width,
        reads.y0 + reads.height,
    ];
    let tail = match (geometry.affine(), grid) {
        (Some(matrix), _) => GpuTail::affine(
            output,
            reads,
            geometry.clamps,
            matrix.map(|value| value as f32),
        ),
        (None, Grid::Held(Some(grid))) => GpuTail::grid(
            output,
            reads,
            geometry.clamps,
            grid.origin,
            grid.spacing,
            (grid.columns, grid.rows),
            grid.nodes
                .iter()
                .flat_map(|node| node.map(f32::to_bits))
                .collect(),
        ),
        (None, Grid::Held(None)) => return Err(Unrunnable::Grid),
        (None, Grid::Sequence) => GpuTail::grid(
            output,
            reads,
            geometry.clamps,
            (0, 0),
            1,
            (2, 2),
            std::sync::Arc::from([0u32; 8]),
        ),
    };
    steps.push(GpuStep::Geometry(tail));
    for operation in &plan.output {
        operation_steps(operation, steps)?;
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
