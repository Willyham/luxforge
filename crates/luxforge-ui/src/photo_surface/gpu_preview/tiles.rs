//! The tile runner (`docs/design/gpu-first.md`, stage 4; `docs/design/gpu-preview.md`,
//! "Qualifying a program"): one tile of a stack drawn on a device of its own and read back, the GPU
//! half of a pixel read and of an export's tile, run on its caller's thread.
//!
//! The desktop's GPU tile worker owns one on a thread of its own and answers the core's tile
//! contract through it, mapping each [`TileFailure`] onto the contract's own reasons: every export
//! the catalog owner's export lane streams is drawn here, the desktop's one production readback.
//!
//! - **Its own device, on the window's adapter.** [`TileRunner::open`] opens a device on the adapter
//!   the window's renderer reports drawing with, found by its backend and name, requested with the
//!   descriptor Iced's renderer requests its own with ([`adapters::open`]), so the stage's programs
//!   compile for the same adapter, driver and limits as the window's. An adapter of that backend
//!   and name the host does not offer is refused by name (`adapter-mismatch`), never replaced by
//!   another. A launch that refused the GPU stage ([`super::refuse_gpu_stage`]) opens nothing
//!   (`refused`); a host with no adapter, an adapter that grants no device and a device the
//!   stage's capability check refuses are `no-adapter`; a device lost later is `device-lost` for
//!   good, and nothing waits for a recovery.
//! - **A tile.** [`TileRunner::submit`] draws a plan as the photo surface draws each tile of its
//!   picture at rest: a fresh evaluation — every link, every spatial pass and the output, no link
//!   state, plane or schedule kept from an earlier tile — over a boundary cut on the GPU from a
//!   window of the source ([`Derivation::Cut`], [`GpuSource::window`]), through the stage's one
//!   compile, its chain of links and its pool of scratch planes, ending in the CPU's output
//!   quantizer's codes ([`TileEnd::Codes`]), or in the linear values that quantizer reads
//!   ([`TileEnd::Linear`]). It copies the output into a readback copy, submits once and returns
//!   without waiting; [`TileRunner::finish`] waits for that submission alone, maps the copy and
//!   unpads its rows, and [`TileRunner::run`] does both at once. At most [`TILES_IN_FLIGHT`] tiles
//!   are submitted and not read back, so the GPU draws one while its caller encodes the next or
//!   reads the one before back. It blocks only its caller, never touches Iced's device or queue,
//!   and starts no thread: it compiles on its caller's thread, which is never the interface thread.
//! - **Held between tiles.** The window of the source its last tile read, which a next tile of the
//!   same window reads again — the desktop's worker hands every tile of a band its band's window, so
//!   each band's rows are uploaded once — and the slot of its last tile's shape ([`SlotKey`]): the
//!   boundary, each link's intermediate and buffers, a tail's intermediate and the output, which a
//!   next tile of that shape draws into again, each written whole before it is read; and at most
//!   two readback copies. A tile of another window or shape lets the old one go first.
//! - **Lights.** A plan whose spatial steps read a global estimate ([`GpuPlan::lights`], Dehaze's
//!   atmospheric light) is drawn as the photo surface draws it, each light computed by its light
//!   link from the whole stage at full resolution: the runner runs the link once for the source
//!   and keeps the light it reads back ([`light::read_light`]), cutting each of the link's tiles
//!   from a window of the source of its own, so it never holds the whole source; every tile of
//!   the plan then reads that light from its light plane, written before its passes
//!   ([`super::spatial::Pool::write_light`]). The runner keeps the last [`TILE_LIGHTS`] lights it
//!   computed, keyed by the source's version and the link's steps, words and blocks, so a stream's
//!   tiles and a call's reads compute each light once. A light behind a spatial step
//!   ([`light::LightInput::Stage`]) is reduced instead from the stage texture the sweep before the
//!   reading one wrote ([`light::read_staged_light`]), once, before that sweep's first tile, and
//!   kept under its input's identity.
//! - **Bounds.** Everything a tile creates is charged to [`GPU_TILE_BUDGET`], beside and apart from
//!   the photo surface's GPU-preview budget, before anything is created: the window of the source,
//!   the boundary, each link's intermediate, words and blocks, every link's kept planes and the
//!   pool once ([`super::chain_charge`]), a geometry tail's intermediate, the output and its
//!   readback copy ([`TileRunner::charge`]), beside what the tiles in flight hold — their readback
//!   copies, and their slot and any window let go since, until the GPU is known to be done with
//!   them; when only those pass the budget the tile waits for the GPU first. A light computed for
//!   it is charged on its own before, its link, the largest window of the source one of its tiles
//!   is cut from and its readback ([`light::read_light_charge`]), beside what the runner keeps. A
//!   tile past it is refused (`tiles-budget`) having created and compiled nothing. Between tiles
//!   the runner holds the window, the slot and the spare readback copies
//!   ([`TileRunner::release`] lets them go, [`TileRunner::abandon`] the tiles in flight), and at
//!   most [`TILE_PIPELINE_CACHE`] compiled sequences beside its spatial passes' own cache of 64
//!   modules; a tile's planes and pool go as it is submitted, and the device frees them once the
//!   GPU is done with them; dropping the runner releases the rest. The window's upload passes
//!   through wgpu's staging, one copy of the window's bytes until the GPU is done with it, which,
//!   as the photo surface's own uploads, is not charged.
//! - **Figures.** Each tile's time on its caller's thread, split into the lights it computed, the
//!   window's upload, fitting the slot and encoding its work, the wait for its device once it is
//!   finished and the readback ([`TileTimes`]): the last tile's and every tile's summed, with the
//!   tiles in flight, the windows uploaded and the slots created ([`TileFigures`]).
//! - **Determinism.** One device, one driver: the same plan over the same window draws the same
//!   bytes every run, each a fresh evaluation in a fixed order with no float atomics.
//! - **Why the reference would answer instead.** Every failure is shaped as the core's tile
//!   contract carries it, though this crate names no core type: unavailable (`no-adapter`,
//!   `refused`, `device-lost`, `adapter-mismatch`), the budget (`tiles-budget`), or the plan's own
//!   reason the stage cannot run it — `pipeline-failed`, `texture-limit`, `buffer-limit` or
//!   `source-missing` for a window whose pixels were let go — exactly as the surface names them.
use super::staged::StageHolder;
use super::{
    BLOCK_CHUNK, BoundaryFormat, Charged, Compiled, Derivation, GpuFallback, GpuPlan, GpuSource,
    GpuStep, MIN_BUFFER, OUTPUT_FORMAT, SAMPLED_FORMAT, Shape, SourceLayouts, SourceSlot,
    SpatialSlot, Support, answered, blocks, buffer_capacity, chain, chain_charge, compile,
    encode_pass_over, gpu_stage_refused, intermediate_bytes, intermediate_format, le_bytes, light,
    output_offset, region_drawable, spatial, storage_buffer, supported,
};
use crate::adapters::{self, Adapter, Unopened};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Instant;

/// What one tile runner may hold on its device at once: the window of the source, the tile's slot,
/// the readback copies of the tiles in flight, and what a tile in flight still holds of a window
/// or a slot let go since. 2 GiB, the owner's decision of 2026-10-06 (`docs/decisions.md`, "GPU-first
/// rendering"): a budget of its own beside the photo surface's 2 GiB GPU-preview budget, whose
/// slots a run never shares, so the two together may hold 4 GiB of unified memory. By the runner's
/// own charge over the core's plan, an estimate and not a measurement, a tile in the interior of a
/// 60-megapixel RAW (9504 × 6336) through Detail and all three Presence fields reads the tile grown
/// by its summed halo of 465 pixels on every side, anchored: at 2048 pixels a 3281-pixel square
/// window, charged 1,331.5 MB; at 1024 a 2257-pixel one, charged 622.6 MB.
pub const GPU_TILE_BUDGET: u64 = 2 << 30;

/// How many tiles a runner keeps submitted and not yet read back: the GPU draws one while its
/// caller encodes the next or reads the one before back.
pub const TILES_IN_FLIGHT: usize = 2;

/// How many compiled program sequences a tile runner keeps, ready and failed ones together, the
/// least recently run evicted first: the photo surface's own bound ([`super::PIPELINE_CACHE`]),
/// which holds every link of the largest plan a tile draws, a link for the colour steps before its
/// first spatial step and one for each of up to 18 spatial steps, with room for both ends of each.
pub const TILE_PIPELINE_CACHE: usize = 64;

/// How many lights a tile runner keeps, the least recently computed let go first: a stream's or a
/// call's plan reads at most one for each estimating layer, and every tile of it reads the same.
pub const TILE_LIGHTS: usize = 8;

/// What a run reads back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileEnd {
    /// The output stage's 8-bit codes, as the CPU's output quantizer gives them and the photo
    /// surface draws them: the picture's own bytes.
    Codes,
    /// The linear values the output quantizer reads, before it: what a read of a stage's linear
    /// values answers, and what the codes are quantized from.
    Linear,
}

impl TileEnd {
    /// The format the last pass writes: the output's codes, or `f32` before them.
    fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Codes => OUTPUT_FORMAT,
            Self::Linear => wgpu::TextureFormat::Rgba32Float,
        }
    }

    /// The bytes an output texel takes.
    fn texel_bytes(self) -> u32 {
        match self {
            Self::Codes => 4,
            Self::Linear => 16,
        }
    }
}

/// A run's output, row by row from its first pixel.
#[derive(Clone, Debug, PartialEq)]
pub enum TilePixels {
    /// Four bytes a pixel, the codes of red, green and blue, and an opaque alpha.
    Codes(Vec<u8>),
    /// Red, green and blue as the `f32` the last step returned.
    Linear(Vec<[f32; 3]>),
}

/// Why a runner cannot draw on its GPU at all: the core's `tiles-unavailable` reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileUnavailable {
    /// No adapter, no device on it, or a device the stage's capability check refuses.
    NoAdapter,
    /// The launch refused the GPU stage (`--no-gpu-render`).
    Refused,
    /// The runner's device was lost; nothing waits for a recovery.
    DeviceLost,
    /// The host offers no adapter of the backend and name the window's renderer draws with, so a
    /// tile could not be the picture's own bytes.
    AdapterMismatch,
}

impl TileUnavailable {
    /// A stable kebab-case name for the reason, the core's own.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoAdapter => "no-adapter",
            Self::Refused => "refused",
            Self::DeviceLost => "device-lost",
            Self::AdapterMismatch => "adapter-mismatch",
        }
    }
}

/// Why a run drew nothing, shaped as the core's tile contract carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileFailure {
    /// The runner cannot draw on its GPU at all (`tiles-unavailable`).
    Unavailable(TileUnavailable),
    /// The run would hold `requested` bytes, past `budget` (`tiles-budget`): refused before
    /// anything was created.
    Budget { requested: u64, budget: u64 },
    /// The stage cannot run the plan, for the reason the photo surface names: `pipeline-failed`,
    /// `texture-limit`, `buffer-limit`, or `source-missing` for a window whose pixels were let go.
    Plan(GpuFallback),
}

impl TileFailure {
    /// A stable kebab-case name: `tiles-unavailable`, `tiles-budget`, or the stage's own code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "tiles-unavailable",
            Self::Budget { .. } => "tiles-budget",
            Self::Plan(fallback) => fallback.as_str(),
        }
    }

    const PIPELINE_FAILED: Self = Self::Plan(GpuFallback::PipelineFailed);
}

/// Why [`TileRunner::open`] opened no runner, and what the host offered when the adapter asked for
/// was not among it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileRefusal {
    pub reason: TileUnavailable,
    /// The adapter asked for and, on a mismatch, every adapter the host offered instead, or why
    /// the adapter granted no device.
    pub detail: String,
}

/// What a runner has held and done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileFigures {
    /// The bytes it holds now, as charged: the window of the source and the slot between runs,
    /// and what its tiles in flight hold.
    pub in_use: u64,
    /// Tiles submitted and not yet read back.
    pub in_flight: u32,
    /// Of `in_use`, the stage textures a staged stream holds ([`TileRunner::hold_stages`]).
    pub stage_bytes: u64,
    /// Slots it created, one a window shape: a run of the shape its slot holds reuses it.
    pub slots: u64,
    /// Windows of the source it uploaded: a run of the window it holds reads it again.
    pub uploads: u64,
    /// The most it has held at once: a run's whole charge.
    pub peak: u64,
    /// Program sequences it has compiled, ready or failed: each one its cache did not hold.
    pub compiles: u64,
    /// Tiles it has drawn and read back.
    pub runs: u64,
    /// Lights it has computed and read back, each once for every tile that reads it.
    pub lights: u64,
    /// Where its last tile's time went, and every tile's summed ([`TileTimes`]).
    pub last: TileTimes,
    pub total: TileTimes,
}

/// Where a tile's time went on the runner's thread, in microseconds: what attributes a slow
/// stream. Each is a wall-clock span of the run, read with two clock reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileTimes {
    /// Computing the lights the plan reads that the runner did not keep ([`light::read_light`]),
    /// each waited for.
    pub light_us: u64,
    /// Uploading the window of the source, when the runner did not hold it already.
    pub upload_us: u64,
    /// Fitting the slot and encoding and submitting the tile's passes.
    pub encode_us: u64,
    /// Waiting for the device to finish them once its caller asks for the tile: what of the GPU's
    /// work the caller's own work did not overlap.
    pub wait_us: u64,
    /// Reading the output back and unpadding its rows.
    pub read_us: u64,
}

impl TileTimes {
    fn add(&mut self, other: &Self) {
        self.light_us += other.light_us;
        self.upload_us += other.upload_us;
        self.encode_us += other.encode_us;
        self.wait_us += other.wait_us;
        self.read_us += other.read_us;
    }
}

/// One tile of a stack drawn on a device of its own and read back, on its caller's thread (the
/// module documentation).
pub struct TileRunner {
    /// The window of the source the last run read, with its bytes as charged, kept for a next run
    /// of the same window.
    window: Option<(SourceSlot, u64)>,
    /// The textures and buffers of the last run's shape, kept for a next run of the same shape.
    slot: Option<Slot>,
    /// What each window and slot held was, told apart from the ones after it.
    generations: (u64, u64),
    /// The tiles submitted and not yet read back, oldest first, at most [`TILES_IN_FLIGHT`].
    in_flight: std::collections::VecDeque<InFlight>,
    /// Readback copies a tile read back left, for the next tiles to copy into.
    readbacks: Vec<(wgpu::Buffer, u64)>,
    tickets: u64,
    /// The stage textures a staged stream's sweeps write and read ([`TileRunner::hold_stages`]),
    /// and what they are charged.
    stages: Vec<StageHolder>,
    stage_bytes: u64,
    sequences: Lru<Key, Sequence>,
    /// The lights it computed last, each under its key ([`light_key`]).
    lights: Lru<u64, [f32; 4]>,
    support: Support,
    /// The passes that cut a boundary from the window; none while the core's output encoding,
    /// whose decode table a JPEG's cut reads, is not installed, so a run makes them once it is.
    layouts: Option<SourceLayouts>,
    /// Set by the device's lost callback.
    lost: Arc<AtomicBool>,
    figures: TileFigures,
    /// Tests only: every link's passes start from NaN in every texture of its pool.
    #[cfg(any(test, feature = "qualification"))]
    poisoned: bool,
    adapter: Adapter,
    queue: wgpu::Queue,
    device: wgpu::Device,
}

impl std::fmt::Debug for TileRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TileRunner")
            .field("adapter", &self.adapter)
            .field("figures", &self.figures)
            .field("lost", &self.lost())
            .finish()
    }
}

/// A compiled sequence's key, as the stage keys its own: each step's signature
/// ([`compile::signature`]) and the format its last pass writes.
type Key = (compile::Signature, wgpu::TextureFormat);

/// A compiled sequence, ready, or failed and kept so, so it is not compiled again at every run;
/// with its identity, which its spatial passes' groups are built for.
type Sequence = (Result<Compiled, Arc<str>>, u64);

/// Whether `key` is `steps` writing `format`, compared without copying a program.
fn holds(key: &Key, steps: &[GpuStep], format: wgpu::TextureFormat) -> bool {
    let (signature, written) = key;
    *written == format
        && signature
            .iter()
            .map(|(kind, entry, source)| (*kind, entry.as_str(), source.as_str()))
            .eq(steps.iter().flat_map(GpuStep::signature))
}

/// A tile submitted, which [`TileRunner::finish`] reads back, or [`TileRunner::finish_stage`] waits
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u64);

/// What a tile's boundary is cut from: a window of the source, or the stage texture a staged
/// sweep before it wrote ([`TileRunner::hold_stages`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileInput {
    Source,
    /// Stage texture `k`: the window is copied out of it, texel for texel.
    Stage(u32),
}

/// What a tile's last link writes: its output, read back as [`TileEnd`] asks; or, for a staged
/// sweep before the last, its last link's output over the boundary — linear, unclamped, as a chain's
/// link writes it, which the sweep's identity tail leaves in the slot's tail intermediate — of which `rect` (`[x0, y0, x1, y1)` of the content stage) is copied into stage
/// texture `writes` at `rect`, nothing read back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileOutput {
    Read(TileEnd),
    Stage { writes: u32, rect: [u32; 4] },
}

impl TileOutput {
    /// What is read back, if anything.
    fn end(self) -> Option<TileEnd> {
        match self {
            Self::Read(end) => Some(end),
            Self::Stage { .. } => None,
        }
    }
}

/// The format a tile's last pass writes: `end`'s, or the chain's intermediate over `boundary` for
/// a tile whose output is copied into a stage texture.
fn end_format(end: Option<TileEnd>, boundary: BoundaryFormat) -> wgpu::TextureFormat {
    end.map_or_else(|| intermediate_format(boundary), TileEnd::format)
}

/// The bytes a texel of a tile's output takes, as [`end_format`] writes it.
fn end_texel_bytes(end: Option<TileEnd>, boundary: BoundaryFormat) -> u32 {
    end.map_or(boundary.texel_bytes() as u32, TileEnd::texel_bytes)
}

/// A tile's output copied out: the readback copy, its mapping's answer and its layout.
struct Readback {
    buffer: wgpu::Buffer,
    bytes: u64,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    size: (u32, u32),
    padded: u32,
    end: TileEnd,
}

/// A tile's work on the GPU, submitted: its readback copy, when it has one, its submission, and
/// what it holds until the GPU is done with it.
struct InFlight {
    ticket: u64,
    index: wgpu::SubmissionIndex,
    readback: Option<Readback>,
    /// The window and the slot it read and drew into, by generation, with their bytes: what it
    /// still holds once the runner lets them go, until the GPU is known to be done with it.
    window: (u64, u64),
    slot: (u64, u64),
    done: bool,
    /// Its lights, the window's upload, and its encoding and submission.
    times: TileTimes,
}

impl InFlight {
    fn readback_bytes(&self) -> u64 {
        self.readback.as_ref().map_or(0, |readback| readback.bytes)
    }
}

/// What decides every texture and buffer of a tile's slot but its spatial planes and its pool,
/// which follow their own keys: the boundary's size and format, the output's size and what it is
/// read back as — or none, for a staged sweep's intermediate — a geometry tail's intermediate, and
/// how many links come before the last.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SlotKey {
    shape: Shape,
    end: Option<TileEnd>,
    links: usize,
}

/// A tile's slot, kept between runs of one shape: each run is still a fresh evaluation — new link
/// states and schedules over these textures, every one of them written whole before it is read —
/// and a run of another shape lets it go first.
struct Slot {
    key: SlotKey,
    boundary: wgpu::Texture,
    /// Each link's intermediate before the last, with its words and blocks buffers.
    links: Vec<(wgpu::Texture, Charged, Charged)>,
    cut: Charged,
    words: Charged,
    blocks: Charged,
    intermediate: Option<wgpu::Texture>,
    output: wgpu::Texture,
    /// What its textures and buffers take; a tile's kept planes and pool are the tile's own.
    bytes: u64,
}

impl Slot {
    /// The textures of `key`, and buffers of the smallest capacity, which each tile fits to what it
    /// writes ([`fitted`]). Charged with the run that creates it ([`TileRunner::submit`]).
    fn create(device: &wgpu::Device, key: SlotKey) -> Result<Self, TileFailure> {
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
        let drawn = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let smallest = buffer_capacity(device, MIN_BUFFER).map_err(TileFailure::Plan)?;
        let buffer = |label| Charged {
            buffer: storage_buffer(device, label, smallest),
            bytes: smallest,
        };
        let size = key.shape.boundary;
        let mut slot = Self {
            key,
            // Cut from the source by a pass, or copied out of a stage texture.
            boundary: texture(
                "luxforge.tiles.boundary",
                size,
                key.shape.format.texture(),
                drawn | wgpu::TextureUsages::COPY_DST,
            ),
            links: (0..key.links)
                .map(|_| {
                    (
                        texture(
                            "luxforge.tiles.link",
                            size,
                            intermediate_format(key.shape.format),
                            drawn,
                        ),
                        buffer("luxforge.tiles.link_words"),
                        buffer("luxforge.tiles.link_blocks"),
                    )
                })
                .collect(),
            cut: buffer("luxforge.tiles.cut"),
            words: buffer("luxforge.tiles.words"),
            blocks: buffer("luxforge.tiles.blocks"),
            intermediate: key.shape.intermediate.map(|format| {
                // A sweep before the last copies it into a stage texture.
                let usage = drawn | wgpu::TextureUsages::COPY_SRC;
                texture("luxforge.tiles.intermediate", size, format, usage)
            }),
            output: texture(
                "luxforge.tiles.output",
                key.shape.output,
                end_format(key.end, key.shape.format),
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            ),
            bytes: 0,
        };
        slot.account();
        Ok(slot)
    }

    /// Count what its textures and buffers take, as their sizes and capacities stand.
    fn account(&mut self) {
        let texels = u64::from(self.key.shape.boundary.0) * u64::from(self.key.shape.boundary.1);
        let (width, height) = self.key.shape.output;
        let tail = self.key.shape.intermediate.map_or(0, |format| {
            texels * u64::from(format.block_copy_size(None).unwrap_or(16))
        });
        self.bytes = texels * self.key.shape.format.texel_bytes() as u64
            + self.links.len() as u64
                * intermediate_bytes(self.key.shape.boundary, self.key.shape.format)
            + self
                .links
                .iter()
                .map(|(_, words, blocks)| words.bytes + blocks.bytes)
                .sum::<u64>()
            + self.cut.bytes
            + self.words.bytes
            + self.blocks.bytes
            + tail
            + u64::from(width)
                * u64::from(height)
                * u64::from(end_texel_bytes(self.key.end, self.key.shape.format));
    }
}

/// A buffer of at least `bytes` from `held`, or a new one of `bytes`' capacity in its place.
fn fitted(
    device: &wgpu::Device,
    held: &mut Charged,
    bytes: u64,
    label: &str,
) -> Result<(), TileFailure> {
    if bytes <= held.bytes {
        return Ok(());
    }
    let capacity = buffer_capacity(device, bytes).map_err(TileFailure::Plan)?;
    *held = Charged {
        buffer: storage_buffer(device, label, capacity),
        bytes: capacity,
    };
    Ok(())
}

/// Microseconds since `since`.
fn micros(since: Instant) -> u64 {
    since.elapsed().as_micros() as u64
}

impl TileRunner {
    /// A runner on a device of its own on the adapter whose backend and name are `backend` and
    /// `name`, as the window's renderer reports the adapter it draws with ([`adapters::open`]),
    /// checked as the stage checks its own device. Refused, having opened nothing, when the launch
    /// refused the GPU stage; refused by name when the host offers no such adapter. Blocking: it
    /// creates a graphics instance and a device, so never on the interface thread.
    pub fn open(backend: &str, name: &str) -> Result<Self, TileRefusal> {
        Self::open_with(gpu_stage_refused(), backend, name)
    }

    /// [`TileRunner::open`] for a launch that `refused` the GPU stage or not.
    fn open_with(refused: bool, backend: &str, name: &str) -> Result<Self, TileRefusal> {
        let asked = format!("{backend} {name:?}");
        if refused {
            return Err(TileRefusal {
                reason: TileUnavailable::Refused,
                detail: format!("the launch refused the GPU stage; {asked} was not opened"),
            });
        }
        let opened = adapters::open(backend, name).map_err(|unopened| match unopened {
            Unopened::NoAdapter => TileRefusal {
                reason: TileUnavailable::NoAdapter,
                detail: format!("the host offers no adapter; {asked} was asked for"),
            },
            Unopened::Mismatch { offered } => TileRefusal {
                reason: TileUnavailable::AdapterMismatch,
                detail: format!(
                    "{asked} is not among the adapters the host offers: {}",
                    offered
                        .iter()
                        .map(|adapter| format!(
                            "{} {:?} ({})",
                            adapter.backend, adapter.name, adapter.device_type
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            Unopened::NoDevice(refusals) => TileRefusal {
                reason: TileUnavailable::NoAdapter,
                detail: format!("{asked} granted no device: {}", refusals.join("; ")),
            },
        })?;
        let adapters::Opened {
            adapter,
            device,
            queue,
        } = opened;
        // The stage's own check of a device, over the sRGB view the photograph's draw samples a
        // GPU frame through: a device it refuses runs no plan.
        if !supported(&device.limits(), SAMPLED_FORMAT) {
            return Err(TileRefusal {
                reason: TileUnavailable::NoAdapter,
                detail: format!("{asked} cannot run the GPU stage"),
            });
        }
        let lost = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&lost);
        device.set_device_lost_callback(move |_reason, _message| {
            signal.store(true, Ordering::Release);
        });
        let support = Support::new(&device);
        let layouts = SourceLayouts::new(&device).ok();
        Ok(Self {
            window: None,
            slot: None,
            generations: (0, 0),
            in_flight: std::collections::VecDeque::new(),
            readbacks: Vec::new(),
            tickets: 0,
            stages: Vec::new(),
            stage_bytes: 0,
            sequences: Lru::new(TILE_PIPELINE_CACHE),
            lights: Lru::new(TILE_LIGHTS),
            support,
            layouts,
            lost,
            figures: TileFigures::default(),
            #[cfg(any(test, feature = "qualification"))]
            poisoned: false,
            adapter,
            queue,
            device,
        })
    }

    /// The adapter the runner's device is on, as evidence records it beside the window's.
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// Whether the runner's device was lost: every later run answers `device-lost`.
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    pub fn figures(&self) -> TileFigures {
        self.figures
    }

    /// Let the window of the source and the slot go, which a run keeps for a next run of the same
    /// window and shape: the runner then holds its compiled sequences alone, beside what its tiles
    /// in flight hold until they are read back or abandoned.
    pub fn release(&mut self) {
        self.drop_window();
        self.drop_slot();
        self.readbacks.clear();
        self.account();
    }

    /// Let every tile in flight go unread, with what it holds: its caller wants none of them.
    pub fn abandon(&mut self) {
        self.in_flight.clear();
        self.account();
    }

    /// Tiles submitted and not yet read back.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    fn drop_window(&mut self) {
        if self.window.take().is_some() {
            self.generations.0 += 1;
        }
    }

    fn drop_slot(&mut self) {
        if self.slot.take().is_some() {
            self.generations.1 += 1;
        }
    }

    /// What the tiles in flight hold beside the window and the slot the runner holds now: their
    /// readback copies, and a window or a slot let go since they were submitted, until the GPU is
    /// known to be done with them.
    fn pinned(&self) -> u64 {
        let (window, slot) = self.generations;
        self.in_flight
            .iter()
            .map(|tile| {
                let stale = |(generation, bytes): (u64, u64), now: u64| {
                    if tile.done || generation == now {
                        0
                    } else {
                        bytes
                    }
                };
                tile.readback_bytes() + stale(tile.window, window) + stale(tile.slot, slot)
            })
            .sum()
    }

    /// What the runner holds now, as charged: the window, the slot, the stage textures and what
    /// its tiles in flight hold.
    fn holding(&self) -> u64 {
        self.window.as_ref().map_or(0, |(_, bytes)| *bytes)
            + self.slot.as_ref().map_or(0, |slot| slot.bytes)
            + self.stage_bytes
            + self.pinned()
            + self.spare()
    }

    /// Hold `count` stage textures of `stage` in `format`, each a content stage in at most 2 × 2
    /// textures ([`StageHolder`]), for a staged stream's sweeps to write and read: charged to
    /// [`GPU_TILE_BUDGET`] beside everything the runner holds before any is created, refused
    /// (`tiles-budget`, or `texture-limit` past 2 × 2) having created none. Any held before go
    /// first. They stay until [`TileRunner::release_stages`]; a call's tiles never touch them.
    pub fn hold_stages(
        &mut self,
        stage: (u32, u32),
        format: BoundaryFormat,
        count: u32,
    ) -> Result<(), TileFailure> {
        self.release_stages();
        let limit = self.device.limits().max_texture_dimension_2d;
        let each =
            StageHolder::charge(stage, format.texture(), limit).map_err(TileFailure::Plan)?;
        let requested = self.holding() + u64::from(count) * each;
        if requested > GPU_TILE_BUDGET {
            return Err(TileFailure::Budget {
                requested,
                budget: GPU_TILE_BUDGET,
            });
        }
        for _ in 0..count {
            let holder = StageHolder::create(&self.device, stage, format.texture(), |_| {
                Ok::<(), GpuFallback>(())
            })
            .map_err(TileFailure::Plan)?;
            self.stage_bytes += holder.bytes();
            self.stages.push(holder);
        }
        self.figures.peak = self.figures.peak.max(self.holding());
        self.account();
        Ok(())
    }

    /// Let the stage textures go: a staged stream ended, drawn whole, cancelled, abandoned or
    /// fallen back. A tile in flight that wrote or read them keeps them until the GPU is done.
    pub fn release_stages(&mut self) {
        self.stages.clear();
        self.stage_bytes = 0;
        self.account();
    }

    /// The stage textures held, and what they are charged.
    pub fn stages(&self) -> (usize, u64) {
        (self.stages.len(), self.stage_bytes)
    }

    /// The readback copies tiles read back left for the next to copy into.
    fn spare(&self) -> u64 {
        self.readbacks.iter().map(|(_, bytes)| *bytes).sum()
    }

    fn account(&mut self) {
        self.figures.in_use = self.holding();
        self.figures.in_flight = self.in_flight.len() as u32;
        self.figures.stage_bytes = self.stage_bytes;
    }

    /// Wait for every tile in flight to be done on the GPU, so what they held of a window or a slot
    /// let go is freed; their readback copies stay theirs until they are read back.
    fn settle(&mut self) {
        if self.in_flight.is_empty() {
            return;
        }
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        for tile in &mut self.in_flight {
            tile.done = true;
        }
    }

    /// What a run of `plan` over `window` of `source`, reading back `end`, would hold at most, as
    /// [`GPU_TILE_BUDGET`] is charged it: the window of the source, the boundary, each link's
    /// intermediate and its words and blocks, every link's kept planes and parameters and the pool
    /// once ([`super::chain_charge`]), the last link's words and blocks, a geometry tail's
    /// intermediate, the output and its readback copy. Creates nothing; refused as a run would be
    /// for a plan the runner cannot run.
    pub fn charge(
        &self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        end: TileEnd,
    ) -> Result<u64, TileFailure> {
        let held = self
            .checked(plan, source, window, TileInput::Source)?
            .ok_or(TileFailure::PIPELINE_FAILED)?;
        let run = held.bytes() + self.slot_bytes(plan, Some(end))?;
        // A light is computed on its own before the run, beside the window kept between runs.
        let mut most = run;
        for light in &plan.lights {
            most = most.max(held.bytes() + self.light_charge(source, light)?);
        }
        Ok(most)
    }

    /// What computing `light` holds: from `source` ([`light::read_light_charge`]), or for a light
    /// behind a spatial step from a stage texture in the source's boundary format
    /// ([`light::read_staged_light_charge`]).
    fn light_charge(
        &self,
        source: &GpuSource,
        light: &light::GpuLight,
    ) -> Result<u64, TileFailure> {
        match light.input {
            light::LightInput::Stage { .. } => {
                light::read_staged_light_charge(&self.device, source.kind().boundary(), light)
            }
            _ => light::read_light_charge(&self.device, source, light),
        }
        .map_err(TileFailure::Plan)
    }

    /// `plan` drawn over a boundary cut from `window` (`[x, y, width, height]` of the content
    /// stage) of `source`, as the photo surface draws a tile of its picture at rest, and read back
    /// as `end` asks: its output's pixels, row by row, the boundary's size, or its region's or
    /// tail's output, the tile submitted ([`TileRunner::submit`]) and read back at once
    /// ([`TileRunner::finish`]). Blocks its caller until its own device is done with it, and so with
    /// any tile submitted before it, whose pixels stay for their own caller to read back.
    pub fn run(
        &mut self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        end: TileEnd,
    ) -> Result<TilePixels, TileFailure> {
        let ticket = self.submit(plan, source, window, end)?;
        self.finish(ticket)
    }

    /// Submit `plan` drawn over a boundary cut from `window` (`[x, y, width, height]` of the
    /// content stage) of `source`, as the photo surface draws a tile of its picture at rest, its
    /// output copied into a readback copy, without waiting for the device: what
    /// [`TileRunner::finish`] reads back. The plan's boundary must be a cut of `source` inside
    /// `window` ([`super::GpuBoundary::derived`]), and the plan hold no clipping marks, whose
    /// colours are not codes; anything else is `pipeline-failed`.
    ///
    /// - **Held between tiles.** The window of the source is uploaded unless the runner holds that
    ///   window already, so a caller handing every tile of a band the band's window uploads it
    ///   once; the slot's textures and buffers are kept for a next tile of the same shape
    ///   ([`SlotKey`]), each tile still a fresh evaluation over them.
    /// - **In flight.** At most [`TILES_IN_FLIGHT`] tiles are submitted and not read back: with as
    ///   many already, it refuses (`pipeline-failed`) having created nothing.
    /// - **Charged.** To [`GPU_TILE_BUDGET`] before anything is created: the window, the slot and
    ///   its readback copy ([`TileRunner::charge`]), beside what the tiles in flight hold; when
    ///   only what they hold of a window or a slot let go passes the budget, it waits for them to be
    ///   done on the GPU first.
    /// - **Errors.** An error the device raises while the tile's work is created and submitted — a
    ///   validation, memory or internal one — is `pipeline-failed` too, and a lost device
    ///   `device-lost`, every tile in flight let go with it.
    pub fn submit(
        &mut self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        end: TileEnd,
    ) -> Result<Ticket, TileFailure> {
        self.submit_to(
            plan,
            source,
            window,
            TileInput::Source,
            TileOutput::Read(end),
        )
    }

    /// [`TileRunner::submit`] for a tile of a staged sweep (`docs/design/gpu-first.md`, "Staged
    /// sweeps"): its boundary, `window` of the content stage, cut from `source` or copied out of a
    /// stage texture (`input`), and its last link's output read back or, for a sweep before the
    /// last, its intermediate's rectangle copied into a stage texture (`output`), which
    /// [`TileRunner::finish_stage`] waits for. The stage textures must be held
    /// ([`TileRunner::hold_stages`]). The lights the plan reads are computed from `source`, as every
    /// tile's are.
    pub fn submit_to(
        &mut self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        input: TileInput,
        output: TileOutput,
    ) -> Result<Ticket, TileFailure> {
        let end = output.end();
        let stage = |k: u32, stages: &[StageHolder]| {
            stages
                .get(k as usize)
                .map(|_| ())
                .ok_or(TileFailure::PIPELINE_FAILED)
        };
        if let TileInput::Stage(k) = input {
            stage(k, &self.stages)?;
        }
        if let TileOutput::Stage { writes, .. } = output {
            stage(writes, &self.stages)?;
        }
        if self.lost() {
            // Nothing it held outlives its device.
            self.in_flight.clear();
            self.release();
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        if self.in_flight.len() >= TILES_IN_FLIGHT {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        if self.layouts.is_none() {
            // Opened before the output encoding was installed: the cut's passes, made now.
            self.layouts = SourceLayouts::new(&self.device).ok();
        }
        // A window of the source for a tile cut from it; none for one copied out of a stage texture.
        let held = self.checked(plan, source, window, input)?;
        let lit = Instant::now();
        let lights = self.lit(&plan.lights, source)?;
        let mut times = TileTimes {
            light_us: micros(lit),
            ..TileTimes::default()
        };
        let slot_bytes = self.slot_bytes(plan, end)?;
        let window_bytes = held.as_ref().map_or(0, GpuSource::bytes);
        let single = window_bytes + slot_bytes + self.stage_bytes;
        if single > GPU_TILE_BUDGET {
            return Err(TileFailure::Budget {
                requested: single,
                budget: GPU_TILE_BUDGET,
            });
        }
        let pipelines = self.pipelines(plan, end)?;
        // A window of another rectangle, or of another source, and a slot of another shape go
        // before the tile's own are created: the runner holds what it was charged, never more.
        // A tile copied out of a stage texture reads no window of the source.
        if held.as_ref().is_none_or(|held| {
            self.window
                .as_ref()
                .is_some_and(|(slot, _)| !slot.holds(held))
        }) {
            self.drop_window();
        }
        let key = SlotKey {
            shape: Shape::of(plan),
            end,
            links: chain::chain(&plan.steps).links.len(),
        };
        if self.slot.as_ref().is_some_and(|slot| slot.key != key) {
            self.drop_slot();
        }
        if single + self.pinned() + self.spare() > GPU_TILE_BUDGET {
            self.settle();
            self.readbacks.clear();
        }
        let requested = single + self.pinned() + self.spare();
        if requested > GPU_TILE_BUDGET {
            return Err(TileFailure::Budget {
                requested,
                budget: GPU_TILE_BUDGET,
            });
        }
        self.figures.peak = self.figures.peak.max(requested);
        // Every error the tile's work raises is the tile's, never wgpu's default handler's.
        for filter in [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::ErrorFilter::Internal,
        ] {
            self.device.push_error_scope(filter);
        }
        let submitted = self.encode(
            plan,
            (held.as_ref(), input, output),
            (window, key, slot_bytes),
            &pipelines,
            &lights,
            &mut times,
        );
        // Every scope is popped, whatever the first one answers.
        let answers: Vec<_> = (0..3)
            .map(|_| answered(self.device.pop_error_scope()))
            .collect();
        let raised = answers.iter().any(|answer| !matches!(answer, Some(None)));
        if self.lost() {
            // Nothing it held outlives its device.
            self.in_flight.clear();
            self.release();
            submitted?;
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        let tile = match (submitted, raised) {
            (Ok(tile), false) => tile,
            (Ok(_), true) => {
                // What it created is in doubt: nothing of it is kept.
                self.drop_slot();
                self.account();
                return Err(TileFailure::PIPELINE_FAILED);
            }
            (Err(failure), _) => {
                self.drop_slot();
                self.account();
                return Err(failure);
            }
        };
        let ticket = Ticket(tile.ticket);
        self.in_flight.push_back(tile);
        self.account();
        Ok(ticket)
    }

    /// Read `ticket`'s tile back as its submission asked: wait for the device to be done with it,
    /// map its copy and unpad its rows. The tiles submitted before it stay in flight for their own
    /// callers. A lost device, or a copy that could not be mapped, is `device-lost`, every tile in
    /// flight let go with it; a ticket not in flight is `pipeline-failed`.
    pub fn finish(&mut self, ticket: Ticket) -> Result<TilePixels, TileFailure> {
        let (mut tile, wait_us) = self.complete(ticket, true)?;
        let readback = tile.readback.take().ok_or(TileFailure::PIPELINE_FAILED)?;
        let reading = Instant::now();
        let read = Self::read(&readback);
        tile.times.wait_us = wait_us;
        tile.times.read_us = micros(reading);
        if self.readbacks.len() < TILES_IN_FLIGHT {
            self.readbacks.push((readback.buffer, readback.bytes));
        }
        self.finished(&tile);
        Ok(read)
    }

    /// Wait for `ticket`'s tile, a staged sweep's whose output went into a stage texture
    /// ([`TileOutput::Stage`]), to be done on the device: what a sweep after it may read. A lost
    /// device is `device-lost`, every tile in flight let go with it.
    pub fn finish_stage(&mut self, ticket: Ticket) -> Result<(), TileFailure> {
        let (mut tile, wait_us) = self.complete(ticket, false)?;
        tile.times.wait_us = wait_us;
        self.finished(&tile);
        Ok(())
    }

    /// One more tile done: its times counted.
    fn finished(&mut self, tile: &InFlight) {
        self.figures.runs += 1;
        self.figures.last = tile.times;
        self.figures.total.add(&tile.times);
        self.account();
    }

    /// Wait for `ticket`'s tile to be done on the device, and take it out of flight with how long
    /// the wait took; its copy, when `mapped` asks for one, mapped.
    fn complete(&mut self, ticket: Ticket, mapped: bool) -> Result<(InFlight, u64), TileFailure> {
        let Some(at) = self
            .in_flight
            .iter()
            .position(|tile| tile.ticket == ticket.0 && tile.readback.is_some() == mapped)
        else {
            return Err(TileFailure::PIPELINE_FAILED);
        };
        let waiting = Instant::now();
        let waited = self
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(self.in_flight[at].index.clone()),
                timeout: None,
            })
            .is_ok();
        let wait_us = micros(waiting);
        // The queue runs its submissions in order: every tile up to this one is done.
        for tile in self.in_flight.iter_mut().take(at + 1) {
            tile.done = true;
        }
        let tile = self.in_flight.remove(at).expect("found above");
        let done = waited
            && tile
                .readback
                .as_ref()
                .is_none_or(|readback| matches!(readback.mapped.try_recv(), Ok(Ok(()))));
        if !done {
            // A submission the device refused never completes: the device's maintenance then runs
            // a destroyed or lost device's callback.
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
        if self.lost() || !done {
            // Nothing it held outlives its device.
            self.in_flight.clear();
            self.release();
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        Ok((tile, wait_us))
    }

    /// Tests only: `light` computed from `source` as a run computes each light its plan reads, or
    /// the one the runner keeps: `[r, g, b, 1]`, read back.
    #[cfg(any(test, feature = "qualification"))]
    pub fn light(
        &mut self,
        source: &GpuSource,
        light: &light::GpuLight,
    ) -> Result<[f32; 4], TileFailure> {
        if self.layouts.is_none() {
            self.layouts = SourceLayouts::new(&self.device).ok();
        }
        let lit = self.lit(std::slice::from_ref(light), source)?;
        Ok(lit[0])
    }

    /// Every light of `computed`, a plan's lights, light `k` the `k`-th: each the runner keeps for
    /// `source`, or computed now from it on the runner's device and kept ([`light::read_light`]),
    /// charged to [`GPU_TILE_BUDGET`] before anything is created beside the window the runner
    /// keeps, which goes first when only that makes room. Blocks its caller until the device is
    /// done.
    fn lit(
        &mut self,
        computed: &[light::GpuLight],
        source: &GpuSource,
    ) -> Result<Vec<[f32; 4]>, TileFailure> {
        let mut lights = Vec::with_capacity(computed.len());
        for computed in computed {
            let key = light_key(source, computed);
            if let Some(kept) = self.lights.find(|held| *held == key) {
                lights.push(*kept);
                continue;
            }
            let charge = self.light_charge(source, computed)?;
            if charge > GPU_TILE_BUDGET {
                return Err(TileFailure::Budget {
                    requested: charge,
                    budget: GPU_TILE_BUDGET,
                });
            }
            if self.figures.in_use + charge > GPU_TILE_BUDGET {
                self.release();
            }
            let (compiled, _) = self.sequence(&computed.steps, light::LIGHT_FORMAT)?;
            let layouts = self.layouts.as_ref().ok_or(TileFailure::PIPELINE_FAILED)?;
            let requested = self.figures.in_use + charge;
            self.figures.peak = self.figures.peak.max(requested);
            for filter in [
                wgpu::ErrorFilter::Validation,
                wgpu::ErrorFilter::OutOfMemory,
                wgpu::ErrorFilter::Internal,
            ] {
                self.device.push_error_scope(filter);
            }
            // A light behind a spatial step is reduced from the stage texture the sweep before it
            // wrote, which the stream's tiles before this one have finished writing.
            let read = match computed.input {
                light::LightInput::Stage { texture, .. } => match self.stages.get(texture as usize)
                {
                    Some(holder) => light::read_staged_light(
                        (&self.device, &self.queue),
                        (&compiled, &self.support),
                        (holder, source.kind().boundary()),
                        computed,
                    ),
                    None => Err(GpuFallback::PipelineFailed),
                },
                _ => light::read_light(
                    (&self.device, &self.queue),
                    (&compiled, &self.support, layouts),
                    source,
                    computed,
                ),
            };
            let answers: Vec<_> = (0..3)
                .map(|_| answered(self.device.pop_error_scope()))
                .collect();
            if self.lost() {
                self.release();
                return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
            }
            if answers.iter().any(|answer| !matches!(answer, Some(None))) {
                return Err(TileFailure::PIPELINE_FAILED);
            }
            let value = read.map_err(|fallback| match fallback {
                GpuFallback::DeviceLost => TileFailure::Unavailable(TileUnavailable::DeviceLost),
                other => TileFailure::Plan(other),
            })?;
            self.figures.lights += 1;
            self.lights.insert(key, value);
            lights.push(value);
        }
        Ok(lights)
    }

    /// While `poisoned`, every later run starts each link's passes from NaN bits in every texture
    /// of its pool, every record of what they hold forgotten (`spatial::Pool::poison`): a tile still
    /// equal to another drawing's shows that no pass read scratch it did not write in that run.
    #[cfg(any(test, feature = "qualification"))]
    pub fn set_poison(&mut self, poisoned: bool) {
        self.poisoned = poisoned;
    }

    /// Lose the runner's device as a reset would: destroyed, and, once wgpu finds its queue empty,
    /// lost through the callback a real loss runs.
    #[cfg(any(test, feature = "qualification"))]
    pub fn simulate_device_loss(&self) {
        self.device.destroy();
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }

    /// `plan` checked as the stage checks it before it draws, against `window` of `source`, before
    /// anything is created: a cut of `source`, in its format, inside a window of it the device can
    /// hold; no clipping marks; a boundary, an output and a region it can draw; a spatial step only
    /// on a device that can run one and over the boundary's own texels. The window of the source
    /// the run holds.
    fn checked(
        &self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        input: TileInput,
    ) -> Result<Option<GpuSource>, TileFailure> {
        let Some((version, Derivation::Cut { origin: cut })) = plan.boundary.derivation() else {
            return Err(TileFailure::PIPELINE_FAILED);
        };
        if *version != source.version()
            || source.kind().boundary() != plan.boundary.format()
            || plan
                .steps
                .iter()
                .any(|step| matches!(step, GpuStep::Clipping(_)))
        {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        let limits = self.device.limits();
        let limit = limits.max_texture_dimension_2d;
        let shape = Shape::of(plan);
        for (width, height) in [shape.boundary, shape.output] {
            if width > limit || height > limit || width == 0 || height == 0 {
                return Err(TileFailure::Plan(GpuFallback::TextureLimit {
                    width,
                    height,
                    limit,
                }));
            }
        }
        // The source's textures take at most two tiles across and two down, as the surface holds a
        // source; a window is held transposed under a turning orientation, so both sides are held
        // to the same.
        let tile = limit.min(8192);
        let [x, y, width, height] = window;
        if width > 2 * tile || height > 2 * tile {
            return Err(TileFailure::Plan(GpuFallback::TextureLimit {
                width,
                height,
                limit: 2 * tile,
            }));
        }
        if !region_drawable(plan) {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        if plan
            .steps
            .iter()
            .any(|step| matches!(step, GpuStep::Spatial(_)))
        {
            if !spatial::supported(&limits) {
                return Err(TileFailure::Unavailable(TileUnavailable::NoAdapter));
            }
            if plan.texels.step != [1.0, 1.0] {
                return Err(TileFailure::PIPELINE_FAILED);
            }
        }
        let (cut_width, cut_height) = shape.boundary;
        if let TileInput::Stage(_) = input {
            // Copied out of a stage texture: the window is the boundary's own, inside the stage.
            let stage = source.stage();
            let exact = [x, y, width, height] == [cut.0, cut.1, cut_width, cut_height]
                && u64::from(x) + u64::from(width) <= u64::from(stage.0)
                && u64::from(y) + u64::from(height) <= u64::from(stage.1);
            return if exact {
                Ok(None)
            } else {
                Err(TileFailure::PIPELINE_FAILED)
            };
        }
        let inside = cut.0 >= x
            && cut.1 >= y
            && u64::from(cut.0) + u64::from(cut_width) <= u64::from(x) + u64::from(width)
            && u64::from(cut.1) + u64::from(cut_height) <= u64::from(y) + u64::from(height);
        if !inside {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        source
            .window(window)
            .map(Some)
            .ok_or(TileFailure::PIPELINE_FAILED)
    }

    /// What the tile's slot takes beside the window of the source, as [`TileRunner::charge`] counts
    /// it: the slot's own figures ([`Shape`], [`super::chain_charge`], each buffer at the capacity
    /// the device gives it) with the output at its exact size in `end`'s format and its readback
    /// copy, row by row padded as a copy lays it out.
    fn slot_bytes(&self, plan: &GpuPlan, end: Option<TileEnd>) -> Result<u64, TileFailure> {
        let device = &self.device;
        let shape = Shape::of(plan);
        let texels = u64::from(shape.boundary.0) * u64::from(shape.boundary.1);
        let origin = texel_origin(plan);
        let chain = chain::chain(&plan.steps);
        let buffers = |words: &[u32], steps: &[GpuStep]| {
            Ok::<u64, GpuFallback>(
                buffer_capacity(device, (words.len() * 4) as u64)?
                    + buffer_capacity(device, (blocks::block_len(steps) * 4) as u64)?,
            )
        };
        let mut words = Vec::new();
        // The cut's words, its header alone, fit the smallest buffer.
        let mut held = buffer_capacity(device, MIN_BUFFER).map_err(TileFailure::Plan)?;
        for steps in chain.links.iter().copied() {
            chain::pack_words(plan.texels, (0, 0), steps, &mut words);
            held += buffers(&words, steps).map_err(TileFailure::Plan)?;
        }
        chain::pack_words(plan.texels, output_offset(plan), chain.last, &mut words);
        held += buffers(&words, chain.last).map_err(TileFailure::Plan)?;
        let tail = shape.intermediate.map_or(0, |format| {
            texels * u64::from(format.block_copy_size(None).unwrap_or(16))
        });
        let (width, height) = shape.output;
        let row = width * end_texel_bytes(end, shape.format);
        let output = u64::from(row) * u64::from(height);
        // A stage sweep's output is copied into a stage texture: nothing is read back.
        let readback = end.map_or(0, |_| u64::from(padded(row)) * u64::from(height));
        Ok(texels * shape.format.texel_bytes() as u64
            + held
            + chain_charge(&plan.steps, shape.boundary, origin, shape.format).total()
            + tail
            + output
            + readback)
    }

    /// Every link's compiled sequence of `plan`, the last one's last, writing a chain's
    /// intermediates and then `end`'s format: each the runner holds, or compiled now on the calling
    /// thread through the stage's one `compile`, and kept, failed or not.
    fn pipelines(
        &mut self,
        plan: &GpuPlan,
        end: Option<TileEnd>,
    ) -> Result<Vec<(Compiled, u64)>, TileFailure> {
        let chain = chain::chain(&plan.steps);
        let intermediate = intermediate_format(plan.boundary.format());
        // A stage sweep's last link writes an intermediate, as the same link of a chain does.
        let last = end_format(end, plan.boundary.format());
        let sequences = chain
            .links
            .iter()
            .map(|steps| (*steps, intermediate))
            .chain(std::iter::once((chain.last, last)));
        let mut ready = Vec::new();
        for (steps, format) in sequences {
            ready.push(self.sequence(steps, format)?);
        }
        Ok(ready)
    }

    /// The compiled sequence of `steps` writing `format`: the runner's, or compiled now on the
    /// calling thread through the stage's one `compile`, and kept, failed or not.
    fn sequence(
        &mut self,
        steps: &[GpuStep],
        format: wgpu::TextureFormat,
    ) -> Result<(Compiled, u64), TileFailure> {
        let found = self
            .sequences
            .find(|key| holds(key, steps, format))
            .cloned();
        let (compiled, id) = match found {
            Some(found) => found,
            None => {
                self.figures.compiles += 1;
                let id = self.figures.compiles;
                let compiled =
                    compile(&self.device, &self.support, steps, format).map_err(Arc::from);
                let key = (compile::signature(steps), format);
                self.sequences.insert(key, (compiled, id)).clone()
            }
        };
        Ok((compiled.map_err(|_| TileFailure::PIPELINE_FAILED)?, id))
    }

    /// The tile's work, encoded and submitted with its mapping asked for: the window of the source
    /// held, or uploaded whole; the slot of `key` held, or created; the boundary cut from the
    /// window; every link before the last into its intermediate; the last into the output, through
    /// a geometry tail's intermediate when it holds one; and the output copied out. Each link's
    /// state, kept planes and pool are the tile's own, a fresh evaluation, and go as it returns, so
    /// the device frees them once the GPU is done with them; the slot's textures and buffers stay
    /// for the next tile of its shape, each written whole before it is read.
    fn encode(
        &mut self,
        plan: &GpuPlan,
        (held, input, output): (Option<&GpuSource>, TileInput, TileOutput),
        (tile_window, key, slot_bytes): ([u32; 4], SlotKey, u64),
        pipelines: &[(Compiled, u64)],
        lights: &[[f32; 4]],
        times: &mut TileTimes,
    ) -> Result<InFlight, TileFailure> {
        let layouts = self.layouts.as_ref().ok_or(TileFailure::PIPELINE_FAILED)?;
        let started = Instant::now();
        if let Some(held) = held
            && self.window.is_none()
        {
            let mut slot = SourceSlot::new(&self.device, held, layouts, |_| Ok(()))
                .map_err(TileFailure::Plan)?;
            while !slot.ready() {
                if slot.upload(&self.queue, held, u64::MAX) == 0 {
                    return Err(TileFailure::Plan(GpuFallback::SourceMissing));
                }
            }
            self.window = Some((slot, held.bytes()));
            self.figures.uploads += 1;
        }
        times.upload_us = micros(started);
        let encoding = Instant::now();
        let chain = chain::chain(&plan.steps);
        let texels = plan.texels;
        let origin = texel_origin(plan);
        let size = key.shape.boundary;
        // The words every link and the last write, packed before the slot is fitted to them.
        let mut link_words = Vec::with_capacity(chain.links.len());
        for steps in chain.links.iter().copied() {
            let mut words = Vec::new();
            chain::pack_words(texels, (0, 0), steps, &mut words);
            link_words.push(words);
        }
        let mut last_words = Vec::new();
        chain::pack_words(texels, output_offset(plan), chain.last, &mut last_words);
        let derivation = plan
            .boundary
            .derivation()
            .map(|(_, derivation)| derivation)
            .ok_or(TileFailure::PIPELINE_FAILED)?;
        // The cut's words, for a boundary cut from the window of the source the runner holds.
        let cut = match input {
            TileInput::Source => {
                let (window, _) = self.window.as_ref().expect("the window held");
                window
                    .words(derivation, size)
                    .ok_or(TileFailure::PIPELINE_FAILED)?
            }
            TileInput::Stage(_) => Vec::new(),
        };
        if self.slot.is_none() {
            self.slot = Some(Slot::create(&self.device, key)?);
            self.figures.slots += 1;
        }
        let device = &self.device;
        let queue = &self.queue;
        let slot = self.slot.as_mut().expect("the slot fitted");
        // Each buffer fitted to what this tile writes: one of a larger capacity replaces it.
        fitted(
            device,
            &mut slot.cut,
            (cut.len() * 4) as u64,
            "luxforge.tiles.cut",
        )?;
        for ((_, words, blocks), (packed, steps)) in slot
            .links
            .iter_mut()
            .zip(link_words.iter().zip(chain.links.iter().copied()))
        {
            fitted(
                device,
                words,
                (packed.len() * 4) as u64,
                "luxforge.tiles.link_words",
            )?;
            fitted(
                device,
                blocks,
                (blocks::block_len(steps) * 4) as u64,
                "luxforge.tiles.link_blocks",
            )?;
        }
        fitted(
            device,
            &mut slot.words,
            (last_words.len() * 4) as u64,
            "luxforge.tiles.words",
        )?;
        fitted(
            device,
            &mut slot.blocks,
            (blocks::block_len(chain.last) * 4) as u64,
            "luxforge.tiles.blocks",
        )?;
        slot.account();
        let slot = self.slot.as_ref().expect("the slot fitted");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.tiles.encoder"),
        });
        match input {
            // The boundary, cut from the window by the derivation's own pass.
            TileInput::Source => {
                queue.write_buffer(&slot.cut.buffer, 0, &le_bytes(&cut));
                let (window, _) = self.window.as_ref().expect("the window held");
                window.encode(
                    device,
                    &mut encoder,
                    layouts,
                    derivation,
                    &slot.cut.buffer,
                    &slot
                        .boundary
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    size,
                );
            }
            // Copied out of the stage texture the sweep before wrote, texel for texel.
            TileInput::Stage(k) => {
                self.stages[k as usize].copy_out(&mut encoder, &slot.boundary, tile_window);
            }
        }
        // The pool every link's scratch planes are taken from, fitted to every link before any
        // link's planes, as the slot fits its own; the tile's own, charged with it.
        let mut pool = spatial::Pool::default();
        let every = chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last));
        pool.fit(
            device,
            &spatial::PoolKey::of(every, size, origin),
            &mut |_| Ok::<(), TileFailure>(()),
            &mut |_, _| {},
        )?;
        #[cfg(any(test, feature = "qualification"))]
        pool.set_poisoned(self.poisoned);
        // Every light the plan reads, in its light plane before any pass reads it.
        for (k, light) in (0u32..).zip(lights) {
            pool.write_light(queue, k, *light);
        }
        // Each link before the last, from the boundary, into its intermediate: a link state of the
        // tile's own over the slot's texture and buffers.
        let mut links: Vec<chain::LinkSlot> = Vec::with_capacity(chain.links.len());
        let mut input = chain::boundary_key(plan.boundary.version());
        let link_parts = slot.links.iter().zip(&link_words);
        for ((steps, (compiled, id)), ((texture, words, blocks), packed)) in
            chain.links.iter().copied().zip(pipelines).zip(link_parts)
        {
            let reads = links.last().map_or(&slot.boundary, |link| &link.texture);
            let bindings = self.bindings(reads, &words.buffer, &blocks.buffer);
            let mut link = chain::LinkSlot::new(
                texture.clone(),
                Charged {
                    buffer: words.buffer.clone(),
                    bytes: words.bytes,
                },
                Charged {
                    buffer: blocks.buffer.clone(),
                    bytes: blocks.bytes,
                },
                bindings,
                intermediate_bytes(size, key.shape.format),
            );
            link.spatial = spatial::PlanesKey::of(steps, size, origin).map(|key| {
                Box::new(SpatialSlot::new(
                    spatial::Planes::create(device, key),
                    &mut pool,
                ))
            });
            link.write(queue, packed, steps);
            link.encode(
                device,
                queue,
                &mut encoder,
                (compiled, *id),
                &mut pool,
                steps,
                packed,
                input,
                (texels, size),
                None,
            );
            input = link.key().unwrap_or(input);
            links.push(link);
        }
        // The last link: its spatial step's passes, then its frame into the output, through the
        // geometry tail's intermediate when it holds one.
        let (compiled, id) = pipelines.last().ok_or(TileFailure::PIPELINE_FAILED)?;
        queue.write_buffer(&slot.words.buffer, 0, &le_bytes(&last_words));
        let mut written = blocks::WrittenBlocks::default();
        written.write(queue, &slot.blocks.buffer, chain.last, BLOCK_CHUNK);
        let reads = links.last().map_or(&slot.boundary, |link| &link.texture);
        let bindings = self.bindings(reads, &slot.words.buffer, &slot.blocks.buffer);
        let mut last = spatial::PlanesKey::of(chain.last, size, origin)
            .map(|key| SpatialSlot::new(spatial::Planes::create(device, key), &mut pool));
        if let Some(spatial) = last.as_mut() {
            spatial.tick(
                device,
                queue,
                &mut encoder,
                (&compiled.spatial, *id),
                &bindings,
                &mut pool,
                chain.last,
                (&last_words, written.words()),
                input,
                (texels, size),
                None,
            );
        }
        let planes = last
            .as_ref()
            .and_then(|spatial| spatial.groups())
            .and_then(|groups| groups.fragment.as_ref());
        let output_size = key.shape.output;
        let output_view = slot
            .output
            .create_view(&wgpu::TextureViewDescriptor::default());
        let as_size = |(width, height): (u32, u32)| (width as f32, height as f32);
        match (&slot.intermediate, &compiled.tail) {
            (Some(intermediate), Some(tail)) => {
                let tail_bindings =
                    self.bindings(intermediate, &slot.words.buffer, &slot.blocks.buffer);
                encode_pass_over(
                    &mut encoder,
                    &intermediate.create_view(&wgpu::TextureViewDescriptor::default()),
                    &compiled.render,
                    (&bindings, planes),
                    as_size(size),
                    None,
                );
                encode_pass_over(
                    &mut encoder,
                    &output_view,
                    tail,
                    (&tail_bindings, None),
                    as_size(output_size),
                    None,
                );
            }
            (None, None) => encode_pass_over(
                &mut encoder,
                &output_view,
                &compiled.render,
                (&bindings, planes),
                as_size(output_size),
                None,
            ),
            _ => return Err(TileFailure::PIPELINE_FAILED),
        }
        let readback = match output {
            // The output copied out, each row padded to the copy's alignment, into a copy a tile
            // read back left or a new one.
            TileOutput::Read(end) => {
                let (width, height) = output_size;
                let padded = padded(width * end.texel_bytes());
                let readback_bytes = u64::from(padded) * u64::from(height);
                let buffer = match self
                    .readbacks
                    .iter()
                    .position(|(_, bytes)| *bytes == readback_bytes)
                {
                    Some(at) => self.readbacks.swap_remove(at).0,
                    None => device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("luxforge.tiles.readback"),
                        size: readback_bytes,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                };
                encoder.copy_texture_to_buffer(
                    slot.output.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(padded),
                            rows_per_image: Some(height),
                        },
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                Some((buffer, readback_bytes, padded, end))
            }
            // The tile's rectangle of its last link's intermediate, which covers its boundary from
            // the cut's origin, into the stage texture at the rectangle.
            TileOutput::Stage { writes, rect } => {
                let Derivation::Cut { origin: (x, y) } = derivation else {
                    return Err(TileFailure::PIPELINE_FAILED);
                };
                // A sweep's identity tail leaves its last link's output over the whole boundary in
                // the tail's intermediate; a plan with no tail and no region leaves it in the
                // output.
                let (from, (width, height)) = match &slot.intermediate {
                    Some(intermediate) => (intermediate, size),
                    None => (&slot.output, output_size),
                };
                if rect[0] < *x
                    || rect[1] < *y
                    || u64::from(rect[2]) > u64::from(*x) + u64::from(width)
                    || u64::from(rect[3]) > u64::from(*y) + u64::from(height)
                {
                    return Err(TileFailure::PIPELINE_FAILED);
                }
                self.stages[writes as usize].copy_in(
                    &mut encoder,
                    from,
                    (rect[0] - x, rect[1] - y),
                    rect,
                );
                None
            }
        };
        let index = queue.submit([encoder.finish()]);
        let readback = readback.map(|(buffer, bytes, padded, end)| {
            let (sender, mapped) = mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            Readback {
                buffer,
                bytes,
                mapped,
                size: output_size,
                padded,
                end,
            }
        });
        times.encode_us = micros(encoding);
        self.tickets += 1;
        let (window_generation, slot_generation) = self.generations;
        let readback_bytes = readback.as_ref().map_or(0, |readback| readback.bytes);
        Ok(InFlight {
            ticket: self.tickets,
            index,
            readback,
            window: (window_generation, held.map_or(0, GpuSource::bytes)),
            slot: (slot_generation, slot_bytes - readback_bytes),
            done: false,
            times: *times,
        })
    }

    /// The mapped copy's rows, unpadded, as its tile's end reads them; the copy unmapped.
    fn read(tile: &Readback) -> TilePixels {
        let (width, height) = tile.size;
        let row = (width * tile.end.texel_bytes()) as usize;
        let mapped = tile.buffer.slice(..).get_mapped_range();
        let rows = mapped
            .chunks_exact(tile.padded as usize)
            .take(height as usize)
            .map(|line| &line[..row]);
        let pixels = match tile.end {
            TileEnd::Codes => {
                let mut codes = Vec::with_capacity(row * height as usize);
                for line in rows {
                    codes.extend_from_slice(line);
                }
                TilePixels::Codes(codes)
            }
            TileEnd::Linear => {
                let mut values = Vec::with_capacity(width as usize * height as usize);
                for line in rows {
                    values.extend(line.chunks_exact(16).map(|texel| {
                        std::array::from_fn(|channel| {
                            let at = channel * 4;
                            f32::from_le_bytes([
                                texel[at],
                                texel[at + 1],
                                texel[at + 2],
                                texel[at + 3],
                            ])
                        })
                    }));
                }
                TilePixels::Linear(values)
            }
        };
        drop(mapped);
        tile.buffer.unmap();
        pixels
    }

    /// Group 0 over `input`: the words, the blocks and the texture a link reads as its boundary,
    /// as the slot binds its own.
    fn bindings(
        &self,
        input: &wgpu::Texture,
        words: &wgpu::Buffer,
        blocks: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let view = input.create_view(&wgpu::TextureViewDescriptor::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.tiles.bindings"),
            layout: &self.support.layout,
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
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        })
    }
}

/// The stage pixel of `plan`'s boundary texel `(0, 0)`.
fn texel_origin(plan: &GpuPlan) -> (u32, u32) {
    (
        plan.texels.origin[0].max(0.0) as u32,
        plan.texels.origin[1].max(0.0) as u32,
    )
}

/// What a light the runner computed is kept under: the source's version, the light's stage, and its
/// steps' programs, words and blocks.
fn light_key(source: &GpuSource, light: &light::GpuLight) -> u64 {
    use std::hash::{Hash, Hasher};
    // A light behind a spatial step is kept under its input's identity, as the photo surface keeps
    // it, whichever light plane a sweep's plan numbers it.
    if let light::LightInput::Stage { key, .. } = light.input {
        let mut hasher = std::hash::DefaultHasher::new();
        ("stage", source.version(), light.stage, key).hash(&mut hasher);
        return hasher.finish();
    }
    let mut words = Vec::new();
    chain::pack_words(super::TexelMap::IDENTITY, (0, 0), &light.steps, &mut words);
    let mut hasher = std::hash::DefaultHasher::new();
    (source.version(), light.stage, words).hash(&mut hasher);
    for step in &light.steps {
        step.each_block(&mut |block| block.hash(&mut hasher));
    }
    for (kind, entry, _) in light.steps.iter().flat_map(GpuStep::signature) {
        (format!("{kind:?}"), entry).hash(&mut hasher);
    }
    hasher.finish()
}

/// A row of `bytes` padded to a texture copy's row alignment.
fn padded(bytes: u32) -> u32 {
    bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// A cache of at most `bound` entries, the least recently found or inserted evicted first when an
/// insertion would pass it.
struct Lru<K, V> {
    entries: Vec<(K, V, u64)>,
    clock: u64,
    bound: usize,
}

impl<K, V> Lru<K, V> {
    fn new(bound: usize) -> Self {
        Self {
            entries: Vec::new(),
            clock: 0,
            bound: bound.max(1),
        }
    }

    /// The value of the first entry whose key `matches`, now the most recently used.
    fn find(&mut self, matches: impl Fn(&K) -> bool) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        let (_, value, used) = self.entries.iter_mut().find(|(key, ..)| matches(key))?;
        *used = clock;
        Some(value)
    }

    /// Keep `value` under `key`, evicting the least recently used entry when the cache is full.
    fn insert(&mut self, key: K, value: V) -> &V {
        self.clock += 1;
        if self.entries.len() >= self.bound
            && let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, (.., used))| *used)
                .map(|(at, _)| at)
        {
            self.entries.swap_remove(oldest);
        }
        self.entries.push((key, value, self.clock));
        let (_, value, _) = self.entries.last().expect("just kept");
        value
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
#[path = "tiles_tests.rs"]
mod tests;
