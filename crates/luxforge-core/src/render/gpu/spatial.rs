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

/// The most planes the applies of a plan's chained spatial operations may read. A pass binds its
/// own inputs, at most [`GPU_PASS_INPUTS`], and the planes of every apply its input runs through,
/// beside the boundary, within the 16 sampled textures a shader stage has on every adapter the
/// surface runs spatial steps on: `16 - 1 - 4`. Detail's three apply planes and Presence's four
/// fit, with a second Presence, through a mask, beside them; a stack past it names
/// `spatial-chain`.
pub const GPU_CHAIN_APPLY_PLANES: usize = 11;

/// The lanes of a [`GpuPassShape::Workgroup`] pass, and how many values `lf_shared` holds.
pub const GPU_WORKGROUP_LANES: u32 = 256;
pub const GPU_SHARED_VALUES: u32 = 1024;

/// What one plane's texels hold: how many channels and whether half precision holds them, and so
/// the texture format the surface keeps it in when it takes a texture of its own.
///
/// A plane may instead take a texture an earlier unit or operation no longer needs, when that
/// texture's format [holds](Self::holds) it: as many channels or more, at its precision or a finer
/// one. Half precision saves memory only for three or four channels, since `rgba16float` is the one
/// half-precision format every adapter stores to: a plane of one or two channels that half
/// precision holds takes `r32float` or `rg32float` when it takes a texture of its own, which costs
/// no more, and an `rgba16float` an earlier unit left free when one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuPlaneFormat {
    /// Four channels at half precision, `rgba16float`.
    Colour,
    /// One accumulator, `r32float`.
    Scalar,
    /// Two accumulators read together, such as a mean and a mean square, `rg32float`.
    Pair,
    /// Four accumulators, or a reduced colour with one more channel beside it, `rgba32float`.
    Quad,
    /// One channel half precision holds: `r32float` of its own.
    HalfScalar,
    /// Two channels half precision holds: `rg32float` of its own.
    HalfPair,
}

impl GpuPlaneFormat {
    /// The bytes one texel takes.
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Scalar | Self::HalfScalar => 4,
            Self::Colour | Self::Pair | Self::HalfPair => 8,
            Self::Quad => 16,
        }
    }

    /// How many channels it holds.
    pub fn channels(self) -> u32 {
        match self {
            Self::Scalar | Self::HalfScalar => 1,
            Self::Pair | Self::HalfPair => 2,
            Self::Colour | Self::Quad => 4,
        }
    }

    /// Whether its texture stores half floats.
    pub fn stores_half(self) -> bool {
        matches!(self, Self::Colour)
    }

    /// Whether half precision holds what it is declared for.
    pub fn half_holds(self) -> bool {
        matches!(self, Self::Colour | Self::HalfScalar | Self::HalfPair)
    }

    /// Whether a texture of this format holds a plane of `plane`'s: as many channels or more, and
    /// full precision unless half precision holds the plane.
    pub fn holds(self, plane: Self) -> bool {
        self.channels() >= plane.channels() && (!self.stores_half() || plane.half_holds())
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
    /// A global estimate is the one the stack the draft was opened over stored, held for the drag
    /// because the store holds none for the drafted stack: approximate too.
    pub held: bool,
    /// The colour operations of its segment, which run on its output before the next spatial
    /// operation of the plan enters: empty for the last, whose segment's colour operations are the
    /// plan's output operations.
    pub after: Vec<super::GpuOperation>,
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
/// it [holds](GpuPlaneFormat::holds), and every pass's `source` set to its unit's index. Every unit
/// must carry the same program, one each of the operation's applies.
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
        held: false,
        after: Vec::new(),
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
                // The smallest free plane that holds it. Every pass of the units before has run
                // before this unit's first, so an apply's plane may take one as well as a scratch
                // plane, and then no later unit may.
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
                let placed = reused.unwrap_or_else(|| {
                    composed.planes.push(*plane);
                    composed.planes.len() - 1
                });
                composed.planes[placed].scratch &= plane.scratch;
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
            });
        }
        composed.applies.push(GpuApply {
            function: unit.apply.function,
            planes: unit
                .apply
                .planes
                .iter()
                .map(|&plane| place(plane))
                .collect::<Result<_, _>>()?,
            words: base + unit.apply.words,
            identity: unit.apply.identity,
        });
        composed.estimated |= unit.estimated;
        for (plane, &at) in unit.planes.iter().zip(&placed) {
            if plane.scratch {
                free.push(at);
            }
        }
    }
    Ok(composed)
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
