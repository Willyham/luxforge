//! What a spatial operation is on the GPU: compute passes that fill planes over the boundary, then
//! a pointwise apply per unit that reads them (`docs/design/gpu-preview.md`, "Spatial programs").
//!
//! A spatial program ([`GpuProgramKind::Spatial`]) is WGSL text the module owns beside its CPU
//! units, under the **spatial convention**: besides the prelude's words and blocks, every module
//! that holds the program also declares
//!
//! ```wgsl
//! var<workgroup> lf_shared: array<f32, 1024>          // a workgroup pass's scratch
//! fn lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32>   // texel `at` of a bound plane, edge-clamped
//! fn lf_plane_size(slot: u32) -> vec2<i32>
//! fn lf_source(at: vec2<i32>) -> vec3<f32>             // the pass's unit's input at boundary texel `at`
//! fn lf_origin() -> vec2<i32>                          // the stage pixel of boundary texel (0, 0)
//! fn lf_size() -> vec2<i32>                            // the boundary's size
//! fn lf_store(at: vec2<i32>, value: vec4<f32>)         // write the pass's output plane
//! ```
//!
//! and the program declares only functions and constants, each named starting with its entry, of
//! two signatures:
//!
//! - **A kernel**, run by a pass: `fn <kernel>(at: vec2<i32>, words: u32, block: u32)`. It reads
//!   the pass's inputs as planes `0..` through `lf_plane`, may read its unit's input through
//!   `lf_source`, and writes its output through `lf_store`. A [`GpuPassShape::Texels`] pass runs it
//!   once for every `span` texels of its output, `at` the first of them; a
//!   [`GpuPassShape::Workgroup`] pass runs it on the 256 lanes of one workgroup, `at` the lane as
//!   `(lane, 0)`, which share `lf_shared`.
//! - **An apply**, a unit's pointwise last step:
//!   `fn <apply>(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32>`.
//!   `rgb` is the unit's input at boundary texel `at`, and the planes the apply reads are bound from
//!   slot `planes` on.
//!
//! A unit's input is the operation's input — the boundary through every step before the spatial
//! one, clamped where the CPU quantizes — with the applies of the units before it run over it.
//! Nothing between them is held as a colour plane: each pass that needs a unit's input computes it
//! at the texel it reads, and the frame's own pass runs every apply in order. The planes a unit's
//! passes write are all it holds, so the operation's memory is its planes and nothing that scales
//! with the number of units' colour outputs.
use super::plan::Planning;
use super::program::{GpuProgram, GpuProgramKind};
use super::{GpuAnswer, GpuFallback, GpuMask, GpuOperation, GpuPlanRequest};
use crate::{
    Error, ModuleRegistry, Recipe,
    mask_field::MaskSampling,
    modules::Stage,
    render::{Compiled, Entry, pipeline::SpatialEntry},
};

pub use luxforge_gpu_types::{
    CHAIN_APPLY_PLANES as GPU_CHAIN_APPLY_PLANES, PASS_INPUTS as GPU_PASS_INPUTS,
    PassShape as GpuPassShape, PlaneFormat as GpuPlaneFormat, PlaneSize as GpuPlaneSize,
    SHARED_VALUES as GPU_SHARED_VALUES, WORKGROUP_LANES as GPU_WORKGROUP_LANES,
};

/// One plane a spatial operation's passes write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuPlane {
    pub format: GpuPlaneFormat,
    pub size: GpuPlaneSize,
    /// Read only by the passes of the unit that writes it, so a later unit may write it again; an
    /// apply's plane, read when the frame is drawn, is not.
    pub scratch: bool,
}

impl GpuPlane {
    /// Its size over a boundary of `size` texels whose texel `(0, 0)` is stage pixel `origin`.
    pub fn extent(&self, origin: (u32, u32), size: (u32, u32)) -> (u32, u32) {
        self.size.extent(origin, size)
    }
}

/// One compute pass: a kernel of the operation's program over one output plane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPass {
    /// The kernel's function name, which starts with the program's entry.
    pub kernel: &'static str,
    /// The planes it reads, bound as `lf_plane` slots `0..`, at most [`GPU_PASS_INPUTS`].
    pub inputs: Vec<usize>,
    /// The plane it writes, which it does not read.
    pub output: usize,
    /// Its first word, an index into the operation's words.
    pub words: usize,
    /// How many of the operation's applies its `lf_source` runs: the index of its unit for a pass
    /// that reads its unit's input, else `0`, so a pass that only reads planes is the same module
    /// whatever unit runs it.
    pub source: usize,
    pub shape: GpuPassShape,
    /// Whether its kernel reads its unit's input through `lf_source`. A description sets it; the
    /// composition sets `source` from it.
    pub reads_source: bool,
    /// The index of its unit in the operation, which the composition sets.
    pub unit: usize,
}

/// One unit's apply: its function, the planes it reads and its first word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuApply {
    pub function: &'static str,
    pub planes: Vec<usize>,
    pub words: usize,
    /// The unit is exactly the identity at its words, and its apply returns its input before it
    /// reads any plane: an amount-0 unit of a drafted layer's GPU shape. Like the words, it is the
    /// tick's and no part of the sequence, so a drag across zero keeps one sequence; a tick runs
    /// none of the passes only its planes need, and they run again once it is not. A unit that is
    /// the identity through its passes, a change of zero they write, as Detail's units are, never
    /// says so: skipping them would leave the change an earlier value wrote.
    pub identity: bool,
}

/// What one spatial unit answers for the GPU ([`crate::modules::SpatialUnit::gpu`]): its program,
/// its words, the planes it writes, the passes that write them and its apply. Plane and word
/// indices are the unit's own; [`compose`] places them in the operation's.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSpatialUnit {
    pub program: &'static GpuProgram,
    pub words: Vec<u32>,
    pub planes: Vec<GpuPlane>,
    pub passes: Vec<GpuPass>,
    pub apply: GpuApply,
}

/// One spatial operation of a plan, at the stage of the content pass it follows: every unit's
/// passes in order, then the frame runs every apply in order over the operation's input.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSpatial {
    /// The layer it was compiled from.
    pub layer: usize,
    pub program: &'static GpuProgram,
    pub words: Vec<u32>,
    pub planes: Vec<GpuPlane>,
    pub passes: Vec<GpuPass>,
    pub applies: Vec<GpuApply>,
    /// The CPU reads the operation's input from a quantized frame and quantizes its output, as the
    /// byte path does: the input is clamped to `[0, 1]` before the first unit and the output after
    /// the last. The RAW linear path holds both unclamped.
    pub clamps: bool,
    /// The mask the operation's output is blended by against its input, as the colour primitive's.
    pub mask: Option<GpuMask>,
    /// The colour operations of its segment, which run on its output before the next spatial
    /// operation of the plan enters: empty for the last, whose segment's colour operations are the
    /// plan's output operations.
    pub after: Vec<super::GpuOperation>,
    /// How far beyond an output pixel, in pixels of its stage, each unit's apply depends on the
    /// unit's input, as the CPU's tiles read it; the operation's output depends on its input as far
    /// as their sum. A masked operation's passes need run only over its mask's bounds grown by the
    /// sum, and a tick that changes part of the input only over that part grown unit by unit.
    pub halos: Vec<u32>,
    /// The plane the operation reads a light from that none of its passes writes: a unit that
    /// prepares a global estimate reads it from a light plane ([`GpuPlaneSize::LIGHT`],
    /// [`crate::modules::SpatialUnit::gpu`]), which its plan's light link of the same layer writes
    /// from the whole stage ([`GpuLight`], [`super::GpuPlan::lights`]). `None` for an operation
    /// that reads no light. A step reading a light is redrawn whole when the light changes,
    /// wherever its input did.
    pub light: Option<usize>,
}

impl GpuSpatial {
    /// The bytes the operation's planes take over a boundary of `size` texels at stage `origin`.
    pub fn plane_bytes(&self, origin: (u32, u32), size: (u32, u32)) -> u64 {
        self.planes
            .iter()
            .map(|plane| {
                let (width, height) = plane.extent(origin, size);
                u64::from(width) * u64::from(height) * plane.format.texel_bytes()
            })
            .sum()
    }
}

/// The units of one operation as one [`GpuSpatial`]: each unit's words after the last's, its planes
/// placed, a scratch plane of one unit given to a later unit's plane of the same size whose format
/// it [holds](GpuPlaneFormat::holds), every plane an apply reads given one writer, and every pass's
/// `source` set to its unit's index. Every unit must carry the same program, one each of the
/// operation's applies. A light plane a unit reads and none of its passes writes is the light the
/// plan's light link writes ([`GpuSpatial::light`]): a plane of its own, never another unit's, and
/// at most one in an operation.
pub(crate) fn compose(
    layer: usize,
    units: Vec<GpuSpatialUnit>,
    clamps: bool,
    mask: Option<GpuMask>,
) -> Result<GpuSpatial, Error> {
    let Some(first) = units.first() else {
        return Err(Error::internal(
            "a spatial operation of no units has no GPU plan",
        ));
    };
    let program = first.program;
    let mut composed = GpuSpatial {
        layer,
        program,
        words: Vec::new(),
        planes: Vec::new(),
        passes: Vec::new(),
        applies: Vec::with_capacity(units.len()),
        clamps,
        mask,
        after: Vec::new(),
        halos: Vec::new(),
        light: None,
    };
    // Scratch planes the units before this one wrote, free for this one.
    let mut free: Vec<usize> = Vec::new();
    for (index, unit) in units.into_iter().enumerate() {
        if !std::ptr::eq(unit.program, program) || unit.program.kind != GpuProgramKind::Spatial {
            return Err(Error::internal(format!(
                "the units of one spatial operation run {} and {}",
                program.entry, unit.program.entry
            )));
        }
        let base = composed.words.len();
        composed.words.extend_from_slice(&unit.words);
        let mut taken: Vec<usize> = Vec::new();
        let read_light = |plane: usize| {
            unit.planes[plane].size == GpuPlaneSize::LIGHT
                && !unit.passes.iter().any(|pass| pass.output == plane)
        };
        let mut placed: Vec<usize> = Vec::with_capacity(unit.planes.len());
        for (number, plane) in unit.planes.iter().enumerate() {
            if read_light(number) {
                // The light the plan's light link writes: a plane of its own, which nothing of
                // the operation writes, so no other unit's plane may take it or be taken by it.
                if composed.light.is_some() {
                    return Err(Error::internal(format!(
                        "{} reads two lights in one operation",
                        program.entry
                    )));
                }
                composed.planes.push(GpuPlane {
                    scratch: false,
                    ..*plane
                });
                composed.light = Some(composed.planes.len() - 1);
                placed.push(composed.planes.len() - 1);
                continue;
            }
            // The smallest free plane that holds it. Every pass of the units before has run
            // before this unit's first, so an apply's plane may take one as well as a scratch
            // plane, and then no later unit may; the apply then reads a plane of its own that
            // only its last writer writes ([`single_writer`]).
            let reused = free
                .iter()
                .enumerate()
                .filter(|&(_, &candidate)| {
                    let held = composed.planes[candidate];
                    held.size == plane.size
                        && held.format.holds(plane.format)
                        && !taken.contains(&candidate)
                })
                .min_by_key(|&(_, &candidate)| composed.planes[candidate].format.texel_bytes())
                .map(|(position, _)| position)
                .map(|position| free.remove(position));
            let at = reused.unwrap_or_else(|| {
                composed.planes.push(*plane);
                composed.planes.len() - 1
            });
            composed.planes[at].scratch &= plane.scratch;
            taken.push(at);
            placed.push(at);
        }
        let place = |plane: usize| -> Result<usize, Error> {
            placed.get(plane).copied().ok_or_else(|| {
                Error::internal(format!(
                    "{} names a plane it does not declare",
                    program.entry
                ))
            })
        };
        for pass in &unit.passes {
            if pass.inputs.len() > GPU_PASS_INPUTS || pass.inputs.contains(&pass.output) {
                return Err(Error::internal(format!(
                    "{} reads more than {GPU_PASS_INPUTS} planes or its own output",
                    pass.kernel
                )));
            }
            composed.passes.push(GpuPass {
                kernel: pass.kernel,
                inputs: pass
                    .inputs
                    .iter()
                    .map(|&plane| place(plane))
                    .collect::<Result<_, _>>()?,
                output: place(pass.output)?,
                words: base + pass.words,
                source: if pass.reads_source { index } else { 0 },
                shape: pass.shape,
                reads_source: pass.reads_source,
                unit: index,
            });
        }
        let mut apply = GpuApply {
            function: unit.apply.function,
            planes: unit
                .apply
                .planes
                .iter()
                .map(|&plane| place(plane))
                .collect::<Result<_, _>>()?,
            words: base + unit.apply.words,
            identity: unit.apply.identity,
        };
        free.extend(single_writer(&mut composed, &mut apply));
        composed.applies.push(apply);
        for (plane, &at) in unit.planes.iter().zip(&placed) {
            if plane.scratch && composed.light != Some(at) {
                free.push(at);
            }
        }
    }
    Ok(composed)
}

/// Give each plane `apply` reads that more than one of the operation's passes write — a unit whose
/// last pass writes its result into a plane an earlier pass of it wrote, as sharpening writes its
/// change where its blur's horizontal pass was, or a plane the unit took from an earlier unit's
/// scratch — a plane of its own, which the last of them writes and the apply reads, and answer the
/// planes it leaves as scratch. The apply's unit is the last composed, so its passes are the last
/// ones.
///
/// So every plane an apply reads has one writer. A tick that changes part of the operation's
/// input runs that writer only where its output changes and every other pass around it
/// (`docs/design/gpu-preview.md`, "Incremental ticks"): a plane the apply reads must keep its
/// values everywhere else, which an earlier pass writing it over the larger rectangle would not.
fn single_writer(composed: &mut GpuSpatial, apply: &mut GpuApply) -> Vec<usize> {
    let mut freed: Vec<(usize, usize)> = Vec::new();
    for read in apply.planes.iter_mut() {
        if let Some(&(_, own)) = freed.iter().find(|(plane, _)| plane == read) {
            *read = own;
            continue;
        }
        let writers: Vec<usize> = (0..composed.passes.len())
            .filter(|&number| composed.passes[number].output == *read)
            .collect();
        let Some((&last, earlier)) = writers.split_last() else {
            continue;
        };
        if earlier.is_empty() {
            continue;
        }
        let own = composed.planes.len();
        composed.planes.push(composed.planes[*read]);
        composed.planes[*read].scratch = true;
        composed.passes[last].output = own;
        // A later pass of the unit reads the value the last writer leaves.
        for pass in &mut composed.passes[last + 1..] {
            for input in &mut pass.inputs {
                if *input == *read {
                    *input = own;
                }
            }
        }
        freed.push((*read, own));
        *read = own;
    }
    freed.into_iter().map(|(plane, _)| plane).collect()
}

/// A spatial operation's units that have no description, or a description whose program does not
/// qualify, as the reason its stack takes the CPU path.
pub(crate) fn admit(
    layer: usize,
    unit: &GpuSpatialUnit,
    qualifying: bool,
) -> Result<Option<GpuFallback>, Error> {
    if unit.program.kind != GpuProgramKind::Spatial {
        return Err(Error::internal(format!(
            "GPU program {} is described as a spatial unit against its {:?}",
            unit.program.entry, unit.program.kind
        )));
    }
    Ok(
        (!unit.program.enabled && !qualifying).then_some(GpuFallback::DisabledProgram {
            layer,
            program: unit.program.entry,
        }),
    )
}

/// Whether a light link reads the spatial operations before its estimating one ([`GpuLight`]):
/// the restoration (Detail) layers and any spatial (Presence) layer before it, whose exact output
/// only the full-resolution stage holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLightRestoration {
    /// The light reads their exact output, which only a staged sweep of the whole stage through
    /// them at full resolution produces ([`GpuLightInput::Stage`]). The link holds each as a
    /// spatial operation of its own ([`GpuLight::spatial`]), which names its input. The light every
    /// frame draws with at rest.
    Included,
    /// The light reads the prefix without them, their colour operations after them joining the
    /// ones before: the stand-in, a drag's per-tick light over the source for a colour drag
    /// between them, since their exact output exists only at rest (the owner's decision of
    /// 2026-10-06, `docs/decisions.md`, "GPU-first rendering").
    LeftOut,
}

/// What one unit that prepares a global estimate runs on the GPU to compute it from its whole input
/// stage: the step of its light link ([`GpuLight`], [`crate::modules::SpatialUnit::gpu_light`]).
/// Its program and words, its planes and the passes that write them, indices its own, as a
/// [`GpuSpatialUnit`]'s are.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuLightPasses {
    pub program: &'static GpuProgram,
    pub words: Vec<u32>,
    /// The block means, the input's `16 × 16` blocks over the whole stage the link reduces
    /// (`GpuPlaneSize::Reduced(16)`), then the light ([`GpuPlaneSize::LIGHT`]).
    pub planes: Vec<GpuPlane>,
    /// The reduction, which reads the light's input and writes the block means a block at a time,
    /// then the selection, one workgroup over the block means, which writes the light.
    pub passes: Vec<GpuPass>,
}

/// A light link (`docs/design/gpu-preview.md`, "The global estimate"): what computes one estimating
/// spatial operation's global estimate on the GPU per frame, from its whole input stage at full
/// resolution, never from a reduced stage, as the measurement of the per-frame light's input set
/// (`docs/specs/performance.md`, "The per-frame light's reduction factor").
///
/// - **Its input** is the whole content stage at full scale, every pixel the source fills,
///   planned from the source as a plan of [`GpuPlanRequest::from_source`] is: the colour
///   operations before the estimating one ([`Self::content`]), run per texel over the source, and
///   with the spatial operations before it included, those operations ([`Self::spatial`]), each
///   reading the light an earlier link writes where it estimates one.
/// - **Its step** ([`Self::light`]) is the estimating unit's light passes: the reduction of the
///   input into the stage's 16-pixel block means beside each block's channel minimum, each block
///   written by the one tile of the stage that holds it, then one workgroup's selection of the
///   brightest blocks into the light plane. It has no apply: the link draws no frame, and the
///   operation that reads the light reads its plane ([`GpuSpatial::light`]).
/// - **Where it runs.** A link over the source ([`Self::over_source`]) is run by any slot that
///   draws a plan reading its light, before the plan's links, whenever its content changes. One
///   holding spatial operations reads their exact output over the whole stage, which a staged sweep
///   writes into a stage texture ([`GpuLightInput::Stage`]): the picture at rest's and an export's
///   sweeps split before the operation reading it, and the light is reduced from that texture
///   before the sweep that reads it ([`super::GpuSweep::lights`]). Every other slot reads the light
///   so computed, kept under its input's key; a drag's slot computes its [`Self::stand_in`] in its
///   place where none is kept.
/// - Planned on the catalog owner from the stack's compilation alone, `O(layers + units)`, reading
///   no pixel ([`gpu_lights`]).
#[derive(Clone, Debug, PartialEq)]
pub struct GpuLight {
    /// The estimating spatial layer whose light it computes.
    pub layer: usize,
    /// The content stage it reduces, whole and at full scale.
    pub stage: Stage,
    /// It previews a RAW's linear path, whose input is not clamped. The byte path's is clamped to
    /// `[0, 1]` before it is reduced, as the frame the CPU's operation reads is.
    pub linear: bool,
    /// The colour operations its input runs before [`Self::spatial`]'s, in recipe order, over the
    /// source's texels: with the spatial operations left out, every colour operation of the
    /// prefix.
    pub content: Vec<GpuOperation>,
    /// The spatial operations its input runs, in recipe order, each with the colour operations
    /// after it ([`GpuSpatial::after`]): empty when they are left out or the prefix holds none.
    pub spatial: Vec<GpuSpatial>,
    /// The spatial layers of the prefix left out of its input.
    pub left_out: Vec<usize>,
    /// Its own step: the estimating unit's light passes, with no apply, its input clamped where
    /// the byte path clamps it.
    pub light: GpuSpatial,
    /// For a link that is not over the source, the same light with every spatial operation before
    /// it left out ([`GpuLightRestoration::LeftOut`]), over the source: what a drag's slot computes
    /// in its place where the exact light is not kept. `None` for a link over the source, and for
    /// a picture at rest's view plan, which never draws with it.
    pub stand_in: Option<Box<GpuLight>>,
    /// Where its input comes from: the source, or a staged sweep's stage texture.
    pub input: GpuLightInput,
}

/// Where a light link's input comes from (`docs/design/gpu-preview.md`, "The global estimate").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuLightInput {
    /// The source, through the link's colour operations ([`GpuLight::content`]), per texel: a
    /// prefix of colour operations alone, or a stand-in with the spatial operations left out.
    Source,
    /// The exact output of the spatial operations before its layer ([`GpuLight::spatial`]) over
    /// the whole content stage, which only a staged sweep of the whole stage writes, into the stage
    /// texture the sweep reading the light cuts its windows from. `key` names that input: the
    /// layers before the light's layer and the masks they read, on the plan's path, so a light
    /// computed at rest is the one any plan whose input is the same reads, wherever it was kept.
    Stage { key: String },
}

impl GpuLight {
    /// Whether a light link over the source evaluates it: its input runs colour operations alone,
    /// per texel. One holding a spatial operation reads that operation's exact output, which only a
    /// sweep of the whole stage at full resolution produces.
    pub fn over_source(&self) -> bool {
        self.spatial.is_empty()
    }

    /// Whether its input is a staged sweep's stage texture ([`GpuLightInput::Stage`]).
    pub fn staged(&self) -> bool {
        matches!(self.input, GpuLightInput::Stage { .. })
    }
}

/// Every light link of `recipe` planned for `request`, in recipe order: one for each spatial
/// operation whose unit prepares a global estimate the GPU computes ([`GpuLightPasses`]), with the
/// spatial operations before it `restoration`'s. The order is the order of the light planes a
/// plan's readers name ([`super::GpuPlan::lights`]).
///
/// `request` plans from the source over its whole content stage at full scale
/// ([`GpuPlanRequest::exact`], [`GpuPlanRequest::from_source`]), drafted and on the linear path as
/// the frame it lights is. The stack is compiled once and walked as [`super::gpu_plan`] walks it,
/// `O(layers + units)`, reading no pixel. A stack of which the GPU plans no frame from the source
/// has none.
pub fn gpu_lights(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    restoration: GpuLightRestoration,
) -> Result<Vec<GpuLight>, Error> {
    if !request.source || request.proxy || request.stage != request.full {
        return Err(Error::validation(
            "a light link is planned from the source over its whole content stage at full scale",
        ));
    }
    let (compiled, answer) = planned(registry, recipe, request)?;
    let GpuAnswer::Plan(plan) = answer else {
        return Ok(Vec::new());
    };
    links(recipe, &compiled, &plan, request, restoration)
}

/// The light links of `plan`, `compiled` from the source over its whole content stage at full
/// scale for `request`, with the spatial operations before each `restoration`'s.
fn links(
    recipe: &Recipe,
    compiled: &Compiled,
    plan: &super::GpuPlan,
    request: GpuPlanRequest,
    restoration: GpuLightRestoration,
) -> Result<Vec<GpuLight>, Error> {
    let mut lights = Vec::new();
    let mut content = plan.content.clone();
    let mut held: Vec<GpuSpatial> = Vec::new();
    let mut left_out = Vec::new();
    for (spatial, entry) in plan.spatial.iter().zip(spatial_entries(compiled, 0)) {
        if let Some(passes) = light_passes(entry, request.stage) {
            let input = match held.is_empty() {
                true => GpuLightInput::Source,
                false => GpuLightInput::Stage {
                    key: stage_key(recipe, spatial.layer, request)?,
                },
            };
            lights.push(GpuLight {
                layer: spatial.layer,
                stage: request.stage,
                linear: request.linear,
                content: content.clone(),
                spatial: held.clone(),
                left_out: left_out.clone(),
                light: light_step(spatial.layer, passes, !request.linear)?,
                stand_in: None,
                input,
            });
        }
        match restoration {
            GpuLightRestoration::LeftOut => {
                left_out.push(spatial.layer);
                content.extend(spatial.after.iter().cloned());
            }
            GpuLightRestoration::Included => held.push(spatial.clone()),
        }
    }
    Ok(lights)
}

/// The key of a staged light's input ([`GpuLightInput::Stage`]): the layers of `recipe` before
/// layer `layer` and the masks they read, sampled at points as a light's whole stage is, on the
/// path and over the stage of `request`. `O(layers)`, no pixel read.
fn stage_key(recipe: &Recipe, layer: usize, request: GpuPlanRequest) -> Result<String, Error> {
    let before = recipe.layers.get(..layer).ok_or_else(|| {
        Error::internal(format!(
            "a light of layer {layer} in a {}-layer stack",
            recipe.layers.len()
        ))
    })?;
    let prefix = crate::render::spatial::prefix_hash(before, &recipe.masks, MaskSampling::Point)?;
    let path = if request.linear { "linear" } else { "byte" };
    Ok(format!(
        "{prefix}+{path}:{}x{}",
        request.stage.width, request.stage.height
    ))
}

/// The request a plan's lights are planned for: `request` from the source over the whole content
/// stage at full scale, drafted, qualifying and on the path as it is.
pub(super) fn light_request(request: GpuPlanRequest) -> GpuPlanRequest {
    GpuPlanRequest {
        boundary: 0,
        stage: request.full,
        full: request.full,
        proxy: false,
        source: true,
        ..request
    }
}

/// The lights a plan of `recipe` for `request` reads, each the exact light the picture at rest
/// draws with ([`GpuLightRestoration::Included`]) and, where that is not over the source, its
/// stand-in with the spatial operations before it left out, which a drag computes where no exact
/// light is kept: planned from the source over the whole
/// content stage at full scale ([`light_request`]), reusing `compiled` and `plan` when they are
/// that request's. Every operation of `plan` reading a light has one of its layer, or the answer is
/// the reason the plan cannot be drawn. `O(layers + units)`, no pixel.
pub(super) fn plan_lights(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    compiled: &Compiled,
    plan: &super::GpuPlan,
) -> Result<Result<Vec<GpuLight>, GpuFallback>, Error> {
    let reads: Vec<usize> = plan
        .spatial
        .iter()
        .filter(|spatial| spatial.light.is_some())
        .map(|spatial| spatial.layer)
        .collect();
    if reads.is_empty() {
        return Ok(Ok(Vec::new()));
    }
    let full = light_request(request);
    let whole;
    let (compiled, plan) = if full == request {
        (compiled, plan)
    } else {
        whole = planned(registry, recipe, full)?;
        match &whole {
            (compiled, GpuAnswer::Plan(plan)) => (compiled, &**plan),
            (_, GpuAnswer::Fallback(reason)) => return Ok(Err(reason.clone())),
        }
    };
    let lights = with_stand_ins(recipe, compiled, plan, full)?;
    if let Some(layer) = reads
        .iter()
        .find(|layer| !lights.iter().any(|light| light.layer == **layer))
    {
        return Ok(Err(GpuFallback::Unplannable(format!(
            "layer {layer} reads a global estimate no light link computes"
        ))));
    }
    Ok(Ok(lights))
}

/// Every light of `recipe` as a plan for `request` reads it, from the source over the whole content
/// stage at full scale: each the exact light the picture at rest draws with, and its stand-in
/// where that is not over the source ([`plan_lights`]). The stack compiled once, `O(layers +
/// units)`, no pixel.
pub(super) fn lights_of(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
) -> Result<Vec<GpuLight>, Error> {
    let full = light_request(request);
    let (compiled, answer) = planned(registry, recipe, full)?;
    match answer {
        GpuAnswer::Plan(plan) => with_stand_ins(recipe, &compiled, &plan, full),
        GpuAnswer::Fallback(_) => Ok(Vec::new()),
    }
}

/// The light links of `plan`, `compiled` for `request` from the source over its whole content
/// stage, with the spatial operations before each included, and beside each that is not over the
/// source its stand-in with them left out.
fn with_stand_ins(
    recipe: &Recipe,
    compiled: &Compiled,
    plan: &super::GpuPlan,
    request: GpuPlanRequest,
) -> Result<Vec<GpuLight>, Error> {
    let mut lights = links(
        recipe,
        compiled,
        plan,
        request,
        GpuLightRestoration::Included,
    )?;
    let left_out = links(
        recipe,
        compiled,
        plan,
        request,
        GpuLightRestoration::LeftOut,
    )?;
    for light in &mut lights {
        if !light.over_source() {
            light.stand_in = left_out
                .iter()
                .find(|stand_in| stand_in.layer == light.layer)
                .cloned()
                .map(Box::new);
        }
    }
    Ok(lights)
}

/// `recipe` compiled for `request` and its plan, as [`super::gpu_plan`] compiles and walks it, its
/// lights not yet planned.
pub(super) fn planned(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
) -> Result<(Compiled, GpuAnswer), Error> {
    let layer = match (request.source, recipe.layers.get(request.boundary)) {
        (true, _) => None,
        (false, Some(layer)) => Some(layer),
        (false, None) => {
            return Err(Error::validation(format!(
                "layer {} is outside the {}-layer stack",
                request.boundary,
                recipe.layers.len()
            )));
        }
    };
    let sampling = if request.proxy {
        MaskSampling::ThinFeature
    } else {
        MaskSampling::Point
    };
    let compiled = registry.compile_shaped(
        request.stage.width,
        request.stage.height,
        request.full.width,
        request.full.height,
        recipe,
        sampling,
        request.drafted,
    )?;
    let answer = compiled.gpu_plan(
        request.boundary,
        request.stage,
        layer.and_then(|layer| registry.effect_stage(&layer.effect_id)),
        Planning {
            qualifying: request.qualifying,
            linear: request.linear,
            source: request.source,
        },
    )?;
    Ok((compiled, answer))
}

/// The spatial entries a plan whose boundary lies in segment `first` chains, in order: the
/// operations of its [`super::GpuPlan::spatial`], one for one, which the walk takes from the
/// segment after the boundary's while each opens with one.
fn spatial_entries(compiled: &Compiled, first: usize) -> impl Iterator<Item = &SpatialEntry> {
    compiled
        .segments
        .iter()
        .skip(first + 1)
        .map_while(|segment| match &segment.entry {
            Some(Entry::Spatial(entry)) => Some(entry),
            _ => None,
        })
}

/// The light passes over `stage` of the first unit of `entry` that computes a light on the GPU.
fn light_passes(entry: &SpatialEntry, stage: Stage) -> Option<GpuLightPasses> {
    entry
        .operation
        .units()
        .iter()
        .find_map(|unit| unit.gpu_light(stage))
}

/// A light link's step of layer `layer` from its unit's `passes`: no apply, its input clamped when
/// `clamps`, its light plane the one its last pass writes.
fn light_step(layer: usize, passes: GpuLightPasses, clamps: bool) -> Result<GpuSpatial, Error> {
    let GpuLightPasses {
        program,
        words,
        planes,
        passes,
    } = passes;
    let well_formed = program.kind == GpuProgramKind::Spatial
        && passes.last().is_some_and(|last| {
            planes.get(last.output).map(|plane| plane.size) == Some(GpuPlaneSize::LIGHT)
        })
        && passes.iter().all(|pass| {
            pass.inputs.len() <= GPU_PASS_INPUTS
                && !pass.inputs.contains(&pass.output)
                && pass.output < planes.len()
                && pass.inputs.iter().all(|&input| input < planes.len())
                && pass.words < words.len()
        });
    if !well_formed {
        return Err(Error::internal(format!(
            "{}'s light passes do not end in a light plane they declare",
            program.entry
        )));
    }
    Ok(GpuSpatial {
        layer,
        program,
        words,
        planes,
        // One unit, whose source runs no apply before it.
        passes: passes
            .into_iter()
            .map(|pass| GpuPass {
                source: 0,
                unit: 0,
                ..pass
            })
            .collect(),
        applies: Vec::new(),
        clamps,
        mask: None,
        after: Vec::new(),
        halos: Vec::new(),
        light: None,
    })
}

/// Words a unit's description writes, in order, each an index its passes and apply name.
#[derive(Debug, Default)]
pub(crate) struct Words(Vec<u32>);

impl Words {
    /// Append `values` and answer the first one's index.
    pub(crate) fn push(&mut self, values: &[Word]) -> usize {
        let at = self.0.len();
        self.0.extend(values.iter().map(|value| match value {
            Word::U(value) => *value,
            Word::F(value) => value.to_bits(),
        }));
        at
    }

    pub(crate) fn into_inner(self) -> Vec<u32> {
        self.0
    }
}

/// One word: an integer or an `f32`'s bits.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Word {
    U(u32),
    F(f32),
}

#[cfg(test)]
mod light_tests {
    use super::*;
    use crate::{
        BASIC_EFFECT, Component, ComponentMode, DETAIL_EFFECT, Layer, Mask, PRESENCE_EFFECT,
        render::gpu::gpu_plan,
    };
    use serde_json::{Value, json};

    const STAGE: Stage = Stage {
        width: 480,
        height: 320,
    };

    fn recipe(layers: &[(&str, Value)]) -> Recipe {
        Recipe {
            layers: layers
                .iter()
                .map(|(effect, payload)| Layer::new(*effect, payload.clone()))
                .collect(),
            ..Recipe::default()
        }
    }

    /// `recipe` with layer `layer` masked by a radial.
    fn masked(mut recipe: Recipe, layer: usize) -> Recipe {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(Component::new(
            "Radial 1",
            ComponentMode::Add,
            "radial",
            json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.22, "angle": 18.0,
                   "feather": 45.0}),
        ));
        recipe.layers[layer].mask = Some(mask.id.clone());
        recipe.masks.push(mask);
        recipe
    }

    fn from_source() -> GpuPlanRequest {
        GpuPlanRequest::exact(0, STAGE).from_source()
    }

    fn plan(recipe: &Recipe, request: GpuPlanRequest) -> super::super::GpuPlan {
        match gpu_plan(&ModuleRegistry::builtin(), recipe, request).unwrap() {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        }
    }

    fn lights(recipe: &Recipe, restoration: GpuLightRestoration) -> Vec<GpuLight> {
        gpu_lights(
            &ModuleRegistry::builtin(),
            recipe,
            from_source(),
            restoration,
        )
        .unwrap()
    }

    /// Each layer whose Dehaze prepares a light has one light link, planned from the source over
    /// the whole content stage: its input the colour operations before it as the plan holds them,
    /// its step Dehaze's reduction into the stage's block means and its selection into the light
    /// plane, with no apply, clamped on the byte path alone. Texture and Clarity take no light.
    #[test]
    fn each_dehaze_layer_has_a_light_link_planned_from_the_source() {
        let registry = ModuleRegistry::builtin();
        let dehaze = recipe(&[(PRESENCE_EFFECT, json!({"dehaze": -40, "texture": 20}))]);
        let [light] = &lights(&dehaze, GpuLightRestoration::LeftOut)[..] else {
            panic!("one light link");
        };
        assert_eq!((light.layer, light.stage, light.linear), (0, STAGE, false));
        assert!(light.content.is_empty() && light.spatial.is_empty());
        assert!(light.left_out.is_empty() && light.over_source());
        let step = &light.light;
        assert!(step.applies.is_empty() && step.clamps && step.light.is_none());
        assert_eq!(step.layer, 0);
        assert_eq!(
            step.planes
                .iter()
                .map(|plane| plane.size)
                .collect::<Vec<_>>(),
            [GpuPlaneSize::Reduced(16), GpuPlaneSize::LIGHT]
        );
        let kernels: Vec<_> = step.passes.iter().map(|pass| pass.kernel).collect();
        assert_eq!(kernels, ["lf_presence_reduce", "lf_presence_atmosphere"]);
        assert_eq!(step.passes[1].output, 1);
        assert!(
            step.passes
                .iter()
                .all(|pass| pass.unit == 0 && pass.source == 0)
        );
        // The block form names the whole stage it reduces.
        assert_eq!(step.words[step.passes[0].words + 2..][..2], [480, 320]);
        // On the linear path the input is not clamped.
        let linear = gpu_lights(
            &registry,
            &dehaze,
            from_source().linear(),
            GpuLightRestoration::LeftOut,
        )
        .unwrap();
        assert!(linear[0].linear && !linear[0].light.clamps);
        // No Dehaze, no light.
        let texture = recipe(&[(PRESENCE_EFFECT, json!({"texture": 40, "clarity": 30}))]);
        assert!(lights(&texture, GpuLightRestoration::LeftOut).is_empty());
        // The colour operations before it, masked ones among them, as the plan holds them.
        let coloured = masked(
            recipe(&[
                (BASIC_EFFECT, json!({"exposure": 0.6, "contrast": 25})),
                (BASIC_EFFECT, json!({"exposure": -0.3})),
                (PRESENCE_EFFECT, json!({"dehaze": 30})),
            ]),
            1,
        );
        let [light] = &lights(&coloured, GpuLightRestoration::LeftOut)[..] else {
            panic!("one light link");
        };
        assert_eq!(light.layer, 2);
        assert_eq!(light.content, plan(&coloured, from_source()).content);
        assert_eq!(light.content.len(), 2);
        assert!(light.content[1].mask.is_some());
        // A light link is planned from the source at full scale only.
        for request in [
            GpuPlanRequest::exact(1, STAGE),
            GpuPlanRequest::fit(0, STAGE, STAGE).from_source(),
        ] {
            assert!(
                gpu_lights(&registry, &coloured, request, GpuLightRestoration::LeftOut).is_err()
            );
        }
    }

    /// A restoration layer before Dehaze is left out of the light's input, the colour operations
    /// after it joining the ones before it, or held as an operation of the link's own, which a
    /// link over the source does not evaluate. A Dehaze layer behind a Presence layer has a light
    /// of its own too: left out, its prefix's colour alone; included, the Presence layer before it,
    /// which reads the first light.
    #[test]
    fn spatial_layers_are_left_out_or_held_and_every_dehaze_layer_has_its_light() {
        let stack = masked(
            recipe(&[
                (BASIC_EFFECT, json!({"exposure": 0.4})),
                (DETAIL_EFFECT, json!({"sharpening": 60})),
                (BASIC_EFFECT, json!({"contrast": 30})),
                (PRESENCE_EFFECT, json!({"dehaze": -100})),
            ]),
            2,
        );
        let planned = plan(&stack, from_source());
        let [left_out] = &lights(&stack, GpuLightRestoration::LeftOut)[..] else {
            panic!("one light link");
        };
        assert_eq!(left_out.layer, 3);
        assert_eq!(left_out.left_out, [1]);
        assert!(left_out.spatial.is_empty() && left_out.over_source());
        let mut content = planned.content.clone();
        content.extend(planned.spatial[0].after.iter().cloned());
        assert_eq!(left_out.content, content);
        assert_eq!(
            left_out
                .content
                .iter()
                .map(|operation| operation.layer)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        let [included] = &lights(&stack, GpuLightRestoration::Included)[..] else {
            panic!("one light link");
        };
        assert!(included.left_out.is_empty() && !included.over_source());
        assert_eq!(included.content, planned.content);
        assert_eq!(included.spatial, planned.spatial[..1]);
        assert_eq!(included.light, left_out.light);
        // Dehaze behind a Presence layer: each has its light, the second's prefix the first layer
        // included, reading the first light, or left out.
        let behind = masked(
            recipe(&[
                (PRESENCE_EFFECT, json!({"dehaze": 30, "clarity": 20})),
                (PRESENCE_EFFECT, json!({"dehaze": 60})),
            ]),
            1,
        );
        let [first, second] = &lights(&behind, GpuLightRestoration::Included)[..] else {
            panic!("two light links");
        };
        assert_eq!((first.layer, second.layer), (0, 1));
        assert!(first.over_source() && !second.over_source());
        let planned = plan(&behind, from_source());
        assert_eq!(second.spatial, planned.spatial[..1]);
        assert!(
            second.spatial[0].light.is_some(),
            "it reads the first light"
        );
        let [_, second] = &lights(&behind, GpuLightRestoration::LeftOut)[..] else {
            panic!("two light links");
        };
        assert!(second.over_source() && second.left_out == [0]);
    }

    /// A plan reads each light its links write: every operation with a Dehaze unit reads its light
    /// from a plane of its own no pass writes, and the plan holds that layer's light link, the one
    /// the picture at rest draws with, with the spatial operations before it included, and beside
    /// a link that is not over the source its stand-in with them left out. From the source, from a
    /// drafted layer's boundary and over a proxy stage, whose lights are the whole stage's at full
    /// scale.
    #[test]
    fn a_plan_reads_each_light_its_links_write() {
        let stack = masked(
            masked(
                recipe(&[
                    (BASIC_EFFECT, json!({"exposure": 0.4})),
                    (
                        PRESENCE_EFFECT,
                        json!({"dehaze": 50, "texture": 20, "clarity": -30}),
                    ),
                    (BASIC_EFFECT, json!({"contrast": 15})),
                    (PRESENCE_EFFECT, json!({"dehaze": 20})),
                ]),
                2,
            ),
            3,
        );
        let exact = lights(&stack, GpuLightRestoration::Included);
        let proxy = Stage {
            width: 240,
            height: 160,
        };
        for request in [
            from_source(),
            GpuPlanRequest::exact(0, STAGE).drafted(1),
            GpuPlanRequest::fit(0, proxy, STAGE).from_source(),
        ] {
            let planned = plan(&stack, request);
            let [first, second] = &planned.spatial[..] else {
                panic!("two spatial operations");
            };
            for spatial in [first, second] {
                let light = spatial.light.expect("each reads a light");
                assert_eq!(spatial.planes[light].size, GpuPlaneSize::LIGHT);
                assert!(!spatial.planes[light].scratch);
                assert!(spatial.passes.iter().all(|pass| pass.output != light));
                assert!(
                    spatial
                        .applies
                        .iter()
                        .any(|apply| apply.planes.contains(&light))
                );
                assert!(
                    !spatial
                        .passes
                        .iter()
                        .any(|pass| pass.kernel == "lf_presence_atmosphere")
                );
            }
            let [one, two] = &planned.lights[..] else {
                panic!("two lights");
            };
            assert_eq!((one.layer, two.layer), (1, 3));
            assert!(one.over_source() && one.stand_in.is_none());
            assert!(!two.over_source());
            let stand_in = two.stand_in.as_deref().expect("a stand-in");
            assert!(stand_in.over_source() && stand_in.left_out == [1]);
            assert!(planned.lights.iter().all(|light| light.stage == STAGE));
            if request.drafted.is_none() {
                assert_eq!(
                    planned
                        .lights
                        .iter()
                        .map(|light| (&light.content, &light.spatial, &light.light))
                        .collect::<Vec<_>>(),
                    exact
                        .iter()
                        .map(|light| (&light.content, &light.spatial, &light.light))
                        .collect::<Vec<_>>()
                );
            }
        }
        // Without Dehaze a plan reads no light.
        let texture = recipe(&[(PRESENCE_EFFECT, json!({"texture": 40}))]);
        let planned = plan(&texture, from_source());
        assert!(planned.lights.is_empty());
        assert!(
            planned
                .spatial
                .iter()
                .all(|spatial| spatial.light.is_none())
        );
        assert!(!planned.reads_lights());
    }

    /// An operation reads one light at most: two units reading one are refused.
    #[test]
    fn an_operation_reads_one_light_at_most() {
        let registry = ModuleRegistry::builtin();
        let compiled = registry
            .compile_shaped(
                STAGE.width,
                STAGE.height,
                STAGE.width,
                STAGE.height,
                &recipe(&[(PRESENCE_EFFECT, json!({"dehaze": 50}))]),
                MaskSampling::Point,
                None,
            )
            .unwrap();
        let entry = spatial_entries(&compiled, 0).next().expect("Presence");
        let reading = entry.operation.units()[0].gpu().expect("Dehaze");
        let one = compose(0, vec![reading.clone()], true, None).unwrap();
        assert_eq!(one.light, Some(0));
        assert!(compose(0, vec![reading.clone(), reading], true, None).is_err());
    }
}
