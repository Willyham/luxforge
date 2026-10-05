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
//! - [`surface_lights`]: the light links whose lights the spatial operations read, each writing the
//!   slot's light plane `k`, its place among the plan's lights, which the reading operation's light
//!   plane is converted to;
//!
//! A lens warp's coordinate grid is converted to the words its tail reads once, when the boundary
//! it was computed with is held ([`WarpGrid`]), and every tick's tail shares them.
//!
//! A part the surface cannot run yet answers the reason ([`Unrunnable`]), and the gesture keeps
//! the CPU path.
//!
//! At a percentage zoom the plan's frame is the visible region of the output stage at full scale
//! ([`surface_plan_over`]): the tail draws that rectangle's output pixels, or, with no tail, the
//! content pass reads the rectangle out of the held window, and the surface places the frame at
//! the rectangle.
//!
//! Every pass the surface draws ends in the CPU's output encoding, whose tables are the core's:
//! [`install_output_encoding`] hands them over once at start.
use luxforge_core::{
    ComponentMode, CoordinateGrid, GpuDescription, GpuGeometry, GpuMask, GpuOperation,
    GpuPassShape, GpuPlaneFormat, GpuPlaneSize, GpuPosition, GpuSpatial, Region, Stage,
};
use luxforge_ui::photo_surface::{
    Coverage, CoverageComponent, CoverageMode, GpuBoundary, GpuPlan, GpuProgram, GpuRegion,
    GpuStep, GpuTail, MaskedColour, PositionMap, TexelMap,
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
    /// A lens warp's tail with no coordinate grid held for it: the grid is computed once for the
    /// boundary's key, off the interface thread, and a warp that needs more nodes than a grid holds
    /// has none.
    Grid,
    /// The boundary held does not lie inside the stage the plan's boundary layer receives.
    Boundary { held: (u32, u32), stage: (u32, u32) },
    /// A position map with a coefficient the surface's `f32` words cannot hold exactly.
    Position { layer: usize },
    /// The region a percentage zoom draws does not lie inside the plan's output stage, or, with no
    /// tail, inside the boundary held for it.
    Region,
    /// A spatial operation reads a light the plan holds no light link of that the surface can run:
    /// the core plans one for every operation that reads one, so this is a plan out of step with
    /// its lights.
    Light { layer: usize },
}

impl Unrunnable {
    /// A stable kebab-case name for the reason, for session state and evidence.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Grid => "warp-grid",
            Self::Boundary { .. } => "boundary-size",
            Self::Position { .. } => "position-range",
            Self::Region => "region-outside",
            Self::Light { .. } => "light-link",
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
/// job computed for the plan's geometry, converted once ([`WarpGrid`]).
pub(crate) fn surface_plan_at(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
    origin: (u32, u32),
    grid: Option<&WarpGrid>,
) -> Result<GpuPlan, Unrunnable> {
    surface_plan_over(plan, boundary, origin, grid, None)
}

/// [`surface_plan_at`], at a percentage zoom drawing only `region` of the plan's output stage at
/// full scale: the tail's output is the rectangle, whose first pixel the surface offsets each pixel
/// by, and with no tail the held window must hold the rectangle, which the content pass reads at
/// one texel a pixel. `None` draws the whole output stage.
pub(crate) fn surface_plan_over(
    plan: &luxforge_core::GpuPlan,
    boundary: GpuBoundary,
    origin: (u32, u32),
    grid: Option<&WarpGrid>,
    region: Option<Region>,
) -> Result<GpuPlan, Unrunnable> {
    let texels = boundary_map(plan.boundary.stage, &boundary, origin)?;
    let steps = steps_drawing(
        plan,
        Grid::Held(grid),
        region.map(|rect| (rect.width, rect.height)),
    )?;
    let region = match region {
        None => None,
        Some(rect) => {
            let stage = plan.geometry.output();
            let corners = [rect.x0, rect.y0, rect.x1(), rect.y1()];
            let tail = steps
                .iter()
                .any(|step| matches!(step, GpuStep::Geometry(_)));
            let (held, inside) = (boundary.size(), |at: u32, end: u32, from: u32, to: u32| {
                from <= at && end <= to
            });
            let drawable = rect.width > 0
                && rect.height > 0
                && corners[2] <= stage.width
                && corners[3] <= stage.height
                && (tail
                    || inside(corners[0], corners[2], origin.0, origin.0 + held.0)
                        && inside(corners[1], corners[3], origin.1, origin.1 + held.1));
            if !drawable {
                return Err(Unrunnable::Region);
            }
            Some(GpuRegion {
                rect: corners,
                stage: (stage.width, stage.height),
            })
        }
    };
    Ok(GpuPlan {
        boundary,
        texels,
        steps,
        region,
        lights: surface_lights(plan)?,
    })
}

/// A lens warp's coordinate grid as the tail reads it: where its nodes sit, and every node's
/// `(u, v)` as `f32` bits, row by row. Converted once, when the boundary the grid was computed with
/// is held, and shared by every tick's tail ([`GpuTail::grid`]), so a tick converts and allocates
/// nothing for it: the words take the place of the core grid's nodes, the same 8 bytes a node.
#[derive(Clone, Debug)]
pub(crate) struct WarpGrid {
    origin: (u32, u32),
    spacing: u32,
    size: (u32, u32),
    nodes: Arc<[u32]>,
}

impl WarpGrid {
    /// `grid` as its tail's words, collected into one allocation of exactly their length.
    pub(crate) fn new(grid: &CoordinateGrid) -> Self {
        let nodes = &grid.nodes;
        // A range's map knows its length, so `Arc<[u32]>` is filled in place, with no `Vec`
        // before it.
        let words = (0..2 * nodes.len())
            .map(|word| nodes[word / 2][word % 2].to_bits())
            .collect();
        Self {
            origin: grid.origin,
            spacing: grid.spacing,
            size: (grid.columns, grid.rows),
            nodes: words,
        }
    }
}

/// Where a warp's tail takes its coordinate grid from.
#[derive(Clone, Copy)]
pub(crate) enum Grid<'a> {
    /// The grid the boundary's job computed, converted when it was held, if it computed one.
    Held(Option<&'a WarpGrid>),
    /// No grid at all: the steps only name the pipeline, whose key the grid's nodes are not part
    /// of, as a warm list does.
    Sequence,
}

/// The steps of `plan` alone, without a boundary, a warp's tail through `grid`.
pub(crate) fn steps_with(
    plan: &luxforge_core::GpuPlan,
    grid: Grid<'_>,
) -> Result<Vec<GpuStep>, Unrunnable> {
    steps_drawing(plan, grid, None)
}

/// [`steps_with`], the tail drawing `output` pixels — a percentage zoom's region — in place of its
/// whole output stage when given.
fn steps_drawing(
    plan: &luxforge_core::GpuPlan,
    grid: Grid<'_>,
    output: Option<(u32, u32)>,
) -> Result<Vec<GpuStep>, Unrunnable> {
    let mut steps =
        Vec::with_capacity(plan.operations().map(|op| op.units.len()).sum::<usize>() + 1);
    for operation in &plan.content {
        operation_steps(operation, &mut steps)?;
    }
    for spatial in &plan.spatial {
        steps.push(spatial_step(spatial, light_of(plan, spatial))?);
        for operation in &spatial.after {
            operation_steps(operation, &mut steps)?;
        }
    }
    geometry_steps(plan, &mut steps, grid, output)?;
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

/// The slot's light `k` `spatial` reads, its place among `plan`'s lights; `None` for an operation
/// that reads none.
fn light_of(plan: &luxforge_core::GpuPlan, spatial: &GpuSpatial) -> Option<u32> {
    spatial
        .light
        .and(plan.light_of(spatial.layer))
        .map(|k| u32::try_from(k).expect("a light index"))
}

/// The light links of `plan` as the surface runs them before its steps ([`gpu_preview::light`]),
/// light `k` the `k`-th: each one's colour operations' steps, as a plan's are converted, then its
/// own step, the plane its selection writes the slot's light `k`. A light behind a spatial
/// operation, whose exact input only the picture at rest's sweep of the whole stage computes, is
/// computed by its stand-in over the source with those operations left out
/// ([`luxforge_core::GpuLight::stand_in`]).
pub(crate) fn surface_lights(
    plan: &luxforge_core::GpuPlan,
) -> Result<Vec<gpu_preview::light::GpuLight>, Unrunnable> {
    plan.lights
        .iter()
        .enumerate()
        .map(|(k, light)| {
            let link = match light.over_source() {
                true => light,
                false => light
                    .stand_in
                    .as_deref()
                    .ok_or(Unrunnable::Light { layer: light.layer })?,
            };
            surface_light(link, u32::try_from(k).expect("a light index"))
        })
        .collect()
}

/// One light link over the source as the surface's, writing the slot's light `k`.
pub(crate) fn surface_light(
    light: &luxforge_core::GpuLight,
    k: u32,
) -> Result<gpu_preview::light::GpuLight, Unrunnable> {
    if !light.over_source() {
        return Err(Unrunnable::Light { layer: light.layer });
    }
    let mut steps = Vec::with_capacity(light.content.len() + 1);
    for operation in &light.content {
        operation_steps(operation, &mut steps)?;
    }
    let mut step = spatial_step(&light.light, None)?;
    let GpuStep::Spatial(spatial) = &mut step else {
        unreachable!("a light's step is spatial");
    };
    let written = spatial
        .passes
        .last()
        .map(|pass| pass.output as usize)
        .filter(|&plane| plane < spatial.planes.len())
        .ok_or(Unrunnable::Light { layer: light.layer })?;
    spatial.planes[written].size = PlaneSize::Light(k);
    steps.push(step);
    Ok(gpu_preview::light::GpuLight {
        stage: (light.stage.width, light.stage.height),
        steps,
    })
}

/// The spatial operation as the surface's spatial step: the core's static program text, borrowed,
/// with the operation's words, its planes, passes and applies as they are, the light plane it
/// reads the slot's light `light` ([`GpuSpatial::light`]), and a masked operation's coverage, which
/// the step blends its output by against its input.
pub(crate) fn spatial_step(
    spatial: &GpuSpatial,
    light: Option<u32>,
) -> Result<GpuStep, Unrunnable> {
    let light = match (spatial.light, light) {
        (Some(plane), Some(k)) => Some((plane, k)),
        (None, _) => None,
        (Some(_), None) => {
            return Err(Unrunnable::Light {
                layer: spatial.layer,
            });
        }
    };
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
            .enumerate()
            .map(|(index, plane)| gpu_preview::GpuPlane {
                format: match plane.format {
                    GpuPlaneFormat::Colour => PlaneFormat::Colour,
                    GpuPlaneFormat::Scalar => PlaneFormat::Scalar,
                    GpuPlaneFormat::Pair => PlaneFormat::Pair,
                    GpuPlaneFormat::Quad => PlaneFormat::Quad,
                    GpuPlaneFormat::HalfScalar => PlaneFormat::HalfScalar,
                    GpuPlaneFormat::HalfPair => PlaneFormat::HalfPair,
                },
                size: match (plane.size, light) {
                    (_, Some((read, k))) if read == index => PlaneSize::Light(k),
                    (GpuPlaneSize::Reduced(s), _) => PlaneSize::Reduced(s),
                    (GpuPlaneSize::Fixed { width, height }, _) => {
                        PlaneSize::Fixed { width, height }
                    }
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
                reads_source: pass.reads_source,
                shape: match pass.shape {
                    GpuPassShape::Texels { span } => PassShape::Texels { span },
                    GpuPassShape::Workgroup => PassShape::Workgroup,
                },
                unit: index(pass.unit),
            })
            .collect(),
        applies: spatial
            .applies
            .iter()
            .map(|apply| gpu_preview::GpuApply {
                function: Cow::Borrowed(apply.function),
                planes: apply.planes.iter().map(|&plane| index(plane)).collect(),
                words: index(apply.words),
                identity: apply.identity,
            })
            .collect(),
        clamps: spatial.clamps,
        mask,
        halos: spatial.halos.clone(),
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
/// output operation after it; otherwise the tail ([`GpuTail`]), through the plan's affine matrix,
/// a perspective warp's homography or a lens warp's coordinate `grid`, its words shared, quantizing
/// where the CPU's segment boundary does, then each
/// output operation's steps at the output pixel ([`operation_steps`]). The tail draws `output`
/// pixels when given — a percentage zoom's region, offset by the surface — and the whole output
/// stage otherwise.
pub(crate) fn geometry_steps(
    plan: &luxforge_core::GpuPlan,
    steps: &mut Vec<GpuStep>,
    grid: Grid<'_>,
    output: Option<(u32, u32)>,
) -> Result<(), Unrunnable> {
    let geometry = &plan.geometry;
    if identity(geometry, plan.boundary.stage) && plan.output.is_empty() {
        return Ok(());
    }
    let stage = geometry.output();
    let output = output.unwrap_or((stage.width, stage.height));
    let reads = geometry.reads;
    let reads = [
        reads.x0,
        reads.y0,
        reads.x0 + reads.width,
        reads.y0 + reads.height,
    ];
    let mut tail = match (geometry.affine(), geometry.projective(), grid) {
        (Some(matrix), _, _) => GpuTail::affine(
            output,
            reads,
            geometry.clamps,
            matrix.map(|value| value as f32),
        ),
        (None, Some(matrix), _) => GpuTail::projective(
            output,
            reads,
            geometry.clamps,
            matrix.map(|value| value as f32),
        ),
        (None, None, Grid::Held(Some(grid))) => GpuTail::grid(
            output,
            reads,
            geometry.clamps,
            grid.origin,
            grid.spacing,
            grid.size,
            Arc::clone(&grid.nodes),
        ),
        (None, None, Grid::Held(None)) => return Err(Unrunnable::Grid),
        (None, None, Grid::Sequence) => GpuTail::grid(
            output,
            reads,
            geometry.clamps,
            (0, 0),
            1,
            (2, 2),
            std::sync::Arc::from([0u32; 8]),
        ),
    };
    if plan.linear {
        tail = tail.preserve_f32();
    }
    steps.push(GpuStep::Geometry(tail));
    for operation in &plan.output {
        operation_steps(operation, steps)?;
    }
    Ok(())
}

/// Whether the surface draws `plan` through a geometry tail ([`geometry_steps`]): any geometry but
/// the identity over its whole boundary stage, or any output operation after it.
pub(crate) fn has_tail(plan: &luxforge_core::GpuPlan) -> bool {
    !(identity(&plan.geometry, plan.boundary.stage) && plan.output.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        BASIC_EFFECT, GpuAnswer, GpuPlanRequest, Layer, ModuleRegistry, Recipe, qualification,
    };

    /// A lens warp's tail draws through the grid converted once, as the boundary it was computed
    /// with is held: two ticks of a drag over it — plans whose words differ — hand their tails the
    /// one allocation of the held grid's words, so a tick converts and allocates nothing for it,
    /// and those words are what each tick's own conversion wrote: every node's `(u, v)` as `f32`
    /// bits, row by row.
    #[test]
    fn gpu_plan_a_warp_grid_is_converted_once_and_every_tick_shares_it() {
        let (width, height) = (360, 240);
        let registry = ModuleRegistry::builtin();
        let tick = |exposure: f64| {
            let recipe = Recipe {
                layers: vec![
                    Layer::new(BASIC_EFFECT, serde_json::json!({"exposure": exposure})),
                    qualification::lens_layer(-0.06, (width, height)),
                ],
                ..Recipe::default()
            };
            let request = GpuPlanRequest::exact(0, Stage { width, height }).qualifying();
            match luxforge_core::gpu_plan(&registry, &recipe, request).expect("a stack") {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{reason}"),
            }
        };
        let plans = [tick(0.2), tick(0.45)];
        let geometry = &plans[0].geometry;
        assert!(geometry.needs_grid(), "a lens warp's tail");
        let output = geometry.output();
        let core = geometry
            .grid(
                Region {
                    x0: 0,
                    y0: 0,
                    width: output.width,
                    height: output.height,
                },
                1.0,
            )
            .expect("a grid")
            .expect("a lens warp's grid");
        let held = WarpGrid::new(&core);
        // Each tick converted the nodes so before.
        let converted: Vec<u32> = core
            .nodes
            .iter()
            .flat_map(|node| node.map(f32::to_bits))
            .collect();
        assert_eq!(held.nodes.len(), 2 * core.nodes.len());
        assert_eq!(
            *held.nodes, *converted,
            "bit for bit the per-tick conversion"
        );
        let stage = plans[0].boundary.stage;
        let boundary = || {
            let texels = Arc::new(vec![0u8; (stage.width * stage.height * 8) as usize]);
            let format = luxforge_ui::photo_surface::BoundaryFormat::Half;
            GpuBoundary::new(texels, stage.width, stage.height, 1, format).expect("a boundary")
        };
        let tails: Vec<GpuTail> = plans
            .iter()
            .map(|plan| {
                surface_plan_at(plan, boundary(), (0, 0), Some(&held))
                    .expect("a runnable plan")
                    .steps
                    .into_iter()
                    .find_map(|step| match step {
                        GpuStep::Geometry(tail) => Some(tail),
                        _ => None,
                    })
                    .expect("the warp's tail")
            })
            .collect();
        for tail in &tails {
            assert!(
                Arc::ptr_eq(tail.block(), &held.nodes),
                "the tail holds the held grid's words"
            );
        }
        // The tail is the one each tick built from its own conversion, in every word.
        let reads = plans[0].geometry.reads;
        let rebuilt = GpuTail::grid(
            (output.width, output.height),
            [
                reads.x0,
                reads.y0,
                reads.x0 + reads.width,
                reads.y0 + reads.height,
            ],
            plans[0].geometry.clamps,
            core.origin,
            core.spacing,
            (core.columns, core.rows),
            Arc::from(converted),
        );
        assert_eq!(tails[0], rebuilt);
    }
}
