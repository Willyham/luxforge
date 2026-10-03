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
use super::program::{GpuProgram, GpuProgramKind};
use super::{GpuFallback, GpuMask};
use crate::Error;

/// How many inputs one pass may read: planes `0..4` of `lf_plane`.
pub const GPU_PASS_INPUTS: usize = 4;

/// The most planes the applies of one spatial operation may read. The surface runs each spatial
/// operation as a link of its own over the texture the link before wrote, so a pass binds only its
/// own inputs, at most [`GPU_PASS_INPUTS`], and its own operation's apply planes, beside that
/// input, within the 16 sampled textures a shader stage has on every adapter the surface runs
/// spatial steps on: `16 - 1 - 4`. Detail's three apply planes and Presence's four fit with room
/// to spare, and a chain of any length holds; an operation past it names `spatial-chain`.
pub const GPU_CHAIN_APPLY_PLANES: usize = 11;

/// The lanes of a [`GpuPassShape::Workgroup`] pass, and how many values `lf_shared` holds.
pub const GPU_WORKGROUP_LANES: u32 = 256;
pub const GPU_SHARED_VALUES: u32 = 1024;

/// What one plane's texels hold, as the texture format the surface keeps it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuPlaneFormat {
    /// `rgba16float`: a colour intermediate at the recorded default precision.
    Colour,
    /// `r32float`: one spatial accumulator.
    Scalar,
    /// `rg32float`: two accumulators read together, such as a mean and a mean square.
    Pair,
    /// `rgba32float`: four accumulators, or a reduced colour with one more channel beside it.
    Quad,
}

impl GpuPlaneFormat {
    /// The bytes one texel takes.
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Scalar => 4,
            Self::Colour | Self::Pair => 8,
            Self::Quad => 16,
        }
    }
}

/// A plane's size against the boundary it is computed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuPlaneSize {
    /// The stage's blocks of `s × s` pixels, anchored at the stage origin, that the boundary
    /// reaches: `s = 1` is the boundary itself. A boundary at stage origin `o` of width `w` holds
    /// reduced columns `floor(o / s)` to `ceil((o + w) / s)`, so a block a window cuts is held, and
    /// a reduced plane is the CPU's own blocks wherever the window holds them whole.
    Reduced(u32),
    /// A fixed number of texels, whatever the boundary: a global estimate's.
    Fixed { width: u32, height: u32 },
}

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
        match self.size {
            GpuPlaneSize::Fixed { width, height } => (width, height),
            GpuPlaneSize::Reduced(s) => {
                let s = s.max(1);
                let axis = |origin: u32, length: u32| (origin + length).div_ceil(s) - origin / s;
                (axis(origin.0, size.0), axis(origin.1, size.1))
            }
        }
    }
}

/// How a pass runs its kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuPassShape {
    /// Once for every `span` texels of the output plane, `at` the first of them: `[1, 1]` for a
    /// pointwise kernel, a run along one axis for a running sum reseeded at each run's start.
    Texels { span: [u32; 2] },
    /// One workgroup of [`GPU_WORKGROUP_LANES`] lanes, for a reduction of a small plane to a few
    /// values.
    Workgroup,
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
    /// The unit computes a global estimate from the stage the GPU holds rather than reading the
    /// one the CPU stored, so its frame is approximate.
    pub estimated: bool,
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
    /// A global estimate is computed on the GPU from the stage it holds instead of read from the
    /// estimate store, so the frame is labelled approximate, as the CPU proxy is.
    pub estimated: bool,
    /// The colour operations of its segment, which run on its output before the next spatial
    /// operation of the plan enters: empty for the last, whose segment's colour operations are the
    /// plan's output operations.
    pub after: Vec<super::GpuOperation>,
    /// How far beyond an output pixel, in pixels of its stage, each unit's apply depends on the
    /// unit's input, as the CPU's tiles read it; the operation's output depends on its input as far
    /// as their sum. A masked operation's passes need run only over its mask's bounds grown by the
    /// sum, and a tick that changes part of the input only over that part grown unit by unit.
    pub halos: Vec<u32>,
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
/// placed, a scratch plane of one unit given to a later unit's scratch plane of the same format and
/// size, and every pass's `source` set to its unit's index. Every unit must carry the same program,
/// one each of the operation's applies.
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
        estimated: false,
        after: Vec::new(),
        halos: Vec::new(),
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
        let placed: Vec<usize> = unit
            .planes
            .iter()
            .map(|plane| {
                let reused = plane
                    .scratch
                    .then(|| {
                        free.iter().position(|&candidate| {
                            composed.planes[candidate] == *plane && !taken.contains(&candidate)
                        })
                    })
                    .flatten()
                    .map(|position| free.remove(position));
                let placed = reused.unwrap_or_else(|| {
                    composed.planes.push(*plane);
                    composed.planes.len() - 1
                });
                taken.push(placed);
                placed
            })
            .collect();
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
        };
        let first = composed.passes.len() - unit.passes.len();
        free.extend(single_writer(&mut composed, first, &mut apply));
        composed.applies.push(apply);
        composed.estimated |= unit.estimated;
        for (plane, &at) in unit.planes.iter().zip(&placed) {
            if plane.scratch {
                free.push(at);
            }
        }
    }
    Ok(composed)
}

/// Give each plane `apply` reads that more than one of the unit's passes write — a unit that
/// holds its smoother's coefficients where its last pass then writes the result — a plane of its
/// own, which the last of them writes and the apply reads, and answer the planes it leaves as the
/// unit's scratch. The unit's passes start at `first`.
///
/// So every plane an apply reads has one writer. A tick that changes part of the operation's
/// input runs that writer only where its output changes and every other pass around it
/// (`docs/design/gpu-preview.md`, "Incremental ticks"): a plane the apply reads must keep its
/// values everywhere else, which an earlier pass writing it over the larger rectangle would not.
fn single_writer(composed: &mut GpuSpatial, first: usize, apply: &mut GpuApply) -> Vec<usize> {
    let mut freed: Vec<(usize, usize)> = Vec::new();
    for read in apply.planes.iter_mut() {
        if let Some(&(_, own)) = freed.iter().find(|(plane, _)| plane == read) {
            *read = own;
            continue;
        }
        let writers: Vec<usize> = (first..composed.passes.len())
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
