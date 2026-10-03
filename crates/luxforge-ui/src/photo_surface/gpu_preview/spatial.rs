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
//!   `[0, 1]` where the step says the CPU quantizes, then the applies of the units before it;
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
//! run, the spatial program, the declarations above and an entry the surface generates. Every
//! number that places a pass in its plan — its step's header, its words, its span, the words of the
//! applies its source runs — is data, read from the pass's slice of the planes' parameter buffer,
//! so a module is its kernel and its shape alone: passes and plans that differ only there share one
//! pipeline, which the stage keeps across sequences ([`PassCache`]). Its planes
//! are textures: `rgba16float`, `r32float`, `rg32float` or `rgba32float` by the plane's
//! [`PlaneFormat`], each sized to the boundary or to the stage's blocks it reaches
//! ([`GpuPlane::extent`]), or a texture an earlier step left free that holds the plane
//! ([`PlanesKey`]), whose format a pass writing the plane then stores in. A half-float texture is
//! written rounded to the nearest half, ties to even ([`HALF_ROUNDING`]): the M4's own conversion of
//! a storage write, and of a render target's, rounds toward zero. A pass or an apply reads the step's words from its offset up to the next
//! offset any pass or apply of its step names; a tick runs only the passes whose words, upstream or
//! inputs changed since the planes the applies read were last written ([`Schedule`]). The passes run in order before the frame's pass, which binds the
//! applies' planes as a second bind group, so the operation holds no colour plane of its own: its
//! memory is its planes, charged to the GPU-preview budget with the slot. Nothing is read back.
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
const GROUP_SIDE: u32 = 8;

/// The binding of a pass's output plane in its second group, after every plane it reads.
const OUTPUT_BINDING: u32 = 64;

/// The binding of a pass's parameters in its second group.
const PARAMS_BINDING: u32 = 65;

/// One pass's slice of the parameter buffer, in bytes and in words: the largest storage-binding
/// offset alignment a device may ask for.
const PARAMS_STRIDE: u64 = 256;
const PARAMS_WORDS: usize = (PARAMS_STRIDE / 4) as usize;

/// A pass's parameters: its step's header index, its words' offset in the step's words, its span,
/// then the offsets of the applies its source runs.
const PARAM_STEP: usize = 0;
const PARAM_WORDS: usize = 1;
const PARAM_SPAN: usize = 2;
const PARAM_APPLIES: usize = 4;

/// How many compiled pass modules the stage keeps across sequences, the least recently used
/// evicted first: every pass of the [`super::PIPELINE_CACHE`] sequences the pipeline keeps holds its
/// pipeline itself, so this bounds only what an evicted sequence can reuse.
pub(super) const PASS_CACHE: usize = 64;

/// What a plane's texels hold — how many channels, and whether half precision holds them — and so
/// the texture format it is kept in when it has a texture of its own. A plane may instead share a
/// texture whose format [holds](Self::holds) it ([`PlanesKey`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    fn channels(self) -> u32 {
        match self {
            Self::Scalar | Self::HalfScalar => 1,
            Self::Pair | Self::HalfPair => 2,
            Self::Colour | Self::Quad => 4,
        }
    }

    /// Whether a texture of this format holds a plane of `plane`'s: as many channels or more, and
    /// full precision unless half precision holds the plane.
    pub fn holds(self, plane: Self) -> bool {
        let half = matches!(plane, Self::Colour | Self::HalfScalar | Self::HalfPair);
        self.channels() >= plane.channels() && (self != Self::Colour || half)
    }
}

/// A plane's size against the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneSize {
    /// The stage's `s × s` blocks, anchored at the stage origin, the boundary reaches: `s = 1` is
    /// the boundary.
    Reduced(u32),
    /// A fixed size whatever the boundary.
    Fixed { width: u32, height: u32 },
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
    pub shape: PassShape,
}

/// One unit's apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuApply {
    pub function: Cow<'static, str>,
    /// The planes it reads, bound from the slot it is handed.
    pub planes: Vec<u32>,
    /// Its first word, after the step's base index.
    pub words: u32,
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
}

/// What decides one pass's pipeline besides its kernel's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PassKey {
    inputs: [u32; PASS_INPUTS],
    count: usize,
    output: u32,
    source: u32,
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

    /// Append the step's words and blocks at the bases the header recorded for it.
    pub(super) fn pack(&self, words: &mut Vec<u32>, blocks: &mut Vec<u32>) {
        blocks.extend_from_slice(&self.program.block);
        if let Some(coverage) = self.coverage() {
            coverage.pack(words, blocks);
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
        .chain(
            self.planes
                .iter()
                .map(|plane| (StepKind::Plane(*plane), "", "")),
        )
        .chain(self.passes.iter().map(|pass| {
            (
                StepKind::Pass(PassKey {
                    inputs: first(&pass.inputs),
                    count: pass.inputs.len(),
                    output: pass.output,
                    source: pass.source,
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
/// passes and applies name with its signature, and every plane, input, word and source in range.
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
    for plane in &spatial.planes {
        let valid = match plane.size {
            PlaneSize::Reduced(s) => s >= 1,
            PlaneSize::Fixed { width, height } => width >= 1 && height >= 1,
        };
        if !valid {
            return Err(format!("{entry:?} declares an empty plane"));
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
/// against the operation's input and its clamp, in a block of their own.
pub(super) fn frame_statements(index: usize, spatial: &GpuSpatial, slots: &Slots) -> String {
    let mut text = String::from("    {\n");
    text.push_str(&applies(
        index,
        spatial,
        spatial.applies.len(),
        slots,
        Offsets::Written,
    ));
    if spatial.mask.is_some() {
        text.push_str(&format!(
            "    let lf_spatial_coverage = lf_surface_mask_{index}(stage, lf_spatial_input);\n    \
             if lf_spatial_coverage == 0.0 {{\n        rgb = lf_spatial_input;\n    }} else {{\n        \
             rgb = (1.0 - lf_spatial_coverage) * lf_spatial_input + lf_spatial_coverage * rgb;\n    \
             }}\n"
        ));
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

/// One pass's whole module and the planes its second group binds, in slot order: the pass's
/// inputs, then the applies' planes its `lf_source` runs, then its output at [`OUTPUT_BINDING`].
pub(super) fn pass_module(
    steps: &[GpuStep],
    index: usize,
    pass: &GpuPass,
) -> Result<(String, Slots), String> {
    let GpuStep::Spatial(spatial) = &steps[index] else {
        return Err("a pass belongs to a spatial step".into());
    };
    // The format of the texture that holds its output, which may be one an earlier step left free.
    spatial
        .planes
        .get(pass.output as usize)
        .ok_or("a pass writes a plane its step does not declare")?;
    let format = written_format(steps, index, pass.output)
        .ok_or("a pass writes a plane no texture holds")?;
    let mut slots = Slots::default();
    slots
        .planes
        .extend(pass.inputs.iter().map(|input| (index, *input)));
    slots.bind_applies(steps, index, pass.source as usize);
    let mut source = String::from(super::PRELUDE);
    let mut included: Vec<&GpuProgram> = Vec::new();
    // The steps before, whose statements its source runs, and its own step's spatial program: not
    // the step's own mask, which only the frame's pass blends by.
    let programs = steps[..index]
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
    let (functions, masked) = masks(steps, 0..index);
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
         fn lf_store(at: vec2<i32>, value: vec4<f32>) {{\n    textureStore(lf_out, at, {stored});\n}}\n\
         @group(1) @binding({PARAMS_BINDING}) var<storage, read> lf_params: array<u32>;\n\
         fn lf_param(i: u32) -> u32 {{\n    return lf_params[i];\n}}\n",
        format.wgsl()
    ));
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
             let at = vec2<i32>(id.xy) * span;\n    \
             let size = vec2<i32>(textureDimensions(lf_out));\n    \
             if at.x >= size.x || at.y >= size.y {{\n        return;\n    }}\n    \
             {call}\n}}\n",
            PARAM_SPAN + 1
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
/// planes' parameter buffer holds for `steps`.
pub(super) fn parameters(steps: &[GpuStep]) -> Vec<u32> {
    let mut words = Vec::new();
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
                .take(PARAMS_WORDS - PARAM_APPLIES)
                .enumerate()
            {
                slice[PARAM_APPLIES + number] = mask_words + apply.words;
            }
            words.extend_from_slice(&slice);
        }
    }
    words
}

/// What the planes of `steps`' spatial steps take of the GPU-preview budget over a boundary of
/// `size` texels whose texel `(0, 0)` is stage pixel `origin`: their textures, chained steps
/// sharing them as a slot shares them ([`PlanesKey`]), and the passes' parameters. What the
/// desktop holds a region's plan to before its boundary exists; it creates nothing.
pub fn plane_bytes(steps: &[GpuStep], size: (u32, u32), origin: (u32, u32)) -> u64 {
    PlanesKey::of(steps, size, origin).map_or(0, |key| key.bytes())
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
/// differ. Only a pipeline whose sequence compiled cleanly joins it.
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
                    validate(&source)?;
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
                        source: wgpu::ShaderSource::Wgsl(Cow::Owned(source.clone())),
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

/// What a spatial plan's planes are keyed by: each spatial step's planes, the boundary's size and
/// its stage origin, and which texture holds each plane.
///
/// A plane no apply reads is scratch: only its own step's passes use it, and they have all run
/// before a later step's first pass. A later step's plane therefore takes an earlier step's scratch
/// texture of the same size whose format [holds](PlaneFormat::holds) it, the smallest such, so
/// chained spatial steps hold one texture for both: a scratch plane, which leaves the texture free
/// for a step after it, or a plane an apply reads, which keeps it, since the frame and every later
/// step's input read it. Which texture each plane takes, and so the format a pass writes, depends
/// on the steps alone ([`texture_formats`]), never on the boundary's size, so the pipelines a
/// sequence compiles serve every boundary.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanesKey {
    planes: Vec<(usize, Vec<GpuPlane>)>,
    /// Each step's planes' textures, indices into `textures`.
    aliases: Vec<(usize, Vec<usize>)>,
    /// The textures, each the plane that first took it.
    textures: Vec<GpuPlane>,
    /// How many passes the parameter buffer holds a slice for.
    passes: usize,
    size: (u32, u32),
    origin: (u32, u32),
}

/// Each spatial step's planes, which texture holds each, and the textures, each the plane that
/// first took it.
type Assignment = (
    Vec<(usize, Vec<GpuPlane>)>,
    Vec<(usize, Vec<usize>)>,
    Vec<GpuPlane>,
);

/// Which texture holds each plane of `steps`' spatial steps ([`PlanesKey`]).
fn assign(steps: &[GpuStep]) -> Assignment {
    let planes: Vec<(usize, Vec<GpuPlane>)> = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            GpuStep::Spatial(spatial) => Some((index, spatial.planes.clone())),
            GpuStep::Colour { .. }
            | GpuStep::Masked(_)
            | GpuStep::Geometry(_)
            | GpuStep::Clipping(_) => None,
        })
        .collect();
    // Earlier steps' scratch textures, free for a later step's planes.
    let mut textures: Vec<GpuPlane> = Vec::new();
    let mut scratch: Vec<usize> = Vec::new();
    let mut aliases = Vec::with_capacity(planes.len());
    for (index, step_planes) in &planes {
        let GpuStep::Spatial(spatial) = &steps[*index] else {
            continue;
        };
        let read = |plane: usize| {
            spatial
                .applies
                .iter()
                .any(|apply| apply.planes.contains(&(plane as u32)))
        };
        let mut taken: Vec<usize> = Vec::new();
        let mut made_scratch: Vec<usize> = Vec::new();
        let step_aliases = step_planes
            .iter()
            .enumerate()
            .map(|(number, plane)| {
                let shared = scratch
                    .iter()
                    .copied()
                    .filter(|texture| {
                        !taken.contains(texture)
                            && textures[*texture].size == plane.size
                            && textures[*texture].format.holds(plane.format)
                    })
                    .min_by_key(|texture| textures[*texture].format.texel_bytes());
                if let Some(texture) = shared
                    && read(number)
                {
                    // An apply reads it from here on: no later step may take it.
                    scratch.retain(|free| *free != texture);
                }
                let texture = shared.unwrap_or_else(|| {
                    textures.push(*plane);
                    if !read(number) {
                        made_scratch.push(textures.len() - 1);
                    }
                    textures.len() - 1
                });
                taken.push(texture);
                texture
            })
            .collect();
        scratch.extend(made_scratch);
        aliases.push((*index, step_aliases));
    }
    (planes, aliases, textures)
}

/// The format of the texture that holds each plane of each spatial step of `steps`, indexed as
/// the steps and their planes are: the format a pass writing the plane stores in.
pub(super) fn texture_formats(steps: &[GpuStep]) -> Vec<(usize, Vec<PlaneFormat>)> {
    let (_, aliases, textures) = assign(steps);
    aliases
        .into_iter()
        .map(|(index, aliases)| {
            let formats = aliases.iter().map(|&texture| textures[texture].format);
            (index, formats.collect())
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
    pub(super) fn of(steps: &[GpuStep], size: (u32, u32), origin: (u32, u32)) -> Option<Self> {
        let (planes, aliases, textures) = assign(steps);
        if planes.is_empty() {
            return None;
        }
        Some(Self {
            planes,
            aliases,
            textures,
            passes: pass_count(steps),
            size,
            origin,
        })
    }

    /// The texture that holds plane `plane` of step `step`.
    pub(super) fn texture(&self, step: usize, plane: u32) -> usize {
        self.aliases
            .iter()
            .find(|(index, _)| *index == step)
            .map_or(usize::MAX, |(_, aliases)| aliases[plane as usize])
    }

    /// The bytes every texture takes, and the passes' parameter buffer.
    pub(super) fn bytes(&self) -> u64 {
        self.textures
            .iter()
            .map(|plane| plane.bytes(self.origin, self.size))
            .sum::<u64>()
            + self.parameter_bytes()
    }

    fn parameter_bytes(&self) -> u64 {
        PARAMS_STRIDE * self.passes.max(1) as u64
    }
}

/// A plan's planes, created at their extents, with their views.
pub(super) struct Planes {
    pub(super) key: PlanesKey,
    /// Each texture of [`PlanesKey`], which one or more planes share.
    textures: Vec<(wgpu::Texture, wgpu::TextureView)>,
    /// Every pass's parameters, a [`PARAMS_STRIDE`] slice each, in plan order.
    parameters: wgpu::Buffer,
    /// What `parameters` holds, so a tick whose passes keep their places writes nothing.
    written: Vec<u32>,
    pub(super) bytes: u64,
}

impl Planes {
    /// Every plane of `key`, created. The caller has charged [`PlanesKey::bytes`].
    pub(super) fn create(device: &wgpu::Device, key: PlanesKey) -> Self {
        let textures = key
            .textures
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
        let bytes = key.bytes();
        Self {
            key,
            textures,
            parameters,
            written: Vec::new(),
            bytes,
        }
    }

    /// Write every pass's parameters for `steps` when they changed, and say whether they did.
    pub(super) fn write_parameters(&mut self, queue: &wgpu::Queue, steps: &[GpuStep]) -> bool {
        let parameters = parameters(steps);
        if parameters == self.written {
            return false;
        }
        queue.write_buffer(&self.parameters, 0, &super::le_bytes(&parameters));
        self.written = parameters;
        true
    }

    fn view(&self, step: usize, plane: u32) -> &wgpu::TextureView {
        &self.textures[self.key.texture(step, plane)].1
    }

    fn extent(&self, step: usize, plane: u32) -> (u32, u32) {
        let (_, planes) = self
            .key
            .planes
            .iter()
            .find(|(index, _)| *index == step)
            .expect("a spatial step's planes");
        planes[plane as usize].extent(self.key.origin, self.key.size)
    }

    /// A second group binding `slots` and, for a pass, its output and its slice `number` of the
    /// parameters.
    fn group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        slots: &Slots,
        output: Option<(usize, u32, usize)>,
    ) -> wgpu::BindGroup {
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = slots
            .planes()
            .iter()
            .enumerate()
            .map(|(binding, (step, plane))| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(self.view(*step, *plane)),
            })
            .collect();
        if let Some((step, plane, number)) = output {
            entries.push(wgpu::BindGroupEntry {
                binding: OUTPUT_BINDING,
                resource: wgpu::BindingResource::TextureView(self.view(step, plane)),
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

/// The second groups of one compiled plan over its planes: each pass's, and the frame's.
pub(super) struct Groups {
    passes: Vec<(wgpu::BindGroup, [u32; 3])>,
    pub(super) fragment: Option<wgpu::BindGroup>,
}

impl Groups {
    pub(super) fn new(device: &wgpu::Device, compiled: &CompiledSpatial, planes: &Planes) -> Self {
        let passes = compiled
            .passes
            .iter()
            .enumerate()
            .map(|(number, pass)| {
                let group = planes.group(
                    device,
                    &pass.layout,
                    &pass.slots,
                    Some((pass.step, pass.output, number)),
                );
                let (width, height) = planes.extent(pass.step, pass.output);
                let dispatch = match pass.shape {
                    PassShape::Texels { span } => [
                        width.div_ceil(span[0]).div_ceil(GROUP_SIDE),
                        height.div_ceil(span[1]).div_ceil(GROUP_SIDE),
                        1,
                    ],
                    PassShape::Workgroup => [1, 1, 1],
                };
                (group, dispatch)
            })
            .collect();
        let fragment = compiled
            .fragment
            .as_ref()
            .map(|(layout, slots)| planes.group(device, layout, slots, None));
        Self { passes, fragment }
    }

    /// Encode the passes `run` marks, in order, group 0 the plan's words, blocks and boundary, and
    /// answer how many.
    pub(super) fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        compiled: &CompiledSpatial,
        programs: &wgpu::BindGroup,
        run: &[bool],
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
        for ((compiled, (group, dispatch)), run) in
            compiled.passes.iter().zip(&self.passes).zip(run)
        {
            if !run {
                continue;
            }
            pass.set_pipeline(&compiled.pipeline);
            pass.set_bind_group(1, group, &[]);
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
/// is kept by texture, so a scratch texture chained steps share holds the key of the step that
/// wrote it last.
#[derive(Default)]
pub(super) struct Schedule {
    /// The key of what each texture holds ([`PlanesKey::texture`]).
    kept: Vec<(usize, u64)>,
}

fn hash_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

impl Schedule {
    /// Forget every plane's content: a new sequence's groups or new planes.
    pub(super) fn reset(&mut self) {
        self.kept.clear();
    }

    fn kept(&self, texture: usize) -> Option<u64> {
        self.kept
            .iter()
            .find(|(at, _)| *at == texture)
            .map(|(_, key)| *key)
    }

    fn keep(&mut self, texture: usize, key: u64) {
        match self.kept.iter_mut().find(|(at, _)| *at == texture) {
            Some((_, kept)) => *kept = key,
            None => self.kept.push((texture, key)),
        }
    }

    /// The passes of `steps` this tick runs, in [`CompiledSpatial`]'s order, given the tick's
    /// packed `words` and `blocks`, the boundary's `version` and the `textures` that hold the
    /// planes; the planes they write are then taken as written.
    pub(super) fn run(
        &mut self,
        steps: &[GpuStep],
        words: &[u32],
        blocks: &[u32],
        version: u64,
        textures: &PlanesKey,
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
                let texture = |plane: u32| textures.texture(index, plane);
                let keys = self.step(&texture, spatial, program, inward, &mut run);
                // A later step's source runs this one's applies over its planes.
                upstream = hash_of((upstream, position, own, own_blocks, keys));
            } else {
                upstream = hash_of((upstream, position, own, own_blocks));
            }
        }
        run
    }

    /// One spatial step's passes, appended to `run`; answers its apply planes' keys.
    fn step(
        &mut self,
        texture: &dyn Fn(u32) -> usize,
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
        // Forward: the key each pass writes, and what each plane holds after the whole tick.
        let mut held: Vec<u64> = vec![0; spatial.planes.len()];
        let holds = |held: &[u64], plane: &u32| held.get(*plane as usize).copied().unwrap_or(0);
        let mut keys = Vec::with_capacity(spatial.passes.len());
        for (number, pass) in spatial.passes.iter().enumerate() {
            // The applies its source runs, by their words and the planes they read.
            let applies: Vec<(&[u32], Vec<u64>)> = spatial
                .applies
                .iter()
                .take(pass.source as usize)
                .map(|apply| {
                    let planes = apply.planes.iter().map(|plane| holds(&held, plane));
                    (slice(apply.words), planes.collect())
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
        let read: Vec<u32> = {
            let mut read: Vec<u32> = spatial
                .applies
                .iter()
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
        let needs = |plane: u32, reader: usize| {
            let mut before = writers(plane).filter(|writer| *writer < reader);
            match before.next_back() {
                None => false,
                Some(writer) => {
                    writers(plane).next() != Some(writer)
                        || self.kept(texture(plane)) != Some(keys[writer])
                }
            }
        };
        // What `plane` holds once the passes `runs` marks have run.
        let after = |plane: u32, runs: &[bool]| {
            writers(plane)
                .rfind(|writer| runs[*writer])
                .map(|writer| keys[writer])
                .or(self.kept(texture(plane)))
        };
        let mut stale: Vec<u32> = read
            .iter()
            .copied()
            .filter(|&plane| writers(plane).next().is_some())
            .filter(|&plane| self.kept(texture(plane)) != Some(holds(&held, &plane)))
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
            self.keep(texture(plane), key);
        }
        run.extend_from_slice(&runs);
        read.iter().map(|plane| holds(&held, plane)).collect()
    }
}
