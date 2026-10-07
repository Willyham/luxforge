//! A gesture drawn on the GPU (`docs/design/gpu-preview.md`, "The GPU source" and "A tick"): the
//! desktop's half, between the owner's plan and the photo surface's GPU stage.
//!
//! - **The plan rides the tick's own answer.** Each `draft.set` is answered with its preview job,
//!   synchronously on this thread ([`super::tasks::draft_set_now`]), and the catalog owner plans
//!   the draft's GPU preview with that job (its `gpu` field): the plan in `O(layers)`, or its
//!   reason, and the boundary it starts from. It is preview state in the desktop's typed owner
//!   reply, never an API result, and a tick adds no hop for it (performance rule 12). A set that
//!   reads a pixel, a colour-limited stroke's first, answers a hop later instead, with its plan
//!   ([`super::tasks::draft_set_task`]): this thread never waits on a pixel read.
//! - **The source and its boundaries.** Every plan starts from the source. The desktop hands the
//!   photo surface the prepared source of the photograph on screen once ([`GpuSource`]), from the
//!   pixels the preview jobs already share, and lets its pixels go once the surface holds them; the
//!   surface uploads it a frame's rows at a time and holds it for every surface. A plan's boundary
//!   is derived from it on the GPU the moment the plan names one ([`GpuBoundary::derived`]): the
//!   source reduced to the plan's proxy at Fit and below 100%, a window of it cut at full scale at
//!   100% and above. No preview job carries a boundary, and none is rendered on the CPU. A lens
//!   warp's coordinate grid is the one part computed on the CPU, once per key, on the runtime's
//!   blocking pool; until it arrives the tick names `boundary-pending`.
//! - **A tick on the GPU.** With the boundary held and the plan converted, the surface draws the
//!   plan in the frame after the update that handled the input. Once the surface reports that it
//!   evaluated this boundary's plan with no fallback — its pipeline is ready and its slot holds the
//!   boundary — a tick makes no preview job and no upload: the converted plan's words are all it
//!   changes. A tick the GPU does not draw holds its frame for a reason that passes within a tick
//!   or an upload, and otherwise has the reference renderer draw one whole frame of the drafted
//!   stack, the newest tick's next (`Editor::cpu_tick`), so a gesture never waits on the GPU.
//! - **Settlement.** The release commits, and the GPU draws the committed stack at rest from its
//!   job's plans, dissolving in from the drag's last GPU frame; where the GPU cannot plan the
//!   committed stack, the reference's frame of it dissolves in instead (`super::gpu_settle`).
//! - **Lifetime.** A drag's boundary is held while its draft is open and, once the draft ends —
//!   commit or cancel — and the frame that replaces the drafted one is drawn, kept as the resident
//!   boundary, with the stack's own plan drawn at rest, so the screen never falls back to an older
//!   drafted frame and the next gesture over the stack starts from it. A tick whose plan names
//!   another key releases it; so does another photograph.
//! - **The resident boundary.** A committed stack's preview job carries the stack's own plan and
//!   the boundary every gesture over it starts from (its `gpu_rest` field's view plan): at the
//!   job's bounds, at Fit and below 100%, and for a view at 100% or more over its region. The
//!   boundary is derived from the source as the job is planned and held as the resident one, the
//!   stack's plan drawn at rest, so a gesture's first tick draws on the GPU.
//! - **Incremental ticks.** Every plan handed to the surface carries a serial and what changed
//!   since a plan of the last 16 handed that the surface evaluated (`Stamps::hand`, from the core's
//!   `GpuPlan::changes_since`): a painted tick's rectangle, so the surface evaluates each link of
//!   the chain only where that change reaches.
//! - **Warming.** A committed stack's preview job carries the plans its gestures are likely to draw
//!   (its `gpu_warm` field), and the surface compiles their sequences before a drag begins.
//! - **Below 100%.** A percentage view below 100% draws the displayed-size proxy of the whole
//!   stage, as Fit draws the display-bounded one, so a tick asks for Fit's plan at the job's bounds
//!   ([`GpuAsk::Fit`]), which there are the stage's displayed size: the boundary, the resident
//!   boundary and the warm list are that size's, held, keyed and bounded as Fit's, and a pan, which
//!   leaves the whole frame as it is, keeps them. The surface draws the plan's whole frame in place
//!   of the reference's frame, through its placement, snapping and filter.
//! - **At 100% and above.** A tick asks for the plan over the visible region of the output stage
//!   at full scale ([`GpuAsk::Region`]), or over the region its drag already asked for while that
//!   still holds the view, so a pan inside it keeps the boundary. The boundary is that region's
//!   window, and the surface draws the plan's frame alone at the region's place in the
//!   photograph. While the frame holds the view it answers it, and nothing is rendered on the CPU.
//!   A pan past it plans the view again as the drag's next tick ([`Editor::drag_view_planned`]),
//!   the frame on screen held until the new region's boundary is. A region whose slot — everything
//!   the surface charges it but its links' words and blocks buffers ([`region_charge`]) — would
//!   pass the GPU-preview budget is drawn as the softer frame from the GPU's reduced stage, and
//!   held or drawn by the reference past that, naming the budget. The mask overlay's region
//!   coverage is laid over the GPU region frame.
use super::{Editor, gpu_plan};
use crate::state::status::CpuReason;
use luxforge_core::{
    BoundaryKey, CoordinateGrid, Draft, DraftId, GpuAnswer, GpuPreview, LinearImage, PreviewSource,
    ProxyCoverage, ProxyIdentity, Region, SourceBoundary,
};
use luxforge_ui::photo_surface::{
    self as surface, AxisCoverage, Derivation, DrawingPath, GpuBoundary, GpuSource, GpuStep,
    GpuWarm, Reduction, SurfaceDiagnostics,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// The core's plan, beside the surface's plain data of the same name.
type CorePlan = luxforge_core::GpuPlan;
/// The surface's fallback, beside the core's reason of the same name.
type SurfaceFallback = surface::GpuFallback;

/// What the surface reports of its last frame that decides a tick's path: the boundary of the
/// plan it last evaluated, drawn or held, and why it fell back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SurfaceReport {
    pub(crate) ready_boundary: Option<u64>,
    pub(crate) fallback: Option<SurfaceFallback>,
    /// The boundary and draft revision whose GPU output the last frame drew.
    pub(crate) drawn: Option<(u64, u64)>,
    /// The serial of the plan whose values the surface's slot holds ([`surface::GpuChange`]).
    pub(crate) evaluated: Option<u64>,
}

impl SurfaceReport {
    fn of(diagnostics: &SurfaceDiagnostics) -> Self {
        Self {
            ready_boundary: diagnostics.gpu_ready_boundary,
            fallback: diagnostics.gpu_fallback,
            drawn: (diagnostics.drawn_path == Some(DrawingPath::Gpu))
                .then(|| {
                    diagnostics
                        .drawn_gpu_boundary
                        .zip(diagnostics.drawn_gpu_tag)
                })
                .flatten(),
            evaluated: diagnostics.gpu_evaluated_serial,
        }
    }
}

/// A boundary held for the open draft or between drafts: its key, the boundary as the surface
/// derives it from the source it holds, and where it lies in the plan's boundary stage.
struct Held {
    key: BoundaryKey,
    boundary: GpuBoundary,
    origin: (u32, u32),
    /// A warp tail's coordinate grid, computed off the interface thread and converted to its
    /// tail's words once, as it is held, for every tick drawn from it to share; `None` for an
    /// affine tail, or a warp whose grid could not be built, which then keeps the CPU path.
    grid: Option<gpu_plan::WarpGrid>,
    /// What that grid is a function of.
    grid_key: Option<GridKey>,
}

impl Held {
    /// Whether a plan asking for `request` draws from this boundary: its key, and the grid of its
    /// lens warp, which a committed lens change over the same key makes another.
    fn serves(&self, request: &SourceBoundary) -> bool {
        self.key == request.key && self.grid_key == GridKey::of(request)
    }
}

/// The prepared source of the photograph on screen as the photo surface holds it on the GPU
/// (`docs/design/gpu-preview.md`, "The GPU source"), and what the desktop knows of it.
struct HeldSource {
    identity: ProxyIdentity,
    /// The content stage it fills, which a cut addresses and a reduction covers.
    stage: (u32, u32),
    /// As the surfaces are handed it: with its pixels until the surface reports it holds them all,
    /// then without, so the desktop keeps no reference to them.
    gpu: GpuSource,
    /// The photograph it was held for: another lets it go.
    asset: Option<luxforge_core::AssetId>,
    /// Why the surface could not hold it, which every boundary of it names until a job hands its
    /// pixels again.
    refused: Option<&'static str>,
}

/// The picture at rest the surfaces draw in tiles (`docs/design/gpu-preview.md`, "The picture at
/// rest"): the core's tiles for the displayed stack, from its job or its exact phase, and the
/// surfaces' plain data once converted — the picture, reduced to the view, where the view draws
/// the stage smaller than it is, and the same tiles for their histogram and clipping counts alone
/// (`docs/design/gpu-first.md`, stage 2).
/// A crop draft's input stage the GPU draws ([`Editor::gpu_stage_from`]): the layer prefix's tiles
/// at full resolution, reduced to the stage's display bounds, as the surfaces are handed them once
/// converted, or why they cannot be.
struct StageRest {
    tiles: Box<luxforge_core::RestTiles>,
    version: u64,
    gpu: Option<surface::GpuRest>,
    refused: Option<&'static str>,
}

/// Where a crop draft's input stage on the GPU has got to ([`Editor::gpu_stage_state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageState {
    /// No stage is drawn on the GPU.
    None,
    /// Its tiles are being converted or drawn.
    Pending,
    /// The surface drew its last tile: the stage under the frame is the GPU's.
    Drawn(u64),
    /// The GPU cannot draw it, for this reason: the reference renders the stage.
    Refused(&'static str),
}

struct HeldRest {
    tiles: Box<luxforge_core::RestTiles>,
    /// Handed to the surfaces, which start over whenever it changes: the picture's version.
    version: u64,
    /// The counts' own variant's version, for the tiles drawn with no reduction.
    counts_version: u64,
    /// The picture as the surfaces are handed it, or the counts' variant where the tiles have no
    /// reduction; `None` until a lens warp's stage grid is held.
    gpu: Option<surface::GpuRest>,
    /// The tiles drawn for their counts alone: handed where the picture is not, while the counts
    /// are wanted.
    counts: Option<surface::GpuRest>,
    /// Why the tiles cannot be drawn on the GPU: their source is not the one the surface holds, or
    /// a tile's plan is one the surface cannot run.
    refused: Option<&'static str>,
    /// The content the GPU presents is this stack's, whose counts are its report
    /// ([`Editor::present_on_gpu`]), and they have not been taken up yet.
    wanted: bool,
}

/// `tiles` as the surfaces draw them under `version`: each tile's plan over its window cut from the
/// source the surface holds, with no clipping marks, through its part of a lens warp's grid of the
/// whole output stage ([`GridKey::stage`]). `Ok(None)` while that grid is computed; the reason when
/// the source is another or a tile's plan is one the surface cannot run. `O(tiles × steps)`, no
/// pixel.
fn rest_of(
    source: Option<&HeldSource>,
    versions: &mut u64,
    grids: &mut Grids,
    tiles: &luxforge_core::RestTiles,
    version: u64,
) -> Result<Option<surface::GpuRest>, &'static str> {
    let source = source
        .filter(|source| source.identity == tiles.source)
        .ok_or("source-missing")?;
    if let Some(reason) = source.refused {
        return Err(reason);
    }
    let stage = match tiles.warp() {
        None => None,
        Some(warp) => match grids.of(GridKey::stage(warp)) {
            Err(()) => return Ok(None),
            Ok(None) => return Err("warp-grid"),
            Ok(Some(grid)) => Some(grid),
        },
    };
    rest_over(&source.gpu, stage.as_deref(), tiles, versions, version).map(Some)
}

/// `tiles` over the source `gpu` as the surfaces draw them under `version`, through `stage`, the
/// whole output stage's grid of their lens warp when they draw through one: each tile's plan over
/// its window cut from the source under a version of its own from `versions`, with no clipping
/// marks. The reason when a tile's plan is one the surface cannot run. `O(tiles × steps)`, no
/// pixel.
fn rest_over(
    gpu: &GpuSource,
    stage: Option<&CoordinateGrid>,
    tiles: &luxforge_core::RestTiles,
    versions: &mut u64,
    version: u64,
) -> Result<surface::GpuRest, &'static str> {
    let mut plans = Vec::with_capacity(tiles.tiles.len());
    for tile in &tiles.tiles {
        let window = tile.window;
        *versions += 1;
        let boundary = GpuBoundary::derived(
            gpu,
            Derivation::Cut {
                origin: (window.x0, window.y0),
            },
            window.width,
            window.height,
            *versions,
        )
        .ok_or("boundary-size")?;
        let grid = match stage {
            None => None,
            Some(stage) => Some(gpu_plan::WarpGrid::new(
                &stage.part(tile.rect).ok_or("warp-grid")?,
            )),
        };
        plans.push(
            gpu_plan::surface_plan_over(
                &tiles.plan,
                boundary,
                (window.x0, window.y0),
                grid.as_ref(),
                Some(tile.rect),
            )
            .map_err(|unrunnable| unrunnable.code())?,
        );
    }
    // The same picture in staged sweeps, where the core planned it so: the first sweep's
    // boundaries cut from the source, a later sweep's copied out of the stage the one before it
    // wrote, the last's drawn through its part of the lens warp's grid as a chained tile is.
    let stages = match &tiles.staging {
        luxforge_core::GpuStaging::Chained(_) => None,
        luxforge_core::GpuStaging::Staged(planned) => {
            let format = gpu_plan::boundary_format(planned.format);
            let mut sweeps = Vec::with_capacity(planned.sweeps.len());
            for sweep in &planned.sweeps {
                let mut plans = Vec::with_capacity(sweep.tiles.len());
                for tile in &sweep.tiles {
                    let window = tile.window;
                    *versions += 1;
                    let boundary = match sweep.reads {
                        None => GpuBoundary::derived(
                            gpu,
                            Derivation::Cut {
                                origin: (window.x0, window.y0),
                            },
                            window.width,
                            window.height,
                            *versions,
                        ),
                        Some(_) => {
                            GpuBoundary::staged(window.width, window.height, *versions, format)
                        }
                    }
                    .ok_or("boundary-size")?;
                    let grid = match (stage, sweep.last) {
                        (Some(stage), true) => Some(gpu_plan::WarpGrid::new(
                            &stage.part(tile.rect).ok_or("warp-grid")?,
                        )),
                        _ => None,
                    };
                    plans.push(
                        gpu_plan::sweep_plan_over(
                            &tiles.plan,
                            sweep,
                            boundary,
                            (window.x0, window.y0),
                            grid.as_ref(),
                            tile.rect,
                        )
                        .map_err(|unrunnable| unrunnable.code())?,
                    );
                }
                sweeps.push(surface::RestSweep {
                    tiles: plans.into(),
                    reads: sweep.reads,
                    writes: sweep.writes,
                });
            }
            Some(surface::RestStages {
                stage: (planned.stage.width, planned.stage.height),
                format,
                textures: planned.textures,
                sweeps: sweeps.into(),
            })
        }
    };
    // Drawn chained with a light behind a spatial layer, the light sweeps that compute it first,
    // each tile cut from the source.
    let mut light_sweeps = Vec::with_capacity(tiles.light_sweeps.len());
    for planned in &tiles.light_sweeps {
        let sweep = gpu_plan::light_sweep_as_sweep(&tiles.plan, planned);
        let mut plans = Vec::with_capacity(planned.tiles.len());
        for tile in &planned.tiles {
            let window = tile.window;
            *versions += 1;
            let boundary = GpuBoundary::derived(
                gpu,
                Derivation::Cut {
                    origin: (window.x0, window.y0),
                },
                window.width,
                window.height,
                *versions,
            )
            .ok_or("boundary-size")?;
            plans.push(
                gpu_plan::sweep_plan_over(
                    &tiles.plan,
                    &sweep,
                    boundary,
                    (window.x0, window.y0),
                    None,
                    tile.rect,
                )
                .map_err(|unrunnable| unrunnable.code())?,
            );
        }
        light_sweeps.push(surface::RestLightSweep {
            light: gpu_plan::light_sweep_light(&tiles.plan, planned)
                .map_err(|unrunnable| unrunnable.code())?,
            side: planned.side,
            format: gpu_plan::boundary_format(tiles.format),
            tiles: plans.into(),
        });
    }
    Ok(surface::GpuRest {
        version,
        tiles: plans.into(),
        light_sweeps: light_sweeps.into(),
        stages,
        reduction: tiles
            .reduction
            .as_ref()
            .map(|reduction| surface::RestReduction {
                view: reduction.view,
                across: axis(reduction.across.clone()),
                down: axis(reduction.down.clone()),
            }),
    })
}

/// For the release gate's harness: `tiles` over the source `gpu` as the surfaces draw them, the
/// whole output stage's grid of a lens warp computed here rather than off the interface thread.
#[cfg(test)]
pub(crate) fn rest_now(
    gpu: &GpuSource,
    tiles: &luxforge_core::RestTiles,
    version: u64,
) -> Result<surface::GpuRest, String> {
    let stage = tiles
        .warp()
        .map(|warp| grid_of(&GridKey::stage(warp)))
        .transpose()?;
    let mut versions = version;
    rest_over(gpu, stage.as_deref(), tiles, &mut versions, version).map_err(str::to_owned)
}

/// A boundary derived for the release gate's harness, where its texels lie in the plan's boundary
/// stage, and its lens warp's grid.
#[cfg(test)]
pub(crate) type DerivedNow = (GpuBoundary, (u32, u32), Option<gpu_plan::WarpGrid>);

/// For the release gate's harness: the boundary `request` names derived from the source `gpu`, as
/// a drag or the stack at rest holds it, under `version`, with where its texels lie in the plan's
/// boundary stage and a lens warp's grid, computed here rather than off the interface thread; or
/// the budget it would pass, as the editor refuses it ([`over_budget`]), naming `budget-exceeded`.
#[cfg(test)]
pub(crate) fn derived_now(
    gpu: &GpuSource,
    plan: &CorePlan,
    request: &SourceBoundary,
    version: u64,
) -> Result<DerivedNow, String> {
    let budget = surface::gpu_preview::GPU_PREVIEW_BUDGET;
    if let Some((requested, bound)) = over_budget(plan, request, budget) {
        return Err(format!(
            "budget-exceeded: {requested} B past the {bound} B the editor holds a boundary and its \
             slot to"
        ));
    }
    let grid = GridKey::of(request)
        .map(|key| grid_of(&key))
        .transpose()?
        .map(|grid| gpu_plan::WarpGrid::new(&grid));
    let Derived {
        derivation,
        size,
        origin,
    } = derivation_of(request, gpu.stage())?;
    let boundary =
        GpuBoundary::derived(gpu, derivation, size.0, size.1, version).ok_or("boundary-size")?;
    Ok((boundary, origin, grid))
}

/// A RAW development's planes as the GPU source uploads them, borrowed through a clone of the image,
/// which shares its planes and keeps the source worker's memory gate counting them while it lives.
struct DevelopedPlanes(LinearImage);

impl AsRef<[f32]> for DevelopedPlanes {
    fn as_ref(&self) -> &[f32] {
        self.0.shared_planes().0
    }
}

/// `source` as the photo surface holds it on the GPU, under `version`: a JPEG's upright codes, or a
/// RAW development's planes through its view, each shared with the preview job, never copied.
pub(crate) fn gpu_source_of(version: u64, source: &PreviewSource) -> Option<GpuSource> {
    match source {
        PreviewSource::Jpeg(image) => {
            GpuSource::codes(version, Arc::clone(&image.rgba), image.width, image.height)
        }
        PreviewSource::Raw { image, .. } => {
            let (_, base, crop, orientation) = image.shared_planes();
            GpuSource::planes(
                version,
                Arc::new(DevelopedPlanes(image.clone())),
                base,
                crop,
                orientation,
            )
        }
    }
}

/// The surface's coverage of one axis of a proxy's area average, from the core's, each weight
/// narrowed to `f32` once.
pub(crate) fn axis(coverage: ProxyCoverage) -> AxisCoverage {
    AxisCoverage {
        first: coverage.first,
        offsets: coverage.offsets,
        weights: coverage
            .weights
            .iter()
            .map(|weight| *weight as f32)
            .collect(),
    }
}

/// How a boundary is derived from the source, its size, and its origin in the plan's boundary
/// stage.
struct Derived {
    derivation: Derivation,
    size: (u32, u32),
    origin: (u32, u32),
}

/// How the boundary `request` names is derived from a source filling `stage`: the source reduced
/// to the key's proxy plan with the CPU proxy build's own coverage, the window of the proxy stage
/// the plan holds, at Fit and below 100%; a window of the source cut at full scale at the exact
/// stage, at Fit or over a region at 100% or more. `O(source + proxy)` for a reduction's coverage,
/// no pixel.
fn derivation_of(request: &SourceBoundary, stage: (u32, u32)) -> Result<Derived, String> {
    match request.key.plan() {
        Some(plan) => {
            let [across, down] = plan.coverage(stage).map_err(|error| error.detail)?;
            let [x, y, width, height] = plan.held();
            Ok(Derived {
                derivation: Derivation::Reduce(Arc::new(Reduction {
                    origin: (x, y),
                    across: axis(across),
                    down: axis(down),
                })),
                size: (width, height),
                origin: (x, y),
            })
        }
        None => {
            let window = request.window.unwrap_or(Region {
                x0: 0,
                y0: 0,
                width: stage.0,
                height: stage.1,
            });
            Ok(Derived {
                derivation: Derivation::Cut {
                    origin: (window.x0, window.y0),
                },
                size: (window.width, window.height),
                origin: (window.x0, window.y0),
            })
        }
    }
}

/// The boundary `request` names, derived on the GPU from the source the surface holds
/// ([`GpuBoundary::derived`]) under a new version, with where it lies in the plan's boundary stage
/// and a lens warp's grid. Refused with the tick's reason: `source-missing` when the desktop holds
/// no source of the request's, `boundary-pending` while a lens warp's grid is computed — asked for
/// here the first time its key is — and `boundary-size` for a derivation the source does not fit.
/// `O(source + proxy)` for a reduction's coverage; no pixel is read.
fn derive_held(
    source: Option<&HeldSource>,
    versions: &mut u64,
    grids: &mut Grids,
    request: &SourceBoundary,
) -> Result<Held, &'static str> {
    let source = source
        .filter(|source| source.identity == *request.key.source())
        .ok_or("source-missing")?;
    if let Some(reason) = source.refused {
        return Err(reason);
    }
    // A warp whose grid could not be built is held with none, and its conversion names
    // `warp-grid`, as before.
    let grid = match GridKey::of(request) {
        None => None,
        Some(key) => grids
            .of(key)
            .map_err(|()| "boundary-pending")?
            .map(|grid| gpu_plan::WarpGrid::new(&grid)),
    };
    let Derived {
        derivation,
        size,
        origin,
    } = derivation_of(request, source.stage).map_err(|_| "boundary-size")?;
    *versions += 1;
    let boundary = GpuBoundary::derived(&source.gpu, derivation, size.0, size.1, *versions)
        .ok_or("boundary-size")?;
    Ok(Held {
        key: request.key.clone(),
        boundary,
        origin,
        grid,
        grid_key: GridKey::of(request),
    })
}

/// How a boundary is derived, as evidence names it: `reduce` to a proxy, a `cut` of the source at
/// full scale, or `texels` handed whole.
fn derivation_name(boundary: &GpuBoundary) -> &'static str {
    match boundary.derivation() {
        Some((_, Derivation::Reduce(_))) => "reduce",
        Some((_, Derivation::Cut { .. })) => "cut",
        None => "texels",
    }
}

/// Each light `plan` reads, as evidence names it: its layer, and `source` for a light the slot
/// computes from the source through the prefix's colour, or `stand-in` for one behind a spatial
/// layer, which the slot computes with that layer left out.
fn light_evidence(plan: &CorePlan) -> Value {
    plan.lights
        .iter()
        .map(|light| {
            json!({"layer": light.layer,
                "computed": if light.over_source() { "source" } else { "stand-in" }})
        })
        .collect()
}

/// What one evaluation of a surface's slot did, as evidence records it: a tick's frame
/// (`surface_frame_drawn`, the state's `gpu_evaluation`) or a picture at rest's tiles summed.
pub(crate) fn evaluation_record(figures: &surface::EvaluationFigures) -> Value {
    json!({"refits": figures.refits, "rebinds": figures.rebinds,
        "links_run": figures.links_run, "spatial_passes": figures.spatial_passes,
        "lights_encoded": figures.lights_encoded, "lights_restored": figures.lights_restored,
        "window_texels": figures.window_texels, "link_texels": figures.link_texels})
}

/// What a picture at rest's tiles did, as evidence records it beside its timing: their
/// evaluations summed, the frames a tile waited for a retirement, and each tile's GPU span from
/// its preparation to when the interface learned the GPU had finished it, an upper bound reported
/// as the queue completes each tile.
pub(crate) fn rest_attribution(figures: &surface::RestFigures) -> Value {
    json!({"evaluation": evaluation_record(&figures.evaluation),
        "retirement_waits": figures.retirement_waits,
        "gpu_tiles_reported": figures.gpu_tiles,
        "gpu_span_ms": figures.gpu_us as f64 / 1000.0,
        "gpu_span_max_ms": figures.gpu_max_us as f64 / 1000.0})
}

/// The evidence of a boundary derived from the source, for a drag or, `resident`, a committed
/// stack's job.
fn derived_evidence(held: &Held, resident: bool) -> Value {
    json!({"held": true, "resident": resident, "version": held.boundary.version(),
        "width": held.boundary.size().0, "height": held.boundary.size().1,
        "origin": [held.origin.0, held.origin.1], "derived": derivation_name(&held.boundary),
        "source": held.boundary.derivation().map(|(source, _)| *source)})
}

/// A lens warp's grid for `key`, or why it could not be built: the part over its region of the whole
/// output stage's grid at its magnification ([`luxforge_core::GpuGeometry::grid`]). Frame work: run
/// off the interface thread.
fn grid_of(key: &GridKey) -> Result<Arc<CoordinateGrid>, String> {
    key.warp
        .grid(key.region, f64::from_bits(key.magnification))
        .map_err(|error| error.to_string())?
        .map(Arc::new)
        .ok_or_else(|| "a warp tail with no grid".into())
}

/// What a lens warp's coordinate grid is a function of: the warp's geometry tail, the
/// magnification the grid is made dense enough for, and the region of the output stage it covers.
/// A boundary key does not name the geometry after the source, so a committed lens change over the
/// same source and view is another grid of the same key.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GridKey {
    warp: luxforge_core::GpuGeometry,
    magnification: u64,
    region: Region,
}

impl GridKey {
    /// The grid `request`'s plan draws through; `None` for a plan with no lens warp.
    pub(crate) fn of(request: &SourceBoundary) -> Option<Self> {
        let (warp, magnification) = request.warp()?;
        Some(Self {
            warp: warp.clone(),
            magnification: magnification.to_bits(),
            region: request.grid_region()?,
        })
    }

    /// The whole output stage's grid of `warp` at one display pixel an output pixel, which every
    /// tile of a picture at rest takes its part of.
    fn stage(warp: &luxforge_core::GpuGeometry) -> Self {
        let output = warp.output();
        Self {
            warp: warp.clone(),
            magnification: 1.0f64.to_bits(),
            region: Region {
                x0: 0,
                y0: 0,
                width: output.width,
                height: output.height,
            },
        }
    }
}

/// A lens warp's coordinate grid for one grid key, computed off the interface thread.
enum GridState {
    /// Asked for; the task that computes it starts after the message that asked.
    Wanted,
    Computing,
    Ready(Arc<CoordinateGrid>),
    /// The warp needs more nodes than a grid holds, or a degenerate map: the drag keeps the CPU
    /// path, naming `warp-grid`.
    Failed,
}

/// The coordinate grids of the last few grid keys a lens warp's plan named.
#[derive(Default)]
struct Grids {
    states: std::collections::VecDeque<(GridKey, GridState)>,
}

/// How many keys' grids are kept: a drag at one view, the resident boundary, the picture at rest's
/// stage and a view or two beside them.
const GRID_KEYS: usize = 6;

impl Grids {
    /// The grid of `key`: `Ok(Some)` once computed, `Ok(None)` when it could not be built, and
    /// `Err` while it is still to come, asked for here the first time.
    fn of(&mut self, key: GridKey) -> Result<Option<Arc<CoordinateGrid>>, ()> {
        match self.states.iter().find(|(held, _)| *held == key) {
            Some((_, GridState::Ready(grid))) => Ok(Some(Arc::clone(grid))),
            Some((_, GridState::Failed)) => Ok(None),
            Some(_) => Err(()),
            None => {
                if self.states.len() == GRID_KEYS {
                    self.states.pop_front();
                }
                self.states.push_back((key, GridState::Wanted));
                Err(())
            }
        }
    }

    /// The keys whose grids are wanted, now computing.
    fn start(&mut self) -> Vec<GridKey> {
        let mut started = Vec::new();
        for (key, state) in &mut self.states {
            if matches!(state, GridState::Wanted) {
                *state = GridState::Computing;
                started.push(key.clone());
            }
        }
        started
    }

    /// The grid of `key` computed, or why it could not be.
    fn finish(&mut self, key: &GridKey, grid: Result<Arc<CoordinateGrid>, String>) {
        if let Some((_, state)) = self.states.iter_mut().find(|(held, _)| held == key) {
            *state = match grid {
                Ok(grid) => GridState::Ready(grid),
                Err(_) => GridState::Failed,
            };
        }
    }
}

/// A lens warp's coordinate grid computed for a grid key off the interface thread, or why it could
/// not be.
#[derive(Clone, Debug)]
pub(crate) struct GridAnswer {
    pub(crate) key: GridKey,
    pub(crate) grid: Result<Arc<CoordinateGrid>, String>,
}

/// A plan handed to the surface: converted, the draft revision it is tagged with, and its serial
/// with where it changes since the plan the surface held when it was converted.
#[derive(Clone)]
struct Handed {
    plan: surface::GpuPlan,
    revision: u64,
    change: surface::GpuChange,
}

/// How many handed plans' core plans are kept to measure a later plan's change from.
const STAMP_HISTORY: usize = 16;

/// What a handed plan draws besides its core plan's operations: the boundary, where its texels
/// lie, the region and the clipping marks. A plan whose context is not an earlier one's changes
/// anywhere, whatever its core plan's change: another boundary is other texels everywhere, and the
/// marks are drawn over the whole frame.
#[derive(Clone, PartialEq)]
struct Context {
    boundary: u64,
    texels: surface::TexelMap,
    region: Option<surface::GpuRegion>,
    marks: Option<surface::ClipMarks>,
}

impl Context {
    fn of(plan: &surface::GpuPlan) -> Self {
        Self {
            boundary: plan.boundary.version(),
            texels: plan.texels,
            region: plan.region,
            marks: plan.steps.iter().find_map(|step| match step {
                GpuStep::Clipping(marks) => Some(*marks),
                _ => None,
            }),
        }
    }
}

/// The serials of the plans handed to the surface, with the core plans they were converted from
/// and their contexts: what a later plan's change is measured from, so the surface evaluates only
/// where it changes ([`surface::GpuChange`]).
#[derive(Default)]
struct Stamps {
    serials: u64,
    history: std::collections::VecDeque<(u64, Arc<CorePlan>, Context)>,
}

impl Stamps {
    /// `plan`, converted from `core`, as handed to a surface whose slot holds the values of the plan
    /// of serial `evaluated`: a new serial, and where it changes since that plan when it is one of
    /// the last few handed and drawn in the same context ([`CorePlan::changes_since`]).
    fn hand(
        &mut self,
        plan: surface::GpuPlan,
        revision: u64,
        core: &CorePlan,
        evaluated: Option<u64>,
    ) -> Handed {
        self.serials += 1;
        let serial = self.serials;
        let context = Context::of(&plan);
        let since = evaluated.and_then(|evaluated| {
            let (_, previous, _) = self
                .history
                .iter()
                .find(|(serial, _, earlier)| *serial == evaluated && *earlier == context)?;
            match core.changes_since(previous) {
                luxforge_core::GpuChange::Nothing => Some((evaluated, [0; 4])),
                luxforge_core::GpuChange::Inside(rect) => Some((
                    evaluated,
                    [
                        rect.x0,
                        rect.y0,
                        rect.x0 + rect.width,
                        rect.y0 + rect.height,
                    ],
                )),
                luxforge_core::GpuChange::Anywhere => None,
            }
        });
        if self.history.len() == STAMP_HISTORY {
            self.history.pop_front();
        }
        self.history
            .push_back((serial, Arc::new(core.clone()), context));
        Handed {
            plan,
            revision,
            change: surface::GpuChange { serial, since },
        }
    }
}

/// A run of ticks that each named `compiling`: when the first did, and how long that had lasted at
/// the latest. The status bar's notice reads the latter, so a tick decides it and the clock never
/// does: a drag that holds still keeps what it last said, and nothing wakes to change it.
#[derive(Clone, Copy, Debug)]
struct Compiling {
    since: Instant,
    lasted: Duration,
}

/// The open draft's GPU preview.
struct Drag {
    draft: DraftId,
    /// The latest tick's plan and the draft revision it was planned at.
    plan: Option<(Box<CorePlan>, u64)>,
    /// The revision of the entry that plan's draft was planned over.
    base: Option<u64>,
    /// The boundary that plan starts from.
    wanted: Option<SourceBoundary>,
    held: Option<Held>,
    /// The plan the surface draws.
    surface: Option<Handed>,
    /// The resident boundary's last plan, which the surface holds behind the CPU frame while this
    /// draft has no plan of its own to draw, so its slot keeps the boundary.
    standby: Option<Handed>,
    /// Why the latest tick took the CPU path.
    reason: Option<String>,
    /// The label the recipe list gives the layer that reason names, when it names one.
    layer: Option<String>,
    /// The run of ticks that have named `compiling`, while the latest does.
    compiling: Option<Compiling>,
    /// The percentage zoom the latest tick was planned at, over its region at 100% or more and at
    /// the displayed-size proxy below; `None` at Fit.
    zoom: Option<f32>,
    /// What a region's boundary or slot would take, and the bound on a boundary or the budget it
    /// passes, when the latest tick derived no boundary because of them ([`region_charge`]).
    over_budget: Option<(u64, u64)>,
    /// The percentage zoom at which this drag's region went past the budget, from when it is drawn
    /// at the reduced stage of the view's area while it stays at that zoom ([`SOFTER`]).
    softer: Option<f32>,
    /// At a percentage zoom, the shape the latest tick's restoration or spatial layer is planned
    /// in when its owner planned both: `gpu`, every unit, or `cpu`, the units its values need,
    /// when only that one fits the budget. `None` when there was no choice.
    shape: Option<&'static str>,
    /// The presented generation when the draft ended; the drag is released once a newer frame is
    /// presented, or nothing more is coming.
    ended: Option<u64>,
    gpu_ticks: u64,
    cpu_ticks: u64,
    /// Boundaries derived from the source for this drag's plans: one for each key it asked for
    /// that no held boundary had.
    derived: u64,
}

impl Drag {
    fn new(draft: DraftId) -> Self {
        Self {
            draft,
            plan: None,
            base: None,
            wanted: None,
            held: None,
            surface: None,
            standby: None,
            reason: None,
            layer: None,
            compiling: None,
            zoom: None,
            over_budget: None,
            softer: None,
            shape: None,
            ended: None,
            gpu_ticks: 0,
            cpu_ticks: 0,
            derived: 0,
        }
    }

    /// The tick at `now` took the CPU path for `reason`, which names the layer the recipe list
    /// labels `layer`, if any. A run of `compiling` keeps when it began; any other reason ends it.
    fn stopped(&mut self, reason: &str, layer: Option<String>, now: Instant) {
        self.compiling = (reason == SurfaceFallback::Compiling.as_str()).then(|| {
            let since = self.compiling.map_or(now, |run| run.since);
            Compiling {
                since,
                lasted: now.saturating_duration_since(since),
            }
        });
        self.reason = Some(reason.into());
        self.layer = layer;
    }

    /// The tick is drawn on the GPU.
    fn drew(&mut self) {
        self.reason = None;
        self.layer = None;
        self.compiling = None;
    }
}

/// A boundary held between drafts: every gesture over one source and view is planned from the
/// same boundary (the source itself), so the next draft finds it, with the surface's slot, its
/// links' intermediates and their planes, still on the GPU and draws its first tick there. The
/// last plan drawn over it is handed to the surface behind the frame on screen, which keeps the
/// slot; the asset it was held for lets a different photograph let it go.
struct Resident {
    held: Held,
    plan: Option<Handed>,
    asset: Option<luxforge_core::AssetId>,
}

/// The committed stack's own view plan over the boundary it is drawn from, which the surface draws
/// in place of the photograph's frame while the stack is at rest ([`Editor::gpu_rest_plan`]): the
/// picture at rest at 100% and above and wherever the view draws the stack at its own size, and
/// elsewhere until its tiles are in. Every committed job sets it or lets it go, so it is never an
/// earlier stack's.
struct AtRest {
    /// The plan as the core planned it, converted again when the clipping overlay changes.
    core: Box<CorePlan>,
    boundary: GpuBoundary,
    origin: (u32, u32),
    grid: Option<gpu_plan::WarpGrid>,
    region: Option<luxforge_core::Region>,
    /// The clipping overlay's classes its marks show.
    clip: Option<[bool; 2]>,
    handed: Handed,
}

/// The GPU picture of the stack on screen when Compare began, retained while Compare is shown:
/// drawn as its After side, and handed back to the photograph when Compare ends, so that stack is
/// drawn again at once, before its own job plans it.
struct Retained {
    at_rest: Option<AtRest>,
    rest: Option<HeldRest>,
}

/// The desktop's GPU previews: the source the surface holds, the open draft's, the boundary held
/// between drafts and the warm list of the committed stack.
#[derive(Default)]
pub(crate) struct GpuPreviews {
    drag: Option<Drag>,
    resident: Option<Resident>,
    /// The committed stack's view plan, drawn at rest.
    at_rest: Option<AtRest>,
    /// The stack on screen when Compare began, its GPU picture retained while Compare is shown.
    compare: Option<Retained>,
    /// The prepared source of the photograph on screen, which every boundary is derived from.
    source: Option<HeldSource>,
    /// The displayed stack's picture at rest, drawn in tiles at Fit and below 100%.
    rest: Option<HeldRest>,
    /// The last picture at rest's version handed out.
    rests: u64,
    /// A crop draft's input stage drawn on the GPU: the layer prefix's picture at rest in tiles,
    /// reduced to the stage's display bounds ([`Editor::gpu_stage_from`]).
    stage: Option<StageRest>,
    /// The last picture at rest the surface was found drawing, as evidence records it.
    rest_drawn: Option<u64>,
    /// The last source version handed out: each source is uploaded once.
    sources: u64,
    /// A lens warp's coordinate grids, by boundary key.
    grids: Grids,
    stamps: Stamps,
    /// The last boundary version handed out: each held boundary is derived once.
    versions: u64,
    warm: Option<GpuWarm>,
    /// The content the GPU presents with no CPU render, whose report its tiles' counts are
    /// ([`super::gpu_counts`]), until they are taken up.
    pub(crate) counts: Option<super::gpu_counts::CountsTarget>,
    /// The content the surface could not draw or count after the GPU presented it: the reference
    /// renders it instead, and the GPU presents it no more.
    pub(crate) refused_content: Option<u64>,
    /// The last tick whose counts are shown in motion, by boundary and revision.
    pub(crate) motion_tick: Option<(u64, u64)>,
    /// Compare waits for the reference's frame of a content the GPU presented without one, its
    /// After side.
    pub(crate) compare_waits: bool,
    /// The Fit bounds the displayed stack's picture at rest was planned at by its job's owner
    /// task: a refit plans it again once they are not the view's ([`Editor::refit_view`]).
    pub(crate) rest_planned_at: Option<luxforge_core::ProxyBounds>,
    /// The content the GPU presented whose picture at rest found its programs compiling, and when
    /// it first did: the reference renders it once that has lasted the `compiling` threshold.
    pub(crate) rest_compiling_since: Option<(u64, std::time::Instant)>,
    /// The compile thread's warm-up as the desktop follows it ([`super::gpu_warm`]).
    pub(crate) warm_up: super::gpu_warm::WarmUpFollow,
    /// What a test reports for the surface, which no test draws.
    #[cfg(test)]
    pub(crate) surface: Option<SurfaceReport>,
    /// The counts a test reports the surface took, which no test draws.
    #[cfg(test)]
    pub(crate) counts_report: Option<surface::SurfaceCounts>,
    /// The budget a test holds a region's boundary to, in place of the surface's.
    #[cfg(test)]
    pub(crate) budget: Option<u64>,
    /// The figure a test asks the owner to plan a region's reduced stage beside it past, in place
    /// of the core's ([`luxforge_core::REDUCED_AFTER_BYTES`]).
    #[cfg(test)]
    pub(crate) reduce_after: Option<u64>,
    /// What a test reports of the source the surface holds, which no test uploads.
    #[cfg(test)]
    pub(crate) source_figures: Option<surface::gpu_preview::SourceFigures>,
    /// A test's committed stacks the GPU draws nothing of at rest, as one it could not plan: the
    /// CPU's frame is then the photograph, and a gesture's frame settles into it.
    #[cfg(test)]
    pub(crate) rest_off: bool,
    /// What a test reports of whether the held picture at rest's programs are compiled, which no
    /// test compiles.
    #[cfg(test)]
    pub(crate) programs_warm: Option<bool>,
}

/// What a tick, or a displayed entry's job, asks the owner to plan its GPU picture for, with its
/// preview job.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum GpuAsk {
    /// Nothing planned: a view job that does not settle, or a view at 100% or more whose visible
    /// region is not known yet, as before the photograph's stage is.
    #[default]
    Off,
    /// A whole frame at the job's display bounds: at Fit, and at a percentage zoom below 100%,
    /// whose bounds are the displayed size of the whole stage, the proxy the CPU path draws there.
    Fit,
    /// At a percentage zoom of 100% or more: this region of the output stage at full scale, drawn
    /// at this many physical pixels an output pixel, with its draft planned at the reduced stage
    /// of the view's area too where the region's own figures pass these bytes
    /// ([`luxforge_core::PreviewRequest::reduce_regions_after`]).
    Region(Region, f64, u64),
}

/// Before a region's boundary is rendered, what the boundary alone takes and what the slot drawing
/// it takes on the GPU, as the surface charges them: the boundary over the window its request
/// names, at its format's bytes a texel; a geometry tail's intermediate of the same size; the frame
/// in its size bucket, a region's or a whole frame's, with its placement uniform
/// ([`surface::gpu_preview::texture_charge`]); and the chain's charge over the window
/// ([`chain_charge`]): each link's intermediate before the last, every link's kept planes and
/// parameters, and the pool of scratch planes the links take in turn, once; and each light link
/// the slot runs before its chain ([`light_charge`]). The surface adds only every link's words and
/// blocks buffers once it is held. At Fit at the exact stage the frame is the whole output stage
/// and the boundary the window it reads, or the whole boundary stage. `None` at a Fit proxy, which
/// the display bounds bound.
pub(crate) fn region_charge(plan: &CorePlan, request: &SourceBoundary) -> Option<(u64, u64)> {
    let whole = |width, height| Region {
        x0: 0,
        y0: 0,
        width,
        height,
    };
    let (rect, window, region) = match (request.key.region(), request.key.plan()) {
        (Some(rect), _) => (rect, request.window?, true),
        (None, None) => {
            let (output, stage) = (plan.geometry.output(), plan.boundary.stage);
            let window = request.window.unwrap_or(whole(stage.width, stage.height));
            (whole(output.width, output.height), window, false)
        }
        (None, Some(_)) => return None,
    };
    let boundary = boundary_bytes(window, request.format);
    // A tail quantizes where the CPU clamps before its resample, and keeps `f32` values on the
    // RAW linear path, as `gpu_plan` builds it.
    let textures = surface::gpu_preview::texture_charge(
        (window.width, window.height),
        gpu_plan::boundary_format(request.format),
        (rect.width, rect.height),
        gpu_plan::has_tail(plan).then_some((plan.geometry.clamps, plan.linear)),
        region,
        super::compare_after::DEVICE_TEXTURE_LIMIT,
    );
    Some((
        boundary,
        textures + chain_charge(plan, window, request.format) + light_charge(plan, request.format),
    ))
}

/// What the light links of `plan` take of the GPU-preview budget, as the surface charges them
/// ([`surface::gpu_preview::light::lights_charge`]): each link's block plane of the whole stage and
/// its buffers, and the one tile texture of the source they cut into in turn, whatever window the
/// plan draws over. Lights the surface cannot run are charged nothing; the tick that converts the
/// plan refuses them.
pub(crate) fn light_charge(plan: &CorePlan, format: luxforge_core::BoundaryFormat) -> u64 {
    gpu_plan::surface_lights(plan).map_or(0, |lights| {
        let runnable: Vec<_> = lights
            .into_iter()
            .filter(|light| light.index().is_some())
            .collect();
        surface::gpu_preview::light::lights_charge(
            &runnable,
            gpu_plan::boundary_format(format),
            super::compare_after::DEVICE_TEXTURE_LIMIT,
            super::compare_after::DEVICE_STORAGE_BINDING,
        )
        .unwrap_or(0)
    })
}

/// A region's boundary, or one at the exact stage at Fit, that would pass the bound on a
/// boundary, or whose slot `budget`, is never derived: the figure, and the bound or budget it
/// passes. `None` within both, and at a Fit proxy, which the display bounds bound.
fn over_budget(plan: &CorePlan, request: &SourceBoundary, budget: u64) -> Option<(u64, u64)> {
    let (boundary, slot) = region_charge(plan, request)?;
    if boundary > luxforge_core::BOUNDARY_MAX_BYTES {
        Some((boundary, luxforge_core::BOUNDARY_MAX_BYTES))
    } else {
        (slot > budget).then_some((slot, budget))
    }
}

/// What a drag's plan at the reduced stage takes when it is drawn at 100% or more in a region's
/// place, the softer frame ([`SOFTER`]): [`region_charge`]'s figures over the window of the reduced
/// stage its boundary holds, its frame the whole reduced output in a region's size bucket.
pub(crate) fn reduced_charge(plan: &CorePlan, request: &SourceBoundary) -> u64 {
    let Some(proxy) = request.key.plan() else {
        return u64::MAX;
    };
    let [x0, y0, width, height] = proxy.held();
    let window = Region {
        x0,
        y0,
        width,
        height,
    };
    let output = plan.geometry.output();
    surface::gpu_preview::texture_charge(
        (window.width, window.height),
        gpu_plan::boundary_format(request.format),
        (output.width, output.height),
        gpu_plan::has_tail(plan).then_some((plan.geometry.clamps, plan.linear)),
        true,
        super::compare_after::DEVICE_TEXTURE_LIMIT,
    ) + chain_charge(plan, window, request.format)
        + light_charge(plan, request.format)
}

/// What a drag's frame drawn at the reduced stage of the view's area at 100% or more is called,
/// where its region's slot would pass the budget: the drag's reason while it is drawn so, which the
/// status bar's notice says, though the GPU draws every tick.
pub(crate) const SOFTER: &str = "budget-reduced";

/// The bytes a boundary over `window` in `format` takes, and so does each of a chain's
/// intermediates over it, which take the boundary's size and format.
fn boundary_bytes(window: Region, format: luxforge_core::BoundaryFormat) -> u64 {
    u64::from(window.width) * u64::from(window.height) * format.texel_bytes() as u64
}

/// What the chain of `plan` takes over a boundary of `window` in `format`, as the surface's slot
/// charges it ([`surface::gpu_preview::chain_charge`]): the plan's steps converted with no boundary
/// ([`gpu_plan::plan_steps`]), split into links as the surface splits them, each link's
/// intermediate before the last, every link's kept planes and its passes' parameters, and the pool
/// of scratch planes every link takes in turn, once. A plan whose steps cannot be converted is
/// charged [`unconverted_chain_charge`].
pub(super) fn chain_charge(
    plan: &CorePlan,
    window: Region,
    format: luxforge_core::BoundaryFormat,
) -> u64 {
    match gpu_plan::plan_steps(plan) {
        Ok(steps) => surface::gpu_preview::chain_charge(
            &steps,
            (window.width, window.height),
            (window.x0, window.y0),
            gpu_plan::boundary_format(format),
        )
        .total(),
        Err(_) => unconverted_chain_charge(plan, window, format),
    }
}

/// The chain charge of a plan whose steps cannot be converted: every plane of every spatial
/// operation in a texture of its own (the core's [`GpuSpatial::plane_bytes`]), and an intermediate
/// for each spatial operation. Naming a warp's steps needs no grid, so only a position map past
/// what an `f32` holds exactly (`position-range`) comes here, and the tick that converts the plan
/// over its boundary refuses it for the same reason.
///
/// It is not an upper bound on the converted figure. It counts every link's scratch planes, where
/// the pool holds for each class only the most any one link holds, and an intermediate for every
/// spatial operation, where the first adds none when no step comes before it. But it leaves out
/// every pass's parameter slice, 256 bytes a pass. So it is above the converted figure wherever
/// the scratch the pool shares outweighs those slices, as it does by far for masked Presence
/// layers, and for a plan of one spatial operation after colour steps, which shares nothing, below
/// it by the slices alone.
///
/// [`GpuSpatial::plane_bytes`]: luxforge_core::GpuSpatial::plane_bytes
pub(super) fn unconverted_chain_charge(
    plan: &CorePlan,
    window: Region,
    format: luxforge_core::BoundaryFormat,
) -> u64 {
    let (origin, size) = ((window.x0, window.y0), (window.width, window.height));
    plan.spatial
        .iter()
        .map(|spatial| spatial.plane_bytes(origin, size) + boundary_bytes(window, format))
        .sum()
}

/// The proxy a boundary is held for, as evidence names it: the whole proxy stage and the display
/// bounds it was fitted to, which at a percentage zoom below 100% are the stage's displayed size;
/// `null` for a boundary of the exact stage, at Fit or over a region.
fn proxy_evidence(key: &BoundaryKey) -> Value {
    key.plan().map_or(Value::Null, |plan| {
        json!({"width": plan.width, "height": plan.height,
            "bounds": [plan.bounds.width, plan.bounds.height]})
    })
}

/// Whether a plan's region frame holds `wanted` of the photograph's full output stage: a region at
/// full scale whose rectangle holds it, or a reduced whole frame placed over the full stage, the
/// softer drag frame, which holds every part of it.
fn holds_view(region: surface::GpuRegion, wanted: Region) -> bool {
    if region.stage != region.full_stage {
        return region.rect == [0, 0, region.stage.0, region.stage.1];
    }
    super::preview::contains_region(rect_of(region), wanted)
}

/// A surface region's rectangle of its stage, as the core's.
fn rect_of(region: surface::GpuRegion) -> Region {
    let [x0, y0, x1, y1] = region.rect;
    Region {
        x0,
        y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

/// One tick's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// The surface draws the plan: no preview job and no upload.
    Gpu,
    /// The tick's preview job goes to the worker as today.
    Cpu,
}

impl GpuPreviews {
    /// The plan the surface is handed this frame, whether it holds it behind the CPU frame, and
    /// the draft revision it is tagged with: the open drag's, or between drafts the resident
    /// boundary's last plan, which the surface holds behind the CPU frame.
    pub(crate) fn surface_plan(&self) -> Option<(&surface::GpuPlan, u64)> {
        self.handed().map(|handed| (&handed.plan, handed.revision))
    }

    /// The plan handed to the surface, from the open drag or the resident boundary.
    fn handed(&self) -> Option<&Handed> {
        match &self.drag {
            Some(drag) => drag.surface.as_ref().or(drag.standby.as_ref()),
            None => self
                .resident
                .as_ref()
                .and_then(|resident| resident.plan.as_ref()),
        }
    }

    /// The serial of the plan handed to the surface, and where it changes since the plan the
    /// surface held when it was converted.
    pub(crate) fn surface_change(&self) -> Option<surface::GpuChange> {
        self.handed().map(|handed| handed.change)
    }

    /// Whether the plan handed to the surface is the resident boundary's, held between drafts or
    /// while a draft has no plan of its own to draw.
    pub(crate) fn resident_plan(&self) -> bool {
        match &self.drag {
            Some(drag) => drag.surface.is_none() && drag.standby.is_some(),
            None => self
                .resident
                .as_ref()
                .is_some_and(|resident| resident.plan.is_some()),
        }
    }

    pub(crate) fn warm(&self) -> Option<&GpuWarm> {
        self.warm.as_ref()
    }

    /// The draft whose plan the surface is handed, open or ended and not yet released.
    pub(crate) fn draft(&self) -> Option<&DraftId> {
        self.drag.as_ref().map(|drag| &drag.draft)
    }

    /// The boundary version a held boundary is drawn under, for the capture's readiness.
    pub(crate) fn held_version(&self) -> Option<u64> {
        self.drag
            .as_ref()
            .and_then(|drag| drag.held.as_ref())
            .or(self.resident.as_ref().map(|resident| &resident.held))
            .map(|held| held.boundary.version())
    }

    /// Whether the committed stack's view plan is held to be drawn at rest.
    pub(crate) fn at_rest_drawable(&self) -> bool {
        self.at_rest.is_some()
    }

    /// Whether the programs of the held picture at rest — its view plan, and its tiles where they
    /// are held — are compiled and ready in the surface's cache, so its frames draw with no compile
    /// to wait for: what makes an open's first picture the GPU's (`docs/design/gpu-preview.md`,
    /// "Warming at launch and open"). Asks for no compile.
    pub(crate) fn rest_programs_ready(&self) -> bool {
        #[cfg(test)]
        if let Some(warm) = self.programs_warm {
            return warm;
        }
        let Some(at_rest) = self.at_rest.as_ref() else {
            return false;
        };
        let tile = self
            .rest
            .as_ref()
            .and_then(|held| held.gpu.as_ref().or(held.counts.as_ref()))
            .and_then(|rest| rest.tiles.first());
        surface::gpu_programs_ready(std::iter::once(&at_rest.handed.plan).chain(tile))
    }

    /// The versions the held picture at rest is handed under — the picture's and its counts'
    /// variant's — when its tiles are cut from `source` and the surface can run them.
    pub(crate) fn rest_versions(&self, source: &ProxyIdentity) -> Option<[u64; 2]> {
        self.rest
            .as_ref()
            .filter(|held| held.tiles.source == *source && held.refused.is_none())
            .map(|held| [held.version, held.counts_version])
    }

    /// Mark the held picture at rest's counts as wanted, or no longer: the surfaces are handed its
    /// tiles for their counts alone while they are wanted and the picture is not handed.
    pub(crate) fn want_rest_counts(&mut self, wanted: bool) {
        if let Some(held) = self.rest.as_mut() {
            held.wanted = wanted;
        }
    }

    /// The open drag's figures, as evidence and the tests read them, with the source the surface
    /// holds.
    pub(crate) fn summary(&self) -> Value {
        let resident = self.resident.as_ref().map(|resident| {
            json!({"version": resident.held.boundary.version(), "layer": resident.held.key.layer(),
                "proxy": proxy_evidence(&resident.held.key)})
        });
        let source = self.source.as_ref().map(|source| {
            json!({"version": source.gpu.version(), "width": source.stage.0,
                "height": source.stage.1, "bytes": source.gpu.bytes(),
                // Whether the desktop still holds the pixels, which it lets go once the surface
                // holds them.
                "pixels_held": source.gpu.holds_pixels()})
        });
        // The picture at rest in tiles: what the surfaces are handed, or why not yet.
        let rest = self.rest.as_ref().map(|held| {
            json!({"version": held.version, "counts_version": held.counts_version,
                "tiles": held.tiles.tiles.len(),
                "view": held.tiles.reduction.as_ref().map(|reduction| [reduction.view.0,
                    reduction.view.1]),
                "output": [held.tiles.output.width, held.tiles.output.height],
                "handed": held.gpu.is_some(), "refused": held.refused,
                "counts_wanted": held.wanted})
        });
        let Some(drag) = &self.drag else {
            return json!({"drag": null, "resident": resident, "source": source, "rest": rest,
                "warm": self.warm.as_ref().map(GpuWarm::version)});
        };
        json!({
            "drag": {
                "draft_id": drag.draft.as_str(),
                "plan_revision": drag.plan.as_ref().map(|(_, revision)| *revision),
                // Each light the plan reads: its layer, and whether the slot computes it from the
                // source through the prefix's colour, or by its stand-in with the spatial layers
                // before it left out.
                "lights": drag.plan.as_ref().map(|(plan, _)| light_evidence(plan)),
                "surface_revision": drag.surface.as_ref().map(|handed| handed.revision),
                "boundary": drag.held.as_ref().map(|held| json!({
                    "version": held.boundary.version(),
                    "width": held.boundary.size().0,
                    "height": held.boundary.size().1,
                    "origin": [held.origin.0, held.origin.1],
                    "layer": held.key.layer(),
                    // How the surface derives it from the source: reduced to a proxy, or cut.
                    "derived": derivation_name(&held.boundary),
                    "region": held.key.region().map(|rect| {
                        [rect.x0, rect.y0, rect.width, rect.height]
                    }),
                    "proxy": proxy_evidence(&held.key),
                })),
                "zoom": drag.zoom,
                "over_budget": drag.over_budget.map(|(requested, budget)| {
                    json!({"requested": requested, "budget": budget})
                }),
                "shape": drag.shape,
                // At 100% and above, the zoom at which the drag went to its reduced stage, the
                // softer frame, past the budget.
                "softer": drag.softer,
                "boundaries_derived": drag.derived,
                "reason": drag.reason,
                "ended": drag.ended.is_some(),
                "gpu_ticks": drag.gpu_ticks,
                "cpu_ticks": drag.cpu_ticks,
            },
            "resident": resident,
            "source": source,
            "rest": rest,
            "warm": self.warm.as_ref().map(GpuWarm::version),
        })
    }

    #[cfg(test)]
    pub(crate) fn ticks(&self) -> (u64, u64, u64) {
        self.drag.as_ref().map_or((0, 0, 0), |drag| {
            (drag.gpu_ticks, drag.cpu_ticks, drag.derived)
        })
    }

    /// The prepared source the surfaces are handed, with its pixels until the surface holds them.
    pub(crate) fn source(&self) -> Option<&GpuSource> {
        self.source.as_ref().map(|source| &source.gpu)
    }

    /// Make the open drag's run of `compiling` ticks have begun `by` earlier, so a test need not
    /// wait out [`crate::state::status::COMPILING_AFTER`].
    #[cfg(test)]
    pub(crate) fn backdate_compiling(&mut self, by: Duration) {
        if let Some(run) = self.drag.as_mut().and_then(|drag| drag.compiling.as_mut()) {
            run.since -= by;
        }
    }

    #[cfg(test)]
    pub(crate) fn holds_boundary(&self) -> bool {
        self.held_version().is_some()
    }

    #[cfg(test)]
    pub(crate) fn has_drag(&self) -> bool {
        self.drag.is_some()
    }

    /// What the latest tick's plan takes over its region ([`region_charge`]).
    #[cfg(test)]
    pub(crate) fn region_charge(&self) -> Option<(u64, u64)> {
        let (plan, request) = self.planned()?;
        region_charge(plan, request)
    }

    /// The latest tick's plan, in the shape it is drawn in, and the boundary it asks for.
    #[cfg(test)]
    pub(crate) fn planned(&self) -> Option<(&CorePlan, &SourceBoundary)> {
        let drag = self.drag.as_ref()?;
        Some((&drag.plan.as_ref()?.0, drag.wanted.as_ref()?))
    }
}

impl Editor {
    /// The GPU-preview budget a region's boundary and frame are held to before it is rendered: the
    /// surface's own.
    fn gpu_budget(&self) -> u64 {
        #[cfg(test)]
        if let Some(budget) = self.gpu.budget {
            return budget;
        }
        surface::gpu_preview::GPU_PREVIEW_BUDGET
    }

    /// What the surface reports of its last frame.
    pub(crate) fn surface_report(&self) -> SurfaceReport {
        #[cfg(test)]
        if let Some(report) = self.gpu.surface {
            return report;
        }
        SurfaceReport::of(&luxforge_ui::surface_diagnostics(
            crate::view::canvas::DEVELOP_SURFACE,
        ))
    }

    /// One tick of the open draft's gesture, answered with the GPU `preview` its job carries:
    /// whether the surface draws it from the plan, with no preview job and no upload, or the job
    /// goes to the worker as today. A plan whose boundary no held one has is derived from the source
    /// the surface holds at once ([`derive_held`]), so the next frame evaluates it.
    pub(crate) fn gpu_tick(&mut self, set: &Draft, preview: Option<Box<GpuPreview>>) -> Tick {
        let now = Instant::now();
        // With the gate refusing — a GPU stage that cannot draw at all — the plan is never handed
        // over, so nothing is asked for it.
        let allowed = self.gpu_preview_allowed();
        // The clipping overlay is derived from the CPU's frames, so over a GPU frame the plan marks
        // its own clipped pixels instead.
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        // At a percentage zoom the mask overlay's region coverage, which the coverage worker
        // computes for each tick over the view's region, is laid over the GPU region frame.
        let zoom = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Percent { value } => Some(value),
            luxforge_core::Zoom::Fit => None,
        };
        // The budget a slot is held to: the GPU-preview budget less the source the surface holds,
        // which is charged to the same budget.
        let budget = self.gpu_budget().saturating_sub(
            self.gpu
                .source
                .as_ref()
                .map_or(0, |source| source.gpu.bytes()),
        );
        let over_budget =
            |plan: &CorePlan, request: &SourceBoundary| over_budget(plan, request, budget);
        let report = self.surface_report();
        let mut released = None;
        let mut derived_now = None;
        let resident = &mut self.gpu.resident;
        let stamps = &mut self.gpu.stamps;
        let source = self.gpu.source.as_ref();
        let versions = &mut self.gpu.versions;
        let grids = &mut self.gpu.grids;
        let drag = match &mut self.gpu.drag {
            Some(drag) if drag.draft == set.draft_id && drag.ended.is_none() => drag,
            slot => {
                // A new draft starts from the boundary held between drafts, which the surface's
                // slot still holds: its first tick is drawn on the GPU when its key is the same.
                let mut drag = Drag::new(set.draft_id.clone());
                if let Some(resident) = resident.take() {
                    drag.held = Some(resident.held);
                    drag.standby = resident.plan;
                }
                slot.insert(drag)
            }
        };
        // A tick with no plan draws nothing of its own; the boundary stays held, behind the CPU
        // frame, for the next tick or draft that plans from it. Only the gate's refusal lets it go:
        // a GPU stage that cannot draw at all — a lost device, an
        // adapter that cannot run it, a launch that refused it — hands the surface no plan, so
        // nothing would ever draw from it.
        let unplanned =
            |drag: &mut Drag, reason: &str, layer: Option<String>, released: &mut Option<u64>| {
                if let Some(handed) = drag.surface.take() {
                    drag.standby = Some(handed);
                }
                drag.plan = None;
                drag.wanted = None;
                if allowed.is_err()
                    && let Some(held) = drag.held.take()
                {
                    *released = Some(held.boundary.version());
                    drag.standby = None;
                }
                drag.stopped(reason, layer, now);
                Tick::Cpu
            };
        let tick = match preview.map(|preview| *preview) {
            _ if allowed.is_err() => unplanned(
                drag,
                allowed.err().unwrap_or("no-adapter"),
                None,
                &mut released,
            ),
            // The job was planned at no view: its bounds, or its region at 100% or more, were not
            // known yet, so nothing could be planned for it.
            None => unplanned(drag, "unplannable", None, &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Fallback(reason),
                layer,
                ..
            }) => unplanned(drag, reason.code(), layer, &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Plan(plan),
                boundary,
                cpu_shape,
                reduced,
                ..
            }) => {
                let Some(request) = boundary else {
                    drag.stopped("unplannable", None, now);
                    drag.cpu_ticks += 1;
                    return Tick::Cpu;
                };
                let revision = set.draft_revision;
                // At a percentage zoom a spatial layer is drawn in its GPU shape when that fits,
                // else in the CPU's shape when that does, with a compile where a value crosses
                // zero; when neither fits, the CPU's shape names the least the drag would take.
                let (plan, over_budget, shape) = match (over_budget(&plan, &request), cpu_shape) {
                    (Some(_), Some(smaller)) => {
                        let over = over_budget(&smaller, &request);
                        (smaller, over, Some("cpu"))
                    }
                    (over, Some(_)) => (plan, over, Some("gpu")),
                    (over, None) => (plan, over, None),
                };
                // A region whose slot would pass the budget, now or earlier in this drag at this
                // zoom, is drawn from the draft's plan at the reduced stage of the view's area,
                // scaled to the view: the softer drag frame, when that fits.
                let full = plan.geometry.output();
                let softer = reduced
                    .filter(|_| {
                        over_budget.is_some() || (drag.softer.is_some() && drag.softer == zoom)
                    })
                    .and_then(|reduced| match *reduced {
                        luxforge_core::GpuPreview {
                            answer: GpuAnswer::Plan(plan),
                            boundary: Some(request),
                            ..
                        } => Some((plan, request)),
                        _ => None,
                    })
                    .filter(|(plan, request)| reduced_charge(plan, request) <= budget);
                let (plan, request, over_budget, shape, placed) = match softer {
                    Some((plan, request)) => {
                        drag.softer = zoom;
                        (plan, request, None, None, Some((full.width, full.height)))
                    }
                    None => (plan, request, over_budget, shape, None),
                };
                if let Some(held) = drag.held.take_if(|held| !held.serves(&request)) {
                    // The plan needs another boundary: the window moved, the bounds changed, the
                    // drag went to its reduced stage, or the source changed.
                    drag.surface = None;
                    released = Some(held.boundary.version());
                }
                drag.shape = shape;
                drag.wanted = Some(request.clone());
                drag.plan = Some((plan, revision));
                drag.base = Some(set.base_revision);
                drag.zoom = zoom;
                drag.over_budget = over_budget;
                // A plan whose boundary no held one has is derived from the source the surface
                // holds, unless it would pass the budget; a lens warp waits for its grid.
                let refused = if drag.held.is_some() {
                    None
                } else if over_budget.is_some() {
                    // The surface's own name for a plan over the budget.
                    Some("budget-exceeded")
                } else {
                    match derive_held(source, versions, grids, &request) {
                        Ok(held) => {
                            derived_now = Some(derived_evidence(&held, false));
                            drag.held = Some(held);
                            drag.derived += 1;
                            None
                        }
                        Err(reason) => Some(reason),
                    }
                };
                match drag.held.as_ref().filter(|_| refused.is_none()) {
                    None => {
                        drag.surface = None;
                        drag.stopped(refused.unwrap_or("boundary-pending"), None, now);
                        Tick::Cpu
                    }
                    Some(held) => {
                        let (plan, _) = drag.plan.as_ref().expect("the plan just kept");
                        match gpu_plan::surface_plan_over(
                            plan,
                            held.boundary.clone(),
                            held.origin,
                            held.grid.as_ref(),
                            held.key.region(),
                        )
                        .map(|converted| super::gpu_settle::marked(converted, plan, clip))
                        .map(|converted| match placed {
                            Some(full) => gpu_plan::placed_over(converted, full),
                            None => converted,
                        }) {
                            Err(unrunnable) => {
                                drag.surface = None;
                                drag.stopped(unrunnable.code(), None, now);
                                Tick::Cpu
                            }
                            Ok(converted) => {
                                let version = held.boundary.version();
                                drag.surface =
                                    Some(stamps.hand(converted, revision, plan, report.evaluated));
                                if report.ready_boundary == Some(version)
                                    && report.fallback.is_none()
                                {
                                    drag.drew();
                                    if placed.is_some() {
                                        // Drawn on the GPU at the reduced stage: the notice says
                                        // the frame is softer than the picture at rest.
                                        drag.reason = Some(SOFTER.into());
                                    }
                                    Tick::Gpu
                                } else {
                                    // A region slot the surface found over the budget, beside
                                    // what else it holds: the next tick draws the reduced stage.
                                    if matches!(
                                        report.fallback,
                                        Some(SurfaceFallback::BudgetExceeded { .. })
                                    ) && held.key.region().is_some()
                                    {
                                        drag.softer = zoom;
                                    }
                                    drag.stopped(
                                        report
                                            .fallback
                                            .map_or("surface-pending", SurfaceFallback::as_str),
                                        None,
                                        now,
                                    );
                                    Tick::Cpu
                                }
                            }
                        }
                    }
                }
            }
        };
        match tick {
            Tick::Gpu => drag.gpu_ticks += 1,
            Tick::Cpu => drag.cpu_ticks += 1,
        }
        if let Some(version) = released {
            // The gate's refusal names itself; any other release is a plan that needs another
            // boundary.
            self.log_release(version, allowed.err().unwrap_or("key-changed"));
        }
        if let Some(detail) = derived_now {
            self.event("gpu_boundary", || detail);
        }
        tick
    }

    /// One tick drawn on the GPU: the evidence that ties it to the frame the surface draws.
    pub(crate) fn gpu_ticked(&mut self, set: &Draft) {
        let version = self.gpu.held_version();
        self.event("gpu_preview_tick", || {
            json!({
                "draft_id": set.draft_id.as_str(),
                "draft_revision": set.draft_revision,
                "path": "gpu",
                "boundary": version,
            })
        });
    }

    /// The surface is handed the plan of the open draft's newest revision, to draw on the GPU.
    pub(crate) fn gpu_draws_newest_tick(&self) -> bool {
        let surfaces = self.surfaces();
        surfaces.gpu.is_some()
            && !surfaces.gpu_hold
            && surfaces.gpu_tag.is_some()
            && surfaces.gpu_tag
                == self
                    .session
                    .draft
                    .as_ref()
                    .map(|draft| draft.draft_revision)
    }

    /// Whether the photograph on screen is the GPU's frame of the open draft's newest revision,
    /// `revision`: the surface is handed that tick's plan, not held behind a CPU frame, and its
    /// last frame drew it.
    pub(crate) fn gpu_shows_revision(&self, revision: u64) -> bool {
        self.gpu_draws_newest_tick()
            && self
                .surface_report()
                .drawn
                .is_some_and(|(_, drawn)| drawn == revision)
    }

    /// A tick drawn on the GPU puts the gesture's frame on screen as a CPU frame of it would, for
    /// whoever waits on one: its newest once nothing the gesture asked for is still to bring a
    /// frame of its own. The surface draws it at the next render.
    pub(crate) fn gpu_tick_presented(&mut self) {
        let Some(gesture) = self.core_gesture() else {
            return;
        };
        let presented = super::outcome::Presented::Draft {
            slider: gesture.slider().is_some(),
            newest: !gesture.draft.frame_pending(),
        };
        self.outcome(super::outcome::Outcome::Presented(presented));
    }

    /// A tick of the open draft that took the CPU path, and why.
    pub(crate) fn gpu_cpu_tick(&self, set: &Draft, generation: u64) {
        let Some(drag) = &self.gpu.drag else {
            return;
        };
        self.event("gpu_preview_tick", || {
            json!({
                "draft_id": set.draft_id.as_str(),
                "draft_revision": set.draft_revision,
                "path": "cpu",
                "reason": drag.reason,
                "generation": generation,
            })
        });
    }

    /// Hold `source`, the prepared source of a preview job about to be queued or of a tick's
    /// answer, as the one every boundary is derived from: a new version whenever its identity
    /// changes — another photograph, a new development, another crop or orientation of a RAW's
    /// view — handed to the surfaces with its pixels, which are the job's own, shared. Once the
    /// surface holds the source the desktop lets them go ([`after_message`]); should the surface
    /// report it no longer holds it, or could not hold it, the pixels are taken again from this
    /// job. `O(1)`: the identity is read and nothing is copied.
    pub(crate) fn gpu_hold_source(&mut self, source: &PreviewSource) {
        if self.gpu_preview_allowed().is_err() {
            return;
        }
        let identity = source.identity();
        let (surface, _) = self.surface_source();
        let asset = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone());
        match &mut self.gpu.source {
            Some(held) if held.identity == identity => {
                held.asset = asset;
                // The pipeline let the version go — a frame no surface handed it, as a gallery
                // page draws — or could not hold it, while the desktop held no pixels for it: hand
                // them again under the same version, which no boundary derived from it changes.
                if !held.gpu.holds_pixels()
                    && surface.is_none_or(|figures| figures.version != held.gpu.version())
                    && let Some(again) = gpu_source_of(held.gpu.version(), source)
                {
                    held.gpu = again;
                    held.refused = None;
                    let version = held.gpu.version();
                    self.event(
                        "gpu_source",
                        || json!({"version": version, "why": "surface-let-go"}),
                    );
                }
            }
            slot => {
                self.gpu.sources += 1;
                let version = self.gpu.sources;
                let Some(gpu) = gpu_source_of(version, source) else {
                    *slot = None;
                    self.event(
                        "gpu_source",
                        || json!({"version": null, "why": "source-unfit"}),
                    );
                    return;
                };
                let stage = gpu.stage();
                let bytes = gpu.bytes();
                *slot = Some(HeldSource {
                    identity,
                    stage,
                    gpu,
                    asset,
                    refused: None,
                });
                self.event("gpu_source", || {
                    json!({"version": version, "width": stage.0, "height": stage.1,
                        "bytes": bytes, "why": "source-changed"})
                });
            }
        }
        // A picture at rest waiting for this source is drawn from it now.
        self.gpu_convert_rest();
        self.gpu_convert_stage();
    }

    /// The prepared source the surfaces are handed, which every GPU boundary is derived from: none
    /// while the gate refuses the GPU stage.
    pub(crate) fn gpu_source_handed(&self) -> Option<&GpuSource> {
        self.gpu_preview_allowed().ok()?;
        self.gpu.source()
    }

    /// What the surface reports of the source the pipeline holds, live, and of a source it could
    /// not hold.
    fn surface_source(
        &self,
    ) -> (
        Option<surface::SourceFigures>,
        Option<(u64, SurfaceFallback)>,
    ) {
        #[cfg(test)]
        if let Some(figures) = self.gpu.source_figures {
            return (Some(figures), None);
        }
        let diagnostics = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        (diagnostics.gpu_source, diagnostics.gpu_source_refused)
    }

    /// Once the surface holds every row of the source the desktop hands it, let its pixels go: the
    /// surfaces are handed the source without them from then on ([`GpuSource::resident`]), so the
    /// desktop keeps no reference to them and a RAW's development can be let go by its worker. A
    /// source the surface could not hold — past the budget, or on a device with no derivation
    /// passes — has its pixels let go too, and every boundary of it names why until a job hands
    /// them again. Run after every message.
    fn gpu_release_source_pixels(&mut self) {
        let (figures, refused) = self.surface_source();
        let Some(held) = self
            .gpu
            .source
            .as_mut()
            .filter(|held| held.gpu.holds_pixels())
        else {
            return;
        };
        let version = held.gpu.version();
        let why = if figures.is_some_and(|figures| figures.version == version && figures.ready) {
            "pixels-let-go"
        } else if let Some((_, fallback)) = refused.filter(|(refused, _)| *refused == version) {
            held.refused = Some(fallback.as_str());
            fallback.as_str()
        } else {
            return;
        };
        held.gpu = held.gpu.resident();
        self.event(
            "gpu_source_resident",
            || json!({"version": version, "why": why}),
        );
    }

    /// The coordinate grids asked for since the last message, each computed off the interface
    /// thread on the runtime's blocking pool and answered as a [`GridAnswer`].
    fn gpu_compute_grids(&mut self) -> iced::Task<super::Message> {
        let started = self.gpu.grids.start();
        if started.is_empty() {
            return iced::Task::none();
        }
        let count = started.len();
        self.event("gpu_grid_requested", || json!({"grids": count}));
        iced::Task::batch(started.into_iter().map(|key| {
            super::tasks::owner_task(
                {
                    let key = key.clone();
                    move || grid_of(&key)
                },
                move |grid| {
                    super::Message::Preview(super::message::preview::PreviewMessage::GridReady(
                        Box::new(GridAnswer { key, grid }),
                    ))
                },
            )
        }))
    }

    /// A lens warp's grid computed for a boundary key: held for every boundary of that key derived
    /// from then on. The tick after it derives the boundary and draws.
    pub(crate) fn gpu_grid_ready(&mut self, answer: GridAnswer) {
        let detail = match &answer.grid {
            Ok(grid) => json!({"held": true, "columns": grid.columns, "rows": grid.rows}),
            Err(why) => json!({"held": false, "why": why}),
        };
        self.event("gpu_grid", || detail);
        self.gpu.grids.finish(&answer.key, answer.grid);
        // A picture at rest through a lens warp waits for its stage's grid.
        self.gpu_convert_rest();
        self.gpu_convert_stage();
    }

    /// A committed stack's job, about to be queued: the plan of the stack itself it carries
    /// (`rest`'s view plan) is the stack's picture at rest's view plan ([`AtRest`]), and is held
    /// behind the CPU frame over the resident boundary, derived from the source the surface holds
    /// when no boundary held has its key, so the surface keeps the stack's outputs and the next
    /// gesture's first tick draws on the GPU. `region` is whether the job's view is a percentage
    /// zoom's region, and `committed` whether the job draws the whole committed stack. Nothing
    /// while the GPU stage is refused; a drag winding down keeps its boundary, which the stack's view
    /// plan is then drawn from at rest.
    pub(crate) fn gpu_resident_from(
        &mut self,
        rest: Option<Box<luxforge_core::GpuRest>>,
        region: bool,
        committed: bool,
    ) {
        if !committed {
            return;
        }
        // Every committed job sets the picture at rest's view plan or lets the last one go: a plan
        // of an earlier stack is never drawn at rest.
        self.gpu.at_rest = None;
        if self.gpu_preview_allowed().is_err() {
            return;
        }
        let Some(GpuPreview {
            answer: GpuAnswer::Plan(plan),
            boundary: Some(request),
            ..
        }) = rest.map(|rest| rest.view)
        else {
            return;
        };
        // A region's plan is held only for a job of a region, and Fit's only for Fit's.
        if region != request.key.region().is_some() {
            return;
        }
        // A committed stack's job may be queued while the gesture that committed it is still
        // winding down; that drag keeps its boundary and leaves it resident once released, and the
        // stack's view plan is drawn from it at rest.
        if let Some(held) = self
            .gpu
            .drag
            .as_ref()
            .and_then(|drag| drag.held.as_ref())
            .filter(|held| held.serves(&request))
        {
            let (boundary, origin, grid) = (held.boundary.clone(), held.origin, held.grid.clone());
            let region = held.key.region();
            self.gpu_at_rest_over(&plan, boundary, origin, grid, region);
            return;
        }
        let held = match self
            .gpu
            .resident
            .take_if(|resident| resident.held.serves(&request))
        {
            Some(resident) => resident.held,
            None => {
                // A boundary a drag would not derive is not derived for the resting stack either.
                if let Some((requested, bound)) = over_budget(&plan, &request, self.gpu_budget()) {
                    self.event("gpu_boundary", || {
                        json!({"held": false, "resident": true, "why": "budget-exceeded",
                            "requested": requested, "budget": bound})
                    });
                    return;
                }
                let derived = derive_held(
                    self.gpu.source.as_ref(),
                    &mut self.gpu.versions,
                    &mut self.gpu.grids,
                    &request,
                );
                let held = match derived {
                    Ok(held) => held,
                    Err(why) => {
                        self.event(
                            "gpu_boundary",
                            || json!({"held": false, "resident": true, "why": why}),
                        );
                        return;
                    }
                };
                let detail = derived_evidence(&held, true);
                self.event("gpu_boundary", || detail);
                held
            }
        };
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        let evaluated = self.surface_report().evaluated;
        let standby = gpu_plan::surface_plan_over(
            &plan,
            held.boundary.clone(),
            held.origin,
            held.grid.as_ref(),
            held.key.region(),
        )
        .ok()
        .map(|converted| {
            self.gpu.stamps.hand(
                super::gpu_settle::marked(converted, &plan, clip),
                0,
                &plan,
                evaluated,
            )
        });
        let (boundary, origin, grid) = (held.boundary.clone(), held.origin, held.grid.clone());
        let region = held.key.region();
        self.gpu_at_rest_over(&plan, boundary, origin, grid, region);
        let asset = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone());
        self.gpu.resident = Some(Resident {
            held,
            plan: standby,
            asset,
        });
    }

    /// The committed stack's view plan `plan` over `boundary`, at `origin` of its stage, through
    /// `grid` of a lens warp, of `region` at a percentage zoom: converted with the clipping
    /// overlay's marks shown now and held as the picture at rest's view plan ([`AtRest`]), or
    /// nothing held where the surface could not run it.
    fn gpu_at_rest_over(
        &mut self,
        plan: &CorePlan,
        boundary: GpuBoundary,
        origin: (u32, u32),
        grid: Option<gpu_plan::WarpGrid>,
        region: Option<luxforge_core::Region>,
    ) {
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        let evaluated = self.surface_report().evaluated;
        let Ok(converted) =
            gpu_plan::surface_plan_over(plan, boundary.clone(), origin, grid.as_ref(), region)
        else {
            return;
        };
        let handed = self.gpu.stamps.hand(
            super::gpu_settle::marked(converted, plan, clip),
            0,
            plan,
            evaluated,
        );
        let version = boundary.version();
        self.gpu.at_rest = Some(AtRest {
            core: Box::new(plan.clone()),
            boundary,
            origin,
            grid,
            region,
            clip,
            handed,
        });
        self.event("gpu_at_rest", || {
            json!({"boundary": version, "region": region.map(|rect| [rect.x0, rect.y0,
                rect.x0 + rect.width, rect.y0 + rect.height])})
        });
    }

    /// After every message: the picture at rest's view plan carries the clipping overlay's marks
    /// shown now, converted again when they change, as a gesture's plans are planned with them.
    pub(crate) fn gpu_mark_at_rest(&mut self) {
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        let Some(at_rest) = self.gpu.at_rest.take() else {
            return;
        };
        if at_rest.clip == clip {
            self.gpu.at_rest = Some(at_rest);
            return;
        }
        let AtRest {
            core,
            boundary,
            origin,
            grid,
            region,
            ..
        } = at_rest;
        self.gpu_at_rest_over(&core, boundary, origin, grid, region);
    }

    /// Whether the photograph is the committed stack at rest, which the GPU draws in place of its
    /// frame — Compare's Before side among them: the gate lets the GPU stage draw, no gesture's
    /// draft is open or its plan still drawn, no crop draft is shown, and no evidence hook hands a
    /// plan of its own.
    pub(crate) fn gpu_at_rest(&self) -> bool {
        #[cfg(test)]
        if self.gpu.rest_off {
            return false;
        }
        self.gpu_preview_allowed().is_ok()
            && self.core_gesture().is_none()
            && self.gpu.drag.is_none()
            && !self.drafting()
            && self
                .evidence
                .as_ref()
                .is_none_or(|evidence| evidence.gpu_identity.is_none())
    }

    /// The committed stack's view plan the surface draws at rest ([`AtRest`]), where the view
    /// shows it: its whole frame at Fit and below 100%, its region at 100% and above while that
    /// region holds the view.
    pub(crate) fn gpu_rest_plan(&self) -> Option<(&surface::GpuPlan, surface::GpuChange)> {
        if !self.gpu_at_rest() {
            return None;
        }
        let at_rest = self.gpu.at_rest.as_ref()?;
        self.gpu_plan_shown(&at_rest.handed.plan)
            .then_some((&at_rest.handed.plan, at_rest.handed.change))
    }

    /// Whether the picture at rest in tiles handed to the surface has not been drawn whole yet:
    /// while it is still to land, the photograph is marked rendering.
    pub(crate) fn gpu_rest_landing(&self) -> bool {
        let Some(rest) = self.gpu_rest_handed() else {
            return false;
        };
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        drawn.drawn_rest != Some(rest.version)
            && !drawn.gpu_rest.is_some_and(|figures| {
                figures.version == rest.version && figures.fallback.is_some()
            })
    }

    /// The displayed stack's picture at rest in tiles, from its job: held for the surfaces to draw
    /// ([`surface::GpuRest`]), under a new version unless they are the tiles held. `None` lets the
    /// one held go: a view that draws the stack at its own size or larger, or tiles the GPU cannot
    /// draw. Nothing while the GPU stage is refused.
    pub(crate) fn gpu_rest_from(&mut self, tiles: Option<Box<luxforge_core::RestTiles>>) {
        if self.gpu_preview_allowed().is_err() {
            self.gpu.rest = None;
            return;
        }
        let Some(tiles) = tiles else {
            if let Some(held) = self.gpu.rest.take() {
                let version = held.version;
                self.event(
                    "gpu_rest_released",
                    || json!({"version": version, "why": "no-tiles"}),
                );
            }
            return;
        };
        if self
            .gpu
            .rest
            .as_ref()
            .is_some_and(|held| held.tiles == tiles)
        {
            return;
        }
        self.gpu.rests += 2;
        self.gpu.rest = Some(HeldRest {
            tiles,
            version: self.gpu.rests - 1,
            counts_version: self.gpu.rests,
            gpu: None,
            counts: None,
            refused: None,
            wanted: false,
        });
        self.gpu_convert_rest();
    }

    /// Draw a crop draft's input stage on the GPU: `tiles`, the layer prefix's picture at rest at
    /// the stage's display bounds, which the owner planned with the stage's job, over `source`,
    /// the job's prepared source, held for the surfaces. Whether the GPU takes it: not while the
    /// gate refuses the GPU stage, for tiles with no reduction, or for tiles a conversion refuses
    /// at once; the reference renders the stage then. `O(tiles × steps)`, no pixel.
    pub(crate) fn gpu_stage_from(
        &mut self,
        tiles: Box<luxforge_core::RestTiles>,
        source: &PreviewSource,
    ) -> bool {
        self.gpu_stage_end();
        if self.gpu_preview_allowed().is_err() || tiles.reduction.is_none() {
            return false;
        }
        self.gpu_hold_source(source);
        self.gpu.rests += 1;
        self.gpu.stage = Some(StageRest {
            tiles,
            version: self.gpu.rests,
            gpu: None,
            refused: None,
        });
        self.gpu_convert_stage();
        !matches!(self.gpu_stage_state(), StageState::Refused(_))
    }

    /// Let the crop stage drawn on the GPU go: its draft ended, or the reference draws the stage.
    pub(crate) fn gpu_stage_end(&mut self) {
        if let Some(stage) = self.gpu.stage.take() {
            let version = stage.version;
            self.event("gpu_stage_released", || json!({"version": version}));
        }
    }

    /// Convert the crop stage held for the surfaces, once its source and any lens warp's stage grid
    /// are held, as [`Self::gpu_convert_rest`] converts the photograph's.
    pub(crate) fn gpu_convert_stage(&mut self) {
        let gpu = &mut self.gpu;
        let Some(held) = gpu
            .stage
            .as_mut()
            .filter(|held| held.gpu.is_none() && held.refused.is_none())
        else {
            return;
        };
        let detail = match rest_of(
            gpu.source.as_ref(),
            &mut gpu.versions,
            &mut gpu.grids,
            &held.tiles,
            held.version,
        ) {
            Ok(None) | Err("source-missing") => return,
            Ok(Some(converted)) => {
                let detail = json!({"version": held.version, "tiles": converted.tiles.len(),
                    "view": converted.reduction.as_ref().map(|reduction| [reduction.view.0,
                        reduction.view.1]),
                    "output": [held.tiles.output.width, held.tiles.output.height]});
                held.gpu = Some(converted);
                detail
            }
            Err(reason) => {
                held.refused = Some(reason);
                json!({"version": held.version, "refused": reason})
            }
        };
        self.event("gpu_stage", || detail);
    }

    /// The crop stage the surfaces are handed to draw under the frame: none while the gate refuses
    /// the GPU stage.
    pub(crate) fn gpu_stage_handed(&self) -> Option<&surface::GpuRest> {
        self.gpu_preview_allowed().ok()?;
        self.gpu.stage.as_ref()?.gpu.as_ref()
    }

    /// Where the crop stage on the GPU has got to: what the conversion and the surface's last draw
    /// say of its version.
    pub(crate) fn gpu_stage_state(&self) -> StageState {
        let Some(stage) = &self.gpu.stage else {
            return StageState::None;
        };
        if let Some(reason) = stage.refused {
            return StageState::Refused(reason);
        }
        if let Err(reason) = self.gpu_preview_allowed() {
            return StageState::Refused(reason);
        }
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if let Some(figures) = drawn
            .gpu_rest
            .filter(|figures| figures.version == stage.version)
            && let Some(fallback) = figures.fallback
        {
            return StageState::Refused(fallback.as_str());
        }
        if drawn.drawn_rest == Some(stage.version) {
            StageState::Drawn(stage.version)
        } else {
            StageState::Pending
        }
    }

    /// Convert the picture at rest held for the surfaces, once its source and any lens warp's
    /// stage grid are held: run when it is held, and after any message that may bring either.
    pub(crate) fn gpu_convert_rest(&mut self) {
        let gpu = &mut self.gpu;
        let Some(held) = gpu
            .rest
            .as_mut()
            .filter(|held| held.gpu.is_none() && held.refused.is_none())
        else {
            return;
        };
        let detail = match rest_of(
            gpu.source.as_ref(),
            &mut gpu.versions,
            &mut gpu.grids,
            &held.tiles,
            held.version,
        ) {
            Ok(None) => return,
            Ok(Some(converted)) => {
                let anchor = held.tiles.plan.anchor();
                let detail = json!({"version": held.version, "tiles": converted.tiles.len(),
                    "view": converted.reduction.as_ref().map(|reduction| [reduction.view.0,
                        reduction.view.1]),
                    "output": [held.tiles.output.width, held.tiles.output.height],
                    "side": held.tiles.tiles.first().map(|tile| tile.rect.width.max(tile.rect.height)),
                    "sweeps": converted.stages.as_ref().map_or(0, |stages| stages.sweeps.len()),
                    "light_sweeps": converted.light_sweeps.len(),
                    "anchor": anchor.multiple, "lead": anchor.lead});
                // The same tiles for their counts alone, under a version of their own, so a
                // surface handed one after the other starts over rather than drawing the picture.
                held.counts = Some(match converted.reduction {
                    Some(_) => surface::GpuRest {
                        version: held.counts_version,
                        tiles: Arc::clone(&converted.tiles),
                        stages: converted.stages.clone(),
                        light_sweeps: Arc::clone(&converted.light_sweeps),
                        reduction: None,
                    },
                    None => converted.clone(),
                });
                held.gpu = Some(converted);
                detail
            }
            // The source the tiles read is not held yet: tried again when it is.
            Err("source-missing") => return,
            Err(reason) => {
                held.refused = Some(reason);
                json!({"version": held.version, "refused": reason})
            }
        };
        self.event("gpu_rest", || detail);
    }

    /// The picture at rest in tiles the surfaces are handed: the committed stack's, while no
    /// gesture's draft is open — a released gesture's last plan may still be drawn, beside which
    /// the tiles are drawn, to dissolve in over it — and no crop draft or clipping overlay is shown,
    /// the overlay's marks being the view plan's; Compare's Before side among them. None while the
    /// gate refuses the GPU stage or an evidence hook hands a plan of its own.
    pub(crate) fn gpu_rest_handed(&self) -> Option<&surface::GpuRest> {
        self.gpu_preview_allowed().ok()?;
        #[cfg(test)]
        if self.gpu.rest_off {
            return None;
        }
        if self.core_gesture().is_some()
            || self.drafting()
            || super::gpu_settle::clip_flags(&self.session.workspace).is_some()
            || self
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.gpu_identity.is_some())
        {
            return None;
        }
        self.gpu
            .rest
            .as_ref()?
            .gpu
            .as_ref()
            .filter(|rest| rest.reduction.is_some())
    }

    /// The picture at rest's tiles for their histogram and clipping counts alone, which the
    /// surfaces are handed while the counts of the content the GPU presents are still to come and
    /// the picture, which counts its tiles as it draws them, is not handed: at 100% and above,
    /// where the view draws the stage at its own size, and while the clipping overlay's marks are
    /// the view plan's. None while a gesture or a crop draft is open, the gate refuses the GPU
    /// stage or an evidence hook hands a plan of its own.
    pub(crate) fn gpu_counts_handed(&self) -> Option<&surface::GpuRest> {
        self.gpu_preview_allowed().ok()?;
        #[cfg(test)]
        if self.gpu.rest_off {
            return None;
        }
        if self.gpu_rest_handed().is_some()
            || self.core_gesture().is_some()
            || self.drafting()
            || self
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.gpu_identity.is_some())
        {
            return None;
        }
        let held = self.gpu.rest.as_ref().filter(|held| held.wanted)?;
        held.counts.as_ref()
    }

    /// Compare begins: the GPU picture of the stack on screen — its view plan and its picture at
    /// rest in tiles — is retained, to be drawn as Compare's After side, and the photograph's
    /// surface waits for the Before's own committed job to plan its own.
    pub(crate) fn gpu_compare_begin(&mut self) {
        if self.gpu.compare.is_some() {
            return;
        }
        let retained = Retained {
            at_rest: self.gpu.at_rest.take(),
            rest: self.gpu.rest.take(),
        };
        let detail = json!({
            "view_boundary": retained.at_rest.as_ref().map(|at_rest| at_rest.boundary.version()),
            "rest": retained.rest.as_ref().map(|rest| rest.version),
        });
        self.gpu.compare = Some(retained);
        self.event("gpu_compare_retained", || detail);
    }

    /// Compare ends: the stack it began over is on screen again, its retained GPU picture handed
    /// back to the photograph at once, which that stack's own job then plans again.
    pub(crate) fn gpu_compare_end(&mut self) {
        let Some(retained) = self.gpu.compare.take() else {
            return;
        };
        let detail = json!({
            "view_boundary": retained.at_rest.as_ref().map(|at_rest| at_rest.boundary.version()),
            "rest": retained.rest.as_ref().map(|rest| rest.version),
        });
        self.gpu.at_rest = retained.at_rest;
        self.gpu.rest = retained.rest;
        self.event("gpu_compare_restored", || detail);
    }

    /// Compare's After side on the GPU: the retained view plan, with its serial, where the view
    /// draws it as a whole frame — at Fit and below 100% — and the retained picture at rest in
    /// tiles; nothing while the gate refuses the GPU stage, or no Compare runs.
    pub(crate) fn gpu_compare_after(
        &self,
    ) -> (
        Option<(&surface::GpuPlan, surface::GpuChange)>,
        Option<&surface::GpuRest>,
    ) {
        let Some(retained) = self
            .gpu
            .compare
            .as_ref()
            .filter(|_| self.presentation.compare_after.is_some())
            .filter(|_| self.gpu_preview_allowed().is_ok())
        else {
            return (None, None);
        };
        let whole = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Fit => true,
            luxforge_core::Zoom::Percent { value } => value < 100.0,
        };
        let plan = retained
            .at_rest
            .as_ref()
            .filter(|at_rest| whole && at_rest.handed.plan.region.is_none())
            .map(|at_rest| (&at_rest.handed.plan, at_rest.handed.change));
        let rest = retained
            .rest
            .as_ref()
            .filter(|_| whole && super::gpu_settle::clip_flags(&self.session.workspace).is_none())
            .and_then(|rest| rest.gpu.as_ref())
            // Tiles with no reduction are the counts' alone: they draw no After side.
            .filter(|rest| rest.reduction.is_some());
        (plan, rest)
    }

    /// Whose picture of the displayed content the surface is handed to draw, as `preview_displayed`
    /// names it: `gpu` while a GPU picture stands in for the CPU's frame — the committed stack's
    /// view plan or its picture at rest in tiles, a gesture's plan drawn in place of its frame —
    /// and `reference` while the CPU's frame is the photograph.
    pub(crate) fn displayed_picture(&self) -> &'static str {
        let surfaces = self.surfaces();
        if (surfaces.gpu.is_some() && !surfaces.gpu_hold) || surfaces.gpu_rest.is_some() {
            "gpu"
        } else {
            "reference"
        }
    }

    /// After every message: the first draw of each picture at rest in tiles, as evidence records
    /// it, with the interface thread's time its tiles took.
    pub(crate) fn gpu_follow_rest_drawn(&mut self) {
        let Some(version) = self.gpu_rest_handed().map(|rest| rest.version) else {
            return;
        };
        if self.gpu.rest_drawn == Some(version) {
            return;
        }
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if drawn.drawn_rest != Some(version) {
            return;
        }
        self.gpu.rest_drawn = Some(version);
        let figures = drawn.gpu_rest.filter(|figures| figures.version == version);
        self.event("gpu_rest_drawn", || {
            json!({"version": version,
                "tiles": figures.map(|figures| figures.tiles),
                "prepare_ms": figures.map(|figures| figures.prepare_us as f64 / 1000.0),
                "attribution": figures.as_ref().map(rest_attribution),
                "dissolve": drawn.drawn_rest_dissolve.map(|dissolve| dissolve.from)})
        });
    }

    /// A committed stack's job carries the plans its gestures are likely to draw, the first `open`
    /// of them the open stack's and then the rest of the program set, and the light links the open
    /// stack's ticks compute: hand their sequences to the surface to compile, in that order, before
    /// a drag begins.
    pub(crate) fn gpu_warm_from(&mut self, warm: Option<&luxforge_core::GpuWarmList>) {
        let Some(warm) = warm else {
            return;
        };
        let plans = &warm.plans;
        // While a clipping overlay is shown the gestures' plans carry its marks.
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        let mut open_sequences = 0;
        let mut sequences: Vec<(Vec<GpuStep>, surface::BoundaryFormat)> = Vec::new();
        for (index, plan) in plans.iter().enumerate() {
            if let Ok(steps) = gpu_plan::plan_steps(plan) {
                sequences.push((
                    super::gpu_settle::marked_steps(steps, plan, clip),
                    gpu_plan::boundary_format(luxforge_core::BoundaryFormat::of(plan.linear)),
                ));
            }
            if index < warm.open {
                open_sequences = sequences.len();
            }
        }
        // The light links the list warms, each writing the light plane its plans read.
        let lights: Vec<Vec<GpuStep>> = warm
            .lights
            .iter()
            .filter_map(|(k, light)| {
                let k = u32::try_from(*k).ok()?;
                gpu_plan::surface_light(light, k).ok()
            })
            .map(|light| light.steps)
            .collect();
        let same = self.gpu.warm.as_ref().is_some_and(|warm| {
            warm.sequences() == sequences.as_slice()
                && warm.lights() == lights.as_slice()
                && warm.open() == open_sequences
        });
        if same || sequences.is_empty() {
            return;
        }
        let version = self.gpu.warm.as_ref().map_or(1, |warm| warm.version() + 1);
        self.event("gpu_preview_warm", || {
            json!({"version": version, "sequences": sequences.len(),
                "open": open_sequences, "lights": lights.len()})
        });
        self.gpu.warm = Some(
            GpuWarm::new(version, sequences)
                .with_open(open_sequences)
                .with_lights(lights),
        );
    }

    /// What the next tick asks the owner to plan its GPU preview for: at Fit and below 100%, a
    /// whole frame at the job's bounds, which below 100% are the stage's displayed size and do not
    /// change with a pan; at 100% or more, the visible region of the output stage — or the region
    /// the open drag asked for at this zoom while it still holds the view, so a pan inside it keeps
    /// its boundary.
    pub(crate) fn gpu_ask(&self) -> GpuAsk {
        let value = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Percent { value } if value >= 100.0 => value,
            luxforge_core::Zoom::Fit | luxforge_core::Zoom::Percent { .. } => return GpuAsk::Fit,
        };
        let Some(wanted) = self
            .presentation
            .dimensions
            .and_then(|stage| self.desired_view_for(stage))
        else {
            return GpuAsk::Off;
        };
        let asked = self
            .gpu
            .drag
            .as_ref()
            .filter(|drag| drag.ended.is_none() && drag.zoom == Some(value))
            .and_then(|drag| drag.wanted.as_ref())
            .and_then(|wanted| wanted.key.region())
            .filter(|rect| super::preview::contains_region(*rect, wanted));
        GpuAsk::Region(
            asked.unwrap_or(wanted),
            f64::from(value) / 100.0,
            self.gpu_reduce_after(),
        )
    }

    /// What a region's own figures pass before the owner plans its draft at the reduced stage of
    /// the view's area too: the core's figure, or a test's.
    fn gpu_reduce_after(&self) -> u64 {
        #[cfg(test)]
        if let Some(bytes) = self.gpu.reduce_after {
            return bytes;
        }
        luxforge_core::REDUCED_AFTER_BYTES
    }

    /// Whether the open gesture's GPU frame is the view's motion frame: the surface draws its plan
    /// of a region holding `wanted`, not held behind a CPU frame, and has evaluated it.
    pub(crate) fn gpu_draws_view(&self, wanted: Region) -> bool {
        let Some((plan, _)) = self.gesture_gpu_plan() else {
            return false;
        };
        plan.region.is_some_and(|region| holds_view(region, wanted))
            && !self.gpu_held()
            && self.surface_report().ready_boundary == Some(plan.boundary.version())
    }

    /// The open gesture's converted plan and the draft revision it draws, where the surface runs
    /// it, with no comparison on screen: a whole frame's plan at Fit and below 100%, and at 100% or
    /// more a region's while its region holds the view.
    pub(crate) fn gesture_gpu_plan(&self) -> Option<(&surface::GpuPlan, u64)> {
        if self.presentation.compare_after.is_some() {
            return None;
        }
        let (plan, revision) = self.gpu.surface_plan()?;
        if !self.gpu_plan_shown(plan) {
            return None;
        }
        // Between drafts the resident boundary's last plan is held behind the CPU frame, which
        // keeps the surface's slot for the next draft.
        if self.gpu.resident_plan() {
            return Some((plan, revision));
        }
        // An open draft that another client's commit conflicted, or that was reapplied over a
        // newer entry and whose first tick there has not answered yet, draws none of its plans:
        // each was planned over the entry before, which is no longer the photograph. The frame is
        // the CPU's until a tick plans over the current entry.
        let drag = self.gpu.drag.as_ref()?;
        if let Some(gesture) = self.core_gesture()
            && gesture.draft.draft_id == drag.draft
            && (gesture.draft.conflicted || Some(gesture.draft.base_revision) != drag.base)
        {
            return None;
        }
        self.gpu.surface_plan()
    }

    /// Whether the view shows what `plan` draws: at Fit and below 100% a whole frame's, the view
    /// drawing its photograph's frame alone as Fit does; at 100% and above a region's while that
    /// region holds the view. Over a stack the GPU presented with no CPU frame, the plan it has is
    /// shown at 100% and above while the view's region is planned — a pan's region, or the whole
    /// frame's plan a zoom from Fit leaves: it is the only picture of that stack, and the frame
    /// under it an earlier stack's.
    fn gpu_plan_shown(&self, plan: &surface::GpuPlan) -> bool {
        let presented =
            self.presentation.gpu_presented == Some(self.presentation.presented_content);
        match (&self.session.preview.view.zoom, plan.region) {
            (luxforge_core::Zoom::Fit, None) => true,
            (luxforge_core::Zoom::Percent { value }, None) if *value < 100.0 => true,
            (luxforge_core::Zoom::Percent { .. }, None) => presented,
            (luxforge_core::Zoom::Percent { value }, Some(region)) if *value >= 100.0 => self
                .presentation
                .dimensions
                .filter(|stage| *stage == region.full_stage)
                .and_then(|stage| self.desired_view_for(stage))
                .is_some_and(|wanted| presented || holds_view(region, wanted)),
            _ => false,
        }
    }

    /// Why the desktop hands the surface no plan for the open gesture's newest tick, or why that
    /// tick took the CPU path: the GPU stage refused, the plan's reason, a boundary not yet held, the
    /// converter's reason or the surface's fallback.
    pub(crate) fn gpu_plan_fallback(&self) -> Option<String> {
        self.gpu_cpu_reason().map(|reason| reason.code.to_owned())
    }

    /// [`Self::gpu_plan_fallback`] with what the status bar's notice is derived from
    /// ([`CpuReason::notice`]): the layer the reason names, and how long the ticks that named
    /// `compiling` have done so. The open gesture's, kept until its drag is released, which is when
    /// its settle ends: the frame replacing the drafted one is presented, or nothing more is coming
    /// (`after_message`).
    pub(crate) fn gpu_cpu_reason(&self) -> Option<CpuReason<'_>> {
        if let Err(code) = self.gpu_preview_allowed() {
            return Some(CpuReason {
                code,
                layer: None,
                compiling_for: None,
            });
        }
        let drag = self.gpu.drag.as_ref()?;
        Some(CpuReason {
            code: drag.reason.as_deref()?,
            layer: drag.layer.as_deref(),
            compiling_for: drag.compiling.map(|run| run.lasted),
        })
    }

    /// Whether the surface holds the drawn plan behind the CPU frame: the CPU frame of the drawn
    /// revision, or a newer one, is presented, and it is the reference.
    pub(crate) fn gpu_held(&self) -> bool {
        let Some((_, revision)) = self.gpu.surface_plan() else {
            return false;
        };
        // The resident boundary's plan is always held: between drafts the CPU frame is drawn.
        if self.gpu.resident_plan() {
            return true;
        }
        let draft = self.gpu.drag.as_ref().map(|drag| &drag.draft);
        self.presentation.displayed_draft_id.as_ref() == draft
            && self
                .presentation
                .displayed_draft_revision
                .is_some_and(|displayed| displayed >= revision)
    }

    /// The evidence of a boundary let go.
    fn log_release(&self, version: u64, why: &str) {
        self.event(
            "gpu_boundary_released",
            || json!({"version": version, "why": why}),
        );
    }
}

/// After every message: a draft that ended keeps its drawn plan until the frame that replaces it
/// is presented, or until nothing more is coming, and then hands its boundary to the resident slot
/// the next draft starts from; a resident boundary of another photograph is let go.
pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> iced::Task<super::Message> {
    editor.gpu_release_source_pixels();
    let asset = editor
        .document
        .state
        .as_ref()
        .map(|state| state.asset.id.clone());
    if let Some(resident) = editor
        .gpu
        .resident
        .take_if(|resident| resident.asset != asset)
    {
        let version = resident.held.boundary.version();
        editor.event(
            "gpu_boundary_released",
            || json!({"version": version, "why": "asset-changed"}),
        );
    }
    // The source of another photograph, or of none, is let go with it: the surfaces are handed
    // none, and the pipeline lets its textures go; so is the picture at rest drawn from it, its
    // view plan and its tiles.
    if let Some(source) = editor.gpu.source.take_if(|source| source.asset != asset) {
        editor.gpu.at_rest = None;
        editor.gpu.compare = None;
        let version = source.gpu.version();
        editor.event(
            "gpu_source_released",
            || json!({"version": version, "why": "asset-changed"}),
        );
        if let Some(rest) = editor.gpu.rest.take() {
            let version = rest.version;
            editor.event(
                "gpu_rest_released",
                || json!({"version": version, "why": "asset-changed"}),
            );
        }
    }
    release_ended_drag(editor, asset);
    // With the GPU stage refused the reference renderer's frames are the photograph: no
    // view plan or tiles are kept to be drawn at rest once it is back, before a committed job
    // plans them for the stack on screen then.
    if editor.gpu_preview_allowed().is_err() {
        editor.gpu.at_rest = None;
        editor.gpu.rest = None;
        editor.gpu.compare = None;
    }
    editor.gpu_mark_at_rest();
    editor.gpu_follow_rest_drawn();
    editor.gpu_follow_warm_up();
    editor.gpu_compute_grids()
}

/// A draft that ended keeps its drawn plan until the frame that replaces it is presented, or
/// until nothing more is coming, and then hands its boundary to the resident slot the next draft
/// starts from.
fn release_ended_drag(editor: &mut Editor, asset: Option<luxforge_core::AssetId>) {
    let open = editor
        .core_gesture()
        .map(|gesture| gesture.draft.draft_id.clone());
    let presented = editor.presentation.presented_generation;
    let idle = !editor.presentation.queue.is_busy() && !editor.presentation.queue.ready();
    let Some(drag) = &mut editor.gpu.drag else {
        return;
    };
    if open.as_ref() == Some(&drag.draft) {
        return;
    }
    let ended = *drag.ended.get_or_insert(presented);
    if presented > ended || idle {
        let Some(drag) = editor.gpu.drag.take() else {
            return;
        };
        match drag.held {
            // The boundary stays on the GPU for the next draft, behind the CPU frame.
            Some(held) => {
                let version = held.boundary.version();
                editor.gpu.resident = Some(Resident {
                    held,
                    plan: drag.surface.or(drag.standby),
                    asset,
                });
                editor.event(
                    "gpu_boundary_resident",
                    || json!({"version": version, "why": "draft-ended"}),
                );
            }
            None => editor.event(
                "gpu_boundary_released",
                || json!({"version": null, "why": "draft-ended"}),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        BASIC_EFFECT, GpuAnswer, GpuPlanRequest, Layer, ModuleRegistry, Recipe, Stage,
    };

    /// A plan's change is measured from an earlier one only in the same context: over another
    /// boundary, or with other clipping marks, it changes anywhere, however its operations compare.
    #[test]
    fn a_change_is_measured_only_over_the_same_boundary_and_marks() {
        let recipe = Recipe {
            layers: vec![Layer::new(BASIC_EFFECT, json!({"exposure": 0.5}))],
            ..Recipe::default()
        };
        let side = Stage {
            width: 64,
            height: 64,
        };
        let request = GpuPlanRequest::fit(0, side, side);
        let GpuAnswer::Plan(core) =
            luxforge_core::gpu_plan(&ModuleRegistry::builtin(), &recipe, request)
                .expect("the stack compiles")
        else {
            panic!("a plan");
        };
        let over = |version: u64| {
            let boundary = GpuBoundary::from_linear(
                surface::BoundaryFormat::Half,
                64,
                64,
                version,
                std::iter::repeat_n([0.25, 0.5, 0.75, 1.0], 64 * 64),
            )
            .expect("a whole boundary");
            gpu_plan::surface_plan(&core, boundary).expect("a runnable plan")
        };
        let marked = |version: u64, flags: [bool; 2]| {
            super::super::gpu_settle::marked(over(version), &core, Some(flags))
        };
        let mut stamps = Stamps::default();
        let first = stamps.hand(over(1), 0, &core, None).change;
        assert_eq!(first.since, None, "nothing evaluated yet");
        let same = stamps.hand(over(1), 0, &core, Some(first.serial)).change;
        assert_eq!(same.since, Some((first.serial, [0; 4])));
        let other = stamps.hand(over(2), 0, &core, Some(same.serial)).change;
        assert_eq!(other.since, None, "another boundary");
        let shadows = stamps
            .hand(marked(2, [true, false]), 0, &core, Some(other.serial))
            .change;
        assert_eq!(shadows.since, None, "marks shown");
        let both = stamps
            .hand(marked(2, [true, true]), 0, &core, Some(shadows.serial))
            .change;
        assert_eq!(both.since, None, "other marks");
        let again = stamps
            .hand(marked(2, [true, true]), 0, &core, Some(both.serial))
            .change;
        assert_eq!(again.since, Some((both.serial, [0; 4])), "the same marks");
    }
}
