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
//! run, the spatial program, the declarations above and an entry the surface generates. Its planes
//! are textures: `rgba16float`, `r32float`, `rg32float` or `rgba32float` by the plane's
//! [`PlaneFormat`], each sized to the boundary or to the stage's blocks it reaches
//! ([`GpuPlane::extent`]). The passes run in order before the frame's pass, which binds the
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

/// What a plane's texels hold, as the texture format it is kept in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneFormat {
    /// `rgba16float`: a colour intermediate.
    Colour,
    /// `r32float`: one accumulator.
    Scalar,
    /// `rg32float`: two accumulators.
    Pair,
    /// `rgba32float`: four.
    Quad,
}

impl PlaneFormat {
    pub(super) fn texture(self) -> wgpu::TextureFormat {
        match self {
            Self::Colour => wgpu::TextureFormat::Rgba16Float,
            Self::Scalar => wgpu::TextureFormat::R32Float,
            Self::Pair => wgpu::TextureFormat::Rg32Float,
            Self::Quad => wgpu::TextureFormat::Rgba32Float,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::Colour => "rgba16float",
            Self::Scalar => "r32float",
            Self::Pair => "rg32float",
            Self::Quad => "rgba32float",
        }
    }

    /// The bytes one texel takes.
    pub fn texel_bytes(self) -> u64 {
        match self {
            Self::Scalar => 4,
            Self::Colour | Self::Pair => 8,
            Self::Quad => 16,
        }
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
    words: u32,
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
                    words: pass.words,
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

/// The statements that take `rgb` through spatial step `index`'s clamp and its first `count`
/// applies, reading each apply's planes from the slot `slots` binds them at, at boundary `texel`.
fn applies(index: usize, spatial: &GpuSpatial, count: usize, slots: &Slots) -> String {
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
        text.push_str(&format!(
            "    rgb = {}(rgb, vec2<i32>(texel), lf_words[{base}u] + {}u, lf_words[{}u], {}u);\n",
            apply.function,
            mask_words + apply.words as usize,
            base + 1,
            slots.apply(index, number)
        ));
    }
    text
}

/// The frame pass's statements for spatial step `index`: its clamp, its applies, its mask's blend
/// against the operation's input and its clamp, in a block of their own.
pub(super) fn frame_statements(index: usize, spatial: &GpuSpatial, slots: &Slots) -> String {
    let mut text = String::from("    {\n");
    text.push_str(&applies(index, spatial, spatial.applies.len(), slots));
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

/// The coverage functions of the masked steps of `steps[..until]`, after the one fold they share,
/// and each masked colour step's frame statements, by step: what a module holding those steps
/// declares and runs.
pub(super) fn masks(steps: &[GpuStep], until: usize) -> (String, Vec<Option<String>>) {
    let mut functions = String::new();
    let mut statements = Vec::with_capacity(until);
    for (index, step) in steps.iter().enumerate().take(until) {
        let base = MAP_WORDS + STEP_WORDS * index;
        let (function, statement) = match step {
            GpuStep::Masked(masked) => {
                let (function, statement) = masked.assemble(index, base);
                (Some(function), Some(statement))
            }
            GpuStep::Spatial(spatial) => (spatial.coverage_function(index, base), None),
            GpuStep::Colour { .. } => (None, None),
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
    let format = spatial
        .planes
        .get(pass.output as usize)
        .ok_or("a pass writes a plane its step does not declare")?
        .format;
    let mut slots = Slots::default();
    slots
        .planes
        .extend(pass.inputs.iter().map(|input| (index, *input)));
    slots.bind_applies(steps, index, pass.source as usize);
    let mut source = String::from(super::PRELUDE);
    let mut included: Vec<&GpuProgram> = Vec::new();
    for (_, program) in steps[..=index].iter().flat_map(GpuStep::programs) {
        if !included.iter().any(|seen| seen.entry == program.entry) {
            source.push_str(&format!("\n// {}\n{}\n", program.entry, program.source));
            included.push(program);
        }
    }
    let (functions, masked) = masks(steps, index);
    source.push_str(&functions);
    source.push_str("\n@group(0) @binding(2) var lf_boundary: texture_2d<f32>;\n");
    source.push_str(&declarations(&slots));
    source.push_str(&format!(
        "@group(1) @binding({OUTPUT_BINDING}) var lf_out: texture_storage_2d<{}, write>;\n\
         fn lf_store(at: vec2<i32>, value: vec4<f32>) {{\n    textureStore(lf_out, at, value);\n}}\n",
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
    source.push_str(&applies(index, spatial, pass.source as usize, &slots));
    source.push_str("    return rgb;\n}\n");
    let base = MAP_WORDS + STEP_WORDS * index;
    let call = format!(
        "{}(at, lf_words[{base}u] + {}u, lf_words[{}u]);",
        pass.kernel,
        spatial.mask_words() + pass.words as usize,
        base + 1
    );
    match pass.shape {
        PassShape::Texels { span } => source.push_str(&format!(
            "\n@compute @workgroup_size({GROUP_SIDE}, {GROUP_SIDE})\n\
             fn lf_pass(@builtin(global_invocation_id) id: vec3<u32>) {{\n    \
             let at = vec2<i32>(id.xy) * vec2<i32>({}, {});\n    \
             let size = vec2<i32>(textureDimensions(lf_out));\n    \
             if at.x >= size.x || at.y >= size.y {{\n        return;\n    }}\n    \
             {call}\n}}\n",
            span[0], span[1]
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
/// scratch, and a storage texture beside the planes a pass reads.
pub(super) fn supported(limits: &wgpu::Limits) -> bool {
    limits.max_compute_invocations_per_workgroup >= WORKGROUP_LANES
        && limits.max_compute_workgroup_size_x >= WORKGROUP_LANES
        && limits.max_compute_workgroup_storage_size >= SHARED_VALUES * 4
        && limits.max_storage_textures_per_shader_stage >= 1
        && limits.max_sampled_textures_per_shader_stage >= 16
        && limits.max_bindings_per_bind_group > OUTPUT_BINDING
}

/// Compile every spatial step's passes of `steps`, each module validated as the surface
/// validates, inside the caller's error scopes. One module serves every pass with the same text.
pub(super) fn compile_passes(
    device: &wgpu::Device,
    support: &Support,
    steps: &[GpuStep],
) -> Result<CompiledSpatial, String> {
    let mut compiled = CompiledSpatial::default();
    let mut modules: Vec<(String, wgpu::ComputePipeline, wgpu::BindGroupLayout)> = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let GpuStep::Spatial(spatial) = step else {
            continue;
        };
        for pass in &spatial.passes {
            let (source, slots) = pass_module(steps, index, pass)?;
            let format = spatial.planes[pass.output as usize].format.texture();
            let (pipeline, layout) = match modules.iter().find(|(text, ..)| *text == source) {
                Some((_, pipeline, layout)) => (pipeline.clone(), layout.clone()),
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
    Ok(compiled)
}

/// What a spatial plan's planes are keyed by: each spatial step's planes, the boundary's size and
/// its stage origin.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanesKey {
    planes: Vec<(usize, Vec<GpuPlane>)>,
    size: (u32, u32),
    origin: (u32, u32),
}

impl PlanesKey {
    pub(super) fn of(steps: &[GpuStep], size: (u32, u32), origin: (u32, u32)) -> Option<Self> {
        let planes: Vec<(usize, Vec<GpuPlane>)> = steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| match step {
                GpuStep::Spatial(spatial) => Some((index, spatial.planes.clone())),
                GpuStep::Colour { .. } | GpuStep::Masked(_) => None,
            })
            .collect();
        (!planes.is_empty()).then_some(Self {
            planes,
            size,
            origin,
        })
    }

    /// The bytes every plane takes.
    pub(super) fn bytes(&self) -> u64 {
        self.planes
            .iter()
            .flat_map(|(_, planes)| planes)
            .map(|plane| plane.bytes(self.origin, self.size))
            .sum()
    }
}

/// A plan's planes, created at their extents, with their views.
pub(super) struct Planes {
    pub(super) key: PlanesKey,
    /// By step, then plane.
    textures: Vec<(usize, Vec<(wgpu::Texture, wgpu::TextureView)>)>,
    pub(super) bytes: u64,
}

impl Planes {
    /// Every plane of `key`, created. The caller has charged [`PlanesKey::bytes`].
    pub(super) fn create(device: &wgpu::Device, key: PlanesKey) -> Self {
        let textures = key
            .planes
            .iter()
            .map(|(step, planes)| {
                let made = planes
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
                (*step, made)
            })
            .collect();
        let bytes = key.bytes();
        Self {
            key,
            textures,
            bytes,
        }
    }

    fn view(&self, step: usize, plane: u32) -> &wgpu::TextureView {
        let (_, planes) = self
            .textures
            .iter()
            .find(|(index, _)| *index == step)
            .expect("a spatial step's planes");
        &planes[plane as usize].1
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

    /// A second group binding `slots` and, for a pass, its output.
    fn group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        slots: &Slots,
        output: Option<(usize, u32)>,
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
        if let Some((step, plane)) = output {
            entries.push(wgpu::BindGroupEntry {
                binding: OUTPUT_BINDING,
                resource: wgpu::BindingResource::TextureView(self.view(step, plane)),
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
            .map(|pass| {
                let group = planes.group(
                    device,
                    &pass.layout,
                    &pass.slots,
                    Some((pass.step, pass.output)),
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

    /// Encode every pass in order, group 0 the plan's words, blocks and boundary.
    pub(super) fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        compiled: &CompiledSpatial,
        programs: &wgpu::BindGroup,
    ) {
        if compiled.passes.is_empty() {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("luxforge.gpu_preview.spatial"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, programs, &[]);
        for (compiled, (group, dispatch)) in compiled.passes.iter().zip(&self.passes) {
            pass.set_pipeline(&compiled.pipeline);
            pass.set_bind_group(1, group, &[]);
            pass.dispatch_workgroups(dispatch[0], dispatch[1], dispatch[2]);
        }
    }
}
