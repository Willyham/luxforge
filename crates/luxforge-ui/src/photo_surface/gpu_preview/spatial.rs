//! The spatial step: a spatial operation's compute passes over the held boundary, which fill planes,
//! and its applies, which the frame's own pass runs over the operation's input reading them
//! (`docs/design/gpu-preview.md`, "Spatial programs").
//!
//! # The spatial convention
//!
//! A spatial program's WGSL declares only functions and constants, each named starting with its
//! entry, which names the program rather than one function. Every module that holds it also
//! declares, after the [`PRELUDE`](super::PRELUDE):
//!
//! - `var<workgroup> lf_shared: array<f32, 1024>`, a workgroup pass's scratch;
//! - `lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32>`, texel `at` of a bound plane, clamped to its
//!   edge, and `lf_plane_size(slot: u32) -> vec2<i32>`;
//! - `lf_source(at: vec2<i32>) -> vec3<f32>`, the input of the pass's unit at boundary texel `at`,
//!   clamped to the boundary: the boundary through every step before the spatial one, clamped to
//!   `[0, 1]` where the step says the CPU quantizes, then the applies of the units before it. Only
//!   a pass that reads its unit's input ([`GpuPass::reads_source`]) computes it; any other module
//!   declares a stub ([`SOURCE_STUB`]);
//! - `lf_origin() -> vec2<i32>`, the stage pixel of boundary texel `(0, 0)`, and `lf_size() ->
//!   vec2<i32>`, the boundary's size;
//! - `lf_store(at: vec2<i32>, value: vec4<f32>)`, which writes the pass's output plane.
//!
//! A **kernel**, `fn <kernel>(at: vec2<i32>, words: u32, block: u32)`, runs in a pass: once for
//! every `span` texels of its output plane for [`PassShape::Texels`], `at` the first of them, or on
//! each of the 256 lanes of one workgroup for [`PassShape::Workgroup`], `at` the lane as
//! `(lane, 0)`. It reads the pass's inputs as slots `0..` of `lf_plane`. An **apply**,
//! `fn <apply>(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32>`,
//! is a unit's pointwise last step over its input at boundary texel `at`, reading its planes from
//! slot `planes` on. `words` and `block` are the step's base indices plus the pass's or apply's own
//! word offset.
//!
//! # Execution
//!
//! A pass is a compute pipeline of its own module: the prelude, the programs the steps before it
//! run when its kernel reads its unit's input, the spatial program, the declarations above and an
//! entry the surface generates. Every number that places a pass in its plan — its step's header,
//! its words, its span, the words of the applies its source runs — is data, read from the pass's
//! slice of the planes' parameter buffer, so a module is its kernel and its shape alone, with the
//! steps before its own only for a pass that reads its unit's input: passes and plans that differ
//! only in those numbers share one pipeline, and a pass that reads only planes shares it with plans
//! whose colour steps before its step differ, which the stage keeps across sequences
//! ([`PassCache`]). Its planes are textures: `rgba16float`, `r32float`, `rg32float` or
//! `rgba32float` by the plane's [`PlaneFormat`], each sized to the boundary or to the stage's blocks
//! it reaches ([`GpuPlane::extent`]). A plane an apply reads is kept, in a texture of the link's
//! own; every other is scratch, read only by its step's own passes within a tick, and held in the
//! slot's pool, which every link of a chain takes its scratch planes from in turn and the budget
//! charges once ([`PlanesKey`], [`Pool`], [`super::chain_charge`]). A half-float texture is written
//! rounded to the nearest half, ties to even ([`HALF_ROUNDING`]): the M4's own conversion of a
//! storage write, and of a render target's, rounds toward zero. A pass or an apply reads the step's
//! words from its offset up to the next offset any pass or apply of its step names; a tick runs only
//! the passes whose words, upstream or inputs changed since the planes the applies read were last
//! written, trusting a pool texture only when the link itself wrote it last, and none that only an
//! identity apply's planes need ([`Schedule`]). The passes run in order before the frame's pass,
//! which binds the applies' planes as a second bind group, so the operation holds no colour plane of
//! its own: its memory is its planes, charged to the GPU-preview budget with the slot. Nothing is
//! read back.
use super::{
    GpuProgram, GpuStep, MAP_WORDS, PositionMap, STEP_WORDS, StepKind, Support, entry_name,
    mask::{self, Coverage, MaskedColour, Role},
    validate, validate_program,
};
use std::borrow::Cow;
use wgpu::naga;

/// How many inputs one pass reads, and how many planes one apply reads.
pub const PASS_INPUTS: usize = 4;
const APPLY_PLANES: usize = 4;

/// The lanes of a workgroup pass, and the values of `lf_shared`.
const WORKGROUP_LANES: u32 = 256;
const SHARED_VALUES: u32 = 1024;

/// One side of a texel pass's workgroup.
pub(super) const GROUP_SIDE: u32 = 8;

/// The binding of a pass's output plane in its second group, after every plane it reads.
pub(super) const OUTPUT_BINDING: u32 = 64;

/// The binding of a pass's parameters in its second group.
pub(super) const PARAMS_BINDING: u32 = 65;

/// One pass's slice of the parameter buffer, in bytes and in words: the largest storage-binding
/// offset alignment a device may ask for.
pub(super) const PARAMS_STRIDE: u64 = 256;
const PARAMS_WORDS: usize = (PARAMS_STRIDE / 4) as usize;

/// A pass's parameters: its step's header index, its words' offset in the step's words, its span,
/// then the offsets of the applies its source runs, the rectangle of its output it writes,
/// `[x0, y0, x1, y1)`, and last the texel of its output its first invocation starts at, the corner
/// of the rectangle a tick runs it over.
const PARAM_STEP: usize = 0;
const PARAM_WORDS: usize = 1;
const PARAM_SPAN: usize = 2;
const PARAM_APPLIES: usize = 4;
const PARAM_LIMIT: usize = PARAMS_WORDS - 6;
const PARAM_ORIGIN: usize = PARAMS_WORDS - 2;

/// A limit's far edge that no plane reaches, which a `vec2<i32>` holds.
pub(super) const UNLIMITED: u32 = i32::MAX as u32;

/// How many compiled pass modules the stage keeps across sequences, the least recently used
/// evicted first: every pass of the [`super::PIPELINE_CACHE`] sequences the pipeline keeps holds its
/// pipeline itself, so this bounds only what a sequence compiled later can reuse. A pass module is
/// its kernel and shape, whatever mask, place or colour steps after it its link holds, so a
/// stack's links share most of theirs: the plan and warm list of sixteen masked Detail and Presence
/// layers of six unit sets run 29, and a link that differs from one compiled before only in those
/// compiles its render pipeline alone.
pub(super) const PASS_CACHE: usize = 64;

/// What a plane's texels hold — how many channels, and whether half precision holds them — and so
/// the texture format it is kept in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaneFormat {
    /// `rgba16float`: four channels at half precision.
    Colour,
    /// `r32float`: one accumulator.
    Scalar,
    /// `rg32float`: two accumulators.
    Pair,
    /// `rgba32float`: four.
    Quad,
    /// One channel half precision holds, `r32float` of its own: `rgba16float` is the one
    /// half-precision format every adapter stores to.
    HalfScalar,
    /// Two channels half precision holds, `rg32float` of its own.
    HalfPair,
}

impl PlaneFormat {
    pub(super) fn texture(self) -> wgpu::TextureFormat {
        match self {
            Self::Colour => wgpu::TextureFormat::Rgba16Float,
            Self::Scalar | Self::HalfScalar => wgpu::TextureFormat::R32Float,
            Self::Pair | Self::HalfPair => wgpu::TextureFormat::Rg32Float,
            Self::Quad => wgpu::TextureFormat::Rgba32Float,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::Colour => "rgba16float",
            Self::Scalar | Self::HalfScalar => "r32float",
            Self::Pair | Self::HalfPair => "rg32float",
            Self::Quad => "rgba32float",
        }
    }

    /// The bytes one texel takes.
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Scalar | Self::HalfScalar => 4,
            Self::Colour | Self::Pair | Self::HalfPair => 8,
            Self::Quad => 16,
        }
    }

    /// The format that names its texture's: `Scalar` for `HalfScalar`, `Pair` for `HalfPair`, and
    /// every other its own, so two formats kept in one texture format answer one.
    fn kept_as(self) -> Self {
        match self {
            Self::HalfScalar => Self::Scalar,
            Self::HalfPair => Self::Pair,
            other => other,
        }
    }
}

/// A plane's size against the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaneSize {
    /// The stage's `s × s` blocks, anchored at the stage origin, the boundary reaches: `s = 1` is
    /// the boundary.
    Reduced(u32),
    /// A fixed size whatever the boundary.
    Fixed { width: u32, height: u32 },
    /// The slot's light plane `k` (`docs/design/gpu-preview.md`, "The global estimate"): one
    /// [`PlaneFormat::Quad`] texel whose `xyz` is a global estimate, Dehaze's atmospheric light,
    /// computed from the whole stage. The slot's pool holds it for every link, whatever their
    /// boundary ([`Pool`]); the plan's `k`-th light link writes it ([`super::light`]), and a
    /// spatial step that declares it reads it, none of its passes writing it.
    Light(u32),
}

/// One plane a spatial step's passes write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuPlane {
    pub format: PlaneFormat,
    pub size: PlaneSize,
}

impl GpuPlane {
    /// Its texels over a boundary of `size` whose texel `(0, 0)` is stage pixel `origin`: for a
    /// reduction `s`, the blocks `floor(origin / s)` to `ceil((origin + size) / s)` on each axis.
    pub fn extent(&self, origin: (u32, u32), size: (u32, u32)) -> (u32, u32) {
        match self.size {
            PlaneSize::Fixed { width, height } => (width, height),
            PlaneSize::Light(_) => (1, 1),
            PlaneSize::Reduced(s) => {
                let s = s.max(1);
                let axis = |origin: u32, length: u32| (origin + length).div_ceil(s) - origin / s;
                (axis(origin.0, size.0), axis(origin.1, size.1))
            }
        }
    }

    /// The bytes it takes over that boundary.
    pub fn bytes(&self, origin: (u32, u32), size: (u32, u32)) -> u64 {
        let (width, height) = self.extent(origin, size);
        u64::from(width) * u64::from(height) * self.format.texel_bytes()
    }
}

/// How a pass runs its kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassShape {
    /// Once for every `span` texels of the output, `at` the first.
    Texels { span: [u32; 2] },
    /// One workgroup of 256 lanes.
    Workgroup,
}

/// One compute pass of a spatial step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPass {
    /// The kernel, a function of the step's program.
    pub kernel: Cow<'static, str>,
    /// The planes it reads, as `lf_plane` slots `0..`.
    pub inputs: Vec<u32>,
    /// The plane it writes.
    pub output: u32,
    /// Its first word, after the step's base index.
    pub words: u32,
    /// How many of the step's applies its `lf_source` runs.
    pub source: u32,
    /// Whether its kernel reads its unit's input through `lf_source`. A pass that reads only
    /// planes runs no apply (`source` is 0), and its module holds its own step's program alone,
    /// whatever steps come before its step: its output's texture takes its plane's own format
    /// ([`PlanesKey`]).
    pub reads_source: bool,
    pub shape: PassShape,
    /// The index of its unit, whose apply is the step's `applies[unit]`.
    pub unit: u32,
}

/// One unit's apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuApply {
    pub function: Cow<'static, str>,
    /// The planes it reads, bound from the slot it is handed.
    pub planes: Vec<u32>,
    /// Its first word, after the step's base index.
    pub words: u32,
    /// Its unit is the identity at this tick's words, which it returns its input for before it
    /// reads a plane: a tick runs none of the passes only its planes need ([`Schedule`]). Like the
    /// words it is the tick's, no part of the sequence's shape.
    pub identity: bool,
}

/// A spatial operation: its program (the words are the operation's), its planes, its passes in
/// order and one apply per unit in order.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSpatial {
    pub program: GpuProgram,
    pub planes: Vec<GpuPlane>,
    pub passes: Vec<GpuPass>,
    pub applies: Vec<GpuApply>,
    /// Clamp the operation's input to `[0, 1]` before its first unit and its output after its
    /// last, where the CPU quantizes both.
    pub clamps: bool,
    /// The mask the operation's output is blended by against its input, per channel in linear
    /// light and the input itself where coverage is exactly zero, as the CPU blends a masked
    /// spatial operation. Its words come first in the step's, in the masked colour step's layout
    /// with no units ([`MaskedColour`]), and its blocks after the program's.
    pub mask: Option<Coverage>,
    /// How far beyond a pixel, in stage pixels, each unit's apply depends on the unit's input, as
    /// the CPU's tiles read it. A masked operation's passes run only over its mask's bounds grown
    /// by every unit's reach ([`GpuSpatial::pass_rect`]), and an incremental tick's only over its
    /// change grown unit by unit. An operation that names no halo for a unit runs whole.
    pub halos: Vec<u32>,
}

/// A half-open rectangle `[x0, x1) × [y0, y1)` of a boundary's texels or of a plane's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    /// The whole of a `size` texels.
    pub fn whole(size: (u32, u32)) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: size.0,
            y1: size.1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// Whether every texel of `other` is one of this rectangle's.
    pub fn contains(&self, other: &Rect) -> bool {
        other.is_empty()
            || (self.x0 <= other.x0
                && self.y0 <= other.y0
                && other.x1 <= self.x1
                && other.y1 <= self.y1)
    }

    /// The rectangle holding both, or the other when one is empty.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// This rectangle grown by `by` texels on every side, within a boundary of `size`; empty
    /// stays empty.
    pub fn grown(&self, by: u32, size: (u32, u32)) -> Rect {
        if self.is_empty() {
            return *self;
        }
        Rect {
            x0: self.x0.saturating_sub(by),
            y0: self.y0.saturating_sub(by),
            x1: self.x1.saturating_add(by).min(size.0),
            y1: self.y1.saturating_add(by).min(size.1),
        }
    }

    /// The texels this rectangle and `other` share.
    pub fn intersect(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    /// This rectangle of a boundary of `origin` stage pixels, as a rectangle of a plane of `size`
    /// whose texels are the stage's `s × s` blocks anchored at the stage origin: every block a
    /// texel of it touches.
    fn reduced(&self, s: u32, origin: (u32, u32), size: (u32, u32)) -> Rect {
        let s = s.max(1);
        let axis = |origin: u32, from: u32, to: u32, limit: u32| {
            let first = origin / s;
            (
                ((origin + from) / s - first).min(limit),
                ((origin + to).div_ceil(s) - first).min(limit),
            )
        };
        let (x0, x1) = axis(origin.0, self.x0, self.x1, size.0);
        let (y0, y1) = axis(origin.1, self.y0, self.y1, size.1);
        Rect { x0, y0, x1, y1 }
    }
}

/// What decides one pass's pipeline besides its kernel's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PassKey {
    inputs: [u32; PASS_INPUTS],
    count: usize,
    output: u32,
    source: u32,
    reads_source: bool,
    shape: PassShape,
}

/// What decides one apply's place in the frame's pipeline besides its function's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ApplyKey {
    planes: [u32; APPLY_PLANES],
    count: usize,
    words: u32,
}

fn first<const N: usize>(values: &[u32]) -> [u32; N] {
    std::array::from_fn(|index| values.get(index).copied().unwrap_or(u32::MAX))
}

impl GpuSpatial {
    /// The mask as the masked colour step's coverage with no units, whose words, blocks and
    /// coverage function a masked spatial step shares.
    fn coverage(&self) -> Option<MaskedColour> {
        self.mask.as_ref().map(|mask| MaskedColour {
            units: Vec::new(),
            position: PositionMap::IDENTITY,
            mask: mask.clone(),
        })
    }

    /// The words the mask takes before the program's.
    fn mask_words(&self) -> usize {
        self.coverage().map_or(0, |coverage| coverage.word_count())
    }

    /// The words the step packs after the header: the mask's, then the program's.
    pub(super) fn word_count(&self) -> usize {
        self.mask_words() + self.program.words.len()
    }

    /// The block words the step packs: the program's, then the mask's.
    pub(super) fn block_count(&self) -> usize {
        self.program.block.len() + self.coverage().map_or(0, |coverage| coverage.block_count())
    }

    /// Append the step's words at the base the header recorded for it, its mask's blocks counted
    /// from `block`, the step's own blocks base, after the program's. Its blocks are its programs',
    /// in [`Self::programs`]' order.
    pub(super) fn pack_words(&self, words: &mut Vec<u32>, block: usize) {
        if let Some(coverage) = self.coverage() {
            coverage.pack_words(words, block + self.program.block.len());
        }
        words.extend_from_slice(&self.program.words);
    }

    /// The spatial program, then the mask's coverage programs.
    pub(super) fn programs(&self) -> impl Iterator<Item = (Role, &GpuProgram)> {
        std::iter::once((Role::Spatial, &self.program)).chain(
            self.mask
                .iter()
                .flat_map(|mask| &mask.components)
                .map(|component| (Role::Coverage, &component.program)),
        )
    }

    /// What decides the step's pipelines besides its programs: the clamp and the mask's words,
    /// every plane, and every pass and apply with its kernel or function.
    pub(super) fn shape(&self) -> impl Iterator<Item = (StepKind, &str, &str)> {
        std::iter::once((
            StepKind::Spatial {
                clamps: self.clamps,
                mask_words: self.mask.as_ref().map(|_| self.mask_words()),
            },
            "",
            "",
        ))
        .chain(self.planes.iter().map(|plane| {
            // Which of the slot's light planes a step reads or writes is bound when it runs — a
            // light link's by the view it is handed ([`super::light`]), a link's by its planes'
            // key, whose locations name the light, so another light is another key and other
            // groups — never compiled: the same sequence whichever light that is.
            let plane = match plane.size {
                PlaneSize::Light(_) => GpuPlane {
                    size: PlaneSize::Light(0),
                    ..*plane
                },
                _ => *plane,
            };
            (StepKind::Plane(plane), "", "")
        }))
        .chain(self.passes.iter().map(|pass| {
            (
                StepKind::Pass(PassKey {
                    inputs: first(&pass.inputs),
                    count: pass.inputs.len(),
                    output: pass.output,
                    source: pass.source,
                    reads_source: pass.reads_source,
                    shape: pass.shape,
                }),
                pass.kernel.as_ref(),
                "",
            )
        }))
        .chain(self.applies.iter().map(|apply| {
            (
                StepKind::Apply(ApplyKey {
                    planes: first(&apply.planes),
                    count: apply.planes.len(),
                    words: apply.words,
                }),
                apply.function.as_ref(),
                "",
            )
        }))
    }

    /// The coverage function of a masked step at header `base`, which its frame statements call.
    fn coverage_function(&self, index: usize, base: usize) -> Option<String> {
        self.coverage()
            .map(|coverage| coverage.assemble(index, base).0)
    }

    /// Whether a pass reads the whole input: a workgroup's pass, or one that writes a plane of a
    /// fixed size. Such an operation's output may change anywhere its input does. And whether it
    /// reads a light ([`Self::lights`]), which the whole stage decides: a change to the light, which
    /// a change anywhere in the stage may make, changes its output everywhere, so a tick redraws
    /// it whole, wherever its input changed.
    pub(super) fn global(&self) -> bool {
        self.passes
            .iter()
            .any(|pass| matches!(pass.shape, PassShape::Workgroup))
            || self
                .planes
                .iter()
                .any(|plane| matches!(plane.size, PlaneSize::Fixed { .. }))
            || self.lights().next().is_some()
    }

    /// The slot's light planes the step declares ([`PlaneSize::Light`]), in plane order: the
    /// lights it reads, or for a light link's step the one it writes.
    pub fn lights(&self) -> impl Iterator<Item = u32> + '_ {
        self.planes.iter().filter_map(|plane| match plane.size {
            PlaneSize::Light(k) => Some(k),
            PlaneSize::Reduced(_) | PlaneSize::Fixed { .. } => None,
        })
    }

    /// The reduction of the plane pass `pass` writes: one texel a block of `s × s` stage pixels.
    fn block(&self, pass: &GpuPass) -> u32 {
        match self
            .planes
            .get(pass.output as usize)
            .map(|plane| plane.size)
        {
            Some(PlaneSize::Reduced(s)) => s.max(1),
            _ => 1,
        }
    }

    /// The passes of unit `unit`.
    fn unit_passes(&self, unit: usize) -> impl Iterator<Item = &GpuPass> {
        self.passes
            .iter()
            .filter(move |pass| pass.unit as usize == unit)
    }

    /// How far, in boundary texels, unit `unit`'s apply depends on the unit's input when its
    /// passes run over part of the boundary and keep, bit for bit, what they give over the whole:
    /// its halo; for each pass that runs along an axis, the texels before its own where a run
    /// starts, which its sums read, in its plane's blocks; and two of the unit's largest blocks,
    /// which a reduced plane's texels round a rectangle out to. `u32::MAX` for a unit the operation
    /// names no halo for.
    pub(super) fn reach(&self, unit: usize) -> u32 {
        let Some(&halo) = self.halos.get(unit) else {
            return u32::MAX;
        };
        let mut along = [0u32; 2];
        for pass in self.unit_passes(unit) {
            let s = self.block(pass);
            if let PassShape::Texels { span } = pass.shape {
                along[0] = along[0].saturating_add(span[0].saturating_sub(1).saturating_mul(s));
                along[1] = along[1].saturating_add(span[1].saturating_sub(1).saturating_mul(s));
            }
        }
        halo.saturating_add(along[0].max(along[1]))
            .saturating_add(self.apply_reach(unit))
    }

    /// How far around a pixel unit `unit`'s apply reads its planes, in boundary texels: two of the
    /// unit's largest blocks, which an upsample's neighbouring texel of a reduced plane reaches.
    pub(super) fn apply_reach(&self, unit: usize) -> u32 {
        2 * self
            .unit_passes(unit)
            .map(|pass| self.block(pass))
            .max()
            .unwrap_or(1)
    }

    /// How far beyond the mask's bounds unit `unit`'s apply plane must hold the operation's values:
    /// its apply's own reach ([`GpuSpatial::apply_reach`]), and for each later unit, whose input
    /// runs this apply, that unit's reach and its apply's.
    pub(super) fn needed(&self, unit: usize) -> u32 {
        (unit + 1..self.applies.len())
            .map(|later| self.reach(later).saturating_add(self.apply_reach(later)))
            .fold(self.apply_reach(unit), u32::saturating_add)
    }

    /// Every unit's reach and its apply's summed: how far beyond the mask's bounds every pass must
    /// run, over one rectangle, for every apply plane to hold the operation's values where it is
    /// needed ([`GpuSpatial::needed`]).
    pub(super) fn reaches(&self) -> u32 {
        (0..self.applies.len())
            .map(|unit| self.reach(unit).saturating_add(self.apply_reach(unit)))
            .fold(0, u32::saturating_add)
    }

    /// The rectangle of a boundary of `size` texels, which `texels` places in the stage, that the
    /// operation's passes must fill for its applies to be exact wherever its mask covers anything:
    /// the mask's rectangle ([`GpuSpatial::mask_rect`]) grown by every unit's reach and its apply's
    /// ([`GpuSpatial::reaches`]); possibly empty, when the mask covers nothing of this boundary. The
    /// whole boundary for an unmasked operation, for one whose passes read the whole input
    /// ([`GpuSpatial::global`]) and for one with a unit of no named halo.
    pub fn pass_rect(&self, texels: super::TexelMap, size: (u32, u32)) -> Rect {
        let reach = self.reaches();
        if self.mask.is_none() || self.global() || reach == u32::MAX {
            return Rect::whole(size);
        }
        self.mask_rect(texels, size, reach)
    }

    /// The rectangle of a boundary of `size` texels, which `texels` places in the stage, where the
    /// operation's mask can cover anything, grown by `by` texels: the mask's bounds, taken back
    /// through its position map to the boundary's texels, within the boundary; possibly empty. The
    /// whole boundary for an unmasked operation, and over texels that are not the stage's own.
    pub fn mask_rect(&self, texels: super::TexelMap, size: (u32, u32), by: u32) -> Rect {
        let whole = Rect::whole(size);
        let Some(mask) = &self.mask else {
            return whole;
        };
        if texels.step != [1.0, 1.0] {
            return whole;
        }
        // The stage pixels the map takes into the bounds: the map's linear part is a signed
        // permutation, so the preimage of a rectangle is the rectangle of its corners' preimages.
        let map = mask.position;
        let [x0, y0, x1, y1] = mask.bounds.map(i64::from);
        if x0 >= x1 || y0 >= y1 {
            return Rect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            };
        }
        let (a, b, c, d) = (
            i64::from(map.a),
            i64::from(map.b),
            i64::from(map.c),
            i64::from(map.d),
        );
        let (tx, ty) = (i64::from(map.tx), i64::from(map.ty));
        // The inverse of a signed permutation is its transpose.
        let back = |qx: i64, qy: i64| {
            let (u, v) = (qx - tx, qy - ty);
            (a * u + c * v, b * u + d * v)
        };
        let corners = [
            back(x0, y0),
            back(x1 - 1, y0),
            back(x0, y1 - 1),
            back(x1 - 1, y1 - 1),
        ];
        let reach = i64::from(by);
        let origin = (
            texels.origin[0].round() as i64,
            texels.origin[1].round() as i64,
        );
        let low = |axis: fn(&(i64, i64)) -> i64, origin: i64, limit: u32| {
            let least = corners.iter().map(axis).min().unwrap_or(0);
            (least - origin - reach).clamp(0, i64::from(limit)) as u32
        };
        let high = |axis: fn(&(i64, i64)) -> i64, origin: i64, limit: u32| {
            let most = corners.iter().map(axis).max().unwrap_or(0);
            (most + 1 - origin + reach).clamp(0, i64::from(limit)) as u32
        };
        Rect {
            x0: low(|corner| corner.0, origin.0, size.0),
            y0: low(|corner| corner.1, origin.1, size.1),
            x1: high(|corner| corner.0, origin.0, size.0),
            y1: high(|corner| corner.1, origin.1, size.1),
        }
    }

    /// The bytes every plane takes over a boundary of `size` at stage `origin`.
    pub fn plane_bytes(&self, origin: (u32, u32), size: (u32, u32)) -> u64 {
        self.planes
            .iter()
            .map(|plane| plane.bytes(origin, size))
            .sum()
    }
}

/// The spatial convention's declarations as stubs, which a program alone is validated against.
pub const SPATIAL_PRELUDE: &str = "
// Luxforge GPU-preview spatial declarations.
var<workgroup> lf_shared: array<f32, 1024>;
fn lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32> { return vec4<f32>(f32(slot), vec2<f32>(at), 1.0); }
fn lf_plane_size(slot: u32) -> vec2<i32> { return vec2<i32>(i32(slot) + 1); }
fn lf_source(at: vec2<i32>) -> vec3<f32> { return vec3<f32>(vec2<f32>(at), 0.5); }
fn lf_origin() -> vec2<i32> { return vec2<i32>(0); }
fn lf_size() -> vec2<i32> { return vec2<i32>(1); }
fn lf_store(at: vec2<i32>, value: vec4<f32>) {}
";

/// The declarations' functions, which a program does not declare itself.
pub(super) const SPATIAL_FUNCTIONS: &[&str] = &[
    "lf_plane",
    "lf_plane_size",
    "lf_source",
    "lf_origin",
    "lf_size",
    "lf_store",
];

/// Check a spatial step on its own: its program under the convention, every kernel and apply its
/// passes and applies name with its signature, every plane, input, word and source in range, and
/// no source for a pass that reads only planes.
pub(super) fn validate_spatial(spatial: &GpuSpatial) -> Result<(), String> {
    let program = &spatial.program;
    let entry = &program.entry;
    entry_name(entry)?;
    let module = validate(&format!(
        "{}{SPATIAL_PRELUDE}\n{}",
        super::PRELUDE,
        program.source
    ))?;
    let only = "a program has only functions and constants";
    if module.global_variables.len() != 3 || !module.overrides.is_empty() {
        return Err(format!(
            "{entry:?} declares a binding, global variable or override; {only}"
        ));
    }
    if let Some((_, named)) = module.types.iter().find(|(_, ty)| ty.name.is_some()) {
        return Err(format!(
            "{entry:?} declares the type {:?}; {only}",
            named.name
        ));
    }
    if !module.entry_points.is_empty() {
        return Err(format!(
            "{entry:?} declares an entry point; the surface generates them"
        ));
    }
    let host =
        |name: &str| super::PRELUDE_FUNCTIONS.contains(&name) || SPATIAL_FUNCTIONS.contains(&name);
    let names = module
        .functions
        .iter()
        .map(|(_, function)| &function.name)
        .filter(|name| !name.as_deref().is_some_and(host))
        .chain(module.constants.iter().map(|(_, constant)| &constant.name));
    for name in names {
        if !name
            .as_deref()
            .is_some_and(|name| name.starts_with(entry.as_ref()))
        {
            return Err(format!(
                "{entry:?} declares {name:?}, which does not start with its entry's name"
            ));
        }
    }
    let signature =
        |name: &str| -> Result<(Vec<naga::TypeInner>, Option<naga::TypeInner>), String> {
            let function = module
                .functions
                .iter()
                .map(|(_, function)| function)
                .find(|function| function.name.as_deref() == Some(name))
                .ok_or_else(|| format!("{entry:?} declares no function {name:?}"))?;
            Ok((
                function
                    .arguments
                    .iter()
                    .map(|argument| module.types[argument.ty].inner.clone())
                    .collect(),
                function
                    .result
                    .as_ref()
                    .map(|result| module.types[result.ty].inner.clone()),
            ))
        };
    let vector = |size, scalar| naga::TypeInner::Vector { size, scalar };
    let unsigned = naga::TypeInner::Scalar(naga::Scalar::U32);
    let at = vector(naga::VectorSize::Bi, naga::Scalar::I32);
    let rgb = vector(naga::VectorSize::Tri, naga::Scalar::F32);
    let words = spatial.program.words.len() as u32;
    let planes = spatial.planes.len() as u32;
    for (number, plane) in spatial.planes.iter().enumerate() {
        let valid = match plane.size {
            PlaneSize::Reduced(s) => s >= 1,
            PlaneSize::Fixed { width, height } => width >= 1 && height >= 1,
            PlaneSize::Light(_) => true,
        };
        if !valid {
            return Err(format!("{entry:?} declares an empty plane"));
        }
        // A light plane is the slot's: one `rgba32float` texel, which only a light link's step,
        // one that draws nothing, writes; a step that draws reads it.
        if let PlaneSize::Light(k) = plane.size {
            let written = spatial
                .passes
                .iter()
                .any(|pass| pass.output as usize == number);
            if plane.format != PlaneFormat::Quad || (written && !spatial.applies.is_empty()) {
                return Err(format!(
                    "{entry:?} declares light plane {k} other than as one rgba32float texel a \
                     light link writes"
                ));
            }
        }
    }
    for pass in &spatial.passes {
        let kernel = &pass.kernel;
        if !kernel.starts_with(entry.as_ref()) {
            return Err(format!(
                "{kernel:?} does not start with its program's name {entry:?}"
            ));
        }
        if signature(kernel)? != (vec![at.clone(), unsigned.clone(), unsigned.clone()], None) {
            return Err(format!(
                "{kernel:?} must be fn {kernel}(at: vec2<i32>, words: u32, block: u32)"
            ));
        }
        if pass.inputs.len() > PASS_INPUTS
            || pass.output >= planes
            || pass
                .inputs
                .iter()
                .any(|&input| input >= planes || input == pass.output)
        {
            return Err(format!(
                "{kernel:?} reads more than {PASS_INPUTS} planes, a plane the step does not \
                 declare, or its own output"
            ));
        }
        if pass.words >= words.max(1) || pass.source as usize > spatial.applies.len() {
            return Err(format!(
                "{kernel:?} names a word or an apply the step does not hold"
            ));
        }
        if !pass.reads_source && pass.source != 0 {
            return Err(format!(
                "{kernel:?} reads only planes, so its source runs no apply"
            ));
        }
        if let PassShape::Texels { span } = pass.shape
            && span.contains(&0)
        {
            return Err(format!("{kernel:?} runs over an empty span"));
        }
    }
    for apply in &spatial.applies {
        let function = &apply.function;
        if !function.starts_with(entry.as_ref()) {
            return Err(format!(
                "{function:?} does not start with its program's name {entry:?}"
            ));
        }
        let wanted = (
            vec![
                rgb.clone(),
                at.clone(),
                unsigned.clone(),
                unsigned.clone(),
                unsigned.clone(),
            ],
            Some(rgb.clone()),
        );
        if signature(function)? != wanted {
            return Err(format!(
                "{function:?} must be fn {function}(rgb: vec3<f32>, at: vec2<i32>, words: u32, \
                 block: u32, planes: u32) -> vec3<f32>"
            ));
        }
        if apply.planes.len() > APPLY_PLANES
            || apply.planes.iter().any(|&plane| plane >= planes)
            || apply.words >= words.max(1)
        {
            return Err(format!(
                "{function:?} reads more than {APPLY_PLANES} planes, or names a plane or a word \
                 the step does not hold"
            ));
        }
    }
    for component in spatial.mask.iter().flat_map(|mask| &mask.components) {
        validate_program(Role::Coverage, &component.program)?;
    }
    Ok(())
}

/// The planes one module binds, in slot order, and where each apply's planes begin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Slots {
    /// The step and plane each slot holds.
    planes: Vec<(usize, u32)>,
    /// The first slot of each bound apply's planes, by step and apply.
    applies: Vec<((usize, usize), usize)>,
}

impl Slots {
    pub(super) fn len(&self) -> usize {
        self.planes.len()
    }

    pub(super) fn planes(&self) -> &[(usize, u32)] {
        &self.planes
    }

    /// The first slot of apply `apply` of step `step`.
    fn apply(&self, step: usize, apply: usize) -> usize {
        self.applies
            .iter()
            .find(|(bound, _)| *bound == (step, apply))
            .map_or(0, |(_, slot)| *slot)
    }

    /// Bind the applies of `steps[..until]` and the first `applied` applies of the step at `until`,
    /// each apply's planes consecutively, so an apply handed its first slot finds the rest after
    /// it.
    fn bind_applies(&mut self, steps: &[GpuStep], until: usize, applied: usize) {
        for (index, step) in steps.iter().enumerate().take(until + 1) {
            let GpuStep::Spatial(spatial) = step else {
                continue;
            };
            let count = if index == until {
                applied
            } else {
                spatial.applies.len()
            };
            for (number, apply) in spatial.applies.iter().enumerate().take(count) {
                self.applies.push(((index, number), self.planes.len()));
                self.planes
                    .extend(apply.planes.iter().map(|plane| (index, *plane)));
            }
        }
    }
}

/// The declarations of a module that binds `slots` in its second group: the bindings, `lf_plane`
/// and `lf_plane_size` over them, `lf_origin`, `lf_size` and the shared scratch.
fn declarations(slots: &Slots) -> String {
    let mut text = String::from("\nvar<workgroup> lf_shared: array<f32, 1024>;\n");
    for slot in 0..slots.len() {
        text.push_str(&format!(
            "@group(1) @binding({slot}) var lf_plane_{slot}: texture_2d<f32>;\n"
        ));
    }
    text.push_str("fn lf_plane_size(slot: u32) -> vec2<i32> {\n    switch slot {\n");
    for slot in 0..slots.len() {
        text.push_str(&format!(
            "        case {slot}u: {{ return vec2<i32>(textureDimensions(lf_plane_{slot})); }}\n"
        ));
    }
    text.push_str("        default: { return vec2<i32>(1); }\n    }\n}\n");
    text.push_str(
        "fn lf_plane(slot: u32, at: vec2<i32>) -> vec4<f32> {\n    \
         let texel = clamp(at, vec2<i32>(0), lf_plane_size(slot) - vec2<i32>(1));\n    \
         switch slot {\n",
    );
    for slot in 0..slots.len() {
        text.push_str(&format!(
            "        case {slot}u: {{ return textureLoad(lf_plane_{slot}, texel, 0); }}\n"
        ));
    }
    text.push_str("        default: { return vec4<f32>(0.0); }\n    }\n}\n");
    text.push_str(
        "fn lf_origin() -> vec2<i32> {\n    return vec2<i32>(i32(lf_f32(0u)), i32(lf_f32(1u)));\n}\n\
         fn lf_size() -> vec2<i32> {\n    return vec2<i32>(textureDimensions(lf_boundary));\n}\n",
    );
    text
}

/// Where the statements of a spatial step's applies read their offsets: written into the text, as
/// the frame's module does, whose pipeline is its sequence's own; or from the pass's parameters, so
/// a pass's module carries none.
#[derive(Clone, Copy)]
enum Offsets {
    Written,
    Parameters,
}

/// The statements that take `rgb` through spatial step `index`'s clamp and its first `count`
/// applies, reading each apply's planes from the slot `slots` binds them at, at boundary `texel`.
fn applies(
    index: usize,
    spatial: &GpuSpatial,
    count: usize,
    slots: &Slots,
    offsets: Offsets,
) -> String {
    let base = MAP_WORDS + STEP_WORDS * index;
    let mask_words = spatial.mask_words();
    let mut text = String::new();
    if spatial.clamps {
        text.push_str("    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));\n");
    }
    if spatial.mask.is_some() {
        text.push_str("    let lf_spatial_input = rgb;\n");
    }
    for (number, apply) in spatial.applies.iter().enumerate().take(count) {
        let (words, block) = match offsets {
            Offsets::Written => (
                format!("lf_words[{base}u] + {}u", mask_words + apply.words as usize),
                format!("lf_words[{}u]", base + 1),
            ),
            Offsets::Parameters => (
                format!(
                    "lf_words[lf_param({PARAM_STEP}u)] + lf_param({}u)",
                    PARAM_APPLIES + number
                ),
                format!("lf_words[lf_param({PARAM_STEP}u) + 1u]"),
            ),
        };
        text.push_str(&format!(
            "    rgb = {}(rgb, vec2<i32>(texel), {words}, {block}, {}u);\n",
            apply.function,
            slots.apply(index, number)
        ));
    }
    text
}

/// The frame pass's statements for spatial step `index`: its clamp, its applies, its mask's blend
/// against the operation's input and its clamp, in a block of their own. A masked step composes its
/// coverage first and runs its applies only where it is not exactly zero, which is all its passes
/// fill the planes for ([`GpuSpatial::pass_rect`]); elsewhere the input is the output, as the
/// blend would make it.
pub(super) fn frame_statements(index: usize, spatial: &GpuSpatial, slots: &Slots) -> String {
    let mut text = String::from("    {\n");
    let applied = applies(
        index,
        spatial,
        spatial.applies.len(),
        slots,
        Offsets::Written,
    );
    if spatial.mask.is_some() {
        // The clamp and the input the blend is against, then the coverage, then the applies.
        let (head, rest) = applied
            .split_once("    let lf_spatial_input = rgb;\n")
            .expect("a masked step's applies keep their input");
        text.push_str(head);
        text.push_str("    let lf_spatial_input = rgb;\n");
        text.push_str(&format!(
            "    let lf_spatial_coverage = lf_surface_mask_{index}(stage, lf_spatial_input);\n    \
             if lf_spatial_coverage != 0.0 {{\n"
        ));
        text.push_str(rest);
        text.push_str(
            "    rgb = (1.0 - lf_spatial_coverage) * lf_spatial_input + lf_spatial_coverage * rgb;\n    \
             }\n",
        );
    } else {
        text.push_str(&applied);
    }
    if spatial.clamps {
        text.push_str("    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));\n");
    }
    text.push_str("    }\n");
    text
}

/// The coverage functions of the masked steps of `steps[range]`, after the one fold they share,
/// and each masked colour step's frame statements, by step index: what a module holding those
/// steps declares and runs.
pub(super) fn masks(
    steps: &[GpuStep],
    range: std::ops::Range<usize>,
) -> (String, Vec<Option<String>>) {
    let mut functions = String::new();
    let mut statements = vec![None; range.start];
    for (index, step) in steps.iter().enumerate().take(range.end).skip(range.start) {
        let base = MAP_WORDS + STEP_WORDS * index;
        let (function, statement) = match step {
            GpuStep::Masked(masked) => {
                let (function, statement) = masked.assemble(index, base);
                (Some(function), Some(statement))
            }
            GpuStep::Spatial(spatial) => (spatial.coverage_function(index, base), None),
            GpuStep::Colour { .. } | GpuStep::Geometry(_) | GpuStep::Clipping(_) => (None, None),
        };
        if let Some(function) = function {
            if functions.is_empty() {
                functions.push_str(mask::COMPOSE);
            }
            functions.push_str(&function);
        }
        statements.push(statement);
    }
    (functions, statements)
}

/// The frame pass's statements for any step, `masked` the masked steps' own from [`masks`].
pub(super) fn statements(
    steps: &[GpuStep],
    index: usize,
    slots: &Slots,
    masked: &[Option<String>],
) -> String {
    let base = MAP_WORDS + STEP_WORDS * index;
    match &steps[index] {
        GpuStep::Colour { program, .. } => format!(
            "    rgb = {}(rgb, {}, lf_words[{base}u], lf_words[{}u]);\n",
            program.entry,
            PositionMap::wgsl(base + 2, "stage"),
            base + 1
        ),
        GpuStep::Masked(_) => masked[index].clone().expect("a masked step's statements"),
        GpuStep::Spatial(spatial) => frame_statements(index, spatial, slots),
        GpuStep::Geometry(_) => unreachable!("a geometry tail splits the passes"),
        GpuStep::Clipping(_) => super::clipping::statement(base),
    }
}

/// The fragment module's spatial declarations and its slots: every apply's planes of every
/// spatial step, with stubs for what only a pass uses.
pub(super) fn fragment_declarations(steps: &[GpuStep]) -> (String, Slots) {
    let mut slots = Slots::default();
    slots.bind_applies(steps, steps.len().saturating_sub(1), usize::MAX);
    let mut text = declarations(&slots);
    text.push_str(
        "fn lf_source(at: vec2<i32>) -> vec3<f32> {\n    return vec3<f32>(0.0);\n}\n\
         fn lf_store(at: vec2<i32>, value: vec4<f32>) {}\n",
    );
    (text, slots)
}

/// What a pass that reads only planes declares as `lf_source`, which its kernel may still name in a
/// branch its words never take: the texel's own coordinates as a colour, as [`SPATIAL_PRELUDE`]'s
/// stub returns. No boundary holds such values, and they change from texel to texel, so a pass
/// described as reading only planes that does read its input fills its plane with what no
/// photograph holds and fails the frame's comparison with the CPU's. A zero, or any constant,
/// could pass unseen: a smoothing of a constant is the constant, Dehaze's dark channel of zero
/// leaves its input alone, and Texture's band of it is zero.
const SOURCE_STUB: &str = "fn lf_source(at: vec2<i32>) -> vec3<f32> {\n    \
     return vec3<f32>(vec2<f32>(at), 0.5);\n}\n";

/// One pass's whole module and the planes its second group binds, in slot order: the pass's
/// inputs, then, for a pass that reads its unit's input, the applies' planes its `lf_source` runs;
/// its output at [`OUTPUT_BINDING`]. Only such a pass holds the programs of the steps before its
/// own and their statements: a pass that reads only planes holds its step's spatial program alone,
/// beside [`SOURCE_STUB`], so the steps before its step decide neither its module, nor its layout,
/// nor its pipeline: its output's texture takes its plane's own format ([`PlanesKey`]).
pub(super) fn pass_module(
    steps: &[GpuStep],
    index: usize,
    pass: &GpuPass,
) -> Result<(String, Slots), String> {
    let GpuStep::Spatial(spatial) = &steps[index] else {
        return Err("a pass belongs to a spatial step".into());
    };
    // The format of the texture that holds its output: its plane's own.
    spatial
        .planes
        .get(pass.output as usize)
        .ok_or("a pass writes a plane its step does not declare")?;
    let format = written_format(steps, index, pass.output)
        .ok_or("a pass writes a plane no texture holds")?;
    // The steps whose statements its source runs: every one before its own, or none.
    let before = if pass.reads_source { index } else { 0 };
    let mut slots = Slots::default();
    slots
        .planes
        .extend(pass.inputs.iter().map(|input| (index, *input)));
    if pass.reads_source {
        slots.bind_applies(steps, index, pass.source as usize);
    }
    let mut source = String::from(super::PRELUDE);
    let mut included: Vec<&GpuProgram> = Vec::new();
    // The steps before, whose statements its source runs, and its own step's spatial program: not
    // the step's own mask, which only the frame's pass blends by.
    let programs = steps[..before]
        .iter()
        .flat_map(GpuStep::programs)
        .map(|(_, program)| program)
        .chain(std::iter::once(&spatial.program));
    for program in programs {
        if !included.iter().any(|seen| seen.entry == program.entry) {
            source.push_str(&format!("\n// {}\n{}\n", program.entry, program.source));
            included.push(program);
        }
    }
    let (functions, masked) = masks(steps, 0..before);
    source.push_str(&functions);
    source.push_str("\n@group(0) @binding(2) var lf_boundary: texture_2d<f32>;\n");
    source.push_str(&declarations(&slots));
    // A half-float texture is written rounded to the nearest half, ties to even, first: a storage
    // write's own conversion is the adapter's, and on the M4 it rounds toward zero, which biases
    // every level of a chain of smoothings that reads the last one's output darker. `f32` keeps a
    // half's ten mantissa bits exactly, so the write then converts nothing but the subnormals.
    let stored = if format == PlaneFormat::Colour {
        source.push_str(HALF_ROUNDING);
        "lf_surface_half(value)"
    } else {
        "value"
    };
    source.push_str(&format!(
        "@group(1) @binding({OUTPUT_BINDING}) var lf_out: texture_storage_2d<{}, write>;\n\
         fn lf_store(at: vec2<i32>, value: vec4<f32>) {{\n    \
         let low = vec2<i32>(i32(lf_param({PARAM_LIMIT}u)), i32(lf_param({}u)));\n    \
         let high = vec2<i32>(i32(lf_param({}u)), i32(lf_param({}u)));\n    \
         if all(at >= low) && all(at < high) {{\n        textureStore(lf_out, at, {stored});\n    }}\n}}\n\
         @group(1) @binding({PARAMS_BINDING}) var<storage, read> lf_params: array<u32>;\n\
         fn lf_param(i: u32) -> u32 {{\n    return lf_params[i];\n}}\n",
        format.wgsl(),
        PARAM_LIMIT + 1,
        PARAM_LIMIT + 2,
        PARAM_LIMIT + 3,
    ));
    if pass.reads_source {
        source.push_str(
            "fn lf_source(at: vec2<i32>) -> vec3<f32> {\n    \
             let texel = vec2<u32>(clamp(at, vec2<i32>(0), lf_size() - vec2<i32>(1)));\n    \
             var rgb = textureLoad(lf_boundary, texel, 0).rgb;\n    \
             let stage = vec2<f32>(lf_f32(0u), lf_f32(1u)) + vec2<f32>(texel) * \
             vec2<f32>(lf_f32(2u), lf_f32(3u));\n",
        );
        for earlier in 0..index {
            source.push_str(&statements(steps, earlier, &slots, &masked));
        }
        source.push_str(&applies(
            index,
            spatial,
            pass.source as usize,
            &slots,
            Offsets::Parameters,
        ));
        source.push_str("    return rgb;\n}\n");
    } else {
        source.push_str(SOURCE_STUB);
    }
    let call = format!(
        "{}(at, lf_words[lf_param({PARAM_STEP}u)] + lf_param({PARAM_WORDS}u), \
         lf_words[lf_param({PARAM_STEP}u) + 1u]);",
        pass.kernel,
    );
    match pass.shape {
        PassShape::Texels { .. } => source.push_str(&format!(
            "\n@compute @workgroup_size({GROUP_SIDE}, {GROUP_SIDE})\n\
             fn lf_pass(@builtin(global_invocation_id) id: vec3<u32>) {{\n    \
             let span = vec2<i32>(i32(lf_param({PARAM_SPAN}u)), i32(lf_param({}u)));\n    \
             let corner = vec2<i32>(i32(lf_param({PARAM_ORIGIN}u)), i32(lf_param({}u)));\n    \
             let at = corner + vec2<i32>(id.xy) * span;\n    \
             let size = vec2<i32>(textureDimensions(lf_out));\n    \
             if at.x >= size.x || at.y >= size.y {{\n        return;\n    }}\n    \
             {call}\n}}\n",
            PARAM_SPAN + 1,
            PARAM_ORIGIN + 1
        )),
        PassShape::Workgroup => source.push_str(&format!(
            "\n@compute @workgroup_size({WORKGROUP_LANES})\n\
             fn lf_pass(@builtin(local_invocation_index) lane: u32) {{\n    \
             let at = vec2<i32>(i32(lane), 0);\n    \
             {call}\n}}\n"
        )),
    }
    Ok((source, slots))
}

/// Every pass's parameters in plan order, each in a slice of [`PARAMS_WORDS`] words: what the
/// planes' parameter buffer holds for `steps`, each pass at its place of `places`, in plan order,
/// or starting at its output's first texel and writing all of it where `places` names none.
pub(super) fn parameters(steps: &[GpuStep], places: &[Place]) -> Vec<u32> {
    let mut words = Vec::new();
    let mut number = 0;
    for (index, step) in steps.iter().enumerate() {
        let GpuStep::Spatial(spatial) = step else {
            continue;
        };
        let mask_words = spatial.mask_words() as u32;
        for pass in &spatial.passes {
            let mut slice = [0u32; PARAMS_WORDS];
            slice[PARAM_STEP] = (MAP_WORDS + STEP_WORDS * index) as u32;
            slice[PARAM_WORDS] = mask_words + pass.words;
            let [x, y] = match pass.shape {
                PassShape::Texels { span } => span,
                PassShape::Workgroup => [1, 1],
            };
            slice[PARAM_SPAN] = x;
            slice[PARAM_SPAN + 1] = y;
            for (number, apply) in spatial
                .applies
                .iter()
                .take(pass.source as usize)
                .take(PARAM_LIMIT - PARAM_APPLIES)
                .enumerate()
            {
                slice[PARAM_APPLIES + number] = mask_words + apply.words;
            }
            let place = places.get(number);
            let [x, y] = place.map_or([0, 0], |place| place.origin);
            slice[PARAM_ORIGIN] = x;
            slice[PARAM_ORIGIN + 1] = y;
            let limit = place.map_or([0, 0, UNLIMITED, UNLIMITED], |place| {
                let Rect { x0, y0, x1, y1 } = place.limit;
                [x0, y0, x1, y1].map(|edge| edge.min(UNLIMITED))
            });
            slice[PARAM_LIMIT..PARAM_LIMIT + 4].copy_from_slice(&limit);
            words.extend_from_slice(&slice);
            number += 1;
        }
    }
    words
}

/// `value` rounded to the nearest half float, ties to even, within the finite halves, as `f32`:
/// its mantissa rounded to ten bits by integer arithmetic on its bits, which carries into the
/// exponent as the rounding does. WGSL's `quantizeToF16` would need a capability the surface's
/// device does not have.
pub(super) const HALF_ROUNDING: &str = "
fn lf_surface_half(value: vec4<f32>) -> vec4<f32> {
    let bits = bitcast<vec4<u32>>(clamp(value, vec4<f32>(-65504.0), vec4<f32>(65504.0)));
    let even = (bits >> vec4<u32>(13u)) & vec4<u32>(1u);
    return bitcast<vec4<f32>>((bits + vec4<u32>(0x0fffu) + even) & vec4<u32>(0xffffe000u));
}
";

/// How many passes `steps` run.
fn pass_count(steps: &[GpuStep]) -> usize {
    steps
        .iter()
        .map(|step| match step {
            GpuStep::Spatial(spatial) => spatial.passes.len(),
            GpuStep::Colour { .. }
            | GpuStep::Masked(_)
            | GpuStep::Geometry(_)
            | GpuStep::Clipping(_) => 0,
        })
        .sum()
}

/// The bind group layout of a module's second group: `planes` sampled planes and, for a pass, its
/// output's storage format.
pub(super) fn plane_layout(
    device: &wgpu::Device,
    planes: usize,
    output: Option<wgpu::TextureFormat>,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayout {
    let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..planes as u32)
        .map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        })
        .collect();
    if let Some(format) = output {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: OUTPUT_BINDING,
            visibility,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: PARAMS_BINDING,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("luxforge.gpu_preview.planes"),
        entries: &entries,
    })
}

/// One compiled pass: its pipeline, its second group's layout and the planes it binds.
#[derive(Clone)]
pub(super) struct CompiledPass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    slots: Slots,
    step: usize,
    output: u32,
    shape: PassShape,
}

impl CompiledPass {
    /// What a light link binds and dispatches its passes with ([`super::light`]), whose planes are
    /// its own and the pool's light planes, never a link's.
    pub(super) fn pipeline(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }

    pub(super) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The step and plane each of its second group's slots binds, in slot order.
    pub(super) fn slots(&self) -> &[(usize, u32)] {
        self.slots.planes()
    }
}

/// Every spatial step's compiled passes, in order, and the frame's second group's layout.
#[derive(Clone, Default)]
pub(super) struct CompiledSpatial {
    pub(super) passes: Vec<CompiledPass>,
    pub(super) fragment: Option<(wgpu::BindGroupLayout, Slots)>,
}

/// Whether a device can run spatial steps: compute shaders with a 256-lane workgroup, its shared
/// scratch, a storage texture beside the planes a pass reads, and a pass's parameters at a
/// 256-byte offset.
pub(super) fn supported(limits: &wgpu::Limits) -> bool {
    limits.max_compute_invocations_per_workgroup >= WORKGROUP_LANES
        && limits.max_compute_workgroup_size_x >= WORKGROUP_LANES
        && limits.max_compute_workgroup_storage_size >= SHARED_VALUES * 4
        && limits.max_storage_textures_per_shader_stage >= 1
        && limits.max_storage_buffers_per_shader_stage >= 3
        && limits.max_sampled_textures_per_shader_stage >= 16
        && limits.max_bindings_per_bind_group > PARAMS_BINDING
        && u64::from(limits.min_storage_buffer_offset_alignment) <= PARAMS_STRIDE
}

/// One compiled pass module: its text, its pipeline and its second group's layout.
struct CachedPass {
    source: String,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    used: u64,
}

/// The pass pipelines the stage keeps across sequences, keyed by their module's text, at most
/// [`PASS_CACHE`], the least recently used evicted first. A module is its kernel and its shape, so a
/// sequence that differs from another in a unit or an estimate compiles only the passes that
/// differ, and one that differs only in the colour steps before a spatial step compiles only the
/// passes that read their unit's input. Only a pipeline whose sequence compiled cleanly joins it.
#[derive(Default)]
pub(super) struct PassCache {
    entries: std::sync::Mutex<(Vec<CachedPass>, u64)>,
    /// How many pass pipelines were created, for the tests that prove the reuse.
    created: std::sync::atomic::AtomicU64,
}

impl PassCache {
    fn lookup(&self, source: &str) -> Option<(wgpu::ComputePipeline, wgpu::BindGroupLayout)> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.1 += 1;
        let clock = entries.1;
        let found = entries
            .0
            .iter_mut()
            .find(|cached| cached.source == source)?;
        found.used = clock;
        Some((found.pipeline.clone(), found.layout.clone()))
    }

    /// Keep the passes a sequence compiled cleanly.
    pub(super) fn keep(&self, made: Vec<(String, wgpu::ComputePipeline, wgpu::BindGroupLayout)>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (source, pipeline, layout) in made {
            entries.1 += 1;
            let used = entries.1;
            if entries.0.iter().any(|cached| cached.source == source) {
                continue;
            }
            if entries.0.len() >= PASS_CACHE
                && let Some(oldest) = entries
                    .0
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, cached)| cached.used)
                    .map(|(index, _)| index)
            {
                entries.0.swap_remove(oldest);
            }
            entries.0.push(CachedPass {
                source,
                pipeline,
                layout,
                used,
            });
        }
    }

    /// How many pass pipelines have been created.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn created(&self) -> u64 {
        self.created.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many pass pipelines are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .len()
    }
}

/// What [`compile_passes`] made that the stage's [`PassCache`] did not hold, to keep once the
/// sequence's error scopes report it clean.
pub(super) type MadePasses = Vec<(String, wgpu::ComputePipeline, wgpu::BindGroupLayout)>;

/// Compile every spatial step's passes of `steps`, each module validated as the surface
/// validates, inside the caller's error scopes. One module serves every pass with the same text,
/// within the sequence and, through the stage's [`PassCache`], across sequences.
///
/// wgpu is handed the module validated here, compacted to its entry point, rather than its text:
/// naga's backends translate every function of a module with one entry point, so the driver would
/// compile the whole spatial program, and every program before it, for every pass. Compacted, it
/// compiles only what the pass's kernel reaches, and nothing is parsed twice.
pub(super) fn compile_passes(
    device: &wgpu::Device,
    support: &Support,
    steps: &[GpuStep],
) -> Result<(CompiledSpatial, MadePasses), String> {
    let mut compiled = CompiledSpatial::default();
    let mut modules: MadePasses = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let GpuStep::Spatial(spatial) = step else {
            continue;
        };
        for pass in &spatial.passes {
            let (source, slots) = pass_module(steps, index, pass)?;
            let format = written_format(steps, index, pass.output)
                .ok_or("a pass writes a plane no texture holds")?
                .texture();
            let made = modules
                .iter()
                .find(|(text, ..)| *text == source)
                .map(|(_, pipeline, layout)| (pipeline.clone(), layout.clone()));
            let (pipeline, layout) = match made.or_else(|| support.passes.lookup(&source)) {
                Some((pipeline, layout)) => (pipeline, layout),
                None => {
                    let mut compacted = validate(&source)?;
                    naga::compact::compact(&mut compacted, naga::compact::KeepUnused::No);
                    let layout = plane_layout(
                        device,
                        slots.len(),
                        Some(format),
                        wgpu::ShaderStages::COMPUTE,
                    );
                    let pipeline_layout =
                        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: Some("luxforge.gpu_preview.pass_layout"),
                            bind_group_layouts: &[&support.layout, &layout],
                            push_constant_ranges: &[],
                        });
                    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("luxforge.gpu_preview.pass"),
                        source: wgpu::ShaderSource::Naga(Cow::Owned(compacted)),
                    });
                    let pipeline =
                        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                            label: Some("luxforge.gpu_preview.pass"),
                            layout: Some(&pipeline_layout),
                            module: &module,
                            entry_point: Some("lf_pass"),
                            compilation_options: wgpu::PipelineCompilationOptions::default(),
                            cache: None,
                        });
                    support
                        .passes
                        .created
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    modules.push((source, pipeline.clone(), layout.clone()));
                    (pipeline, layout)
                }
            };
            compiled.passes.push(CompiledPass {
                pipeline,
                layout,
                slots,
                step: index,
                output: pass.output,
                shape: pass.shape,
            });
        }
    }
    if steps.iter().any(|step| matches!(step, GpuStep::Spatial(_))) {
        let (_, slots) = fragment_declarations(steps);
        let layout = plane_layout(device, slots.len(), None, wgpu::ShaderStages::FRAGMENT);
        compiled.fragment = Some((layout, slots));
    }
    Ok((compiled, modules))
}

/// A texture's class: the texture format a plane's own format gives it, and the plane's size.
/// `Scalar` and `HalfScalar` planes are of one class format, `r32float`, and `Pair` and `HalfPair`
/// of another, `rg32float`. Every link of a slot covers the boundary's size and origin, so a class
/// has one extent there. Classes are ordered, so a layout compares by what it holds, not by the
/// order its planes were seen in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct Class {
    /// The plane format that names the class's texture format ([`PlaneFormat::kept_as`]).
    pub(super) format: PlaneFormat,
    pub(super) size: PlaneSize,
}

impl Class {
    pub(super) fn of(plane: GpuPlane) -> Self {
        Self {
            format: plane.format.kept_as(),
            size: plane.size,
        }
    }

    /// A plane of the class, whose texture format, extent and bytes are the class's.
    pub(super) fn plane(self) -> GpuPlane {
        GpuPlane {
            format: self.format,
            size: self.size,
        }
    }
}

/// Where one plane of a link's spatial step is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum PlaneTexture {
    /// The link's kept texture at this index: a plane an apply of its step reads.
    Kept(usize),
    /// The pool's `k`-th texture of the class ([`PoolKey`]): a scratch plane, the link's `k`-th of
    /// its class in its own plane order.
    Pool(Class, usize),
    /// The slot's light plane `k` ([`PlaneSize::Light`]), which the pool holds for every link and
    /// the plan's `k`-th light link writes.
    Light(u32),
}

/// How a link lays out its planes, and what decides whether the planes it holds serve another
/// plan: each spatial step's planes, where each is held, the link's kept textures, how many scratch
/// planes of each class it holds, its passes, and the boundary's size and stage origin.
///
/// A plane any apply of its step reads ([`GpuApply::planes`]) is **kept** in a texture of the
/// link's own: the frame's pass reads it, and so does every later tick that changes only an apply's
/// word or rewrites the plane only around a change. Every other plane is **scratch**, reduced and
/// fixed-size ones included: only its own step's passes read it, within the tick that writes it. A
/// link's `k`-th scratch plane of a [`Class`], in its own plane order, is texture `k` of that class
/// ([`PlaneTexture::Pool`]), so where a plane is held depends on the link's own steps alone, and one
/// pool holding, for each class, the most any link of a chain holds can serve every link in turn
/// ([`PoolKey`]).
///
/// No two planes of a link share a texture. A link holds at most one spatial step
/// ([`super::chain::chain`]), whose composition already gave every plane alive at once its own
/// (`compose` in the core), and a scratch plane takes a texture of its own class alone, never one of
/// another format that would hold it. Each pass therefore writes the format its plane's own
/// [`PlaneFormat`] gives it ([`texture_formats`]), whatever the boundary, so the pipelines a sequence
/// compiles serve every boundary. [`Planes`] creates the link's kept textures; its scratch planes
/// are the slot's pool's ([`Pool`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanesKey {
    /// Each spatial step's index among the steps, and its planes.
    planes: Vec<(usize, Vec<GpuPlane>)>,
    /// Where each spatial step's planes are held, indexed as `planes`.
    locations: Vec<(usize, Vec<PlaneTexture>)>,
    /// The kept textures, each the plane it holds, in plane order.
    kept: Vec<GpuPlane>,
    /// How many scratch planes of each class the link holds, in class order.
    scratch: Vec<(Class, usize)>,
    /// How many passes the parameter buffer holds a slice for.
    passes: usize,
    size: (u32, u32),
    origin: (u32, u32),
}

/// The format of the texture that holds each plane of each spatial step of `steps`, indexed as
/// the steps and their planes are: the plane's own, which a pass writing the plane stores in.
pub(super) fn texture_formats(steps: &[GpuStep]) -> Vec<(usize, Vec<PlaneFormat>)> {
    steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            GpuStep::Spatial(spatial) => Some((
                index,
                spatial.planes.iter().map(|plane| plane.format).collect(),
            )),
            GpuStep::Colour { .. }
            | GpuStep::Masked(_)
            | GpuStep::Geometry(_)
            | GpuStep::Clipping(_) => None,
        })
        .collect()
}

/// The format a pass of step `index` writing plane `plane` stores in.
pub(super) fn written_format(steps: &[GpuStep], index: usize, plane: u32) -> Option<PlaneFormat> {
    texture_formats(steps)
        .into_iter()
        .find(|(step, _)| *step == index)
        .and_then(|(_, formats)| formats.get(plane as usize).copied())
}

impl PlanesKey {
    /// The layout of `steps`' spatial steps over a boundary of `size` texels whose texel `(0, 0)`
    /// is stage pixel `origin`, or `None` for steps without one.
    pub(super) fn of(steps: &[GpuStep], size: (u32, u32), origin: (u32, u32)) -> Option<Self> {
        let mut planes = Vec::new();
        let mut locations = Vec::new();
        let mut kept = Vec::new();
        let mut scratch = std::collections::BTreeMap::<Class, usize>::new();
        for (index, step) in steps.iter().enumerate() {
            let GpuStep::Spatial(spatial) = step else {
                continue;
            };
            let read = |plane: usize| {
                spatial
                    .applies
                    .iter()
                    .any(|apply| apply.planes.contains(&(plane as u32)))
            };
            let held = spatial
                .planes
                .iter()
                .enumerate()
                .map(|(number, plane)| {
                    if let PlaneSize::Light(k) = plane.size {
                        // The slot's, whichever link reads it: never a texture of the link's.
                        PlaneTexture::Light(k)
                    } else if read(number) {
                        kept.push(*plane);
                        PlaneTexture::Kept(kept.len() - 1)
                    } else {
                        let class = Class::of(*plane);
                        let count = scratch.entry(class).or_default();
                        *count += 1;
                        PlaneTexture::Pool(class, *count - 1)
                    }
                })
                .collect();
            planes.push((index, spatial.planes.clone()));
            locations.push((index, held));
        }
        if planes.is_empty() {
            return None;
        }
        Some(Self {
            planes,
            locations,
            kept,
            scratch: scratch.into_iter().collect(),
            passes: pass_count(steps),
            size,
            origin,
        })
    }

    /// Where plane `plane` of step `step` is held.
    pub(super) fn location(&self, step: usize, plane: u32) -> Option<PlaneTexture> {
        self.locations
            .iter()
            .find(|(index, _)| *index == step)
            .and_then(|(_, held)| held.get(plane as usize).copied())
    }

    /// How many scratch planes of each class the link holds, in class order.
    pub(super) fn scratch(&self) -> &[(Class, usize)] {
        &self.scratch
    }

    /// Each kept texture's plane, in plane order ([`PlaneTexture::Kept`]).
    #[cfg(test)]
    pub(super) fn kept(&self) -> &[GpuPlane] {
        &self.kept
    }

    /// The bytes the kept textures take, and the passes' parameter buffer: what the link holds
    /// beside the pool.
    pub(super) fn kept_bytes(&self) -> u64 {
        self.kept
            .iter()
            .map(|plane| plane.bytes(self.origin, self.size))
            .sum::<u64>()
            + self.parameter_bytes()
    }

    /// The bytes its kept textures and the passes' parameter buffer take, with a texture of its own
    /// for each scratch plane as well: for a link alone, whose pool is its own scratch, its chain's
    /// whole charge ([`super::chain_charge`]).
    #[cfg(test)]
    pub(super) fn bytes(&self) -> u64 {
        self.kept_bytes()
            + self
                .scratch
                .iter()
                .map(|&(class, count)| count as u64 * class.plane().bytes(self.origin, self.size))
                .sum::<u64>()
    }

    fn parameter_bytes(&self) -> u64 {
        PARAMS_STRIDE * self.passes.max(1) as u64
    }
}

/// The pool of scratch textures the links of a chain can take their scratch planes from in turn:
/// for each class, as many textures as the most scratch planes of that class any one link holds,
/// over the boundary's size and stage origin every link shares. A link's scratch plane
/// `(class, k)` ([`PlaneTexture::Pool`]) is the pool's texture `k` of that class, so distinct
/// scratch planes of one link, alive together within its tick, take distinct textures, and no plane
/// an apply reads is one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PoolKey {
    /// How many textures of each class it holds, in class order.
    textures: Vec<(Class, usize)>,
    size: (u32, u32),
    origin: (u32, u32),
    /// How many light planes it holds ([`PlaneSize::Light`]): one past the largest any link
    /// declares, whatever the boundary.
    lights: u32,
}

/// The bytes one light plane takes: one `rgba32float` texel.
pub(super) const LIGHT_BYTES: u64 = 16;

impl PoolKey {
    /// The pool the spatial steps of `links` take their scratch planes from over a boundary of
    /// `size` texels whose texel `(0, 0)` is stage pixel `origin`: every link of a chain, the last
    /// one included ([`super::chain::Chain`]), and the light planes any of them declares.
    pub(super) fn of<'a>(
        links: impl IntoIterator<Item = &'a [GpuStep]>,
        size: (u32, u32),
        origin: (u32, u32),
    ) -> Self {
        let mut most = std::collections::BTreeMap::<Class, usize>::new();
        let mut lights = 0;
        for steps in links {
            for step in steps {
                if let GpuStep::Spatial(spatial) = step {
                    lights = spatial.lights().map(|k| k + 1).fold(lights, u32::max);
                }
            }
            let Some(key) = PlanesKey::of(steps, size, origin) else {
                continue;
            };
            for &(class, count) in key.scratch() {
                let held = most.entry(class).or_default();
                *held = (*held).max(count);
            }
        }
        Self {
            textures: most.into_iter().collect(),
            size,
            origin,
            lights,
        }
    }

    /// The pool of `self`'s scratch textures holding `lights` light planes: one fitted for a light
    /// link, which declares the light it writes, beside the links of the plan whose readers do.
    #[cfg_attr(
        not(any(test, feature = "qualification")),
        expect(dead_code, reason = "the slot runs no light link yet")
    )]
    pub(super) fn with_lights(self, lights: u32) -> Self {
        Self {
            lights: self.lights.max(lights),
            ..self
        }
    }

    /// How many textures of each class it holds, in class order.
    pub(super) fn textures(&self) -> &[(Class, usize)] {
        &self.textures
    }

    /// How many textures of `class` it holds.
    fn count(&self, class: Class) -> usize {
        self.textures
            .iter()
            .find(|(held, _)| *held == class)
            .map_or(0, |(_, count)| *count)
    }

    /// The bytes one of its textures of `class` takes.
    fn texture_bytes(&self, class: Class) -> u64 {
        let (width, height) = self.extent(class);
        u64::from(width) * u64::from(height) * class.format.texel_bytes()
    }

    /// The extent of each of its textures of `class`.
    pub(super) fn extent(&self, class: Class) -> (u32, u32) {
        class.plane().extent(self.origin, self.size)
    }

    /// The bytes its textures take, its light planes' among them.
    pub(super) fn bytes(&self) -> u64 {
        self.textures()
            .iter()
            .map(|&(class, count)| count as u64 * self.texture_bytes(class))
            .sum::<u64>()
            + u64::from(self.lights) * LIGHT_BYTES
    }
}

/// One texture of a [`Pool`], with its view.
pub(super) struct PoolTexture {
    /// Held with its view; only the poison writes it directly.
    #[cfg_attr(not(any(test, feature = "qualification")), allow(dead_code))]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl PoolTexture {
    /// `texture` with its view, for a light link's own textures to retire as a pool's do
    /// ([`super::light`]).
    pub(super) fn new(texture: wgpu::Texture) -> Self {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }

    pub(super) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The texture, for a readback.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// One light plane of a [`Pool`] ([`PlaneSize::Light`]), with the content key of the light its
/// light link last wrote into it: what a step reading it folds into its passes' keys, so a light
/// that changes runs again everything that reads it ([`Schedule`]).
pub(super) struct LightPlane {
    texture: PoolTexture,
    key: Option<u64>,
}

/// What a light plane is created with: what a plane is, a light link's storage write among it, and
/// in a build with a readback the copies a test reads it by.
#[cfg(not(any(test, feature = "qualification")))]
const LIGHT_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::STORAGE_BINDING);
#[cfg(any(test, feature = "qualification"))]
const LIGHT_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::STORAGE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// The scratch textures a slot's links take their scratch planes from in turn, laid out as a
/// [`PoolKey`] says, with a record of what each holds. The slot holds one for its whole life, and
/// so does a qualification session, each fitting it through [`Pool::fit`], so the two never lay
/// out differently.
///
/// - **Fitting.** Before any link's planes, the pool is fitted to every link of the plan: a texture
///   the plan needs beyond what it holds is charged, then created, and changes no binding; one
///   past the need is handed back to retire; a new boundary size or origin replaces them all.
/// - **Generation.** Removing or replacing a texture bumps it. A link whose groups were built
///   under another generation rebuilds them and forgets what its planes hold ([`Groups`]).
/// - **Records.** Each texture's record is who last wrote it and what: the holder of the
///   [`Schedule`] whose pass wrote it, and that pass's content key. A schedule reads a texture's
///   key only from a record of its own, so a link whose scratch another link wrote since runs the
///   passes that write it as for planes never written.
/// - **Holders.** A counter, never restarted, hands each schedule its holder when its link's
///   planes are created and again at every reset, so a reset makes every record it wrote foreign
///   and no two schedules ever hold one.
/// - **Light planes.** Beside the scratch, the slot's light planes ([`PlaneSize::Light`]): one
///   texel each, kept, whatever the boundary, so a new boundary size or origin keeps them, and
///   never poisoned. The plan's `k`-th light link writes light `k` and records its key
///   ([`Pool::set_light_key`]); every link reading it binds it and folds that key into its
///   passes' ([`Schedule`]) and its own ([`fold_lights`]).
#[derive(Default)]
pub(super) struct Pool {
    /// The boundary's size and stage origin its textures cover.
    size: (u32, u32),
    origin: (u32, u32),
    /// Each class's textures, in class order: a link's scratch plane `(class, k)` is the `k`-th.
    textures: Vec<(Class, Vec<PoolTexture>)>,
    /// Each written texture's record, by its class and number: its writer's holder and key.
    records: Vec<((Class, usize), (u64, u64))>,
    generation: u64,
    /// The last holder handed out.
    holders: u64,
    /// What the scratch textures take.
    bytes: u64,
    /// The light planes, light `k` the `k`-th, and what they take.
    lights: Vec<LightPlane>,
    light_bytes: u64,
    /// Tests only: write a sentinel into every texture, and forget every record, before each
    /// link's passes ([`Pool::poison`]).
    #[cfg(any(test, feature = "qualification"))]
    poisoned: bool,
    /// The sentinel's sources, `f32` and half-float NaN bits, each at least as large as a texture
    /// it is copied into.
    #[cfg(any(test, feature = "qualification"))]
    sentinels: [Option<wgpu::Buffer>; 2],
}

/// What a pool texture is created with: what a plane is, and in a build with the poison a copy's
/// destination, which the sentinel is written as.
#[cfg(not(any(test, feature = "qualification")))]
const POOL_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::STORAGE_BINDING);
#[cfg(any(test, feature = "qualification"))]
const POOL_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::STORAGE_BINDING)
    .union(wgpu::TextureUsages::COPY_DST);

impl Pool {
    /// Make the pool hold what `key` lays out, before any link's planes are fitted: when `key`
    /// covers another boundary size or origin, every scratch texture is handed to `retire` with its
    /// bytes; so is each texture of a class past what `key` needs, the last of the class first; and
    /// for each class `key` needs more of, the missing textures' bytes are passed to `charge` before
    /// they are created. The light planes follow `key`'s count alike, whatever the boundary.
    /// Removing or replacing a texture bumps the generation; adding one leaves every texture where
    /// it was. A refused charge is answered at once, what was created before it held and charged,
    /// and nothing is created twice: the next fit adds only what is still missing.
    pub(super) fn fit<E>(
        &mut self,
        device: &wgpu::Device,
        key: &PoolKey,
        charge: &mut dyn FnMut(u64) -> Result<(), E>,
        retire: &mut dyn FnMut(Vec<PoolTexture>, u64),
    ) -> Result<(), E> {
        // Another boundary: every texture takes another extent.
        if (self.size, self.origin) != (key.size, key.origin) {
            let replaced: Vec<PoolTexture> = self
                .textures
                .drain(..)
                .flat_map(|(_, textures)| textures)
                .collect();
            if !replaced.is_empty() {
                retire(replaced, std::mem::take(&mut self.bytes));
                self.generation += 1;
            }
            self.records.clear();
            (self.size, self.origin) = (key.size, key.origin);
        }
        // The textures past the plan's need.
        let mut removed = Vec::new();
        let mut removed_bytes = 0;
        for (class, textures) in &mut self.textures {
            let need = key.count(*class);
            if textures.len() > need {
                removed_bytes += (textures.len() - need) as u64 * key.texture_bytes(*class);
                removed.extend(textures.drain(need..));
                self.records
                    .retain(|((held, number), _)| *held != *class || *number < need);
            }
        }
        self.textures.retain(|(_, textures)| !textures.is_empty());
        if !removed.is_empty() {
            self.bytes -= removed_bytes;
            self.generation += 1;
            retire(removed, removed_bytes);
        }
        // The textures the plan needs beyond what the pool holds, class by class.
        for &(class, need) in key.textures() {
            let at = match self.textures.binary_search_by(|(held, _)| held.cmp(&class)) {
                Ok(at) => at,
                Err(at) => {
                    self.textures.insert(at, (class, Vec::new()));
                    at
                }
            };
            let held = self.textures[at].1.len();
            if held >= need {
                continue;
            }
            let each = key.texture_bytes(class);
            charge((need - held) as u64 * each)?;
            let (width, height) = key.extent(class);
            for _ in held..need {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("luxforge.gpu_preview.scratch"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: class.format.texture(),
                    usage: POOL_USAGE,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.textures[at].1.push(PoolTexture { texture, view });
                self.bytes += each;
            }
        }
        // The light planes, whatever the boundary: those past the plan's need retire, the last
        // first, and those it needs beyond what the pool holds are charged, then created, holding
        // no light yet.
        let need = key.lights as usize;
        if self.lights.len() > need {
            let removed: Vec<PoolTexture> = self
                .lights
                .drain(need..)
                .map(|light| light.texture)
                .collect();
            let bytes = removed.len() as u64 * LIGHT_BYTES;
            self.light_bytes -= bytes;
            self.generation += 1;
            retire(removed, bytes);
        }
        if self.lights.len() < need {
            charge((need - self.lights.len()) as u64 * LIGHT_BYTES)?;
            while self.lights.len() < need {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("luxforge.gpu_preview.light"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: PlaneFormat::Quad.texture(),
                    usage: LIGHT_USAGE,
                    view_formats: &[],
                });
                self.lights.push(LightPlane {
                    texture: PoolTexture::new(texture),
                    key: None,
                });
                self.light_bytes += LIGHT_BYTES;
            }
        }
        Ok(())
    }

    /// Light plane `k`'s view, when the pool holds it: what a link reading it binds, and the light
    /// link writing it stores to.
    pub(super) fn light_view(&self, k: u32) -> Option<&wgpu::TextureView> {
        self.lights
            .get(k as usize)
            .map(|light| light.texture.view())
    }

    /// The content key of the light light plane `k` holds: `None` before its light link has
    /// written one, or for a plane the pool does not hold.
    pub(super) fn light_key(&self, k: u32) -> Option<u64> {
        self.lights.get(k as usize).and_then(|light| light.key)
    }

    /// Record that light plane `k` now holds the light of content key `key`, which its light link
    /// wrote in a submission before any that reads it.
    pub(super) fn set_light_key(&mut self, k: u32, key: u64) {
        if let Some(light) = self.lights.get_mut(k as usize) {
            light.key = Some(key);
        }
    }

    /// Light plane `k`'s texture, for a readback.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn light_texture(&self, k: u32) -> Option<&wgpu::Texture> {
        self.lights
            .get(k as usize)
            .map(|light| light.texture.texture())
    }

    /// What the pool holds, as a layout, which a qualification session holds a later plan's to.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn key(&self) -> PoolKey {
        PoolKey {
            textures: self
                .textures
                .iter()
                .map(|(class, textures)| (*class, textures.len()))
                .collect(),
            size: self.size,
            origin: self.origin,
            lights: self.lights.len() as u32,
        }
    }

    /// The bytes its textures take, its light planes' among them.
    pub(super) fn bytes(&self) -> u64 {
        self.bytes + self.light_bytes
    }

    /// Bumped whenever a texture is removed or replaced.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// The `number`-th texture of `class`'s view. Fitting the pool to every link of a plan before
    /// any link binds it makes every scratch plane of theirs one it holds.
    fn view(&self, class: Class, number: usize) -> &wgpu::TextureView {
        &self
            .textures
            .iter()
            .find(|(held, _)| *held == class)
            .and_then(|(_, textures)| textures.get(number))
            .expect("the pool holds every scratch plane of the plan's links")
            .view
    }

    /// A new schedule's holder.
    fn holder(&mut self) -> u64 {
        self.holders += 1;
        self.holders
    }

    /// The key of what texture `number` of `class` holds, when `holder` wrote it last.
    fn held(&self, (class, number): (Class, usize), holder: u64) -> Option<u64> {
        self.records
            .iter()
            .find(|(at, (by, _))| *at == (class, number) && *by == holder)
            .map(|(_, (_, key))| *key)
    }

    /// Record that `holder`'s pass wrote `key` into texture `number` of `class`.
    fn record(&mut self, at: (Class, usize), holder: u64, key: u64) {
        match self.records.iter_mut().find(|(held, _)| *held == at) {
            Some((_, record)) => *record = (holder, key),
            None => self.records.push((at, (holder, key))),
        }
    }

    /// Forget every record `holder` wrote.
    fn release(&mut self, holder: u64) {
        self.records.retain(|(_, (by, _))| *by != holder);
    }

    /// Each texture's class and number with the holder of its record, for the tests that hold a
    /// texture to one holder.
    #[cfg(test)]
    pub(super) fn holders(&self) -> Vec<((Class, usize), u64)> {
        self.records
            .iter()
            .map(|(at, (holder, _))| (*at, *holder))
            .collect()
    }

    /// Every texture, with its class and number, for the tests that measure what the slot holds.
    #[cfg(test)]
    pub(super) fn textures(&self) -> Vec<((Class, usize), &wgpu::Texture)> {
        self.textures
            .iter()
            .flat_map(|(class, textures)| {
                textures
                    .iter()
                    .enumerate()
                    .map(move |(number, held)| ((*class, number), &held.texture))
            })
            .collect()
    }

    /// Tests only: whether each link's passes start from the sentinel ([`Pool::poison`]).
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn set_poisoned(&mut self, poisoned: bool) {
        self.poisoned = poisoned;
    }

    /// Tests only, and only when [`Pool::set_poisoned`] switched it on: write NaN bits into every
    /// texel of every texture, `0x7fc00000` in each channel of an `f32` format and `0x7e00` in each
    /// of `rgba16float`, and forget every record, so the link whose passes `encoder` encodes next
    /// reads nothing it did not write itself in this tick. A pass that reads scratch beyond the
    /// cone [`GpuSpatial::reach`] bounds then carries NaN into its plane, and a frame that differs
    /// from a whole evaluation fails its comparison. Copied from a source buffer in `encoder`, not
    /// with `queue.write_texture`, whose writes land before the whole submission: the poison must
    /// fall between one link's passes and the next's.
    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn poison(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        if !self.poisoned {
            return;
        }
        self.records.clear();
        // Each class's copy: whether its format is half-float, its extent and its padded row.
        let copies: Vec<(bool, (u32, u32), u32)> = self
            .textures
            .iter()
            .map(|(class, _)| {
                let format = class.format.texture();
                let (width, height) = class.plane().extent(self.origin, self.size);
                let texel = format.block_copy_size(None).unwrap_or(16);
                let row = (width * texel).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                    * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                (class.format == PlaneFormat::Colour, (width, height), row)
            })
            .collect();
        for &(half, (_, height), row) in &copies {
            let bytes = u64::from(row) * u64::from(height);
            let source = &mut self.sentinels[usize::from(half)];
            if source.as_ref().is_none_or(|buffer| buffer.size() < bytes) {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luxforge.gpu_preview.sentinel"),
                    size: bytes,
                    usage: wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: true,
                });
                {
                    let pattern: &[u8] = if half {
                        &0x7e00u16.to_le_bytes()
                    } else {
                        &0x7fc0_0000u32.to_le_bytes()
                    };
                    let mut mapped = buffer.slice(..).get_mapped_range_mut();
                    for chunk in mapped.chunks_exact_mut(pattern.len()) {
                        chunk.copy_from_slice(pattern);
                    }
                }
                buffer.unmap();
                *source = Some(buffer);
            }
        }
        for ((_, textures), &(half, (width, height), row)) in self.textures.iter().zip(&copies) {
            let source = self.sentinels[usize::from(half)]
                .as_ref()
                .expect("a sentinel for every class");
            for held in textures {
                encoder.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: source,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(row),
                            rows_per_image: Some(height),
                        },
                    },
                    held.texture.as_image_copy(),
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }
}

/// A link's planes: its kept textures, created at their extents with their views, its passes'
/// parameters, and where each plane is held ([`PlanesKey`]); its scratch planes are the slot's
/// pool's ([`Pool`]).
pub(super) struct Planes {
    pub(super) key: PlanesKey,
    /// Each kept texture ([`PlaneTexture::Kept`]), in plane order.
    textures: Vec<(wgpu::Texture, wgpu::TextureView)>,
    /// Every pass's parameters, a [`PARAMS_STRIDE`] slice each, in plan order.
    parameters: wgpu::Buffer,
    /// What `parameters` holds, so a tick whose passes keep their places writes nothing.
    written: Vec<u32>,
    /// The parameters with every pass starting at its output's first texel: what places the
    /// passes in their plan, which the rectangle a tick runs them over does not change.
    placed: Vec<u32>,
    pub(super) bytes: u64,
}

impl Planes {
    /// The link's kept textures and parameters of `key`, created. The caller has charged
    /// [`PlanesKey::kept_bytes`].
    pub(super) fn create(device: &wgpu::Device, key: PlanesKey) -> Self {
        let textures = key
            .kept
            .iter()
            .map(|plane| {
                let (width, height) = plane.extent(key.origin, key.size);
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("luxforge.gpu_preview.plane"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: plane.format.texture(),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                (texture, view)
            })
            .collect();
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.gpu_preview.pass_parameters"),
            size: key.parameter_bytes(),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bytes = key.kept_bytes();
        Self {
            key,
            textures,
            parameters,
            written: Vec::new(),
            placed: Vec::new(),
            bytes,
        }
    }

    /// The kept textures, in plane order, and the parameter buffer, for the tests that measure what
    /// the slot holds.
    #[cfg(test)]
    pub(super) fn resources(&self) -> (Vec<&wgpu::Texture>, &wgpu::Buffer) {
        (
            self.textures.iter().map(|(texture, _)| texture).collect(),
            &self.parameters,
        )
    }

    /// Whether the passes of `steps` take other places in their plan than the last ones did,
    /// which the planes' contents then no longer follow.
    pub(super) fn moved(&mut self, steps: &[GpuStep]) -> bool {
        let placed = parameters(steps, &[]);
        let moved = placed != self.placed;
        self.placed = placed;
        moved
    }

    /// Write every pass's parameters for `steps`, each pass at its place of `places`, when they
    /// changed.
    pub(super) fn write_parameters(
        &mut self,
        queue: &wgpu::Queue,
        steps: &[GpuStep],
        places: &[Place],
    ) {
        let parameters = parameters(steps, places);
        if parameters != self.written {
            queue.write_buffer(&self.parameters, 0, &super::le_bytes(&parameters));
            self.written = parameters;
        }
    }

    /// The view of the texture plane `plane` of step `step` is held in: a kept texture of the
    /// link's, a scratch texture of `pool`, or one of its light planes, which fitting the pool to
    /// every link of the plan makes it hold.
    fn view<'a>(&'a self, step: usize, plane: u32, pool: &'a Pool) -> &'a wgpu::TextureView {
        match self.key.location(step, plane) {
            Some(PlaneTexture::Kept(index)) => &self.textures[index].1,
            Some(PlaneTexture::Pool(class, number)) => pool.view(class, number),
            Some(PlaneTexture::Light(k)) => pool
                .light_view(k)
                .expect("the pool holds every light plane of the plan's links"),
            None => panic!("plane {plane} of step {step} is one of the link's"),
        }
    }

    fn extent(&self, step: usize, plane: u32) -> (u32, u32) {
        self.plane(step, plane)
            .extent(self.key.origin, self.key.size)
    }

    fn plane(&self, step: usize, plane: u32) -> GpuPlane {
        let (_, planes) = self
            .key
            .planes
            .iter()
            .find(|(index, _)| *index == step)
            .expect("a spatial step's planes");
        planes[plane as usize]
    }

    /// A second group binding `slots` and, for a pass, its output and its slice `number` of the
    /// parameters, each plane's view from the link's kept textures or from `pool`.
    fn group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        slots: &Slots,
        output: Option<(usize, u32, usize)>,
        pool: &Pool,
    ) -> wgpu::BindGroup {
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = slots
            .planes()
            .iter()
            .enumerate()
            .map(|(binding, (step, plane))| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(self.view(*step, *plane, pool)),
            })
            .collect();
        if let Some((step, plane, number)) = output {
            entries.push(wgpu::BindGroupEntry {
                binding: OUTPUT_BINDING,
                resource: wgpu::BindingResource::TextureView(self.view(step, plane, pool)),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: PARAMS_BINDING,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &self.parameters,
                    offset: PARAMS_STRIDE * number as u64,
                    size: std::num::NonZeroU64::new(PARAMS_STRIDE),
                }),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_preview.planes"),
            layout,
            entries: &entries,
        })
    }
}

/// Where one pass runs: the texel of its output its first invocation starts at, the rectangle of
/// its output it writes, and the workgroups it dispatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) origin: [u32; 2],
    pub(super) limit: Rect,
    pub(super) dispatch: [u32; 3],
}

/// One pass's second group, with the plane it writes, that plane's extent and how it runs.
struct BoundPass {
    group: wgpu::BindGroup,
    plane: GpuPlane,
    extent: (u32, u32),
    shape: PassShape,
}

/// The second groups of one compiled plan over its planes: each pass's, and the frame's. They bind
/// the link's kept textures and the pool's, so they hold for the pool's generation they were built
/// under ([`Pool::generation`]).
pub(super) struct Groups {
    passes: Vec<BoundPass>,
    /// The stage pixel of the boundary's first texel, which the reduced planes' blocks are
    /// anchored against.
    origin: (u32, u32),
    pub(super) fragment: Option<wgpu::BindGroup>,
}

impl Groups {
    /// `compiled`'s groups over `planes`, each plane's view from the link's kept textures or from
    /// `pool` ([`PlanesKey::location`]).
    pub(super) fn new(
        device: &wgpu::Device,
        compiled: &CompiledSpatial,
        planes: &Planes,
        pool: &Pool,
    ) -> Self {
        let passes = compiled
            .passes
            .iter()
            .enumerate()
            .map(|(number, pass)| BoundPass {
                group: planes.group(
                    device,
                    &pass.layout,
                    &pass.slots,
                    Some((pass.step, pass.output, number)),
                    pool,
                ),
                plane: planes.plane(pass.step, pass.output),
                extent: planes.extent(pass.step, pass.output),
                shape: pass.shape,
            })
            .collect();
        let fragment = compiled
            .fragment
            .as_ref()
            .map(|(layout, slots)| planes.group(device, layout, slots, None, pool));
        Self {
            passes,
            origin: planes.key.origin,
            fragment,
        }
    }

    /// Where each pass runs to fill `rect` of the boundary: its output plane's texels over it, a
    /// reduced plane's blocks the rectangle touches, and a fixed plane, or a workgroup's pass,
    /// whole.
    pub(super) fn places(&self, rect: Rect) -> Vec<Place> {
        self.places_each(&vec![rect; self.passes.len()])
    }

    /// [`Groups::places`] with each pass over its own rectangle of `rects`, in plan order.
    pub(super) fn places_each(&self, rects: &[Rect]) -> Vec<Place> {
        self.passes
            .iter()
            .zip(rects)
            .map(|(pass, rect)| {
                let covered = match pass.plane.size {
                    PlaneSize::Reduced(s) => rect.reduced(s, self.origin, pass.extent),
                    PlaneSize::Fixed { .. } | PlaneSize::Light(_) => Rect::whole(pass.extent),
                };
                match pass.shape {
                    // A pass's invocations start where they would over the whole plane, every
                    // `span` texels from its first: a running sum's drift depends on where its run
                    // begins, so the texels a smaller rectangle fills are those the whole plane's
                    // pass gives, bit for bit. It writes only the rectangle's texels, not the rest
                    // of the runs and workgroups that reach past it.
                    PassShape::Texels { span } if !covered.is_empty() => {
                        let x0 = covered.x0 / span[0] * span[0];
                        let y0 = covered.y0 / span[1] * span[1];
                        Place {
                            origin: [x0, y0],
                            limit: covered,
                            dispatch: [
                                (covered.x1 - x0).div_ceil(span[0]).div_ceil(GROUP_SIDE),
                                (covered.y1 - y0).div_ceil(span[1]).div_ceil(GROUP_SIDE),
                                1,
                            ],
                        }
                    }
                    PassShape::Texels { .. } => Place {
                        origin: [0, 0],
                        limit: covered,
                        dispatch: [0, 0, 0],
                    },
                    PassShape::Workgroup => Place {
                        origin: [0, 0],
                        limit: Rect::whole((UNLIMITED, UNLIMITED)),
                        dispatch: [1, 1, 1],
                    },
                }
            })
            .collect()
    }

    /// Encode the passes `run` marks, in order, each at its place of `places`, group 0 the plan's
    /// words, blocks and boundary, and answer how many.
    pub(super) fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        compiled: &CompiledSpatial,
        programs: &wgpu::BindGroup,
        run: &[bool],
        places: &[Place],
    ) -> u64 {
        let count = run.iter().filter(|run| **run).count() as u64;
        if count == 0 {
            return 0;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("luxforge.gpu_preview.spatial"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, programs, &[]);
        for ((compiled, bound), (run, Place { dispatch, .. })) in compiled
            .passes
            .iter()
            .zip(&self.passes)
            .zip(run.iter().zip(places))
        {
            if !run || dispatch.contains(&0) {
                continue;
            }
            pass.set_pipeline(&compiled.pipeline);
            pass.set_bind_group(1, &bound.group, &[]);
            pass.dispatch_workgroups(dispatch[0], dispatch[1], dispatch[2]);
        }
        count
    }
}

/// Which passes a tick runs: only those the applies' planes need because something a pass reads
/// changed since the planes were last written.
///
/// A pass's content key hashes what its output depends on: its own words, the step's upstream (the
/// boundary's version, the texel map and every step before it, words and blocks), the words of the
/// applies its source runs and the keys of the planes they read, and the keys of its inputs as the
/// passes before it in the tick leave them. Each plane keeps the key of what it holds. An apply's
/// plane whose key differs from the one its last writer would give it is stale, and walking the
/// passes backwards, a pass whose output is stale or needed runs and needs its inputs in turn,
/// unless an input's only writer before it is its first in the tick and the plane already holds
/// what that writer would write. A pass that writes an apply's plane on the way to its last writer
/// runs that last writer too, so an apply never reads a plane a scratch use left behind. A new
/// sequence or new planes start from nothing kept, which runs every pass the applies need. The key
/// is kept by texture ([`PlanesKey::location`]): a kept texture's here, a pool texture's in the
/// pool's record of it, which this schedule reads only when it wrote that record itself, under its
/// current holder ([`Pool`]). Another link's pass, or this one's before a reset, leaves the texture
/// unknown, and the passes that write it run as for planes never written.
///
/// An identity apply ([`GpuApply::identity`]) reads no plane, in the frame or in a later unit's
/// source: its planes need not be current, and they are in no key of what reads through it. A pass
/// only they need does not run and keeps no key, writing no record, so the planes keep the key of
/// what they last held, and once the unit is not the identity, within a drag too, they are stale
/// against what its passes would write, or unknown when another link wrote them since, and run
/// with every input they need.
///
/// A light plane ([`PlaneSize::Light`]) holds the key its light link recorded in the pool, which
/// no pass of the step changes: it is in the key of every pass that reads it and every pass after,
/// so a light that changes runs them, and of the step's applies, so a later step's passes run too.
pub(super) struct Schedule {
    /// The key of what each of the link's kept textures holds, by its index.
    kept: Vec<(usize, u64)>,
    /// Its holder in the pool's records, drawn when the link's planes are created and again at
    /// every reset.
    holder: u64,
}

fn hash_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// What a light plane its light link has not written yet holds, as a key: no light link's.
const UNLIT: u64 = u64::MAX;

/// `input`, the content key of what a link's input holds, with the key of every light its `steps`
/// read ([`Pool::light_key`]): what the link's content key folds in, so a light that changes —
/// which changes nothing of the link's input, words or blocks — runs the link again, and each
/// step reading it runs its passes and draws whole ([`GpuSpatial::global`]). `input` itself for
/// steps that read no light.
#[cfg_attr(
    not(any(test, feature = "qualification")),
    expect(dead_code, reason = "the slot runs no light link yet")
)]
pub(super) fn fold_lights(input: u64, steps: &[GpuStep], pool: &Pool) -> u64 {
    let lights: Vec<(u32, Option<u64>)> = steps
        .iter()
        .filter_map(|step| match step {
            GpuStep::Spatial(spatial) => Some(spatial),
            _ => None,
        })
        .flat_map(|spatial| spatial.lights())
        .map(|k| (k, pool.light_key(k)))
        .collect();
    if lights.is_empty() {
        input
    } else {
        hash_of((input, lights))
    }
}

impl Schedule {
    /// A schedule of new planes, which hold nothing yet, with a holder drawn from `pool`.
    pub(super) fn new(pool: &mut Pool) -> Self {
        Self {
            kept: Vec::new(),
            holder: pool.holder(),
        }
    }

    /// Forget every plane's content — a new sequence's groups, new planes or another pool
    /// generation — under a new holder, so every pool record this schedule wrote is foreign.
    pub(super) fn reset(&mut self, pool: &mut Pool) {
        pool.release(self.holder);
        self.kept.clear();
        self.holder = pool.holder();
    }

    /// Forget what every kept texture but `textures` holds, and every pool texture this schedule
    /// wrote: an incremental tick's passes wrote them only where they ran.
    pub(super) fn keep_only(&mut self, textures: &[usize], pool: &mut Pool) {
        self.kept.retain(|(texture, _)| textures.contains(texture));
        pool.release(self.holder);
    }

    /// Its holder in the pool's records.
    #[cfg(test)]
    pub(super) fn holder(&self) -> u64 {
        self.holder
    }

    /// The key of what `texture` holds, as far as this schedule knows: a light plane's is the one
    /// its light link recorded.
    fn kept(&self, texture: PlaneTexture, pool: &Pool) -> Option<u64> {
        match texture {
            PlaneTexture::Kept(index) => self
                .kept
                .iter()
                .find(|(at, _)| *at == index)
                .map(|(_, key)| *key),
            PlaneTexture::Pool(class, number) => pool.held((class, number), self.holder),
            PlaneTexture::Light(k) => pool.light_key(k),
        }
    }

    fn keep(&mut self, texture: PlaneTexture, key: u64, pool: &mut Pool) {
        match texture {
            PlaneTexture::Kept(index) => match self.kept.iter_mut().find(|(at, _)| *at == index) {
                Some((_, kept)) => *kept = key,
                None => self.kept.push((index, key)),
            },
            PlaneTexture::Pool(class, number) => pool.record((class, number), self.holder, key),
            // Only its light link writes a light plane, never a step a schedule runs.
            PlaneTexture::Light(_) => {}
        }
    }

    /// The passes of `steps` this tick runs, in [`CompiledSpatial`]'s order, given the tick's
    /// packed `words` and `blocks`, the boundary's `version`, the `textures` that hold the planes
    /// and the `pool` that holds their scratch; the planes they write are then taken as written,
    /// the pool's recorded as this schedule's.
    pub(super) fn run(
        &mut self,
        steps: &[GpuStep],
        words: &[u32],
        blocks: &[u32],
        version: u64,
        textures: &PlanesKey,
        pool: &mut Pool,
    ) -> Vec<bool> {
        let mut run = Vec::new();
        // The upstream of each step: the boundary, the texel map, every step before it, by
        // content, not by where the packing put it.
        let mut upstream = hash_of((version, words.get(..MAP_WORDS)));
        for (index, step) in steps.iter().enumerate() {
            let header = MAP_WORDS + STEP_WORDS * index;
            let base = words.get(header).copied().unwrap_or(0) as usize;
            let block = words.get(header + 1).copied().unwrap_or(0) as usize;
            let position = words.get(header + 2..header + STEP_WORDS).unwrap_or(&[]);
            let own = words.get(base..base + step.word_count()).unwrap_or(&[]);
            let own_blocks = blocks.get(block..block + step.block_count()).unwrap_or(&[]);
            if let GpuStep::Spatial(spatial) = step {
                // Its passes read its program's words and blocks, not its mask's.
                let program = own.get(spatial.mask_words()..).unwrap_or(&[]);
                let program_blocks = own_blocks.get(..spatial.program.block.len()).unwrap_or(&[]);
                let inward = hash_of((upstream, position, program_blocks));
                let texture = |plane: u32| {
                    textures
                        .location(index, plane)
                        .expect("a plane of the link's own step")
                };
                let keys = self.step(&texture, pool, spatial, program, inward, &mut run);
                // A later step's source runs this one's applies over the planes they read.
                let identities: Vec<bool> =
                    spatial.applies.iter().map(|apply| apply.identity).collect();
                upstream = hash_of((upstream, position, own, own_blocks, identities, keys));
            } else {
                upstream = hash_of((upstream, position, own, own_blocks));
            }
        }
        run
    }

    /// One spatial step's passes, appended to `run`; answers the keys of the planes its applies
    /// read, an identity apply none.
    fn step(
        &mut self,
        texture: &dyn Fn(u32) -> PlaneTexture,
        pool: &mut Pool,
        spatial: &GpuSpatial,
        program: &[u32],
        upstream: u64,
        run: &mut Vec<bool>,
    ) -> Vec<u64> {
        // A pass or apply reads from its offset up to the next one its step names.
        let mut offsets: Vec<u32> = spatial
            .passes
            .iter()
            .map(|pass| pass.words)
            .chain(spatial.applies.iter().map(|apply| apply.words))
            .chain(std::iter::once(program.len() as u32))
            .collect();
        offsets.sort_unstable();
        offsets.dedup();
        let slice = |start: u32| -> &[u32] {
            let end = offsets
                .iter()
                .copied()
                .find(|offset| *offset > start)
                .unwrap_or(program.len() as u32);
            program.get(start as usize..end as usize).unwrap_or(&[])
        };
        // Forward: the key each pass writes, and what each plane holds after the whole tick. A
        // light plane holds what its light link last wrote, which no pass of the step changes: a
        // pass reading it, and everything after that pass, runs again when the light does.
        let mut held: Vec<u64> = spatial
            .planes
            .iter()
            .map(|plane| match plane.size {
                PlaneSize::Light(k) => pool.light_key(k).unwrap_or(UNLIT),
                PlaneSize::Reduced(_) | PlaneSize::Fixed { .. } => 0,
            })
            .collect();
        let holds = |held: &[u64], plane: &u32| held.get(*plane as usize).copied().unwrap_or(0);
        let mut keys = Vec::with_capacity(spatial.passes.len());
        for (number, pass) in spatial.passes.iter().enumerate() {
            // The applies its source runs, by their words and the planes they read: an identity
            // apply reads none.
            let applies: Vec<(&[u32], Option<Vec<u64>>)> = spatial
                .applies
                .iter()
                .take(pass.source as usize)
                .map(|apply| {
                    let planes = apply.planes.iter().map(|plane| holds(&held, plane));
                    let read = (!apply.identity).then(|| planes.collect());
                    (slice(apply.words), read)
                })
                .collect();
            let inputs: Vec<u64> = pass
                .inputs
                .iter()
                .map(|plane| holds(&held, plane))
                .collect();
            let key = hash_of((number, upstream, slice(pass.words), applies, inputs));
            if let Some(plane) = held.get_mut(pass.output as usize) {
                *plane = key;
            }
            keys.push(key);
        }
        // The planes the frame's applies read, which must be current: none of an identity apply's.
        let read: Vec<u32> = {
            let mut read: Vec<u32> = spatial
                .applies
                .iter()
                .filter(|apply| !apply.identity)
                .flat_map(|apply| apply.planes.iter().copied())
                .collect();
            read.sort_unstable();
            read.dedup();
            read
        };
        let writers = |plane: u32| {
            spatial
                .passes
                .iter()
                .enumerate()
                .filter(move |(_, pass)| pass.output == plane)
                .map(|(number, _)| number)
        };
        // An input `plane` of the pass `reader` needs its writer run, unless that writer is the
        // plane's first and the plane already holds what it writes.
        let known = &*pool;
        let needs = |plane: u32, reader: usize| {
            let mut before = writers(plane).filter(|writer| *writer < reader);
            match before.next_back() {
                None => false,
                Some(writer) => {
                    writers(plane).next() != Some(writer)
                        || self.kept(texture(plane), known) != Some(keys[writer])
                }
            }
        };
        // What `plane` holds once the passes `runs` marks have run.
        let after = |plane: u32, runs: &[bool]| {
            writers(plane)
                .rfind(|writer| runs[*writer])
                .map(|writer| keys[writer])
                .or(self.kept(texture(plane), known))
        };
        let mut stale: Vec<u32> = read
            .iter()
            .copied()
            .filter(|&plane| writers(plane).next().is_some())
            .filter(|&plane| self.kept(texture(plane), known) != Some(holds(&held, &plane)))
            .collect();
        // Backward from the stale apply planes, until no apply plane is left half written.
        let runs = loop {
            let mut needed = stale.clone();
            let mut runs = vec![false; spatial.passes.len()];
            for (number, pass) in spatial.passes.iter().enumerate().rev() {
                if needed.contains(&pass.output) {
                    runs[number] = true;
                    needed.retain(|plane| *plane != pass.output);
                    for &input in &pass.inputs {
                        if !needed.contains(&input) && needs(input, number) {
                            needed.push(input);
                        }
                    }
                }
            }
            let left: Vec<u32> = read
                .iter()
                .copied()
                .filter(|&plane| writers(plane).next().is_some())
                .filter(|&plane| after(plane, &runs) != Some(holds(&held, &plane)))
                .filter(|plane| !stale.contains(plane))
                .collect();
            if left.is_empty() {
                break runs;
            }
            stale.extend(left);
        };
        // Each plane a pass wrote now holds what its last writer to run wrote.
        let written: Vec<(u32, u64)> = (0..spatial.planes.len() as u32)
            .filter_map(|plane| {
                let last = writers(plane).rfind(|writer| runs[*writer])?;
                Some((plane, keys[last]))
            })
            .collect();
        for (plane, key) in written {
            self.keep(texture(plane), key, pool);
        }
        run.extend_from_slice(&runs);
        read.iter().map(|plane| holds(&held, plane)).collect()
    }
}
