//! The GPU stage: a preview plan evaluated over a held input boundary in the frame that draws it.
//!
//! The plan is plain data — WGSL text, uniform words, storage words and an `rgba16float` boundary —
//! so this crate still names no core type. [`GpuPlan`] is what a caller hands the photograph's
//! surface with [`PhotoSurface::gpu_preview`](super::PhotoSurface::gpu_preview); `prepare` then
//! uploads the boundary when its version changes, writes the tick's words with one
//! `queue.write_buffer`, encodes one pass that runs the programs into the surface's GPU-preview
//! output texture and submits it. `draw` samples that texture exactly as it samples the photograph,
//! through the same placement, snapping and filter; the output is held in the photograph's own
//! size bucket with its edge texels repeated past the frame, so a GPU frame and the CPU frame of the
//! same codes draw identically. The UI thread only encodes commands: it never waits on the GPU,
//! reads a pixel back or touches one on the CPU.
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
//! part of the key, so a tick never compiles.
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
//! Shader compilation is checked without waiting: the assembled WGSL is validated with the `naga`
//! that `wgpu` itself uses before a module is created, and pipeline creation runs inside error
//! scopes whose answers wgpu's native backends give immediately; they are polled once and never
//! awaited. A first sequence's pipeline is still compiled on the UI thread inside `prepare`.

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

/// The GPU-preview budget's recorded default: every GPU-preview texture and buffer, resident or
/// retiring, of every surface together. A Fit boundary of 8 MP is 64 MiB at 8 bytes a texel and its
/// output 32 MiB, so one replacement may overlap the slot it replaces.
pub const GPU_PREVIEW_BUDGET: u64 = 256 * 1024 * 1024;

/// How many compiled program sequences, failed ones included, a pipeline keeps.
pub const PIPELINE_CACHE: usize = 8;

/// The words before any step's: the texel map's origin and step.
const MAP_WORDS: usize = 4;

/// Each step's header words: its program's two base indices and its position map.
const STEP_WORDS: usize = 2 + PositionMap::WORDS;

/// A words or blocks buffer is never smaller than this, so a plan's first few ticks do not each
/// outgrow the last one's buffer.
const MIN_BUFFER: u64 = 1024;

/// The upload chunk, as the photograph's: the surface stages no copy of its own.
const UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;

/// The format the programs' output is written in and sampled from: the sRGB-typed format the
/// surface's photograph textures have when the renderer gamma corrects.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

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
}

impl GpuStep {
    /// A colour step whose `pos` is the stage pixel the plan's texel map gives a boundary texel.
    pub fn colour(program: GpuProgram) -> Self {
        Self::Colour {
            program,
            position: PositionMap::IDENTITY,
        }
    }

    fn kind(&self) -> StepKind {
        match self {
            Self::Colour { .. } => StepKind::Colour,
        }
    }

    pub(super) fn program(&self) -> &GpuProgram {
        match self {
            Self::Colour { program, .. } => program,
        }
    }

    fn position(&self) -> PositionMap {
        match self {
            Self::Colour { position, .. } => *position,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepKind {
    Colour,
}

/// The held input boundary: `rgba16float` texels of scene-linear sRGB, eight bytes each, rows
/// top to bottom, every half float little-endian. The version decides whether it is uploaded
/// again, as a [`Frame`](super::Frame)'s does: it changes whenever the texels do and never
/// otherwise.
#[derive(Clone)]
pub struct GpuBoundary {
    texels: Arc<dyn AsRef<[u8]> + Send + Sync>,
    width: u32,
    height: u32,
    version: u64,
}

impl std::fmt::Debug for GpuBoundary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuBoundary")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("version", &self.version)
            .finish()
    }
}

impl GpuBoundary {
    /// Bytes per texel: four half floats.
    pub const TEXEL_BYTES: usize = 8;

    /// A boundary of `width` × `height` texels held in `texels` as they are, or `None` when the
    /// buffer does not hold exactly that many. Nothing is copied.
    pub fn new<P: AsRef<[u8]> + Send + Sync + 'static>(
        texels: Arc<P>,
        width: u32,
        height: u32,
        version: u64,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(Self::TEXEL_BYTES)?;
        (width > 0 && height > 0 && (*texels).as_ref().len() == expected).then(|| Self {
            texels,
            width,
            height,
            version,
        })
    }

    /// A boundary of `width` × `height` texels from linear RGBA values in row order, each rounded
    /// to the nearest half float, or `None` when `pixels` does not yield exactly that many. This is
    /// frame work: a caller runs it on a worker, never the UI thread.
    pub fn from_linear(
        width: u32,
        height: u32,
        version: u64,
        pixels: impl IntoIterator<Item = [f32; 4]>,
    ) -> Option<Self> {
        let count = (width as usize).checked_mul(height as usize)?;
        // Zeroed, so its pages are faulted in by the pass that writes them.
        let mut texels = vec![0u8; count.checked_mul(Self::TEXEL_BYTES)?];
        let mut written = 0;
        for pixel in pixels {
            let texel =
                texels.get_mut(written * Self::TEXEL_BYTES..(written + 1) * Self::TEXEL_BYTES)?;
            for (bytes, value) in texel.chunks_exact_mut(2).zip(pixel) {
                bytes.copy_from_slice(&half::f16::from_f32(value).to_bits().to_le_bytes());
            }
            written += 1;
        }
        (written == count).then_some(())?;
        Self::new(Arc::new(texels), width, height, version)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn version(&self) -> u64 {
        self.version
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
}

impl GpuFallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoAdapter => "no-adapter",
            Self::DeviceLost => "device-lost",
            Self::PipelineFailed => "pipeline-failed",
            Self::BudgetExceeded { .. } => "budget-exceeded",
            Self::TextureLimit { .. } => "texture-limit",
            Self::BufferLimit { .. } => "buffer-limit",
        }
    }
}

/// What one pipeline counts of its GPU-preview work, beside its photo-texture figures.
#[derive(Default)]
pub(super) struct Figures {
    budget: AtomicU64,
    in_use: AtomicU64,
    peak: AtomicU64,
    passes: AtomicU64,
    compiles: AtomicU64,
}

impl Figures {
    pub(super) fn budget(&self) -> u64 {
        self.budget.load(Ordering::Acquire)
    }

    pub(super) fn in_use(&self) -> u64 {
        self.in_use.load(Ordering::Acquire)
    }

    pub(super) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
    }

    pub(super) fn passes(&self) -> u64 {
        self.passes.load(Ordering::Acquire)
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

    fn discharge(&self, bytes: u64) {
        self.in_use.fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// A buffer and the bytes charged for it.
struct Charged {
    buffer: wgpu::Buffer,
    bytes: u64,
}

/// What retires through the surface's retirement worker: a whole slot, or a buffer a slot outgrew.
/// Never read: it is held until the GPU is done with it, then dropped.
#[allow(dead_code)]
enum Held {
    Slot(Box<GpuSlot>),
    Buffer(wgpu::Buffer),
}

/// A GPU-preview resource on its way out, with its charge, which ends when the GPU is done with it.
pub(super) struct RetiredPreview {
    held: Held,
    bytes: u64,
}

/// Ends one retirement: the resources go and their charge with them.
pub(super) fn finish_retirement(figures: &SurfaceFigures, retired: RetiredPreview, failed: bool) {
    let RetiredPreview { held, bytes } = retired;
    drop(held);
    figures.preview.discharge(bytes);
    figures.retirement_pending.fetch_sub(1, Ordering::AcqRel);
    if failed {
        figures.diagnostics().gpu_retirement_failures += 1;
    }
    wake_surface();
}

/// One surface's GPU-preview slot: the boundary, the output the programs write and the photograph's
/// draw samples, and the words and blocks the programs read.
pub(super) struct GpuSlot {
    size: (u32, u32),
    boundary: wgpu::Texture,
    boundary_version: Option<u64>,
    output: Picture,
    target: wgpu::TextureView,
    words: Charged,
    blocks: Charged,
    bindings: wgpu::BindGroup,
    /// The boundary texture, the output texture and its placement uniform.
    texture_bytes: u64,
    /// The words last written, compared with each tick's so an unchanged plan writes nothing.
    written_words: Vec<u32>,
    /// The blocks last written, compared likewise.
    written_blocks: Vec<u32>,
    /// The cached pipeline last run into `output`.
    evaluated: Option<u64>,
    /// The interface thread's time, in microseconds, to prepare what `output` holds: fitting the
    /// slot, writing the words and blocks, uploading a new boundary, encoding and submitting the
    /// pass. The device Iced creates has no timestamp queries, so the GPU's own time is not read.
    frame_us: u64,
}

impl GpuSlot {
    fn bytes(&self) -> u64 {
        self.texture_bytes + self.words.bytes + self.blocks.bytes
    }

    pub(super) fn output(&self) -> &Picture {
        &self.output
    }

    pub(super) fn frame_us(&self) -> u64 {
        self.frame_us
    }
}

/// One compiled — or failed — program sequence.
struct Cached {
    signature: Vec<(StepKind, Cow<'static, str>, Cow<'static, str>)>,
    pipeline: Result<wgpu::RenderPipeline, Arc<str>>,
    id: u64,
    used: u64,
}

impl Cached {
    fn matches(&self, steps: &[GpuStep]) -> bool {
        self.signature.len() == steps.len()
            && self
                .signature
                .iter()
                .zip(steps)
                .all(|((kind, entry, source), step)| {
                    *kind == step.kind()
                        && *entry == step.program().entry
                        && *source == step.program().source
                })
    }
}

/// What the device must offer for the stage to run, made once with the pipeline.
struct Support {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
}

impl Support {
    /// The programs' bind group layout — the words, the blocks and the boundary — and the pipeline
    /// layout over it.
    fn new(device: &wgpu::Device) -> Self {
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
        }
    }
}

/// The stage's state for one pipeline: whether the device can run it, its lost flag, its compiled
/// sequences and the tick's scratch words.
pub(super) struct GpuStage {
    support: Option<Support>,
    lost: Arc<AtomicBool>,
    cache: Vec<Cached>,
    clock: u64,
    words: Vec<u32>,
    blocks: Vec<u32>,
}

/// Whether a device with `limits`, drawing to a target of `format`, can run the stage.
fn supported(limits: &wgpu::Limits, format: wgpu::TextureFormat) -> bool {
    format.is_srgb()
        && limits.max_storage_buffers_per_shader_stage >= 2
        && limits.max_bind_groups >= 1
        && u64::from(limits.max_storage_buffer_binding_size) >= MIN_BUFFER
}

/// What a lost device's callback does, and what a test that simulates one calls.
fn device_lost(lost: &AtomicBool) {
    lost.store(true, Ordering::Release);
    wake_surface();
}

impl GpuStage {
    /// The stage for a pipeline on `device`, drawing to `format`, counting into `figures`. Its lost
    /// flag is the device's lost callback.
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        figures: &Figures,
    ) -> Self {
        figures.budget.store(GPU_PREVIEW_BUDGET, Ordering::Release);
        let lost = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&lost);
        device.set_device_lost_callback(move |_reason, _message| device_lost(&signal));
        let support = supported(&device.limits(), format).then(|| Support::new(device));
        Self {
            support,
            lost,
            cache: Vec::new(),
            clock: 0,
            words: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// The cached pipeline for `steps`, compiled on first use. A failed sequence stays failed.
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        steps: &[GpuStep],
        figures: &Figures,
    ) -> Result<(wgpu::RenderPipeline, u64), GpuFallback> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(cached) = self.cache.iter_mut().find(|cached| cached.matches(steps)) {
            cached.used = clock;
            return match &cached.pipeline {
                Ok(pipeline) => Ok((pipeline.clone(), cached.id)),
                Err(_) => Err(GpuFallback::PipelineFailed),
            };
        }
        let support = self.support.as_ref().ok_or(GpuFallback::NoAdapter)?;
        figures.compiles.fetch_add(1, Ordering::Relaxed);
        let pipeline = compile(device, &support.pipeline_layout, steps, OUTPUT_FORMAT)
            .map_err(Arc::<str>::from);
        if self.cache.len() >= PIPELINE_CACHE
            && let Some(oldest) = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(index, _)| index)
        {
            self.cache.swap_remove(oldest);
        }
        let answer = match &pipeline {
            Ok(pipeline) => Ok((pipeline.clone(), clock)),
            Err(_) => Err(GpuFallback::PipelineFailed),
        };
        self.cache.push(Cached {
            signature: steps
                .iter()
                .map(|step| {
                    let program = step.program();
                    (step.kind(), program.entry.clone(), program.source.clone())
                })
                .collect(),
            pipeline,
            id: clock,
            used: clock,
        });
        answer
    }

    #[cfg(test)]
    fn failure(&self, steps: &[GpuStep]) -> Option<Arc<str>> {
        self.cache
            .iter()
            .find(|cached| cached.matches(steps))
            .and_then(|cached| cached.pipeline.as_ref().err().cloned())
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
    if SURFACE_NAMES.contains(&name) {
        return Err(format!("{name:?} is one of the surface's own names"));
    }
    Ok(())
}

/// The whole shader for `steps`: the prelude, each program once, then the entry points.
fn assemble(steps: &[GpuStep]) -> Result<String, String> {
    let mut source = String::from(PRELUDE);
    let mut included: Vec<&GpuProgram> = Vec::new();
    for step in steps {
        let program = step.program();
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
    source.push_str(BOUNDARY_BINDING);
    source.push_str(
        "
@fragment
fn lf_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // The border column and row past the boundary repeat its edge texels.
    let texel = min(vec2<u32>(position.xy), textureDimensions(lf_boundary) - vec2<u32>(1u));
    var rgb = textureLoad(lf_boundary, texel, 0).rgb;
    let stage = vec2<f32>(lf_f32(0u), lf_f32(1u)) + vec2<f32>(texel) * vec2<f32>(lf_f32(2u), lf_f32(3u));
",
    );
    for (index, step) in steps.iter().enumerate() {
        let base = MAP_WORDS + STEP_WORDS * index;
        match step {
            GpuStep::Colour { program, .. } => source.push_str(&format!(
                "    rgb = {}(rgb, {}, lf_words[{base}u], lf_words[{}u]);\n",
                program.entry,
                PositionMap::wgsl(base + 2, "stage"),
                base + 1
            )),
        }
    }
    source.push_str("    return vec4<f32>(rgb, 1.0);\n}\n");
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
    let program = step.program();
    let entry = &program.entry;
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
    let (arguments, result, signature) = match step {
        GpuStep::Colour { .. } => (
            [
                vector(naga::VectorSize::Tri),
                vector(naga::VectorSize::Bi),
                unsigned.clone(),
                unsigned,
            ],
            vector(naga::VectorSize::Tri),
            "(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32>",
        ),
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

/// Assemble, validate and compile `steps` into a pipeline that writes `format`, naming why it
/// failed. The stage writes [`OUTPUT_FORMAT`]; only qualification asks for another.
fn compile(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    steps: &[GpuStep],
    format: wgpu::TextureFormat,
) -> Result<wgpu::RenderPipeline, String> {
    for step in steps {
        validate_step(step)?;
    }
    let source = assemble(steps)?;
    validate(&source)?;
    device.push_error_scope(wgpu::ErrorFilter::Internal);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("luxforge.gpu_preview.shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
    });
    let validation = answered(device.pop_error_scope());
    let internal = answered(device.pop_error_scope());
    match (validation, internal) {
        (Some(None), Some(None)) => Ok(pipeline),
        (Some(Some(error)), _) | (_, Some(Some(error))) => Err(error.to_string()),
        _ => Err("the pipeline's error scopes were not answered without waiting".into()),
    }
}

/// The tick's words — the texel map, each step's base indices and position map, then every
/// program's words — and its blocks, concatenated, at least one word.
pub(super) fn pack(plan: &GpuPlan, words: &mut Vec<u32>, blocks: &mut Vec<u32>) {
    words.clear();
    blocks.clear();
    let map = plan.texels;
    words.extend([map.origin[0], map.origin[1], map.step[0], map.step[1]].map(f32::to_bits));
    let header = MAP_WORDS + STEP_WORDS * plan.steps.len();
    let (mut word, mut block) = (header, 0);
    for step in &plan.steps {
        let program = step.program();
        words.extend([word as u32, block as u32]);
        words.extend(step.position().words());
        word += program.words.len();
        block += program.block.len();
    }
    for step in &plan.steps {
        let program = step.program();
        words.extend_from_slice(&program.words);
        blocks.extend_from_slice(&program.block);
    }
    if blocks.is_empty() {
        blocks.push(0);
    }
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
    /// slot evaluates it, or the frame is the CPU's and names why. `surface` is out of the map.
    pub(super) fn prepare_gpu(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<&GpuPlan>,
        dissolve: Option<DissolveFrame>,
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
        let outcome = self.evaluate(surface, device, queue, plan);
        if outcome.is_err() {
            self.release_gpu(surface);
        }
        surface.gpu_outcome = Some(outcome);
    }

    /// Retire `surface`'s GPU-preview slot, if it holds one.
    pub(super) fn release_gpu(&self, surface: &mut SurfaceSlots) {
        if let Some(slot) = surface.gpu.take() {
            self.retire_slot(slot);
        }
    }

    fn evaluate(
        &mut self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuPlan,
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
        let (pipeline, pipeline_id) =
            self.gpu
                .pipeline(device, &plan.steps, &self.figures.preview)?;
        let mut words = std::mem::take(&mut self.gpu.words);
        let mut blocks = std::mem::take(&mut self.gpu.blocks);
        pack(plan, &mut words, &mut blocks);
        let result = self.run(
            surface,
            device,
            queue,
            plan,
            (&pipeline, pipeline_id),
            &words,
            &blocks,
        );
        self.gpu.words = words;
        self.gpu.blocks = blocks;
        result
    }

    /// Fit the slot to the plan, write what changed and encode the pass when anything did.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: &GpuPlan,
        (pipeline, pipeline_id): (&wgpu::RenderPipeline, u64),
        words: &[u32],
        blocks: &[u32],
    ) -> Result<u64, GpuFallback> {
        let started = std::time::Instant::now();
        let size = plan.boundary.size();
        let word_bytes = (words.len() * 4) as u64;
        let block_bytes = (blocks.len() * 4) as u64;
        if surface.gpu.as_ref().is_some_and(|slot| slot.size != size)
            && let Some(slot) = surface.gpu.take()
        {
            self.retire_slot(slot);
        }
        if surface.gpu.is_none() {
            surface.gpu = Some(self.allocate(device, size, word_bytes, block_bytes)?);
        }
        let slot = surface.gpu.as_mut().expect("an admitted slot");
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
                &slot.boundary,
                &slot.words.buffer,
                &slot.blocks.buffer,
            );
            slot.written_words.clear();
            slot.written_blocks.clear();
            slot.evaluated = None;
        }
        let mut changed = slot.evaluated != Some(pipeline_id);
        if slot.boundary_version != Some(plan.boundary.version) {
            upload_boundary(queue, &slot.boundary, &plan.boundary);
            slot.boundary_version = Some(plan.boundary.version);
            changed = true;
        }
        if slot.written_words != words {
            // The tick's one write: the header and every program's words.
            queue.write_buffer(&slot.words.buffer, 0, &le_bytes(words));
            slot.written_words.clear();
            slot.written_words.extend_from_slice(words);
            changed = true;
        }
        if slot.written_blocks != blocks {
            queue.write_buffer(&slot.blocks.buffer, 0, &le_bytes(blocks));
            slot.written_blocks.clear();
            slot.written_blocks.extend_from_slice(blocks);
            changed = true;
        }
        if changed {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luxforge.gpu_preview.encoder"),
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("luxforge.gpu_preview.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &slot.target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                // The frame and, where the bucket has room, one more column and row: the edge
                // texels again, which the linear filter reads across the frame's edge as it reads
                // the photograph's.
                let (width, height) = slot.size;
                let (columns, rows) = slot.output.capacity;
                pass.set_viewport(
                    0.0,
                    0.0,
                    (width + 1).min(columns) as f32,
                    (height + 1).min(rows) as f32,
                    0.0,
                    1.0,
                );
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &slot.bindings, &[]);
                pass.draw(0..3, 0..1);
            }
            // Submitted now, ahead of the frame's own submission, whose draw samples the output;
            // the queue's writes above are flushed with it. Nothing waits for it.
            queue.submit([encoder.finish()]);
            slot.evaluated = Some(pipeline_id);
            slot.output.version = plan.boundary.version;
            slot.frame_us = started.elapsed().as_micros() as u64;
            self.figures.preview.passes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(plan.boundary.version)
    }

    /// A slot for a boundary of `size`, charged before anything is created.
    fn allocate(
        &self,
        device: &wgpu::Device,
        (width, height): (u32, u32),
        word_bytes: u64,
        block_bytes: u64,
    ) -> Result<GpuSlot, GpuFallback> {
        let limit = device.limits().max_texture_dimension_2d;
        // The output is reserved in the photograph's own size bucket, so the draw samples it over
        // the same texture extent, and with the same filter weights, as the CPU frame it stands
        // in for; the pass writes its edge column and row into the border as an upload copies
        // them. The boundary is only loaded, never sampled, so it is exactly its size.
        let capacity = super::full_capacity((width, height), limit);
        let texels = u64::from(width) * u64::from(height);
        let output_bytes = u64::from(capacity.0) * u64::from(capacity.1) * 4;
        let texture_bytes =
            texels * GpuBoundary::TEXEL_BYTES as u64 + output_bytes + UNIFORM_SIZE as u64;
        let (words, blocks) = (
            buffer_capacity(device, word_bytes)?,
            buffer_capacity(device, block_bytes)?,
        );
        self.figures
            .preview
            .charge(texture_bytes + words + blocks)?;
        let texture = |label, (width, height), format, usage| {
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
                view_formats: &[],
            })
        };
        let boundary = texture(
            "luxforge.gpu_preview.boundary",
            (width, height),
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let output = texture(
            "luxforge.gpu_preview.output",
            capacity,
            OUTPUT_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let target = output.create_view(&wgpu::TextureViewDescriptor::default());
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
                    resource: wgpu::BindingResource::TextureView(&target),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear),
                },
            ],
        });
        let layout = TileLayout {
            content: [0, 0, width, height],
            texels: [0, 0, width, height],
        };
        let picture = Picture {
            tiles: vec![Tile {
                layout,
                capacity,
                texture: output,
                uniform,
                bindings: photo_bindings,
            }],
            width,
            height,
            capacity,
            grid: (1, 1),
            limit,
            version: 0,
            content_id: None,
            region_key: None,
            allocated_bytes: output_bytes,
            // Evaluated at the boundary's own size, about the display's at Fit: never minified
            // far enough to need a chain.
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
        Ok(GpuSlot {
            size: (width, height),
            boundary,
            boundary_version: None,
            output: picture,
            target,
            words,
            blocks,
            bindings,
            texture_bytes,
            written_words: Vec::new(),
            written_blocks: Vec::new(),
            evaluated: None,
            frame_us: 0,
        })
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

    /// Hand `held` to the retirement worker, still charged, as a photograph texture is handed.
    fn retire_preview(&self, held: Held, bytes: u64) {
        self.figures
            .retirement_pending
            .fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self
            .retirement_sender
            .send(super::Retired::Preview(RetiredPreview { held, bytes }))
            && let super::Retired::Preview(retired) = error.0
        {
            // The worker has ended with the device: its resources are gone with it.
            finish_retirement(&self.figures, retired, true);
        }
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

    /// A pipeline whose device has no adapter able to run the stage, for the photograph's draw.
    #[cfg(test)]
    pub(super) fn without_gpu_stage(mut self) -> Self {
        self.gpu.support = None;
        self
    }
}

/// `words` as the little-endian bytes the GPU reads.
fn le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// Queue the boundary's texels straight from the caller's buffer, in bounded chunks of rows.
fn upload_boundary(queue: &wgpu::Queue, texture: &wgpu::Texture, boundary: &GpuBoundary) {
    let row_bytes = u64::from(boundary.width) * GpuBoundary::TEXEL_BYTES as u64;
    let rows_per_chunk = (UPLOAD_CHUNK / row_bytes).max(1) as u32;
    let mut row = 0;
    while row < boundary.height {
        let rows = rows_per_chunk.min(boundary.height - row);
        let mut destination = texture.as_image_copy();
        destination.origin.y = row;
        queue.write_texture(
            destination,
            (*boundary.texels).as_ref(),
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
    }
}

mod position;
pub use position::PositionMap;

mod dissolve;
pub(crate) use dissolve::{DissolveFrame, dissolving, photo_uniform};
pub use dissolve::{DISSOLVE_DURATION, Dissolve, DrawnDissolve};

#[cfg(feature = "qualification")]
pub mod qualification;

#[cfg(test)]
mod tests;
