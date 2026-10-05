//! The GPU stage: a preview plan evaluated over a held input boundary in the frame that draws it.
//!
//! The plan is plain data — WGSL text, uniform words, storage words and a path-specific boundary —
//! so this crate still names no core type. [`GpuPlan`] is what a caller hands the photograph's
//! surface with [`PhotoSurface::gpu_preview`](super::PhotoSurface::gpu_preview); `prepare` then
//! uploads the boundary when its version changes, writes the tick's words with one
//! `queue.write_buffer`, encodes one pass that runs the programs into the surface's GPU-preview
//! output texture and submits it. `draw` samples that texture exactly as it samples the photograph,
//! through the same placement, snapping and filter; the output is held in the size bucket of the
//! CPU frame it stands in for, the photograph's own or a region picture's, with its edge texels
//! repeated past the frame, so a GPU frame and the CPU frame of the same codes draw identically.
//! The UI thread only encodes commands: it never waits on the GPU, reads a pixel back or touches
//! one on the CPU.
//!
//! # The calling convention
//!
//! Every assembled shader starts with [`PRELUDE`]. It declares two read-only storage bindings and
//! four helpers:
//!
//! - `lf_words: array<u32>` at binding 0: the surface's header, then every program's uniform words,
//!   concatenated in program order. The header is the boundary-texel-to-stage mapping (four `f32`
//!   words: origin x and y, step x and y) followed, for each step, by its program's two base
//!   indices and its position map's six coefficients as `f32` words (`a, b, tx, c, d, ty`).
//! - `lf_blocks: array<u32>` at binding 1: every program's storage block, concatenated in program
//!   order; at least one word even when every block is empty.
//! - `lf_word(i)`, `lf_f32(i)`: word `i` of `lf_words`, raw or bit-cast to `f32`.
//! - `lf_block_word(i)`, `lf_block_f32(i)`: the same over `lf_blocks`.
//!
//! A program's WGSL declares only functions and constants: no bindings, global variables,
//! overrides, named types or entry points. Every name it declares starts with its entry function's
//! name, so two programs never collide, and the entry is none of the surface's own names (the
//! prelude's, `lf_boundary`, `lf_vertex` and `lf_fragment`); the core names its entries
//! `lf_<module>_<unit>`. A program is called with `words` and `block`, its base indices into
//! `lf_words` and `lf_blocks`, so its first uniform word is `lf_f32(words)`. Its entry function's
//! signature depends on the step that holds it:
//!
//! - **Pointwise colour** ([`GpuStep::Colour`]):
//!   `fn <entry>(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>`. `rgb` is
//!   scene-linear sRGB, unclamped. `pos` is the pixel's integer coordinate, as `f32`, in the stage
//!   the CPU unit's `apply_row(y, x0, ..)` addresses: the plan's [`TexelMap`] takes a boundary texel
//!   to the pixel of the stage the boundary holds, and the step's own [`PositionMap`] takes that
//!   pixel to `pos`. Every colour step runs in that one pass today; a step after a geometry step,
//!   when there is one, will run in output space, its map taking the output pixel to `pos`.
//! - **A spatial operation** ([`GpuStep::Spatial`]): compute passes over the boundary that fill
//!   planes, then one apply per unit in the frame's pass, under the spatial convention
//!   ([`spatial`]). Its planes are charged to the budget with the slot.
//!
//! The surface generates the entry points: a vertex stage that covers the output with one triangle,
//! and a fragment stage that loads the boundary texel, chains each program's entry in step order,
//! passing its base indices from the header, and writes the result into an sRGB-typed target, whose
//! hardware encoding is the output encoding. Two programs with one entry name must carry one source,
//! which is then included once: two layers of the same unit share their functions.
//!
//! # Pipelines, bounds and fallback
//!
//! One render pipeline is kept per program sequence (each step's kind, entry and source), at most
//! [`PIPELINE_CACHE`] of them, the least recently used evicted first; a sequence that failed is kept
//! as failed, so it is not compiled again every frame. The words and the boundary's contents are not
//! part of the key, so a tick never compiles. Pipelines compile on a thread of their own, never the
//! UI thread ([`compile`](mod@compile)): a frame whose sequence is first seen or still compiling draws
//! the CPU frame and names why, and the desktop warms the sequences a gesture is likely to need.
//!
//! Every GPU-preview texture and buffer is charged to one budget, [`GPU_PREVIEW_BUDGET`] by
//! default, shared by every surface of the pipeline and reported beside the photo-texture figures:
//! the boundary, the output texture and its placement uniform, and the words and blocks buffers. A
//! surface holds at most one GPU-preview slot. An allocation that would pass the budget is refused
//! before anything is created. A replaced or released slot, or an outgrown buffer, retires through
//! the surface's retirement worker and stays charged until the GPU is done with it, so the in-use
//! figure returns to zero once a released slot has retired.
//!
//! When the GPU stage cannot draw a frame, the surface draws the CPU frame it was given — the
//! surface always holds one — and names why ([`GpuFallback`]). A device that cannot run the stage,
//! a lost device, a failed pipeline, an exceeded budget, or a boundary, words or blocks past what
//! the device allows a texture or a storage binding to be each make that frame and every later one
//! take the CPU path until the cause goes; nothing the stage creates can then be an error wgpu's
//! default handler would panic on. A lost device is noticed through its callback, an atomic flag,
//! so nothing waits for a recovery.
//!
//! A boundary is uploaded from the CPU's texels, or derived on the GPU from the prepared source the
//! pipeline holds for every surface ([`GpuSource`], [`GpuBoundary::derived`]): a window of it cut
//! at full scale, or its area average at a proxy plan, by one fixed pass each ([`source`]).
//!
//! Shader compilation is checked without waiting: the assembled WGSL is validated with the `naga`
//! that `wgpu` itself uses before a module is created, and pipeline creation runs inside error
//! scopes whose answers wgpu's native backends give immediately; they are polled once and never
//! awaited. A spatial pass's module is handed over as the validated `naga` module, compacted to its
//! entry point, so the driver compiles only what the pass runs ([`spatial`]). All of it runs on the
//! compile thread.

use super::{
    PhotoPipeline, Picture, SurfaceFigures, SurfaceSlots, Tile, TileLayout, UNIFORM_SIZE,
    wake_surface,
};
use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use wgpu::naga;

/// The GPU-preview budget: every GPU-preview texture and buffer, resident or retiring, of every
/// surface together. A plan runs as a chain of links, each spatial operation's output kept by
/// content in an intermediate the boundary's size beside its kept planes, its scratch planes in the
/// slot's one pool, so a full-screen Fit slot of a RAW's `f32` boundary with Detail and three
/// masked Presence layers holds 0.56 GB, a 100% region of three masked Presence layers 0.54 GB,
/// and a Fit slot of sixteen 0.82 GB; 2 GiB holds them and a slot overlapping the one it replaces
/// (owner, 2026-10-03: interactive speed comes before memory). A 100% window grows with every
/// chained spatial layer, so a region of more than eight masked Presence layers in the paint
/// harness's view passes it.
pub const GPU_PREVIEW_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// The words before any step's: the texel map's origin and step, then the offset of the output's
/// first pixel ([`GpuRegion`]): in boundary texels for the content pass of a plan with no tail, in
/// output-stage pixels for a tail's pass; zero for a whole-frame plan.
const MAP_WORDS: usize = 6;

/// Each step's header words: its program's two base indices and its position map.
const STEP_WORDS: usize = 2 + PositionMap::WORDS;

/// A words or blocks buffer is never smaller than this, so a plan's first few ticks do not each
/// outgrow the last one's buffer.
const MIN_BUFFER: u64 = 1024;

/// The upload chunk, as the photograph's: the surface stages no copy of its own.
const UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;

/// The most of a new boundary one frame's `prepare` uploads: four chunks, 32 MiB. Each frame's
/// chunks are copied into wgpu's staging and submitted with that frame, and the staging is freed
/// once the GPU has copied them, so at most a frame's chunks wait in staging while the next
/// frame's are written: 64 MiB at the most, where a boundary written in one frame stages all of
/// it at once. A boundary of the 256 MiB the bound allows arrives over eight frames, a Fit
/// boundary of a JPEG's 8 MP (64 MiB) over two, and one of 32 MiB or less in its first frame, as
/// before. The frame's copy stays a few chunks of `memcpy` on the interface thread, which never
/// waits for the GPU.
pub const UPLOAD_PER_FRAME: u64 = 4 * UPLOAD_CHUNK;

/// The words the blocks are compared and written in, 1 KiB: a tick writes the chunks that changed.
const BLOCK_CHUNK: usize = 256;

/// The format the last pass writes the output's 8-bit codes through, which the shader computes as
/// the CPU's quantizer does ([`tail`]).
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the draw samples the output through: the sRGB-typed format the surface's photograph
/// textures have when the renderer gamma corrects, so the codes draw as the CPU frame's do.
const SAMPLED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The WGSL every assembled GPU-preview shader starts with; see the [module documentation](self).
/// A program's own WGSL, appended to this, must validate on its own.
pub const PRELUDE: &str = "\
// Luxforge GPU-preview prelude: the bindings and helpers every program may use.
@group(0) @binding(0) var<storage, read> lf_words: array<u32>;
@group(0) @binding(1) var<storage, read> lf_blocks: array<u32>;

fn lf_word(i: u32) -> u32 {
    return lf_words[i];
}

fn lf_f32(i: u32) -> f32 {
    return bitcast<f32>(lf_words[i]);
}

fn lf_block_word(i: u32) -> u32 {
    return lf_blocks[i];
}

fn lf_block_f32(i: u32) -> f32 {
    return bitcast<f32>(lf_blocks[i]);
}
";

/// The surface's own binding and entry points, after the prelude and the programs.
const BOUNDARY_BINDING: &str = "
@group(0) @binding(2) var lf_boundary: texture_2d<f32>;

@vertex
fn lf_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole target: (-1, 1), (3, 1), (-1, -3).
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
}
";

/// One GPU program: WGSL functions and constants, the entry function the surface calls, and what
/// it reads. Plain data, so a caller converts its own description into it.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuProgram {
    /// The entry function's name. Every name the source declares starts with it.
    pub entry: Cow<'static, str>,
    /// Functions and constants only, without bindings or entry points.
    pub source: Cow<'static, str>,
    /// The program's uniform words, read through `lf_word(words + i)` or `lf_f32(words + i)`.
    pub words: Vec<u32>,
    /// The program's storage block, read through `lf_block_word(block + i)` or
    /// `lf_block_f32(block + i)`: data too large or too rarely changed for the words, such as a
    /// curve's knots or a brush's segment grid. Shared, so handing it over each tick copies
    /// nothing; the surface writes the blocks only when their contents change.
    pub block: Arc<[u32]>,
}

impl GpuProgram {
    /// A program with no words and an empty block.
    pub fn new(entry: impl Into<Cow<'static, str>>, source: impl Into<Cow<'static, str>>) -> Self {
        Self {
            entry: entry.into(),
            source: source.into(),
            words: Vec::new(),
            block: Arc::from([]),
        }
    }
}

/// One step of a plan, in the order the surface runs them. Each kind of step is one variant, with
/// what that kind needs; a kind the surface does not run yet has no variant.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuStep {
    /// A pointwise colour program,
    /// `fn <entry>(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>`, handed
    /// the `pos` its position map gives the pixel of the pass it runs in.
    Colour {
        program: GpuProgram,
        position: PositionMap,
    },
    /// A pointwise colour operation blended by its mask's coverage against its own input
    /// ([`MaskedColour`]): its units are colour programs, its components coverage programs,
    /// `fn <entry>(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32`.
    Masked(MaskedColour),
    /// The geometry tail ([`GpuTail`]): the content steps before it write the boundary's window,
    /// and it draws the output stage from them; the steps after it run at the output pixel.
    Geometry(GpuTail),
    /// A spatial operation: its passes before the frame's pass, its applies in it, over the
    /// boundary's texels.
    Spatial(Box<GpuSpatial>),
    /// The clipping overlay's marks over the output ([`ClipMarks`]): a plan's last step, run in
    /// its last pass after every other.
    Clipping(ClipMarks),
}

impl GpuStep {
    /// A colour step whose `pos` is the stage pixel the plan's texel map gives a boundary texel.
    pub fn colour(program: GpuProgram) -> Self {
        Self::Colour {
            program,
            position: PositionMap::IDENTITY,
        }
    }

    /// Every program the step runs, with the signature its role gives it.
    fn programs(&self) -> Box<dyn Iterator<Item = (mask::Role, &GpuProgram)> + '_> {
        match self {
            Self::Colour { program, .. } => {
                Box::new(std::iter::once((mask::Role::Colour, program)))
            }
            Self::Masked(masked) => Box::new(masked.programs()),
            // The tail's mapping and the marks are the surface's own text, never a module's program.
            Self::Geometry(_) | Self::Clipping(_) => Box::new(std::iter::empty()),
            Self::Spatial(spatial) => Box::new(spatial.programs()),
        }
    }

    /// What decides the step's pipeline: the shape of a masked step, or a spatial step's planes,
    /// passes and applies, then each program's role, entry and source in order. Its words are
    /// data and are not part of it.
    fn signature(&self) -> impl Iterator<Item = (StepKind, &str, &str)> {
        let shape: Box<dyn Iterator<Item = (StepKind, &str, &str)> + '_> = match self {
            Self::Colour { .. } => Box::new(std::iter::empty()),
            Self::Geometry(tail) => Box::new(std::iter::once((
                StepKind::Geometry {
                    quantize: tail.quantizes(),
                    preserve_f32: tail.preserves_f32(),
                },
                tail.program().entry.as_ref(),
                tail.program().source.as_ref(),
            ))),
            Self::Masked(masked) => Box::new(std::iter::once((
                StepKind::Masked {
                    units: masked.units.len(),
                    components: masked.mask.components.len(),
                },
                "",
                "",
            ))),
            Self::Spatial(spatial) => Box::new(spatial.shape()),
            Self::Clipping(_) => Box::new(std::iter::once((StepKind::Clipping, "", ""))),
        };
        shape.chain(self.programs().map(|(role, program)| {
            (
                StepKind::Program(role),
                program.entry.as_ref(),
                program.source.as_ref(),
            )
        }))
    }

    fn position(&self) -> PositionMap {
        match self {
            Self::Colour { position, .. } => *position,
            Self::Masked(masked) => masked.position,
            Self::Geometry(_) => PositionMap::IDENTITY,
            Self::Spatial(_) => PositionMap::IDENTITY,
            Self::Clipping(_) => PositionMap::IDENTITY,
        }
    }

    /// The words the step packs after the header.
    fn word_count(&self) -> usize {
        match self {
            Self::Colour { program, .. } => program.words.len(),
            Self::Masked(masked) => masked.word_count(),
            Self::Geometry(tail) => tail.program().words.len(),
            Self::Spatial(spatial) => spatial.word_count(),
            Self::Clipping(_) => ClipMarks::WORDS,
        }
    }

    /// The block words the step packs.
    fn block_count(&self) -> usize {
        match self {
            Self::Colour { program, .. } => program.block.len(),
            Self::Masked(masked) => masked.block_count(),
            Self::Geometry(tail) => tail.program().block.len(),
            Self::Spatial(spatial) => spatial.block_count(),
            Self::Clipping(_) => 0,
        }
    }

    /// Each storage block the step packs, shared, in packing order: a colour step's or the tail's
    /// program's, a masked step's components' then its units', a spatial step's program's then its
    /// mask's components'. The marks hold none.
    fn each_block<'a>(&'a self, visit: &mut impl FnMut(&'a Arc<[u32]>)) {
        match self {
            Self::Colour { program, .. } => visit(&program.block),
            Self::Masked(masked) => {
                for (_, program) in masked.programs() {
                    visit(&program.block);
                }
            }
            Self::Geometry(tail) => visit(&tail.program().block),
            Self::Spatial(spatial) => {
                for (_, program) in spatial.programs() {
                    visit(&program.block);
                }
            }
            Self::Clipping(_) => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepKind {
    Program(mask::Role),
    Masked {
        units: usize,
        components: usize,
    },
    /// A spatial step's clamp and the words its mask takes before its program's.
    Spatial {
        clamps: bool,
        mask_words: Option<usize>,
    },
    Plane(spatial::GpuPlane),
    Pass(spatial::PassKey),
    Apply(spatial::ApplyKey),
    Geometry {
        quantize: bool,
        preserve_f32: bool,
    },
    Clipping,
}

/// How the held boundary's texels are stored: four little-endian half floats (`rgba16float`), as a
/// JPEG's byte path holds them, or four little-endian `f32` (`rgba32float`), as a developed RAW's
/// linear path does, where half rounding of a near-black value can flip the sign a spatial
/// operation divides by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryFormat {
    Half,
    Float,
}

impl BoundaryFormat {
    /// Bytes per texel.
    pub const fn texel_bytes(self) -> usize {
        match self {
            Self::Half => 8,
            Self::Float => 16,
        }
    }

    /// The texture the boundary is uploaded to.
    pub(super) fn texture(self) -> wgpu::TextureFormat {
        match self {
            Self::Half => wgpu::TextureFormat::Rgba16Float,
            Self::Float => wgpu::TextureFormat::Rgba32Float,
        }
    }
}

/// The held input boundary: texels of scene-linear sRGB in `format`, rows top to bottom. The
/// version decides whether it is uploaded again, as a [`Frame`](super::Frame)'s does: it changes
/// whenever the texels do and never otherwise.
#[derive(Clone)]
pub struct GpuBoundary {
    /// The texels, until the caller lets them go once the slot holds them ([`Self::resident`]).
    texels: Option<Arc<dyn AsRef<[u8]> + Send + Sync>>,
    /// Or, for a boundary derived on the GPU from the source the pipeline holds ([`Self::derived`]),
    /// that source's version and the derivation: no texels on the CPU at all.
    derived: Option<(u64, Derivation)>,
    width: u32,
    height: u32,
    version: u64,
    format: BoundaryFormat,
}

impl std::fmt::Debug for GpuBoundary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuBoundary")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("version", &self.version)
            .field("format", &self.format)
            .field("derived", &self.derived.as_ref().map(|(source, _)| source))
            .finish()
    }
}

impl GpuBoundary {
    /// A boundary of `width` × `height` texels of `format` held in `texels` as they are, or `None`
    /// when the buffer does not hold exactly that many. Nothing is copied.
    pub fn new<P: AsRef<[u8]> + Send + Sync + 'static>(
        texels: Arc<P>,
        width: u32,
        height: u32,
        version: u64,
        format: BoundaryFormat,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(format.texel_bytes())?;
        (width > 0 && height > 0 && (*texels).as_ref().len() == expected).then(|| Self {
            texels: Some(texels),
            derived: None,
            width,
            height,
            version,
            format,
        })
    }

    /// A boundary of `width` × `height` texels derived on the GPU from `source`, which the
    /// pipeline must hold when a slot first draws it ([`PhotoSurface::gpu_source`]), as
    /// `derivation` says: a window of it at full scale, or its area average at a proxy plan
    /// (`docs/design/gpu-preview.md`, "The GPU source"). Its format is the source's: half floats
    /// from a JPEG's codes, `f32` from a RAW's planes. Nothing is copied or uploaded: the slot's
    /// boundary texture is written by one pass.
    ///
    /// [`PhotoSurface::gpu_source`]: super::PhotoSurface::gpu_source
    pub fn derived(
        source: &GpuSource,
        derivation: Derivation,
        width: u32,
        height: u32,
        version: u64,
    ) -> Option<Self> {
        (width > 0 && height > 0).then(|| Self {
            texels: None,
            derived: Some((source.version(), derivation)),
            width,
            height,
            version,
            format: source.kind().boundary(),
        })
    }

    /// The source version and the derivation a derived boundary is drawn from, or `None` for a
    /// boundary of texels.
    pub fn derivation(&self) -> Option<&(u64, Derivation)> {
        self.derived.as_ref()
    }

    /// A boundary of `width` × `height` texels of `format` from linear RGBA values in row order,
    /// each rounded to the nearest half float or held as the `f32` it is, or `None` when `pixels`
    /// does not yield exactly that many. This is frame work: a caller runs it on a worker, never
    /// the UI thread.
    pub fn from_linear(
        format: BoundaryFormat,
        width: u32,
        height: u32,
        version: u64,
        pixels: impl IntoIterator<Item = [f32; 4]>,
    ) -> Option<Self> {
        let count = (width as usize).checked_mul(height as usize)?;
        let bytes = format.texel_bytes();
        // Zeroed, so its pages are faulted in by the pass that writes them.
        let mut texels = vec![0u8; count.checked_mul(bytes)?];
        let mut written = 0;
        for pixel in pixels {
            let texel = texels.get_mut(written * bytes..(written + 1) * bytes)?;
            match format {
                BoundaryFormat::Half => {
                    for (bytes, value) in texel.chunks_exact_mut(2).zip(pixel) {
                        bytes.copy_from_slice(&half::f16::from_f32(value).to_bits().to_le_bytes());
                    }
                }
                BoundaryFormat::Float => {
                    for (bytes, value) in texel.chunks_exact_mut(4).zip(pixel) {
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                }
            }
            written += 1;
        }
        (written == count).then_some(())?;
        Self::new(Arc::new(texels), width, height, version, format)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn format(&self) -> BoundaryFormat {
        self.format
    }

    /// The bytes the boundary's texels take, as uploaded.
    pub fn bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * self.format.texel_bytes() as u64
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// This boundary with its texels let go: what a plan names once the surface's slot holds them
    /// (its `gpu_ready_boundary` is this version), so the caller keeps no copy of them for the
    /// rest of the gesture. The slot draws it from its own texture; a slot that no longer holds
    /// it — released, or refitted to another shape — cannot, and the frame is the CPU's with
    /// [`GpuFallback::BoundaryReleased`], for the caller to bring the texels again.
    pub fn resident(&self) -> Self {
        Self {
            texels: None,
            ..self.clone()
        }
    }

    /// Whether the boundary still holds its texels, which a slot that does not hold them uploads.
    pub fn holds_texels(&self) -> bool {
        self.texels.is_some()
    }
}

/// Where a boundary texel is in the stage the programs address: texel `(x, y)` is at
/// `origin + (x, y) * step`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexelMap {
    pub origin: [f32; 2],
    pub step: [f32; 2],
}

impl TexelMap {
    /// The boundary is the stage itself.
    pub const IDENTITY: Self = Self {
        origin: [0.0, 0.0],
        step: [1.0, 1.0],
    };
}

/// What the surface evaluates on the GPU: the held boundary, where its texels are in the stage,
/// and the steps after it, in order.
#[derive(Clone, Debug)]
pub struct GpuPlan {
    pub boundary: GpuBoundary,
    pub texels: TexelMap,
    pub steps: Vec<GpuStep>,
    /// At a percentage zoom of 100% or more, the rectangle of the output stage the plan's frame
    /// holds, drawn as a region of the photograph; `None` for a whole frame, at Fit and below 100%.
    pub region: Option<GpuRegion>,
}

/// The rectangle of a plan's output stage its frame holds at a percentage zoom of 100% or more:
/// the visible region at full scale. The last pass draws only these pixels, and the draw places them at the rectangle
/// in the whole stage, as a region of the photograph is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuRegion {
    /// `[x0, y0, x1, y1]` of the output stage, half-open.
    pub rect: [u32; 4],
    /// The whole output stage.
    pub stage: (u32, u32),
}

impl GpuRegion {
    /// The frame's size: the rectangle's.
    pub fn size(&self) -> (u32, u32) {
        (
            self.rect[2].saturating_sub(self.rect[0]),
            self.rect[3].saturating_sub(self.rect[1]),
        )
    }

    fn valid(&self) -> bool {
        self.rect[0] < self.rect[2]
            && self.rect[1] < self.rect[3]
            && self.rect[2] <= self.stage.0
            && self.rect[3] <= self.stage.1
    }
}

/// A plan's serial, which its caller gives every plan it hands over, and where it draws other
/// values than an earlier one: `since` names that plan's serial and the rectangle `[x0, y0, x1,
/// y1)` of the plan's boundary stage the changes lie inside, before any spatial step's
/// neighbourhood grows them — a painted tick's new segments; `None` for a change anywhere. When the
/// slot last evaluated that plan it evaluates only that part of this one again: each link of the
/// chain over the rectangle its input's changes reach, and the rest of what it holds kept. An empty
/// rectangle is no change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuChange {
    pub serial: u64,
    pub since: Option<(u64, [u32; 4])>,
}

/// Which path drew a surface's photograph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawingPath {
    /// The GPU stage's output over a held boundary.
    Gpu,
    /// The CPU frame the surface was given.
    Cpu,
}

impl DrawingPath {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// Why a frame that asked for the GPU stage was drawn from the CPU frame instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFallback {
    /// No adapter that can run the stage: the renderer's device lacks read-only storage buffers in
    /// the fragment stage, or draws to a target that is not sRGB, which the stage does not encode
    /// for.
    NoAdapter,
    /// The device was lost. The stage stays off for that device; nothing waits for a recovery.
    DeviceLost,
    /// The assembled shader or its pipeline failed: a program's WGSL did not validate, an entry
    /// name was refused, or wgpu refused the pipeline.
    PipelineFailed,
    /// The plan's program sequence is first seen, or still compiling on the compile thread: the
    /// UI thread never compiles one.
    Compiling,
    /// Allocating what the plan needs would pass the GPU-preview budget.
    BudgetExceeded {
        requested: u64,
        in_use: u64,
        budget: u64,
    },
    /// The boundary is larger than the device allows a texture to be.
    TextureLimit { width: u32, height: u32, limit: u32 },
    /// The words or the blocks are larger than the device allows a storage binding to be.
    BufferLimit { bytes: u64, limit: u64 },
    /// The plan's boundary is still being uploaded, at most [`UPLOAD_PER_FRAME`] a frame: `uploaded`
    /// of its `bytes` so far. The slot is kept, and the frame that writes its last chunk draws it.
    BoundaryUploading { uploaded: u64, bytes: u64 },
    /// The plan names a boundary whose texels its caller let go ([`GpuBoundary::resident`]), and
    /// the slot no longer holds them.
    BoundaryReleased,
    /// The plan's boundary is derived from a source the pipeline is still uploading, at most
    /// [`UPLOAD_PER_FRAME`] a frame: `uploaded` of its `bytes` so far. The frame that writes its
    /// last rows draws the plan.
    SourceUploading { uploaded: u64, bytes: u64 },
    /// The plan's boundary is derived from a source the pipeline does not hold: none handed to a
    /// surface this frame, another version, or one whose pixels its caller let go before it was
    /// uploaded.
    SourceMissing,
}

impl GpuFallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoAdapter => "no-adapter",
            Self::DeviceLost => "device-lost",
            Self::PipelineFailed => "pipeline-failed",
            Self::Compiling => "compiling",
            Self::BudgetExceeded { .. } => "budget-exceeded",
            Self::TextureLimit { .. } => "texture-limit",
            Self::BufferLimit { .. } => "buffer-limit",
            Self::BoundaryReleased => "boundary-released",
            Self::BoundaryUploading { .. } => "boundary-uploading",
            Self::SourceUploading { .. } => "source-uploading",
            Self::SourceMissing => "source-missing",
        }
    }
}

/// The compile thread's figures, which it counts as each compile ends, whether or not a frame is
/// drawn: compiles finished, and the longest and the last one's wall-clock time, in microseconds;
/// and what waits for it.
#[derive(Default)]
pub(super) struct CompileFigures {
    compiled: AtomicU64,
    max_us: AtomicU64,
    last_us: AtomicU64,
    /// Sequences queued or compiling: each queued adds one, and each that finishes, or that a
    /// frame's sequence takes the place of in a full queue, takes one away.
    pending: AtomicU64,
    /// The newest warm list's version the pipeline has queued, plus one; zero before any.
    warmed: AtomicU64,
}

impl CompileFigures {
    /// One compile finished after `elapsed`.
    fn finished(&self, elapsed: std::time::Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.compiled.fetch_add(1, Ordering::AcqRel);
        self.max_us.fetch_max(micros, Ordering::AcqRel);
        self.last_us.store(micros, Ordering::Release);
        self.leave(1);
    }

    /// `count` sequences queued.
    fn queued(&self, count: u64) {
        self.pending.fetch_add(count, Ordering::AcqRel);
    }

    /// `count` sequences no longer wait: finished, or dropped from the queue unqueued.
    fn leave(&self, count: u64) {
        let _ = self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                Some(pending.saturating_sub(count))
            });
    }
}

/// What one pipeline counts of its GPU-preview work, beside its photo-texture figures.
#[derive(Default)]
pub(super) struct Figures {
    /// What the stage's capability check answered ([`GpuStageState`]), and the device's lost flag,
    /// which the stage's lost callback sets: both read live.
    stage: stage::StageFigure,
    lost: Arc<AtomicBool>,
    budget: AtomicU64,
    in_use: AtomicU64,
    peak: AtomicU64,
    /// Of `in_use`, every slot's pool of scratch textures, resident or retiring.
    scratch: AtomicU64,
    passes: AtomicU64,
    /// Compute passes the spatial steps have dispatched.
    spatial_passes: AtomicU64,
    /// Sequences handed to the compile thread.
    compiles: AtomicU64,
    /// What the compile thread counts as each compile ends.
    compile: Arc<CompileFigures>,
    /// Words written to the blocks buffers, for the tests of what a tick writes.
    block_words: AtomicU64,
    /// Tests only: words of the tick's blocks compared with what a buffer held, and copied into
    /// what it holds ([`blocks::WrittenBlocks::update`]).
    #[cfg(test)]
    block_compared: AtomicU64,
    #[cfg(test)]
    block_copied: AtomicU64,
    /// Bytes of boundary texels written into wgpu's staging, over every frame, a source's rows
    /// among them.
    staged: AtomicU64,
    /// Boundaries derived on the GPU from the source the pipeline holds ([`source`]).
    derived: AtomicU64,
    /// The most of a new boundary one frame uploads: [`UPLOAD_PER_FRAME`], or a test's.
    upload_per_frame: AtomicU64,
    /// Tests only: each link's passes start from a sentinel in every pool texture
    /// ([`spatial::Pool::poison`]).
    #[cfg(test)]
    poison: AtomicBool,
}

impl Figures {
    /// Whether the stage can draw at all on this pipeline's device, read live.
    pub(super) fn stage_state(&self) -> GpuStageState {
        self.stage.state(&self.lost)
    }

    pub(super) fn budget(&self) -> u64 {
        self.budget.load(Ordering::Acquire)
    }

    pub(super) fn in_use(&self) -> u64 {
        self.in_use.load(Ordering::Acquire)
    }

    pub(super) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
    }

    pub(super) fn scratch(&self) -> u64 {
        self.scratch.load(Ordering::Acquire)
    }

    pub(super) fn passes(&self) -> u64 {
        self.passes.load(Ordering::Acquire)
    }

    pub(super) fn spatial_passes(&self) -> u64 {
        self.spatial_passes.load(Ordering::Acquire)
    }

    pub(super) fn staged(&self) -> u64 {
        self.staged.load(Ordering::Acquire)
    }

    pub(super) fn derived(&self) -> u64 {
        self.derived.load(Ordering::Acquire)
    }

    pub(super) fn compiles(&self) -> u64 {
        self.compiles.load(Ordering::Acquire)
    }

    pub(super) fn compile_us(&self) -> (u64, u64, u64) {
        let compile = &self.compile;
        (
            compile.compiled.load(Ordering::Acquire),
            compile.max_us.load(Ordering::Acquire),
            compile.last_us.load(Ordering::Acquire),
        )
    }

    /// Sequences queued or compiling, and the newest warm list's version queued.
    pub(super) fn compile_pending(&self) -> (u64, Option<u64>) {
        let compile = &self.compile;
        (
            compile.pending.load(Ordering::Acquire),
            compile.warmed.load(Ordering::Acquire).checked_sub(1),
        )
    }

    /// Charge `bytes` if they fit the budget beside everything charged already.
    fn charge(&self, bytes: u64) -> Result<(), GpuFallback> {
        let budget = self.budget();
        let mut in_use = self.in_use();
        loop {
            if in_use.saturating_add(bytes) > budget {
                return Err(GpuFallback::BudgetExceeded {
                    requested: bytes,
                    in_use,
                    budget,
                });
            }
            match self.in_use.compare_exchange_weak(
                in_use,
                in_use + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.peak.fetch_max(in_use + bytes, Ordering::AcqRel);
                    return Ok(());
                }
                Err(now) => in_use = now,
            }
        }
    }

    /// Count what one buffer's blocks update wrote, and in tests what it compared and copied.
    fn blocks_updated(&self, update: &blocks::Update) {
        self.block_words
            .fetch_add(update.written(), Ordering::Relaxed);
        #[cfg(test)]
        {
            self.block_compared
                .fetch_add(update.compared, Ordering::Relaxed);
            self.block_copied
                .fetch_add(update.copied, Ordering::Relaxed);
        }
    }

    fn discharge(&self, bytes: u64) {
        self.in_use.fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// A buffer and the bytes charged for it.
struct Charged {
    buffer: wgpu::Buffer,
    bytes: u64,
}

/// What retires through the surface's retirement worker: a whole slot, a buffer a slot outgrew, a
/// link's planes or a link, or textures its pool no longer holds. Never read: it is held until the
/// GPU is done with it, then dropped.
#[allow(dead_code)]
enum Held {
    Slot(Box<GpuSlot>),
    Buffer(wgpu::Buffer),
    Planes(Box<SpatialSlot>),
    Link(Box<chain::LinkSlot>),
    Pool(Vec<spatial::PoolTexture>),
    Source(Box<SourceSlot>),
}

/// A GPU-preview resource on its way out, with its charge, which ends when the GPU is done with it,
/// and how much of it is scratch textures.
pub(super) struct RetiredPreview {
    held: Held,
    bytes: u64,
    scratch: u64,
}

/// Ends one retirement: the resources go and their charge with them.
pub(super) fn finish_retirement(figures: &SurfaceFigures, retired: RetiredPreview, failed: bool) {
    let RetiredPreview {
        held,
        bytes,
        scratch,
    } = retired;
    drop(held);
    figures.preview.discharge(bytes);
    figures.preview.scratch.fetch_sub(scratch, Ordering::AcqRel);
    figures.retirement_pending.fetch_sub(1, Ordering::AcqRel);
    if failed {
        figures.diagnostics().gpu_retirement_failures += 1;
    }
    wake_surface();
}

/// One surface's GPU-preview slot: the boundary, the output the programs write and the photograph's
/// draw samples, and the words and blocks the programs read.
pub(super) struct GpuSlot {
    /// The boundary's size, the output's and the intermediate's format, which a plan's slot is
    /// allocated for.
    shape: Shape,
    boundary: wgpu::Texture,
    /// A geometry tail's intermediate: the content pass's result, which the tail reads.
    intermediate: Option<Intermediate>,
    /// The boundary the texture holds whole.
    boundary_version: Option<u64>,
    /// A boundary being uploaded, a frame's chunks at a time: its version and the rows written.
    uploading: Option<(u64, u32)>,
    output: Picture,
    target: wgpu::TextureView,
    words: Charged,
    blocks: Charged,
    /// The words a derived boundary's pass reads: the source's map and the origin, and a
    /// reduction's coverage tables ([`source`]). Created with the first derived boundary.
    derivation: Option<Charged>,
    bindings: wgpu::BindGroup,
    /// The boundary texture, the output texture and its placement uniform.
    texture_bytes: u64,
    /// The words last written, compared with each tick's so an unchanged plan writes nothing.
    written_words: Vec<u32>,
    /// The blocks last written, by the shared block each came from: a tick compares and writes only
    /// the blocks it does not hold at the same place ([`blocks`]).
    written_blocks: blocks::WrittenBlocks,
    /// The cached pipeline last run into `output`.
    evaluated: Option<u64>,
    /// The interface thread's time, in microseconds, to prepare what `output` holds: fitting the
    /// slot, writing the words and blocks, uploading a new boundary, encoding and submitting the
    /// pass.
    frame_us: u64,
    /// How many passes the slot has submitted, which names each to its clock.
    passes: u64,
    /// When the GPU finished the slot's passes, as the queue reports it ([`timing`]).
    clock: Arc<PassClock>,
    /// A spatial plan's planes and the groups that bind them, for the pipeline they were made for.
    spatial: Option<Box<SpatialSlot>>,
    /// The links of the plan's chain before its last, each with the intermediate it writes
    /// ([`chain`]); the last link reads the last of them as its boundary.
    chain: Vec<chain::LinkSlot>,
    /// The scratch textures every link's spatial step takes its scratch planes from in turn, with
    /// the records of who wrote each and the counter that hands each link's schedule its holder
    /// ([`spatial::Pool`]): held for the slot's life and refitted to each plan.
    pool: spatial::Pool,
    /// The content key of what the last link's input held when the output was last evaluated.
    input_key: Option<u64>,
    /// The serial of the plan whose values every link and the output hold, when the caller gave
    /// one, and the version of the boundary they were evaluated over: what a later plan's change is
    /// measured from over the same boundary ([`GpuChange::since`]).
    evaluated_serial: Option<(u64, u64)>,
}

/// A link's spatial planes and, for one compiled sequence and one generation of the slot's pool,
/// the groups that bind them, with which passes a tick still needs to run and over which rectangle
/// of the boundary the planes hold it.
pub(super) struct SpatialSlot {
    planes: spatial::Planes,
    /// The groups, with the pipeline they were made for and the pool generation they bind.
    groups: Option<(u64, u64, spatial::Groups)>,
    schedule: spatial::Schedule,
    /// How many passes the slot has dispatched.
    dispatched: u64,
    /// The rectangle of the boundary over which the planes hold what the schedule says they do:
    /// a masked step's passes run only over its mask's bounds grown by its halo, so outside it they
    /// hold an earlier tick's values. `None` before anything is written.
    valid: Option<spatial::Rect>,
}

impl SpatialSlot {
    /// New planes, which hold nothing yet, their schedule's holder drawn from `pool`.
    fn new(planes: spatial::Planes, pool: &mut spatial::Pool) -> Self {
        Self {
            planes,
            groups: None,
            schedule: spatial::Schedule::new(pool),
            dispatched: 0,
            valid: None,
        }
    }

    /// The groups that bind the planes, when they have been built.
    fn groups(&self) -> Option<&spatial::Groups> {
        self.groups.as_ref().map(|(_, _, groups)| groups)
    }

    /// Whether the groups were built for `pipeline` over `pool`'s current generation.
    fn bound(&self, pipeline: u64, pool: &spatial::Pool) -> bool {
        self.groups
            .as_ref()
            .is_some_and(|(id, generation, _)| (*id, *generation) == (pipeline, pool.generation()))
    }

    /// Forget what the planes hold: every pass the applies need runs again.
    fn forget(&mut self, pool: &mut spatial::Pool) {
        self.schedule.reset(pool);
        self.valid = None;
    }

    /// Encode this tick's passes of `steps` over `input`, whose key is `input`, a boundary of
    /// `size` texels placed by `texels`, with `programs` as group 0: the passes whose content
    /// changed, over the rectangle the step's mask needs ([`spatial::GpuSpatial::pass_rect`]).
    /// When the planes hold nothing over part of that rectangle, every pass the applies need runs:
    /// over the rectangle when the content changed anyway, and over the whole boundary when it did
    /// not, so a mask that grows over an unchanged input runs them once.
    ///
    /// On an incremental tick (`dirty`, the rectangle of the input and of the step's own coverage
    /// that changed since the planes were written) the planes must hold the step's values over the
    /// rectangle its mask needs, or every pass the applies need runs over the whole boundary once
    /// — a mask that grew, as a painted one does — after which they hold them everywhere. Then
    /// each unit's passes run only around where its apply can change — the change so far grown by
    /// the unit's reach ([`spatial::GpuSpatial::reach`]) — and, while the planes hold the step's
    /// values only where its mask needs them, only there, reading what the planes hold beyond it:
    /// the pass that writes a plane the apply reads over that rectangle alone, so the plane keeps
    /// its values everywhere else, and every other pass over it grown by the reach once more, so
    /// what that pass reads there is this tick's. Answers how many passes ran, and on an
    /// incremental tick the rectangle the step's output can have changed in: its input's change
    /// and, where its mask covers anything, its last unit's.
    ///
    /// The scratch planes are `pool`'s, which every link of the chain writes in turn: the schedule
    /// trusts what a pool texture holds only when this link wrote it last ([`spatial::Schedule`]),
    /// and a pool of another generation than the groups were built under rebuilds them. Where a
    /// pass runs is the same either way: a pass's output is read only within the cone its unit's
    /// reach bounds, which the tick writes itself.
    #[allow(clippy::too_many_arguments)]
    fn tick(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        (compiled, pipeline): (&spatial::CompiledSpatial, u64),
        programs: &wgpu::BindGroup,
        pool: &mut spatial::Pool,
        steps: &[GpuStep],
        (words, blocks): (&[u32], &[u32]),
        input: u64,
        (texels, size): (TexelMap, (u32, u32)),
        dirty: Option<spatial::Rect>,
    ) -> (u64, Option<spatial::Rect>) {
        #[cfg(any(test, feature = "qualification"))]
        pool.poison(device, encoder);
        if !self.bound(pipeline, pool) {
            self.groups = Some((
                pipeline,
                pool.generation(),
                spatial::Groups::new(device, compiled, &self.planes, pool),
            ));
            self.forget(pool);
        }
        if self.planes.moved(steps) {
            self.forget(pool);
        }
        let whole = spatial::Rect::whole(size);
        let spatial_steps = steps
            .iter()
            .filter(|step| matches!(step, GpuStep::Spatial(_)))
            .count();
        let single = match steps.first() {
            Some(GpuStep::Spatial(spatial)) if spatial_steps == 1 => Some(spatial),
            _ => None,
        };
        let rect = single.map_or(whole, |spatial| spatial.pass_rect(texels, size));
        let mut run = self
            .schedule
            .run(steps, words, blocks, input, &self.planes.key, pool);
        let changed = run.iter().any(|run| *run);
        // A step whose output can change only near its input's changes.
        let local = single.filter(|spatial| !spatial.global() && spatial.reaches() != u32::MAX);
        if let Some(dirty) = dirty
            && let Some(spatial) = local
        {
            // Where the mask can cover anything: the applies read the planes nowhere else, so no
            // pass runs, and the output does not change, past what that needs.
            let bounds = spatial.mask_rect(texels, size, 0);
            let reaches: Vec<u32> = (0..spatial.applies.len())
                .map(|unit| spatial.reach(unit))
                .collect();
            // Each unit's apply can change where the change so far grown by its reach does.
            let mut changes = Vec::with_capacity(reaches.len());
            let mut reached = dirty;
            for reach in &reaches {
                reached = reached.grown(*reach, size);
                changes.push(reached);
            }
            // The frame reads the last apply's planes around each pixel.
            let read = spatial.apply_reach(reaches.len().saturating_sub(1));
            let output = dirty.union(&reached.grown(read, size).intersect(&bounds.grown(1, size)));
            let rects = if !self.valid.is_some_and(|valid| valid.contains(&rect)) {
                // The planes do not hold what the applies now need — a mask that grew, as a
                // painted one does at nearly every tick: every pass over the whole boundary once,
                // after which they hold the step's values everywhere and a tick runs only around
                // its change.
                self.schedule.reset(pool);
                run = self
                    .schedule
                    .run(steps, words, blocks, input, &self.planes.key, pool);
                self.valid = Some(whole);
                vec![whole; run.len()]
            } else if !changed {
                return (0, Some(dirty));
            } else {
                // Every pass the applies need, since a scratch plane holds what an earlier
                // incremental tick wrote only where it ran.
                self.schedule.reset(pool);
                run = self
                    .schedule
                    .run(steps, words, blocks, input, &self.planes.key, pool);
                // Planes that hold the step's values only where its mask needs them are kept so:
                // each unit's apply plane where the mask covers anything, grown by what reads it
                // there ([`spatial::GpuSpatial::needed`]), and on a side where the pass rectangle
                // reaches the boundary's edge, out to that edge, as a run over the rectangle leaves
                // it. A mask that grows toward that edge stays inside `valid`, so what its apply
                // then reads there must be this input's.
                let clipped = self.valid != Some(whole);
                let kept = |unit: usize| {
                    let needed = bounds.grown(spatial.needed(unit), size);
                    spatial::Rect {
                        x0: if rect.x0 == 0 { 0 } else { needed.x0 },
                        y0: if rect.y0 == 0 { 0 } else { needed.y0 },
                        x1: if rect.x1 == size.0 { size.0 } else { needed.x1 },
                        y1: if rect.y1 == size.1 { size.1 } else { needed.y1 },
                    }
                };
                let mut rects = vec![spatial::Rect::whole((0, 0)); run.len()];
                for (unit, apply) in spatial.applies.iter().enumerate() {
                    let writes = if clipped {
                        changes[unit].intersect(&kept(unit))
                    } else {
                        changes[unit]
                    };
                    let reads = writes.grown(reaches[unit], size);
                    for (number, pass) in spatial.passes.iter().enumerate() {
                        if pass.unit as usize == unit {
                            rects[number] = if apply.planes.contains(&pass.output) {
                                writes
                            } else {
                                reads
                            };
                        }
                    }
                }
                // Clipped, they hold them only where this mask needs them now, so a mask that
                // shrank and grows again past that runs every pass once more.
                if clipped {
                    self.valid = Some(rect);
                }
                rects
            };
            let Some((_, _, groups)) = &self.groups else {
                return (0, Some(output));
            };
            let places = groups.places_each(&rects);
            self.planes.write_parameters(queue, steps, &places);
            let dispatched = groups.encode(encoder, compiled, programs, &run, &places);
            self.dispatched += dispatched;
            // Only the planes the applies read hold their values past this tick's rectangles: the
            // link's kept textures, never the pool's.
            let applied: Vec<usize> = spatial
                .applies
                .iter()
                .flat_map(|apply| &apply.planes)
                .filter_map(|plane| match self.planes.key.location(0, *plane) {
                    Some(spatial::PlaneTexture::Kept(index)) => Some(index),
                    _ => None,
                })
                .collect();
            self.schedule.keep_only(&applied, pool);
            return (dispatched, Some(output));
        }
        let over = if self.valid.is_some_and(|valid| valid.contains(&rect)) {
            if changed {
                self.valid = Some(rect);
            }
            rect
        } else {
            self.schedule.reset(pool);
            run = self
                .schedule
                .run(steps, words, blocks, input, &self.planes.key, pool);
            let over = if changed { rect } else { whole };
            self.valid = Some(over);
            over
        };
        let Some((_, _, groups)) = &self.groups else {
            return (0, None);
        };
        let places = groups.places(over);
        self.planes.write_parameters(queue, steps, &places);
        let dispatched = groups.encode(encoder, compiled, programs, &run, &places);
        self.dispatched += dispatched;
        (dispatched, dirty.map(|_| whole))
    }
}

/// What a slot is allocated for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    boundary: (u32, u32),
    /// How the boundary's texels are stored, which its texture's format follows.
    format: BoundaryFormat,
    output: (u32, u32),
    /// The tail's intermediate format, when the plan has a tail.
    intermediate: Option<wgpu::TextureFormat>,
    /// The output is a region of its stage, drawn at its rectangle of the photograph.
    region: bool,
}

impl Shape {
    /// The slot `plan` draws into: its boundary's size, its tail's output and intermediate, and
    /// whether it draws a region.
    fn of(plan: &GpuPlan) -> Self {
        let boundary = plan.boundary.size();
        let tail = plan.steps.iter().find_map(|step| match step {
            GpuStep::Geometry(tail) => Some(tail),
            _ => None,
        });
        Self {
            boundary,
            format: plan.boundary.format(),
            output: tail.map_or_else(
                || plan.region.map_or(boundary, |region| region.size()),
                GpuTail::output,
            ),
            intermediate: tail.map(GpuTail::intermediate),
            region: plan.region.is_some(),
        }
    }

    /// The output's texture on a device whose largest texture is `limit`: the size bucket of the
    /// CPU frame it stands in for, so the draw samples it over the same extent, and with the same
    /// filter weights. A whole frame's is the photograph's own; a region's is the one the CPU's
    /// region picture of the same rectangle reserves, its footprint and a small margin, where the
    /// photograph's square bucket would hold the region's longer side on both axes.
    fn capacity(&self, limit: u32) -> (u32, u32) {
        if self.region {
            super::exact_region_capacity(self.output, limit)
        } else {
            super::full_capacity(self.output, limit)
        }
    }

    /// The boundary, a tail's intermediate, the output and its placement uniform. The boundary is
    /// only loaded, never sampled, so it is exactly its size, and so is the intermediate.
    fn texture_bytes(&self, limit: u32) -> u64 {
        let texels = u64::from(self.boundary.0) * u64::from(self.boundary.1);
        let capacity = self.capacity(limit);
        let intermediate = self.intermediate.map_or(0, |format| {
            texels * u64::from(format.block_copy_size(None).unwrap_or(8))
        });
        texels * self.format.texel_bytes() as u64
            + intermediate
            + u64::from(capacity.0) * u64::from(capacity.1) * 4
            + UNIFORM_SIZE as u64
    }
}

/// What a slot's textures take of the GPU-preview budget on a device whose largest texture is
/// `limit`, as it is charged them when allocated: a boundary of `boundary` texels in `format`, a
/// geometry tail's intermediate of the same size when there is a tail, `tail` saying whether it
/// quantizes and whether it keeps `f32` values ([`GpuTail::preserve_f32`]), and the output of
/// `output` pixels in its size bucket, a region's when `region`, with its placement uniform. What
/// the desktop holds a plan to before its boundary exists, beside its chain's charge
/// ([`chain_charge`]): the slot adds only every link's words and blocks buffers once it holds it.
/// It creates nothing.
pub fn texture_charge(
    boundary: (u32, u32),
    format: BoundaryFormat,
    output: (u32, u32),
    tail: Option<(bool, bool)>,
    region: bool,
    limit: u32,
) -> u64 {
    Shape {
        boundary,
        format,
        output,
        intermediate: tail
            .map(|(quantize, preserve_f32)| tail::intermediate(quantize, preserve_f32)),
        region,
    }
    .texture_bytes(limit)
}

/// A geometry tail's intermediate texture, the boundary's size, with the view the content pass
/// writes and the bindings the tail pass reads it through.
struct Intermediate {
    texture: wgpu::Texture,
    target: wgpu::TextureView,
    bindings: wgpu::BindGroup,
}

/// One render pass of `pipeline` into `target` over a viewport of `size`, binding the programs'
/// `bindings` and, for a spatial plan's content pass, its applies' planes: over the whole target,
/// or when `scissor` is given only over it, keeping the rest of what the target holds — an
/// incremental tick's pass ([`GpuChange`]).
fn encode_pass_over(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    (bindings, planes): (&wgpu::BindGroup, Option<&wgpu::BindGroup>),
    size: (f32, f32),
    scissor: Option<spatial::Rect>,
) {
    if scissor.is_some_and(|rect| rect.is_empty()) {
        return;
    }
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("luxforge.gpu_preview.pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: match scissor {
                    Some(_) => wgpu::LoadOp::Load,
                    None => wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    pass.set_viewport(0.0, 0.0, size.0, size.1, 0.0, 1.0);
    if let Some(rect) = scissor {
        pass.set_scissor_rect(rect.x0, rect.y0, rect.x1 - rect.x0, rect.y1 - rect.y0);
    }
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bindings, &[]);
    if let Some(planes) = planes {
        pass.set_bind_group(1, planes, &[]);
    }
    pass.draw(0..3, 0..1);
}

impl GpuSlot {
    /// Everything the slot holds, as charged: its textures and buffers, the last link's kept
    /// planes, each earlier link and the pool, once.
    fn bytes(&self) -> u64 {
        self.texture_bytes
            + self.words.bytes
            + self.blocks.bytes
            + self.derivation.as_ref().map_or(0, |words| words.bytes)
            + self
                .spatial
                .as_ref()
                .map_or(0, |spatial| spatial.planes.bytes)
            + self.chain.iter().map(chain::LinkSlot::bytes).sum::<u64>()
            + self.pool.bytes()
    }

    /// Whether the slot holds boundary `version` whole in a texture a slot of `shape` would hold
    /// it in: the same size and format, whatever the output and the tail.
    fn holds(&self, shape: Shape, version: u64) -> bool {
        self.shape.boundary == shape.boundary
            && self.shape.format == shape.format
            && self.boundary_version == Some(version)
            && self.uploading.is_none()
    }

    /// Make the slot's derivation words hold `words`, a buffer charged before it is created and
    /// grown as a bigger reduction's tables need, the old one retiring with its charge.
    fn fit_derivation(
        &mut self,
        pipeline: &PhotoPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        words: &[u32],
    ) -> Result<(), GpuFallback> {
        let bytes = (words.len() * 4) as u64;
        if self
            .derivation
            .as_ref()
            .is_none_or(|charged| charged.bytes < bytes)
        {
            let capacity = buffer_capacity(device, bytes)?;
            pipeline.figures.preview.charge(capacity)?;
            let fresh = Charged {
                buffer: storage_buffer(device, "luxforge.gpu_source.derivation", capacity),
                bytes: capacity,
            };
            if let Some(old) = self.derivation.replace(fresh) {
                pipeline.retire_preview(Held::Buffer(old.buffer), old.bytes);
            }
        }
        let charged = self.derivation.as_ref().expect("the derivation's words");
        queue.write_buffer(&charged.buffer, 0, &le_bytes(words));
        Ok(())
    }

    /// What the last link reads as its boundary: the last intermediate, or the boundary itself.
    fn last_input(&self) -> &wgpu::Texture {
        self.chain
            .last()
            .map_or(&self.boundary, |link| &link.texture)
    }

    pub(super) fn output(&self) -> &Picture {
        &self.output
    }

    pub(super) fn frame_us(&self) -> u64 {
        self.frame_us
    }

    /// The serial of the plan whose values the slot holds ([`GpuChange`]).
    pub(super) fn evaluated_serial(&self) -> Option<u64> {
        self.evaluated_serial.map(|(serial, _)| serial)
    }

    /// The clock the queue reports the slot's passes complete to.
    pub(super) fn clock(&self) -> Arc<PassClock> {
        Arc::clone(&self.clock)
    }
}

/// A compiled sequence: the content steps' pass, a geometry tail's pass after it, and every spatial
/// step's compute passes before them.
#[derive(Clone)]
struct Compiled {
    /// The content steps' pass: into the output, or into a tail's intermediate.
    render: wgpu::RenderPipeline,
    /// The tail's pass, from the intermediate into the output.
    tail: Option<wgpu::RenderPipeline>,
    spatial: spatial::CompiledSpatial,
}

/// What the device must offer for the stage to run, made once with the pipeline.
struct Support {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    /// The spatial passes' pipelines, kept across sequences.
    passes: spatial::PassCache,
}

impl Support {
    /// The programs' bind group layout — the words, the blocks and the boundary — and the pipeline
    /// layout over it.
    fn new(device: &wgpu::Device) -> Self {
        // The spatial step's passes bind the same words, blocks and boundary as the frame's pass.
        let visibility = wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.gpu_preview.layout"),
            entries: &[
                storage(0),
                storage(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("luxforge.gpu_preview.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        Self {
            layout,
            pipeline_layout,
            passes: spatial::PassCache::default(),
        }
    }
}

/// The stage's state for one pipeline: whether the device can run it, its lost flag, its compiled
/// sequences and the tick's scratch words, the last link's and each earlier link's in turn.
pub(super) struct GpuStage {
    /// Shared with the compile thread, which compiles through it.
    support: Option<Arc<Support>>,
    lost: Arc<AtomicBool>,
    /// The compiled sequences and the thread that compiles them ([`compile`]).
    pipelines: compile::Pipelines,
    /// The version of the warm list last handed to the compile thread.
    warmed: Option<u64>,
    words: Vec<u32>,
    link_words: Vec<u32>,
    /// The prepared source every derived boundary is drawn from, shared by every surface of the
    /// pipeline ([`source`]), and the passes that derive one, made once.
    source: Option<SourceSlot>,
    layouts: Option<SourceLayouts>,
    /// Whether a surface handed the source this frame; one no surface hands retires at the frame's
    /// end. And the bytes of it written this frame, which every surface's `prepare` shares.
    source_handed: bool,
    source_written: u64,
}

/// Whether a device with `limits`, drawing to a target of `format`, can run the stage.
fn supported(limits: &wgpu::Limits, format: wgpu::TextureFormat) -> bool {
    format.is_srgb()
        && limits.max_storage_buffers_per_shader_stage >= 2
        && limits.max_bind_groups >= 1
        && u64::from(limits.max_storage_buffer_binding_size) >= MIN_BUFFER
}

/// The format a chain's intermediates take over a boundary of `format`: the boundary's own, so a
/// link's output holds its values as the boundary held its input.
fn intermediate_format(format: BoundaryFormat) -> wgpu::TextureFormat {
    format.texture()
}

/// The bytes one of a chain's intermediates takes over a boundary of `size` texels in `format`.
fn intermediate_bytes((width, height): (u32, u32), format: BoundaryFormat) -> u64 {
    u64::from(width)
        * u64::from(height)
        * u64::from(
            intermediate_format(format)
                .block_copy_size(None)
                .unwrap_or(16),
        )
}

/// Each link of `steps`' chain with the format its last pass writes: an intermediate for every
/// link but the last, and the output's codes for the last.
fn link_sequences(
    steps: &[GpuStep],
    boundary: BoundaryFormat,
) -> impl Iterator<Item = (&[GpuStep], wgpu::TextureFormat)> {
    let chain = chain::chain(steps);
    let intermediate = intermediate_format(boundary);
    chain
        .links
        .into_iter()
        .map(move |link| (link, intermediate))
        .chain(std::iter::once((chain.last, OUTPUT_FORMAT)))
}

/// What a lost device's callback does, and what a test that simulates one calls.
fn device_lost(lost: &AtomicBool) {
    lost.store(true, Ordering::Release);
    wake_surface();
}

impl GpuStage {
    /// The stage for a pipeline on `device`, drawing to `format`, counting into `figures`. Its lost
    /// flag, which the device's lost callback sets, is the figures' own. `refused` is the launch's
    /// refusal of the stage ([`refuse_gpu_stage`]): the capability check then answers unavailable
    /// and nothing of the stage is created. What the check answered is published in `figures`, and
    /// the desktop woken to read it.
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        figures: &Figures,
        refused: bool,
    ) -> Self {
        figures.budget.store(GPU_PREVIEW_BUDGET, Ordering::Release);
        figures
            .upload_per_frame
            .store(UPLOAD_PER_FRAME, Ordering::Release);
        let lost = Arc::clone(&figures.lost);
        let signal = Arc::clone(&lost);
        device.set_device_lost_callback(move |_reason, _message| device_lost(&signal));
        let support = (!refused && supported(&device.limits(), format))
            .then(|| Arc::new(Support::new(device)));
        figures.stage.checked(support.is_some(), refused);
        // The derivation passes are fixed, so they are built with the stage, as the mip levels'
        // pass is: the first boundary derived compiles nothing.
        let layouts = support
            .as_ref()
            .and_then(|_| SourceLayouts::new(device).ok());
        wake_surface();
        Self {
            support,
            lost,
            pipelines: compile::Pipelines::default(),
            warmed: None,
            words: Vec::new(),
            link_words: Vec::new(),
            source: None,
            layouts,
            source_handed: false,
            source_written: 0,
        }
    }

    /// The ready pipeline for `steps`, or why the frame draws the CPU's: never compiled here, on
    /// the UI thread. A first-seen sequence goes to the compile thread and answers
    /// [`GpuFallback::Compiling`] until it is ready; a failed one stays failed.
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        steps: &[GpuStep],
        format: wgpu::TextureFormat,
        figures: &Figures,
    ) -> Result<(Compiled, u64), GpuFallback> {
        let support = self.support.as_ref().ok_or(GpuFallback::NoAdapter)?;
        self.pipelines.get(device, support, steps, format, figures)
    }

    /// The ready pipelines of every link of `steps`' chain over a boundary of `boundary`, the last
    /// one's last, or why the frame draws the CPU's. Every link not ready yet is asked for in the
    /// same call, so a chain's links compile together rather than one a frame; a failed link
    /// fails the plan.
    fn chain_pipelines(
        &mut self,
        device: &wgpu::Device,
        steps: &[GpuStep],
        boundary: BoundaryFormat,
        figures: &Figures,
    ) -> Result<Vec<(Compiled, u64)>, GpuFallback> {
        let mut ready = Vec::new();
        let mut missing = None;
        for (steps, format) in link_sequences(steps, boundary) {
            match self.pipeline(device, steps, format, figures) {
                Ok(found) => ready.push(found),
                Err(GpuFallback::Compiling) => {
                    missing.get_or_insert(GpuFallback::Compiling);
                }
                Err(other) => missing = Some(other),
            }
        }
        match missing {
            Some(fallback) => Err(fallback),
            None => Ok(ready),
        }
    }

    /// Hand `sequences` the stage does not hold yet to the compile thread.
    fn warm(&mut self, device: &wgpu::Device, sequences: &[compile::Sequence], figures: &Figures) {
        if let Some(support) = &self.support {
            self.pipelines.warm(device, support, sequences, figures);
        }
    }

    #[cfg(test)]
    fn failure(&self, steps: &[GpuStep]) -> Option<Arc<str>> {
        self.pipelines.failure(steps, OUTPUT_FORMAT)
    }
}

/// The prelude's helpers, which every assembled module declares beside a program's functions.
const PRELUDE_FUNCTIONS: &[&str] = &["lf_word", "lf_f32", "lf_block_word", "lf_block_f32"];

/// Every name the surface declares in an assembled shader: the prelude's, the boundary and the
/// generated entry points. A program's names start with its entry's, so an entry that is none of
/// these cannot collide with them; the core names its entries `lf_<module>_<unit>`.
const SURFACE_NAMES: &[&str] = &[
    "lf_words",
    "lf_blocks",
    "lf_word",
    "lf_f32",
    "lf_block_word",
    "lf_block_f32",
    "lf_boundary",
    "lf_vertex",
    "lf_fragment",
    "lf_shared",
    "lf_plane",
    "lf_plane_size",
    "lf_source",
    "lf_origin",
    "lf_size",
    "lf_store",
    "lf_out",
    "lf_pass",
];

/// Whether `name` may name an entry function: a WGSL identifier that is none of the surface's own.
fn entry_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first = chars.next().ok_or("an entry function needs a name")?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name == "_"
        || name.starts_with("__")
    {
        return Err(format!("{name:?} is not a WGSL identifier"));
    }
    // A program's names start with its entry, so an entry no surface name starts with cannot
    // collide with one, nor with the planes a module binds as `lf_plane_<slot>`.
    if SURFACE_NAMES
        .iter()
        .any(|surface| surface.starts_with(name))
        || name.starts_with(mask::GENERATED)
        || tail::GENERATED
            .iter()
            .any(|prefix| name.starts_with(prefix))
    {
        return Err(format!("{name:?} is one of the surface's own names"));
    }
    Ok(())
}

/// The whole shader for `steps` with no geometry tail: the prelude, each program once, then the
/// entry points, writing the output's codes. The first pass's shader of a plan with a tail.
#[cfg(test)]
fn assemble(steps: &[GpuStep]) -> Result<String, String> {
    Ok(assemble_passes(steps, End::Codes)?.swap_remove(0))
}

/// What a pass's fragment stage starts from.
enum Head<'a> {
    /// The boundary texel and its stage pixel; `offset` when the pass is the plan's last, whose
    /// first output pixel is the header's offset into the boundary.
    Boundary { offset: bool },
    /// The geometry tail, step `index`: the output pixel and the tail's blend there.
    Tail(&'a GpuTail, usize),
}

/// What a pass's fragment stage writes.
pub(super) enum End {
    /// The linear value, into an `rgba32float` target: a qualification's, or a chain's intermediate
    /// over a RAW's boundary.
    Linear,
    /// The linear value rounded to the nearest half, ties to even, into an `rgba16float` target: a
    /// half-precision tail's intermediate, or a chain's intermediate over a JPEG's boundary. The
    /// M4 converts a render target's value toward zero, which would hold every texel up to half a
    /// step darker than the CPU's value, again at every link of a chain.
    Half,
    /// The CPU quantizer's 8-bit codes, through a Unorm view.
    Codes,
}

/// The shaders of `steps`, one per pass: the content steps, then, when a geometry tail splits them,
/// the tail and the steps after it. The last pass writes as `last` says; a quantizing tail's
/// content pass writes codes.
pub(super) fn assemble_passes(steps: &[GpuStep], last: End) -> Result<Vec<String>, String> {
    // The marks read the output the last pass is about to encode, after every other step.
    if steps
        .iter()
        .rev()
        .skip(1)
        .any(|step| matches!(step, GpuStep::Clipping(_)))
    {
        return Err("clipping marks are a plan's last step".into());
    }
    let tails: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| matches!(step, GpuStep::Geometry(_)).then_some(index))
        .collect();
    match tails.as_slice() {
        [] => Ok(vec![pass_source(
            steps,
            0..steps.len(),
            Head::Boundary { offset: true },
            last,
        )?]),
        [index] => {
            let GpuStep::Geometry(tail) = &steps[*index] else {
                unreachable!("the step found")
            };
            // A spatial step reads the boundary's own texels, which only the content pass holds.
            if steps[index + 1..]
                .iter()
                .any(|step| matches!(step, GpuStep::Spatial(_)))
            {
                return Err("a spatial step runs before the geometry tail".into());
            }
            let content = if tail.quantizes() {
                End::Codes
            } else if tail.preserves_f32() {
                End::Linear
            } else {
                End::Half
            };
            Ok(vec![
                pass_source(steps, 0..*index, Head::Boundary { offset: false }, content)?,
                pass_source(
                    steps,
                    index + 1..steps.len(),
                    Head::Tail(tail, *index),
                    last,
                )?,
            ])
        }
        _ => Err("a plan has one geometry tail at most".into()),
    }
}

/// One pass's shader: the steps of `range` after `head`, writing `end`.
fn pass_source(
    steps: &[GpuStep],
    range: std::ops::Range<usize>,
    head: Head<'_>,
    end: End,
) -> Result<String, String> {
    let mut source = String::from(PRELUDE);
    let mut included: Vec<&GpuProgram> = Vec::new();
    for (_, program) in steps[range.clone()].iter().flat_map(GpuStep::programs) {
        entry_name(&program.entry)?;
        match included.iter().find(|seen| seen.entry == program.entry) {
            Some(seen) if seen.source != program.source => {
                return Err(format!(
                    "two programs are named {:?} with different sources",
                    program.entry
                ));
            }
            Some(_) => {}
            None => {
                source.push_str("\n// ");
                source.push_str(&program.entry);
                source.push('\n');
                source.push_str(&program.source);
                source.push('\n');
                included.push(program);
            }
        }
    }
    // Each masked step's coverage, composed by a function of its own.
    let (functions, masked) = spatial::masks(steps, range.clone());
    source.push_str(&functions);
    let quantizing = matches!(head, Head::Tail(tail, _) if tail.quantizes());
    let marks = steps[range.clone()]
        .iter()
        .any(|step| matches!(step, GpuStep::Clipping(_)));
    if quantizing || marks || matches!(end, End::Codes) {
        source.push_str(tail::encoding()?);
    }
    if marks {
        source.push_str(clipping::SOURCE);
    }
    source.push_str(BOUNDARY_BINDING);
    // A spatial step's applies read its planes in the content pass, the one it precedes.
    let spatial = steps[range.clone()]
        .iter()
        .any(|step| matches!(step, GpuStep::Spatial(_)));
    let (declarations, slots) = if spatial {
        spatial::fragment_declarations(steps)
    } else {
        (String::new(), spatial::Slots::default())
    };
    source.push_str(&declarations);
    match head {
        Head::Boundary { offset: true } => source.push_str(
            "
@fragment
fn lf_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // The output's first pixel is the header's offset into the boundary: zero for a whole frame,
    // a region's origin in a percentage view. The border column and row past the boundary repeat
    // its edge texels.
    let lf_offset = vec2<u32>(lf_word(4u), lf_word(5u));
    let texel = min(vec2<u32>(position.xy) + lf_offset, textureDimensions(lf_boundary) - vec2<u32>(1u));
    var rgb = textureLoad(lf_boundary, texel, 0).rgb;
    let stage = vec2<f32>(lf_f32(0u), lf_f32(1u)) + vec2<f32>(texel) * vec2<f32>(lf_f32(2u), lf_f32(3u));
",
        ),
        Head::Boundary { offset: false } => source.push_str(
            "
@fragment
fn lf_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // The border column and row past the boundary repeat its edge texels.
    let texel = min(vec2<u32>(position.xy), textureDimensions(lf_boundary) - vec2<u32>(1u));
    var rgb = textureLoad(lf_boundary, texel, 0).rgb;
    let stage = vec2<f32>(lf_f32(0u), lf_f32(1u)) + vec2<f32>(texel) * vec2<f32>(lf_f32(2u), lf_f32(3u));
",
        ),
        Head::Tail(tail, index) => {
            source.push_str(&tail.program().source);
            source.push_str(&tail::sampling(tail.quantizes()));
            source.push_str(&tail::fragment(tail, MAP_WORDS + STEP_WORDS * index));
        }
    }
    for index in range {
        source.push_str(&spatial::statements(steps, index, &slots, &masked));
    }
    source.push_str(match end {
        End::Linear => "    return vec4<f32>(rgb, 1.0);\n}\n",
        End::Half => "    return lf_surface_half(vec4<f32>(rgb, 1.0));\n}\n",
        End::Codes => "    return vec4<f32>(lf_output_encode(rgb), 1.0);\n}\n",
    });
    if matches!(end, End::Half) {
        source.push_str(spatial::HALF_ROUNDING);
    }
    Ok(source)
}

/// Validate `source` as the stage will compile it, without a device: naga's own validation, the
/// one wgpu runs, with no optional capability.
fn validate(source: &str) -> Result<naga::Module, String> {
    let module =
        naga::front::wgsl::parse_str(source).map_err(|error| error.emit_to_string(source))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| error.emit_to_string(source))?;
    Ok(module)
}

/// Check one step's program against the calling convention on its own, as the stage checks every
/// program before it compiles a sequence: its entry name is none of the surface's own, [`PRELUDE`]
/// and its source validate with naga's full validation and no optional capability, it declares
/// only functions and constants — no binding, global variable, override, named type or entry
/// point — each named starting with its entry's name, and its entry function has the signature
/// its step needs. A caller's tests can check every program it hands the surface with this.
pub fn validate_step(step: &GpuStep) -> Result<(), String> {
    if let GpuStep::Spatial(spatial) = step {
        return spatial::validate_spatial(spatial);
    }
    for (role, program) in step.programs() {
        validate_program(role, program)?;
    }
    Ok(())
}

/// One program of a step against the convention, with its role's signature.
fn validate_program(role: mask::Role, program: &GpuProgram) -> Result<(), String> {
    let entry = &program.entry;
    if role == mask::Role::Spatial {
        return Err(format!(
            "{entry:?} is a spatial program, which only its step checks"
        ));
    }
    entry_name(entry)?;
    let module = validate(&format!("{PRELUDE}\n{}", program.source))?;
    let only = "a program has only functions and constants";
    let prelude_globals = 2;
    if module.global_variables.len() != prelude_globals || !module.overrides.is_empty() {
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
    let names = module
        .functions
        .iter()
        .map(|(_, function)| &function.name)
        .filter(|name| {
            !name
                .as_deref()
                .is_some_and(|name| PRELUDE_FUNCTIONS.contains(&name))
        })
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
    let function = module
        .functions
        .iter()
        .map(|(_, function)| function)
        .find(|function| function.name.as_deref() == Some(entry.as_ref()))
        .ok_or_else(|| format!("{entry:?} names no function its source declares"))?;
    let vector = |size| naga::TypeInner::Vector {
        size,
        scalar: naga::Scalar::F32,
    };
    let unsigned = naga::TypeInner::Scalar(naga::Scalar::U32);
    let (arguments, result, signature) = match role {
        mask::Role::Colour => (
            [
                vector(naga::VectorSize::Tri),
                vector(naga::VectorSize::Bi),
                unsigned.clone(),
                unsigned,
            ],
            vector(naga::VectorSize::Tri),
            "(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>",
        ),
        mask::Role::Coverage => (
            [
                vector(naga::VectorSize::Bi),
                vector(naga::VectorSize::Tri),
                unsigned.clone(),
                unsigned,
            ],
            naga::TypeInner::Scalar(naga::Scalar::F32),
            "(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32",
        ),
        mask::Role::Spatial => unreachable!("a spatial program is refused above"),
    };
    let declared: Vec<&naga::TypeInner> = function
        .arguments
        .iter()
        .map(|argument| &module.types[argument.ty].inner)
        .collect();
    let returns = function
        .result
        .as_ref()
        .map(|result| &module.types[result.ty].inner);
    if declared.len() != arguments.len()
        || declared
            .iter()
            .zip(&arguments)
            .any(|(declared, wanted)| *declared != wanted)
        || returns != Some(&result)
    {
        return Err(format!("{entry:?} must be fn {entry}{signature}"));
    }
    Ok(())
}

/// Take a future's answer if it is already there; never wait for one.
fn answered<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}

/// Assemble, validate and compile `steps` into one pipeline per pass, the last writing `format`,
/// naming why it failed. The stage writes [`OUTPUT_FORMAT`], whose codes the last pass computes;
/// only qualification asks for a float format, which takes the linear values. A plan with a
/// geometry tail has a second pass, whose first writes the tail's intermediate format.
fn compile(
    device: &wgpu::Device,
    support: &Support,
    steps: &[GpuStep],
    format: wgpu::TextureFormat,
) -> Result<Compiled, String> {
    for step in steps {
        validate_step(step)?;
    }
    let last = match format {
        wgpu::TextureFormat::Rgba32Float => End::Linear,
        wgpu::TextureFormat::Rgba16Float => End::Half,
        _ => End::Codes,
    };
    let sources = assemble_passes(steps, last)?;
    for source in &sources {
        validate(source)?;
    }
    let intermediate = steps.iter().find_map(|step| match step {
        GpuStep::Geometry(tail) => Some(tail.intermediate()),
        _ => None,
    });
    device.push_error_scope(wgpu::ErrorFilter::Internal);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let spatial = spatial::compile_passes(device, support, steps);
    // A spatial plan's content pass binds its applies' planes as a second group.
    let planes_layout;
    let spatial_layout;
    let layout = match spatial
        .as_ref()
        .ok()
        .and_then(|(compiled, _)| compiled.fragment.as_ref())
    {
        Some((planes, _)) => {
            planes_layout = planes.clone();
            spatial_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("luxforge.gpu_preview.spatial_layout"),
                bind_group_layouts: &[&support.layout, &planes_layout],
                push_constant_ranges: &[],
            });
            &spatial_layout
        }
        None => &support.pipeline_layout,
    };
    let mut sources = sources.into_iter();
    let content = sources.next().ok_or("a plan has a pass")?;
    let render = render_pipeline(device, layout, content, intermediate.unwrap_or(format));
    let tail = sources
        .next()
        .map(|source| render_pipeline(device, &support.pipeline_layout, source, format));
    let validation = answered(device.pop_error_scope());
    let internal = answered(device.pop_error_scope());
    let (spatial, made) = spatial?;
    match (validation, internal) {
        (Some(None), Some(None)) => {
            support.passes.keep(made);
            Ok(Compiled {
                render,
                tail,
                spatial,
            })
        }
        (Some(Some(error)), _) | (_, Some(Some(error))) => Err(error.to_string()),
        _ => Err("the pipeline's error scopes were not answered without waiting".into()),
    }
}

/// One pass's render pipeline over `layout`, writing `format`, inside the caller's error scopes.
fn render_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    source: String,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("luxforge.gpu_preview.shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("luxforge.gpu_preview.pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("lf_vertex"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("lf_fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// The tick's words — the texel map, each step's base indices and position map, then every
/// program's words — and its blocks, concatenated, at least one word.
#[cfg(test)]
pub(super) fn pack(plan: &GpuPlan, words: &mut Vec<u32>, blocks: &mut Vec<u32>) {
    chain::pack_steps(plan.texels, output_offset(plan), &plan.steps, words, blocks);
}

/// The offset of `plan`'s first output pixel: zero for a whole frame; for a region, its origin in
/// the output stage when a tail's pass draws it, and in the boundary's texels when the content pass
/// does, whose texel map then has a step of one.
fn output_offset(plan: &GpuPlan) -> (u32, u32) {
    let Some(region) = plan.region else {
        return (0, 0);
    };
    if plan
        .steps
        .iter()
        .any(|step| matches!(step, GpuStep::Geometry(_)))
    {
        (region.rect[0], region.rect[1])
    } else {
        let origin = plan.texels.origin.map(|value| value.max(0.0) as u32);
        (
            region.rect[0].saturating_sub(origin[0]),
            region.rect[1].saturating_sub(origin[1]),
        )
    }
}

/// Whether a region plan's frame lies inside what the plan can draw: a rectangle of its stage
/// and, without a tail, of the boundary's texels at one texel a stage pixel.
fn region_drawable(plan: &GpuPlan) -> bool {
    let Some(region) = plan.region else {
        return true;
    };
    if !region.valid() {
        return false;
    }
    if plan
        .steps
        .iter()
        .any(|step| matches!(step, GpuStep::Geometry(_)))
    {
        return true;
    }
    let origin = plan.texels.origin;
    let (width, height) = plan.boundary.size();
    plan.texels.step == [1.0, 1.0]
        && origin[0] >= 0.0
        && origin[1] >= 0.0
        && origin[0] <= region.rect[0] as f32
        && origin[1] <= region.rect[1] as f32
        && region.rect[2] as f32 <= origin[0] + width as f32
        && region.rect[3] as f32 <= origin[1] + height as f32
}

/// A storage buffer able to hold `bytes`, rounded up so a few more words do not each reallocate,
/// within the device's largest storage binding, or the refusal that names that limit.
fn buffer_capacity(device: &wgpu::Device, bytes: u64) -> Result<u64, GpuFallback> {
    // A multiple of four, as a buffer binding must be.
    let limit = u64::from(device.limits().max_storage_buffer_binding_size) & !3;
    if bytes > limit {
        return Err(GpuFallback::BufferLimit { bytes, limit });
    }
    Ok(bytes.max(MIN_BUFFER).next_power_of_two().min(limit))
}

fn storage_buffer(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl PhotoPipeline {
    /// Make `surface`'s GPU-preview slot hold what `plan` draws and record how this frame is drawn:
    /// with no plan, the slot is released and the frame is the CPU's, unless `dissolve` runs from
    /// the GPU frame the last draw showed, which the slot then keeps ([`dissolve`]); with one, the
    /// slot evaluates it, or the frame is the CPU's and names why, and a `dissolve` beside it — the
    /// caller hands one only while the plan is held — runs from the slot's output. `surface` is out
    /// of the map.
    pub(super) fn prepare_gpu(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&GpuPlan>,
        dissolve: Option<DissolveFrame>,
        change: Option<GpuChange>,
    ) {
        // The GPU frame the last draw showed, which is all a dissolve may start from.
        let shown = surface.gpu_output().is_some() || surface.dissolving.is_some();
        surface.gpu_outcome = None;
        surface.dissolving = None;
        let Some(plan) = plan else {
            match dissolve {
                Some(frame) if shown && surface.gpu.is_some() => surface.dissolving = Some(frame),
                _ => self.release_gpu(surface),
            }
            return;
        };
        let outcome = self.evaluate(surface, device, queue, plan, change);
        // A sequence still compiling leaves the slot as it was, its boundary included, for the
        // frame that finds the pipeline ready, and a boundary still uploading leaves it for the
        // frame that writes the next chunks; any other fallback lets the slot go.
        if outcome.is_err()
            && !matches!(
                outcome,
                Err(GpuFallback::Compiling | GpuFallback::BoundaryUploading { .. })
            )
        {
            self.release_gpu(surface);
        }
        // A dissolve handed beside a plan runs behind it while it is held: the slot keeps the
        // output of the GPU frame the last draw showed, which the plan's unchanged words leave as
        // it was.
        if outcome.is_ok() && shown {
            surface.dissolving = dissolve;
        }
        surface.gpu_outcome = Some(outcome);
    }

    /// Hand `warm`'s sequences to the compile thread, once per version.
    pub(super) fn warm_gpu(&mut self, device: &wgpu::Device, warm: Option<&GpuWarm>) {
        if let Some(warm) = warm
            && self.gpu.warmed != Some(warm.version())
        {
            self.gpu.warmed = Some(warm.version());
            // Each link of each plan's chain is a sequence of its own.
            let sequences: Vec<compile::Sequence> = warm
                .sequences()
                .iter()
                .flat_map(|(steps, format)| {
                    link_sequences(steps, *format).map(|(link, format)| (link.to_vec(), format))
                })
                .collect();
            self.gpu.warm(device, &sequences, &self.figures.preview);
            self.figures
                .preview
                .compile
                .warmed
                .store(warm.version().saturating_add(1), Ordering::Release);
        }
    }

    /// Retire `surface`'s GPU-preview slot, if it holds one.
    pub(super) fn release_gpu(&self, surface: &mut SurfaceSlots) {
        if let Some(slot) = surface.gpu.take() {
            self.retire_slot(slot);
        }
    }

    /// Make the pipeline hold `source`, which a surface hands this frame ([`source`]): another
    /// version retires the one held, with its charge; a new one is charged and created, if it
    /// still has its pixels; and the rows not written yet are, at most [`UPLOAD_PER_FRAME`] a
    /// frame across every surface that hands it. An upload's start and its end each wake the
    /// desktop once. A source the device cannot hold says why in the surface's diagnostics and
    /// holds nothing; a boundary derived from it then falls back.
    pub(super) fn fit_source(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: Option<&GpuSource>,
    ) {
        let Some(source) = source else {
            return;
        };
        self.gpu.source_handed = true;
        if self.gpu.support.is_none() || self.gpu.lost.load(Ordering::Acquire) {
            return;
        }
        if let Some(held) = self
            .gpu
            .source
            .take_if(|held| held.version() != source.version())
        {
            let bytes = held.bytes();
            self.retire_preview(Held::Source(Box::new(held)), bytes);
        }
        if self.gpu.source.is_none() && source.holds_pixels() {
            let Some(layouts) = self.gpu.layouts.as_ref() else {
                // No derivation passes on this device: nothing could be derived from it.
                self.figures.diagnostics().gpu_source_refused = Some(GpuFallback::PipelineFailed);
                return;
            };
            let preview = &self.figures.preview;
            match SourceSlot::new(device, source, layouts, |bytes| preview.charge(bytes)) {
                Ok(slot) => {
                    self.gpu.source = Some(slot);
                    wake_surface();
                }
                Err(fallback) => {
                    self.figures.diagnostics().gpu_source_refused = Some(fallback);
                    return;
                }
            }
        }
        let Some(slot) = self.gpu.source.as_mut() else {
            return;
        };
        if !slot.ready() {
            let limit = self
                .figures
                .preview
                .upload_per_frame
                .load(Ordering::Acquire)
                .saturating_sub(self.gpu.source_written);
            if limit > 0 {
                let written = slot.upload(queue, source, limit);
                self.gpu.source_written += written;
                self.figures
                    .preview
                    .staged
                    .fetch_add(written, Ordering::AcqRel);
                if slot.ready() {
                    wake_surface();
                }
            }
        }
        let mut diagnostics = self.figures.diagnostics();
        diagnostics.gpu_source = Some(slot.figures());
        diagnostics.gpu_source_refused = None;
    }

    /// At the end of every frame: the source no surface handed retires, with its charge, and the
    /// next frame's upload starts afresh.
    pub(super) fn trim_source(&mut self) {
        self.gpu.source_written = 0;
        if std::mem::take(&mut self.gpu.source_handed) {
            return;
        }
        if let Some(held) = self.gpu.source.take() {
            let bytes = held.bytes();
            self.retire_preview(Held::Source(Box::new(held)), bytes);
            self.figures.diagnostics().gpu_source = None;
        }
    }

    fn evaluate(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuPlan,
        change: Option<GpuChange>,
    ) -> Result<u64, GpuFallback> {
        if self.gpu.support.is_none() {
            return Err(GpuFallback::NoAdapter);
        }
        if self.gpu.lost.load(Ordering::Acquire) {
            return Err(GpuFallback::DeviceLost);
        }
        let (width, height) = plan.boundary.size();
        let limit = device.limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            return Err(GpuFallback::TextureLimit {
                width,
                height,
                limit,
            });
        }
        let tail = plan.steps.iter().find_map(|step| match step {
            GpuStep::Geometry(tail) => Some(tail),
            _ => None,
        });
        if let Some((width, height)) = tail.map(GpuTail::output)
            && (width > limit || height > limit || width == 0 || height == 0)
        {
            return Err(GpuFallback::TextureLimit {
                width,
                height,
                limit,
            });
        }
        if !region_drawable(plan) {
            return Err(GpuFallback::PipelineFailed);
        }
        if plan
            .steps
            .iter()
            .any(|step| matches!(step, GpuStep::Spatial(_)))
        {
            // A spatial step runs compute passes over the boundary's own texels.
            if !spatial::supported(&device.limits()) {
                return Err(GpuFallback::NoAdapter);
            }
            if plan.texels.step != [1.0, 1.0] {
                return Err(GpuFallback::PipelineFailed);
            }
        }
        let pipelines = self.gpu.chain_pipelines(
            device,
            &plan.steps,
            plan.boundary.format(),
            &self.figures.preview,
        )?;
        let mut words = std::mem::take(&mut self.gpu.words);
        let mut link_words = std::mem::take(&mut self.gpu.link_words);
        let shape = Shape::of(plan);
        let result = self.run(
            surface,
            device,
            queue,
            plan,
            shape,
            &pipelines,
            (&mut words, &mut link_words),
            change,
        );
        self.gpu.words = words;
        self.gpu.link_words = link_words;
        result
    }

    /// Fit the slot to the plan, write what changed and encode the passes when anything did: each
    /// link of the plan's chain whose content changed into its intermediate, then the last link
    /// into the output ([`chain`]). `pipelines` holds every link's, the last link's last.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuPlan,
        shape: Shape,
        pipelines: &[(Compiled, u64)],
        (words, link_words): (&mut Vec<u32>, &mut Vec<u32>),
        change: Option<GpuChange>,
    ) -> Result<u64, GpuFallback> {
        let started = std::time::Instant::now();
        let chain = chain::chain(&plan.steps);
        let (pipeline, pipeline_id) = pipelines
            .last()
            .map(|(compiled, id)| (compiled, *id))
            .ok_or(GpuFallback::PipelineFailed)?;
        if pipelines.len() != chain.links.len() + 1 {
            return Err(GpuFallback::PipelineFailed);
        }
        // The last link's words: over the boundary's texels, with the output's offset. Its blocks
        // are written from the steps' own below, only those that changed copied ([`blocks`]).
        chain::pack_words(plan.texels, output_offset(plan), chain.last, words);
        let words: &[u32] = words;
        let word_bytes = (words.len() * 4) as u64;
        let block_bytes = (blocks::block_len(chain.last) * 4) as u64;
        let held = surface
            .gpu
            .as_ref()
            .is_some_and(|slot| slot.holds(shape, plan.boundary.version));
        // A boundary whose texels were let go is drawn only from the slot that holds them.
        if !plan.boundary.holds_texels() && plan.boundary.derived.is_none() && !held {
            return Err(GpuFallback::BoundaryReleased);
        }
        // A derived boundary the slot does not hold yet is drawn from the source the pipeline
        // holds, once all of it is uploaded, by the derivation's pass; nothing is allocated for it
        // before then.
        if let (Some((version, _)), false) = (&plan.boundary.derived, held) {
            let source = self
                .gpu
                .source
                .as_ref()
                .filter(|source| source.version() == *version)
                .ok_or(GpuFallback::SourceMissing)?;
            if !source.ready() {
                let figures = source.figures();
                return Err(GpuFallback::SourceUploading {
                    uploaded: figures.uploaded,
                    bytes: figures.bytes,
                });
            }
            if self.gpu.layouts.is_none() || source.kind().boundary() != plan.boundary.format {
                return Err(GpuFallback::PipelineFailed);
            }
        }
        if surface.gpu.as_ref().is_some_and(|slot| slot.shape != shape)
            && let Some(slot) = surface.gpu.take()
        {
            // A slot refitted to another output or tail over the same boundary keeps the
            // boundary's texture and what it holds, whose texels the caller may have let go.
            let kept = slot
                .boundary_version
                .filter(|version| slot.holds(shape, *version))
                .map(|version| (slot.boundary.clone(), version));
            self.retire_slot(slot);
            surface.gpu = Some(self.allocate(device, shape, word_bytes, block_bytes, kept)?);
        }
        if surface.gpu.is_none() {
            surface.gpu = Some(self.allocate(device, shape, word_bytes, block_bytes, None)?);
        }
        let slot = surface.gpu.as_mut().expect("an admitted slot");
        // A region plan's frame is placed at its rectangle of the whole stage, as a region of the
        // photograph is; a whole frame stretches over the photograph.
        slot.output.region_key = plan.region.map(|region| super::RegionKey {
            rect: region.rect,
            stage: region.stage,
            full_stage: region.stage,
            quality: crate::RegionQuality::Interactive,
            content_id: 0,
            generation: 0,
        });
        // The chain's intermediates, before the last link's bindings, which read the last of them.
        self.fit_chain(slot, device, chain.links.len())?;
        let origin = (
            plan.texels.origin[0].max(0.0) as u32,
            plan.texels.origin[1].max(0.0) as u32,
        );
        // The pool every link's scratch planes are taken from, before any link's planes.
        self.fit_pool(slot, device, &chain, shape.boundary, origin)?;
        #[cfg(test)]
        slot.pool
            .set_poisoned(self.figures.preview.poison.load(Ordering::Acquire));
        let mut rebind = false;
        for (charged, bytes, label) in [
            (&mut slot.words, word_bytes, "luxforge.gpu_preview.words"),
            (&mut slot.blocks, block_bytes, "luxforge.gpu_preview.blocks"),
        ] {
            if bytes > charged.bytes {
                let capacity = buffer_capacity(device, bytes)?;
                self.figures.preview.charge(capacity)?;
                let old = std::mem::replace(
                    charged,
                    Charged {
                        buffer: storage_buffer(device, label, capacity),
                        bytes: capacity,
                    },
                );
                self.retire_preview(Held::Buffer(old.buffer), old.bytes);
                rebind = true;
            }
        }
        if rebind {
            slot.bindings = self.program_bindings(
                device,
                slot.last_input(),
                &slot.words.buffer,
                &slot.blocks.buffer,
            );
            if let Some(intermediate) = &mut slot.intermediate {
                intermediate.bindings = self.program_bindings(
                    device,
                    &intermediate.texture,
                    &slot.words.buffer,
                    &slot.blocks.buffer,
                );
            }
            slot.written_words.clear();
            slot.written_blocks.forget();
            slot.evaluated = None;
        }
        if self.fit_spatial(
            &mut slot.spatial,
            &mut slot.pool,
            device,
            chain.last,
            shape.boundary,
            origin,
        )? {
            slot.evaluated = None;
        }
        let mut changed = slot.evaluated != Some(pipeline_id);
        if let Some(spatial) = slot.spatial.as_ref()
            && !spatial.bound(pipeline_id, &slot.pool)
        {
            changed = true;
        }
        if slot.boundary_version != Some(plan.boundary.version) {
            match &plan.boundary.derived {
                // Derived from the source the pipeline holds, all of it uploaded (above): one pass
                // writes the slot's boundary texture, submitted at once, ahead of the chain's, so
                // the slot holds what it says it holds whatever this frame does next.
                Some((_, derivation)) => {
                    let (source, layouts) = self
                        .gpu
                        .source
                        .as_ref()
                        .zip(self.gpu.layouts.as_ref())
                        .ok_or(GpuFallback::SourceMissing)?;
                    let size = plan.boundary.size();
                    let words = source
                        .words(derivation, size)
                        .ok_or(GpuFallback::PipelineFailed)?;
                    slot.fit_derivation(self, device, queue, &words)?;
                    let target = slot
                        .boundary
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    let mut derive =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("luxforge.gpu_source.derive_encoder"),
                        });
                    source.encode(
                        device,
                        &mut derive,
                        layouts,
                        derivation,
                        &slot.derivation.as_ref().expect("fitted").buffer,
                        &target,
                        size,
                    );
                    queue.submit([derive.finish()]);
                    self.figures.preview.derived.fetch_add(1, Ordering::Relaxed);
                    slot.uploading = None;
                }
                None => {
                    // A frame's chunks of it, from the row the last frame reached.
                    let first = slot
                        .uploading
                        .filter(|(version, _)| *version == plan.boundary.version)
                        .map_or(0, |(_, row)| row);
                    let limit = self
                        .figures
                        .preview
                        .upload_per_frame
                        .load(Ordering::Acquire);
                    let (next, staged) =
                        upload_rows(queue, &slot.boundary, &plan.boundary, first, limit);
                    self.figures
                        .preview
                        .staged
                        .fetch_add(staged, Ordering::AcqRel);
                    if next < plan.boundary.height {
                        slot.uploading = Some((plan.boundary.version, next));
                        let row_bytes = u64::from(plan.boundary.width)
                            * plan.boundary.format.texel_bytes() as u64;
                        return Err(GpuFallback::BoundaryUploading {
                            uploaded: u64::from(next) * row_bytes,
                            bytes: plan.boundary.bytes(),
                        });
                    }
                    slot.uploading = None;
                }
            }
            slot.boundary_version = Some(plan.boundary.version);
            changed = true;
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.gpu_preview.encoder"),
        });
        // An incremental tick: the slot holds the values of the plan this one changes, every link
        // and the output, over this boundary, so each link evaluates again only where its input's
        // changes reach. Another boundary is other texels everywhere, whatever the plans' change.
        // The change is in the boundary's stage; the boundary's texel `(0, 0)` is its origin.
        let size = shape.boundary;
        let incremental = change
            .and_then(|change| change.since)
            .filter(|(since, _)| {
                slot.evaluated_serial == Some((*since, plan.boundary.version))
                    && slot.evaluated == Some(pipeline_id)
                    && slot.input_key.is_some()
            })
            .map(|(_, [x0, y0, x1, y1])| {
                let texel = |at: u32, origin: u32, limit: u32| at.saturating_sub(origin).min(limit);
                spatial::Rect {
                    x0: texel(x0, origin.0, size.0),
                    y0: texel(y0, origin.1, size.1),
                    x1: texel(x1, origin.0, size.0),
                    y1: texel(y1, origin.1, size.1),
                }
            });
        // Each link before the last, from the boundary: run only when what it would write is not
        // what its intermediate holds, and on an incremental tick only where its input's changes
        // reach, which grows link by link with each spatial step's neighbourhood.
        let mut input = chain::boundary_key(plan.boundary.version);
        let mut encoded = false;
        let mut dirty = incremental;
        for (index, steps) in chain.links.iter().enumerate() {
            let (compiled, id) = &pipelines[index];
            chain::pack_words(plan.texels, (0, 0), steps, link_words);
            self.fit_link(
                slot,
                device,
                index,
                link_words.len(),
                blocks::block_len(steps),
            )?;
            let link = &mut slot.chain[index];
            let update = link.write(queue, link_words, steps);
            self.figures.preview.blocks_updated(&update);
            if self.fit_spatial(
                &mut link.spatial,
                &mut slot.pool,
                device,
                steps,
                shape.boundary,
                origin,
            )? {
                link.forget(&mut slot.pool);
            }
            let (ran, dispatched, reached) = link.encode(
                device,
                queue,
                &mut encoder,
                (compiled, *id),
                &mut slot.pool,
                steps,
                link_words,
                input,
                (plan.texels, shape.boundary),
                dirty,
            );
            // The next link's input changed where this one evaluated again; its own steps' changes
            // are the plan's, which every link's rectangle holds.
            dirty = incremental
                .zip(reached)
                .map(|(changed, reached)| changed.union(&reached));
            encoded |= ran;
            self.figures
                .preview
                .spatial_passes
                .fetch_add(dispatched, Ordering::Relaxed);
            input = link.key().unwrap_or(input);
        }
        changed |= slot.input_key != Some(input);
        if slot.written_words != words {
            // The tick's one write: the header and every program's words.
            queue.write_buffer(&slot.words.buffer, 0, &le_bytes(words));
            slot.written_words.clear();
            slot.written_words.extend_from_slice(words);
            changed = true;
        }
        // Only the chunks of the blocks that changed: a painted stroke's tick writes its new
        // segments and the index after them, not the segments its block already holds, and a block
        // handed again at the same place — a warp's grid — is not even compared.
        let update = slot
            .written_blocks
            .write(queue, &slot.blocks.buffer, chain.last, BLOCK_CHUNK);
        self.figures.preview.blocks_updated(&update);
        changed |= update.changed;
        let blocks = slot.written_blocks.words();
        if changed {
            // The last link's spatial step's compute passes fill its planes first, from its
            // input: only the passes this tick changes, over what its mask needs.
            let mut reached = dirty;
            if let Some(spatial) = slot.spatial.as_mut() {
                let (dispatched, over) = spatial.tick(
                    device,
                    queue,
                    &mut encoder,
                    (&pipeline.spatial, pipeline_id),
                    &slot.bindings,
                    &mut slot.pool,
                    chain.last,
                    (words, blocks),
                    input,
                    (plan.texels, shape.boundary),
                    dirty,
                );
                reached = dirty.zip(over).map(|(dirty, over)| dirty.union(&over));
                self.figures
                    .preview
                    .spatial_passes
                    .fetch_add(dispatched, Ordering::Relaxed);
            }
            let groups = slot.spatial.as_ref().and_then(|spatial| spatial.groups());
            let planes = groups.and_then(|groups| groups.fragment.as_ref());
            // The frame and, where the bucket has room, one more column and row: the edge
            // texels again, which the linear filter reads across the frame's edge as it reads the
            // photograph's.
            let (width, height) = shape.output;
            let (columns, rows) = slot.output.capacity;
            let frame = (
                (width + 1).min(columns) as f32,
                (height + 1).min(rows) as f32,
            );
            match (&slot.intermediate, &pipeline.tail) {
                // A plan with a geometry tail runs its content steps into the intermediate first,
                // over exactly the boundary's texels, and its tail reads them. An identity tail
                // takes each output pixel from its own stage pixel's texel, so an incremental tick
                // draws both passes only where the changes reached; any other resamples, and
                // draws whole.
                (Some(intermediate), Some(tail)) => {
                    let (width, height) = shape.boundary;
                    let identity = plan
                        .steps
                        .iter()
                        .any(|step| matches!(step, GpuStep::Geometry(tail) if tail.identity()));
                    let scissors = reached.filter(|_| identity).map(|reached| {
                        let grown = reached.grown(1, size);
                        // Output pixel `q` is the stage pixel `q` plus the region's origin, and the
                        // texel the stage pixel less the boundary's origin.
                        let (dx, dy) = output_offset(plan);
                        let origin = plan.texels.origin.map(|value| value.max(0.0) as u32);
                        let bound = (frame.0 as u32, frame.1 as u32);
                        let to = |at: u32, origin: u32, offset: u32, limit: u32| {
                            (at + origin).saturating_sub(offset).min(limit)
                        };
                        let output = spatial::Rect {
                            x0: to(grown.x0, origin[0], dx, bound.0),
                            y0: to(grown.y0, origin[1], dy, bound.1),
                            x1: to(grown.x1, origin[0], dx, bound.0),
                            y1: to(grown.y1, origin[1], dy, bound.1),
                        };
                        // The edge texels repeated past the frame follow the edge they repeat.
                        let (columns, rows) = shape.output;
                        let output = spatial::Rect {
                            x1: if output.x1 >= columns {
                                bound.0
                            } else {
                                output.x1
                            },
                            y1: if output.y1 >= rows {
                                bound.1
                            } else {
                                output.y1
                            },
                            ..output
                        };
                        (grown, output)
                    });
                    encode_pass_over(
                        &mut encoder,
                        &intermediate.target,
                        &pipeline.render,
                        (&slot.bindings, planes),
                        (width as f32, height as f32),
                        scissors.map(|(content, _)| content),
                    );
                    encode_pass_over(
                        &mut encoder,
                        &slot.target,
                        tail,
                        (&intermediate.bindings, None),
                        frame,
                        scissors.map(|(_, output)| output),
                    );
                }
                // With no tail the output's pixel is the boundary's texel less the region's
                // offset: an incremental tick draws only where the changes reached, and the edge
                // column and row past the frame with them.
                (None, None) => {
                    let scissor = reached.map(|reached| {
                        let (dx, dy) = output_offset(plan);
                        let bound = (frame.0 as u32, frame.1 as u32);
                        let grown = reached.grown(1, size);
                        spatial::Rect {
                            x0: grown.x0.saturating_sub(dx).min(bound.0),
                            y0: grown.y0.saturating_sub(dy).min(bound.1),
                            x1: grown.x1.saturating_sub(dx).min(bound.0),
                            y1: grown.y1.saturating_sub(dy).min(bound.1),
                        }
                    });
                    let scissor = scissor.map(|rect| {
                        // The edge texels repeated past the frame follow the edge they repeat.
                        let (width, height) = shape.output;
                        spatial::Rect {
                            x1: if rect.x1 >= width {
                                (width + 1).min(frame.0 as u32)
                            } else {
                                rect.x1
                            },
                            y1: if rect.y1 >= height {
                                (height + 1).min(frame.1 as u32)
                            } else {
                                rect.y1
                            },
                            ..rect
                        }
                    });
                    encode_pass_over(
                        &mut encoder,
                        &slot.target,
                        &pipeline.render,
                        (&slot.bindings, planes),
                        frame,
                        scissor,
                    );
                }
                _ => return Err(GpuFallback::PipelineFailed),
            }
        }
        // The slot now holds this plan's values, whatever ran to make them so.
        slot.evaluated_serial = change.map(|change| (change.serial, plan.boundary.version));
        if changed || encoded {
            // Submitted now, ahead of the frame's own submission, whose draw samples the output;
            // the queue's writes above are flushed with it. Nothing waits for it.
            queue.submit([encoder.finish()]);
            slot.passes += 1;
            slot.clock.follow(queue, slot.passes, started);
            slot.evaluated = Some(pipeline_id);
            slot.input_key = Some(input);
            slot.output.version = plan.boundary.version;
            slot.frame_us = started.elapsed().as_micros() as u64;
            self.figures.preview.passes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(plan.boundary.version)
    }

    /// A slot of `shape`, charged before anything is created, over `kept` when it is given: a
    /// boundary texture of the shape's size and format and the version it holds whole.
    fn allocate(
        &self,
        device: &wgpu::Device,
        shape: Shape,
        word_bytes: u64,
        block_bytes: u64,
        kept: Option<(wgpu::Texture, u64)>,
    ) -> Result<GpuSlot, GpuFallback> {
        let limit = device.limits().max_texture_dimension_2d;
        let (width, height) = shape.boundary;
        let (output_width, output_height) = shape.output;
        // The output is reserved in the size bucket of the CPU frame it stands in for; the last
        // pass writes its edge column and row into the border as an upload copies them.
        let capacity = shape.capacity(limit);
        let output_bytes = u64::from(capacity.0) * u64::from(capacity.1) * 4;
        let texture_bytes = shape.texture_bytes(limit);
        let (words, blocks) = (
            buffer_capacity(device, word_bytes)?,
            buffer_capacity(device, block_bytes)?,
        );
        self.figures
            .preview
            .charge(texture_bytes + words + blocks)?;
        let texture =
            |label, (width, height), format, usage, view_formats: &[wgpu::TextureFormat]| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats,
                })
            };
        let (boundary, boundary_version) = match kept {
            Some((texture, version)) => (texture, Some(version)),
            None => (
                texture(
                    "luxforge.gpu_preview.boundary",
                    (width, height),
                    shape.format.texture(),
                    // Uploaded from the CPU, or written by the pass that derives it from the
                    // source the pipeline holds.
                    wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    &[],
                ),
                None,
            ),
        };
        // Written through a Unorm view as the codes the last pass computes, and sampled through an
        // sRGB-typed view, as the photograph's textures are.
        let output = texture(
            "luxforge.gpu_preview.output",
            capacity,
            OUTPUT_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            &[SAMPLED_FORMAT],
        );
        let target = output.create_view(&wgpu::TextureViewDescriptor::default());
        let sampled = output.create_view(&wgpu::TextureViewDescriptor {
            format: Some(SAMPLED_FORMAT),
            ..wgpu::TextureViewDescriptor::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.gpu_preview.uniform"),
            size: UNIFORM_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Drawn as a photograph tile is drawn: the same layout, uniform and linear sampler.
        let photo_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_preview.photo_bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&sampled),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear),
                },
            ],
        });
        let layout = TileLayout {
            content: [0, 0, output_width, output_height],
            texels: [0, 0, output_width, output_height],
        };
        let picture = Picture {
            tiles: vec![Tile {
                layout,
                capacity,
                texture: output,
                uniform,
                written_uniform: std::sync::Mutex::new(None),
                bindings: photo_bindings,
            }],
            width: output_width,
            height: output_height,
            capacity,
            grid: (1, 1),
            limit,
            version: 0,
            content_id: None,
            region_key: None,
            allocated_bytes: output_bytes,
            // Evaluated at the boundary's own size, about the displayed size at Fit and below 100%:
            // never minified far enough to need a chain.
            mip_levels: 1,
            mip_bytes: 0,
            mips_current: false,
        };
        let words = Charged {
            buffer: storage_buffer(device, "luxforge.gpu_preview.words", words),
            bytes: words,
        };
        let blocks = Charged {
            buffer: storage_buffer(device, "luxforge.gpu_preview.blocks", blocks),
            bytes: blocks,
        };
        let bindings = self.program_bindings(device, &boundary, &words.buffer, &blocks.buffer);
        let intermediate = shape.intermediate.map(|format| {
            let texture = texture(
                "luxforge.gpu_preview.intermediate",
                (width, height),
                format,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                &[],
            );
            let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bindings = self.program_bindings(device, &texture, &words.buffer, &blocks.buffer);
            Intermediate {
                texture,
                target,
                bindings,
            }
        });
        Ok(GpuSlot {
            shape,
            boundary,
            intermediate,
            boundary_version,
            uploading: None,
            output: picture,
            target,
            words,
            blocks,
            derivation: None,
            bindings,
            texture_bytes,
            written_words: Vec::new(),
            written_blocks: blocks::WrittenBlocks::default(),
            evaluated: None,
            spatial: None,
            chain: Vec::new(),
            pool: spatial::Pool::default(),
            input_key: None,
            evaluated_serial: None,
            frame_us: 0,
            passes: 0,
            clock: Arc::default(),
        })
    }

    /// Make `held` hold the planes `steps`' spatial steps write over a boundary of `size` at stage
    /// `origin`, charged before anything is created: none for steps without one, and new ones when
    /// the planes or the boundary they cover change, the old ones retiring with their charge. A
    /// link holds its kept textures and its passes' parameters; its scratch planes are `pool`'s,
    /// which the slot has fitted to every link already, and its schedule's holder is drawn from
    /// it. Answers whether the planes changed, so what they held is no longer known.
    fn fit_spatial(
        &self,
        held: &mut Option<Box<SpatialSlot>>,
        pool: &mut spatial::Pool,
        device: &wgpu::Device,
        steps: &[GpuStep],
        size: (u32, u32),
        origin: (u32, u32),
    ) -> Result<bool, GpuFallback> {
        let key = spatial::PlanesKey::of(steps, size, origin);
        if held.as_ref().map(|spatial| &spatial.planes.key) == key.as_ref() {
            return Ok(false);
        }
        if let Some(old) = held.take() {
            let bytes = old.planes.bytes;
            self.retire_preview(Held::Planes(old), bytes);
        }
        if let Some(key) = key {
            self.figures.preview.charge(key.kept_bytes())?;
            *held = Some(Box::new(SpatialSlot::new(
                spatial::Planes::create(device, key),
                pool,
            )));
        }
        Ok(true)
    }

    /// Make `slot`'s pool hold the scratch textures every link of `chain` takes in turn over a
    /// boundary of `size` at stage `origin` ([`spatial::PoolKey`]), before any link's planes: what
    /// the plan needs beyond what the pool holds is charged before it is created; what it no
    /// longer needs, or all of it for another boundary size or origin, retires with its charge
    /// and bumps the pool's generation. A refusal leaves what was fitted before it held, for the
    /// slot's release to retire.
    fn fit_pool(
        &self,
        slot: &mut GpuSlot,
        device: &wgpu::Device,
        chain: &chain::Chain<'_>,
        size: (u32, u32),
        origin: (u32, u32),
    ) -> Result<(), GpuFallback> {
        let links = chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last));
        let key = spatial::PoolKey::of(links, size, origin);
        let preview = &self.figures.preview;
        slot.pool.fit(
            device,
            &key,
            &mut |bytes| {
                preview.charge(bytes)?;
                preview.scratch.fetch_add(bytes, Ordering::AcqRel);
                Ok(())
            },
            &mut |textures, bytes| self.retire_preview(Held::Pool(textures), bytes),
        )
    }

    /// Make `slot` hold `count` links before its last, each with an intermediate of the boundary's
    /// size and format, charged before it is created; and bind the last link to the last of them.
    fn fit_chain(
        &self,
        slot: &mut GpuSlot,
        device: &wgpu::Device,
        count: usize,
    ) -> Result<(), GpuFallback> {
        if slot.chain.len() == count {
            return Ok(());
        }
        while slot.chain.len() > count {
            if let Some(link) = slot.chain.pop() {
                let bytes = link.bytes();
                self.retire_preview(Held::Link(Box::new(link)), bytes);
            }
        }
        let (width, height) = slot.shape.boundary;
        let format = intermediate_format(slot.shape.format);
        let texture_bytes = intermediate_bytes(slot.shape.boundary, slot.shape.format);
        while slot.chain.len() < count {
            let (words, blocks) = (
                buffer_capacity(device, MIN_BUFFER)?,
                buffer_capacity(device, MIN_BUFFER)?,
            );
            self.figures
                .preview
                .charge(texture_bytes + words + blocks)?;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("luxforge.gpu_preview.link"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let words = Charged {
                buffer: storage_buffer(device, "luxforge.gpu_preview.link_words", words),
                bytes: words,
            };
            let blocks = Charged {
                buffer: storage_buffer(device, "luxforge.gpu_preview.link_blocks", blocks),
                bytes: blocks,
            };
            let bindings =
                self.program_bindings(device, slot.last_input(), &words.buffer, &blocks.buffer);
            slot.chain.push(chain::LinkSlot::new(
                texture,
                words,
                blocks,
                bindings,
                texture_bytes,
            ));
        }
        slot.bindings = self.program_bindings(
            device,
            slot.last_input(),
            &slot.words.buffer,
            &slot.blocks.buffer,
        );
        slot.evaluated = None;
        slot.input_key = None;
        Ok(())
    }

    /// Make link `index` of `slot`'s chain hold buffers large enough for `words` and `blocks` words,
    /// rebinding it to what it reads when they grow.
    fn fit_link(
        &self,
        slot: &mut GpuSlot,
        device: &wgpu::Device,
        index: usize,
        words: usize,
        blocks: usize,
    ) -> Result<(), GpuFallback> {
        let (before, rest) = slot.chain.split_at_mut(index);
        let input = before.last().map_or(&slot.boundary, |link| &link.texture);
        let link = &mut rest[0];
        let mut rebind = false;
        for (charged, bytes, label) in [
            (
                &mut link.words,
                (words * 4) as u64,
                "luxforge.gpu_preview.link_words",
            ),
            (
                &mut link.blocks,
                (blocks * 4) as u64,
                "luxforge.gpu_preview.link_blocks",
            ),
        ] {
            if bytes > charged.bytes {
                let capacity = buffer_capacity(device, bytes)?;
                self.figures.preview.charge(capacity)?;
                let old = std::mem::replace(
                    charged,
                    Charged {
                        buffer: storage_buffer(device, label, capacity),
                        bytes: capacity,
                    },
                );
                self.retire_preview(Held::Buffer(old.buffer), old.bytes);
                rebind = true;
            }
        }
        if rebind {
            link.bindings =
                self.program_bindings(device, input, &link.words.buffer, &link.blocks.buffer);
            link.forget(&mut slot.pool);
        }
        Ok(())
    }

    /// The programs' bindings: the words, the blocks and the boundary.
    fn program_bindings(
        &self,
        device: &wgpu::Device,
        boundary: &wgpu::Texture,
        words: &wgpu::Buffer,
        blocks: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let boundary = boundary.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.gpu_preview.bindings"),
            layout: &self.gpu.support.as_ref().expect("a supported stage").layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: words.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: blocks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&boundary),
                },
            ],
        })
    }

    fn retire_slot(&self, slot: GpuSlot) {
        let bytes = slot.bytes();
        self.retire_preview(Held::Slot(Box::new(slot)), bytes);
    }

    /// Hand `held` to the retirement worker, still charged, as a photograph texture is handed; its
    /// scratch textures stay counted as scratch until then too.
    fn retire_preview(&self, held: Held, bytes: u64) {
        self.figures
            .retirement_pending
            .fetch_add(1, Ordering::AcqRel);
        let scratch = match &held {
            Held::Slot(slot) => slot.pool.bytes(),
            Held::Pool(_) => bytes,
            Held::Buffer(_) | Held::Planes(_) | Held::Link(_) | Held::Source(_) => 0,
        };
        if let Err(error) = self
            .retirement_sender
            .send(super::Retired::Preview(RetiredPreview {
                held,
                bytes,
                scratch,
            }))
            && let super::Retired::Preview(retired) = error.0
        {
            // The worker has ended with the device: its resources are gone with it.
            finish_retirement(&self.figures, retired, true);
        }
    }

    /// Compile `steps` on the compile thread and wait until it has finished, as a frame after the
    /// compile would find it: what a test that is not about compiling does before it draws.
    #[cfg(test)]
    pub(super) fn compile_now(&mut self, device: &wgpu::Device, plan: &GpuPlan) {
        let figures = Arc::clone(&self.figures);
        let format = plan.boundary.format();
        let _ = self
            .gpu
            .chain_pipelines(device, &plan.steps, format, &figures.preview);
        let sequences: Vec<compile::Sequence> = link_sequences(&plan.steps, format)
            .map(|(link, format)| (link.to_vec(), format))
            .collect();
        luxforge_testbase::wait_until("the sequence's compile", || {
            sequences
                .iter()
                .all(|(steps, format)| !self.gpu.pipelines.compiling(steps, *format))
        });
    }

    /// Simulate the device's loss through the handler its lost callback runs.
    #[cfg(test)]
    pub(super) fn simulate_device_loss(&self) {
        device_lost(&self.gpu.lost);
    }

    /// Run the GPU stage with `budget` bytes instead of the recorded default.
    #[cfg(test)]
    pub(super) fn set_gpu_budget(&self, budget: u64) {
        self.figures.preview.budget.store(budget, Ordering::Release);
    }

    /// Start each link's passes from NaN in every texture of its slot's pool, every record
    /// forgotten, while `poisoned` ([`spatial::Pool::poison`]): a frame still equal to a fresh
    /// evaluation shows that no pass read scratch it did not write in that tick.
    #[cfg(test)]
    pub(super) fn set_scratch_poison(&self, poisoned: bool) {
        self.figures
            .preview
            .poison
            .store(poisoned, Ordering::Release);
    }

    /// Upload at most `bytes` of a new boundary a frame instead of [`UPLOAD_PER_FRAME`].
    #[cfg(test)]
    pub(super) fn set_upload_per_frame(&self, bytes: u64) {
        self.figures
            .preview
            .upload_per_frame
            .store(bytes, Ordering::Release);
    }

    /// A pipeline whose device has no adapter able to run the stage, for the photograph's draw.
    #[cfg(test)]
    pub(super) fn without_gpu_stage(mut self) -> Self {
        self.gpu.support = None;
        self
    }
}

/// What a slot holding `plan` charges the budget on `device`, as `allocate`, `fit_chain`,
/// `fit_pool`, `fit_link` and `fit_spatial` charge it: the chain's charge ([`chain_charge`]) —
/// each earlier link's intermediate, every link's kept planes and parameters and the pool once —
/// beside what needs the device: the boundary, the output in its size bucket and its placement
/// uniform, and every link's words and blocks buffers at their capacities. For a report and the
/// tests that hold it to the slot's own figure.
#[cfg(any(test, feature = "qualification"))]
pub(super) fn slot_charge(device: &wgpu::Device, plan: &GpuPlan) -> Result<u64, GpuFallback> {
    let shape = Shape::of(plan);
    let limit = device.limits().max_texture_dimension_2d;
    let origin = (
        plan.texels.origin[0].max(0.0) as u32,
        plan.texels.origin[1].max(0.0) as u32,
    );
    Ok(shape.texture_bytes(limit)
        + slot_buffers(device, plan)?.iter().sum::<u64>()
        + chain_charge(&plan.steps, shape.boundary, origin, shape.format).total())
}

/// Each link's words and blocks buffers together, at the capacities a slot holding `plan` gives
/// them on `device`, in chain order, the last link's last: the part of [`slot_charge`] beside the
/// textures and the chain's charge.
#[cfg(any(test, feature = "qualification"))]
pub(super) fn slot_buffers(device: &wgpu::Device, plan: &GpuPlan) -> Result<Vec<u64>, GpuFallback> {
    let chain = chain::chain(&plan.steps);
    let buffers = |steps: &[GpuStep], offset: (u32, u32)| {
        let (mut words, mut blocks) = (Vec::new(), Vec::new());
        chain::pack_steps(plan.texels, offset, steps, &mut words, &mut blocks);
        Ok::<u64, GpuFallback>(
            buffer_capacity(device, (words.len() * 4) as u64)?
                + buffer_capacity(device, (blocks.len() * 4) as u64)?,
        )
    };
    chain
        .links
        .iter()
        .map(|link| buffers(link, (0, 0)))
        .chain(std::iter::once(buffers(chain.last, output_offset(plan))))
        .collect()
}

/// What a chain's planes and intermediates take of the GPU-preview budget with its links' scratch
/// planes in one pool ([`chain_charge`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainCharge {
    /// Each link's intermediate, every link's but the last's, in chain order.
    pub intermediates: Vec<u64>,
    /// Each link's kept planes and its passes' parameter slices, in chain order, the last link's
    /// last: zero for a link without a spatial step.
    pub kept: Vec<u64>,
    /// The pool of scratch planes every link's spatial step takes in turn, counted once.
    pub pool: u64,
}

impl ChainCharge {
    /// Everything it counts.
    pub fn total(&self) -> u64 {
        self.intermediates.iter().sum::<u64>() + self.kept.iter().sum::<u64>() + self.pool
    }
}

/// What the chain of `steps` takes of the GPU-preview budget over a boundary of `size` texels in
/// `format` whose texel `(0, 0)` is stage pixel `origin`, its links' scratch planes laid out in one
/// pool ([`spatial::PoolKey`]): each link's intermediate before the last, the boundary's texels in
/// the format a chain's intermediates take; each link's kept planes and its passes' parameter
/// slices ([`spatial::PlanesKey`]); and the pool, once. A link without a spatial step, such as the
/// colour steps before the first one, adds only its intermediate. The boundary, the output with its
/// placement uniform, and the words and blocks buffers, which need the device's limits, are the
/// slot's beside it ([`texture_charge`]). It creates nothing.
pub fn chain_charge(
    steps: &[GpuStep],
    size: (u32, u32),
    origin: (u32, u32),
    format: BoundaryFormat,
) -> ChainCharge {
    let chain = chain::chain(steps);
    let links = || {
        chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last))
    };
    ChainCharge {
        intermediates: vec![intermediate_bytes(size, format); chain.links.len()],
        kept: links()
            .map(|link| {
                spatial::PlanesKey::of(link, size, origin).map_or(0, |key| key.kept_bytes())
            })
            .collect(),
        pool: spatial::PoolKey::of(links(), size, origin).bytes(),
    }
}

/// `words` as the little-endian bytes the GPU reads.
fn le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// Queue rows of the boundary's texels from `first` straight from the caller's buffer, in bounded
/// chunks of rows, until `limit` bytes are written or the boundary is: the row it reached, and the
/// bytes written. A resident boundary writes nothing.
fn upload_rows(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    boundary: &GpuBoundary,
    first: u32,
    limit: u64,
) -> (u32, u64) {
    let Some(texels) = &boundary.texels else {
        return (first, 0);
    };
    let row_bytes = u64::from(boundary.width) * boundary.format.texel_bytes() as u64;
    let rows_per_chunk = (UPLOAD_CHUNK / row_bytes).max(1) as u32;
    let mut row = first;
    let mut written = 0;
    while row < boundary.height && (written == 0 || written < limit) {
        let rows = rows_per_chunk
            .min(boundary.height - row)
            .min(((limit - written) / row_bytes).max(1) as u32);
        let mut destination = texture.as_image_copy();
        destination.origin.y = row;
        queue.write_texture(
            destination,
            (**texels).as_ref(),
            wgpu::TexelCopyBufferLayout {
                offset: u64::from(row) * row_bytes,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: boundary.width,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        row += rows;
        written += u64::from(rows) * row_bytes;
    }
    (row, written)
}

mod blocks;
mod chain;
mod compile;
mod source;
pub(super) use compile::GpuOptions;
pub use source::{AxisCoverage, Derivation, GpuSource, Reduction, SourceFigures, SourceKind};
use source::{Layouts as SourceLayouts, SourceSlot};
mod stage;
pub use compile::{GpuWarm, PIPELINE_CACHE};
pub(super) use stage::gpu_stage_refused;
pub use stage::{GpuStageState, refuse_gpu_stage};
mod tail;
pub use tail::{GpuTail, OutputEncoding, install_output_encoding, output_encoding};
mod mask;
mod position;
pub use mask::{Coverage, CoverageComponent, CoverageMode, MaskedColour};
pub use position::PositionMap;

pub mod spatial;
pub use spatial::{
    GpuApply, GpuPass, GpuPlane, GpuSpatial, PASS_INPUTS, PassShape, PlaneFormat, PlaneSize,
    SPATIAL_PRELUDE,
};

mod dissolve;
pub use dissolve::{DISSOLVE_DURATION, Dissolve, DrawnDissolve};
pub(crate) use dissolve::{DissolveFrame, dissolving, photo_uniform};

mod timing;
pub(crate) use timing::PassClock;

mod clipping;
pub use clipping::ClipMarks;
pub mod histogram;

#[cfg(any(test, feature = "qualification"))]
pub mod qualification;

#[cfg(test)]
mod compile_tests;
#[cfg(test)]
mod tail_tests;
#[cfg(test)]
mod tests;
