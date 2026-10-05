//! The tile runner (`docs/design/gpu-first.md`, stage 4; `docs/design/gpu-preview.md`,
//! "Qualifying a program"): one tile of a stack drawn on a device of its own and read back, the GPU
//! half of a pixel read and of an export's tile, run on its caller's thread.
//!
//! Built and not yet wired: nothing in the desktop opens a [`TileRunner`] yet. The desktop's tile
//! worker, which will own one on a thread of its own and answer the core's tile contract through
//! it, mapping each [`TileFailure`] onto the contract's own reasons, is the next step.
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
//! - **A tile.** [`TileRunner::run`] draws a plan as the photo surface draws each tile of its
//!   picture at rest: a fresh evaluation — every link, every spatial pass and the output, nothing
//!   kept from an earlier run but the compiled sequences — over a boundary cut on the GPU from a
//!   window of the source ([`Derivation::Cut`], [`GpuSource::window`]), through the stage's one
//!   compile, its chain of links and its pool of scratch planes, ending in the CPU's output
//!   quantizer's codes ([`TileEnd::Codes`]), or in the linear values that quantizer reads
//!   ([`TileEnd::Linear`]). It copies the output into a buffer of its own, submits once, waits for
//!   its own device once (`poll(Wait)`), maps the copy and unpads its rows. It blocks only its
//!   caller, never touches Iced's device or queue, and starts no thread: it compiles on its
//!   caller's thread, which is never the interface thread.
//! - **Bounds.** Everything a run creates is charged to [`GPU_TILE_BUDGET`], beside and apart from
//!   the photo surface's GPU-preview budget, before anything is created: the window of the source,
//!   the boundary, each link's intermediate, words and blocks, every link's kept planes and the
//!   pool once ([`super::chain_charge`]), a geometry tail's intermediate, the output and its
//!   readback copy ([`TileRunner::charge`]). A run past it is refused (`tiles-budget`) having
//!   created and compiled nothing. Between runs the runner holds the window of the source its last
//!   run read, which the next run of the same window reads again ([`TileRunner::release`] lets it
//!   go), and at most [`TILE_PIPELINE_CACHE`] compiled sequences beside its spatial passes' own
//!   cache of 64 modules. Everything else a run created goes before its wait, and the device frees
//!   it once that wait ends; dropping the runner releases the rest. The window's upload passes
//!   through wgpu's staging, one copy of the window's bytes until the wait ends, which, as the
//!   photo surface's own uploads, is not charged.
//! - **Determinism.** One device, one driver: the same plan over the same window draws the same
//!   bytes every run, each a fresh evaluation in a fixed order with no float atomics.
//! - **Why the reference would answer instead.** Every failure is shaped as the core's tile
//!   contract carries it, though this crate names no core type: unavailable (`no-adapter`,
//!   `refused`, `device-lost`, `adapter-mismatch`), the budget (`tiles-budget`), or the plan's own
//!   reason the stage cannot run it — `pipeline-failed`, `texture-limit`, `buffer-limit` or
//!   `source-missing` for a window whose pixels were let go — exactly as the surface names them.
use super::{
    BLOCK_CHUNK, Charged, Compiled, Derivation, GpuFallback, GpuPlan, GpuSource, GpuStep,
    MIN_BUFFER, OUTPUT_FORMAT, SAMPLED_FORMAT, Shape, SourceLayouts, SourceSlot, SpatialSlot,
    Support, answered, blocks, buffer_capacity, chain, chain_charge, compile, encode_pass_over,
    gpu_stage_refused, intermediate_bytes, intermediate_format, le_bytes, output_offset,
    region_drawable, spatial, storage_buffer, supported,
};
use crate::adapters::{self, Adapter, Unopened};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// What one tile runner may hold on its device at once: the window of the source, the tile's slot
/// and its readback copy, the most a run holds. Proposed, not decided: 1 GiB, a budget of its own
/// beside the photo surface's 2 GiB GPU-preview budget, whose slots a run never shares, so the
/// two together may hold 3 GiB of unified memory. By the runner's own charge over the core's plan,
/// an estimate and not a measurement, a tile in the interior of a 60-megapixel RAW (9504 × 6336)
/// through Detail and all three Presence fields reads the tile grown by its summed halo of 465
/// pixels on every side, anchored: at 2048 pixels a 3281-pixel square window, charged 1,331.5 MB,
/// past it; at 1024 a 2257-pixel one, charged 622.6 MB.
pub const GPU_TILE_BUDGET: u64 = 1 << 30;

/// How many compiled program sequences a tile runner keeps, ready and failed ones together, the
/// least recently run evicted first: the photo surface's own bound ([`super::PIPELINE_CACHE`]),
/// which holds every link of the largest plan a tile draws, a link for the colour steps before its
/// first spatial step and one for each of up to 18 spatial steps, with room for both ends of each.
pub const TILE_PIPELINE_CACHE: usize = 64;

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
    /// The bytes it holds now, as charged: the window of the source between runs.
    pub in_use: u64,
    /// The most it has held at once: a run's whole charge.
    pub peak: u64,
    /// Program sequences it has compiled, ready or failed: each one its cache did not hold.
    pub compiles: u64,
    /// Tiles it has drawn and read back.
    pub runs: u64,
}

/// One tile of a stack drawn on a device of its own and read back, on its caller's thread (the
/// module documentation).
pub struct TileRunner {
    /// The window of the source the last run read, with its bytes as charged, kept for a next run
    /// of the same window.
    window: Option<(SourceSlot, u64)>,
    sequences: Lru<Key, Sequence>,
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

/// A run's work on the GPU, submitted: the readback copy, its mapping's answer and its layout.
struct Submitted {
    readback: wgpu::Buffer,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    size: (u32, u32),
    padded: u32,
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
            sequences: Lru::new(TILE_PIPELINE_CACHE),
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

    /// Let the window of the source go, which a run keeps for a next run of the same window: the
    /// runner then holds its compiled sequences alone.
    pub fn release(&mut self) {
        self.window = None;
        self.figures.in_use = 0;
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
        let held = self.checked(plan, source, window)?;
        Ok(held.bytes() + self.slot_bytes(plan, end)?)
    }

    /// `plan` drawn over a boundary cut from `window` (`[x, y, width, height]` of the content
    /// stage) of `source`, as the photo surface draws a tile of its picture at rest, and read back
    /// as `end` asks: its output's pixels, row by row, the boundary's size, or its region's or
    /// tail's output. The plan's boundary must be a cut of `source` inside `window`
    /// ([`super::GpuBoundary::derived`]), and the plan hold no clipping marks, whose colours are not
    /// codes; anything else is `pipeline-failed`. Charged to [`GPU_TILE_BUDGET`] before anything
    /// is created; blocks its caller until its own device is done. An error the device raises
    /// while the run's work is created and submitted — a validation, memory or internal one — is
    /// `pipeline-failed` too, and a device lost, or a copy that could not be mapped, `device-lost`.
    pub fn run(
        &mut self,
        plan: &GpuPlan,
        source: &GpuSource,
        window: [u32; 4],
        end: TileEnd,
    ) -> Result<TilePixels, TileFailure> {
        if self.lost() {
            // Nothing it held outlives its device.
            self.release();
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        if self.layouts.is_none() {
            // Opened before the output encoding was installed: the cut's passes, made now.
            self.layouts = SourceLayouts::new(&self.device).ok();
        }
        let held = self.checked(plan, source, window)?;
        let requested = held.bytes() + self.slot_bytes(plan, end)?;
        if requested > GPU_TILE_BUDGET {
            return Err(TileFailure::Budget {
                requested,
                budget: GPU_TILE_BUDGET,
            });
        }
        let pipelines = self.pipelines(plan, end)?;
        // A window of another rectangle, or of another source, goes before the run's own is
        // created: it holds what it was charged, never more.
        if self
            .window
            .as_ref()
            .is_some_and(|(slot, _)| !slot.holds(&held))
        {
            self.release();
        }
        self.figures.in_use = requested;
        self.figures.peak = self.figures.peak.max(requested);
        // Every error the run's work raises is the run's, never wgpu's default handler's.
        for filter in [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::ErrorFilter::Internal,
        ] {
            self.device.push_error_scope(filter);
        }
        let waited = self
            .submit(plan, &held, end, &pipelines)
            .map(|submitted| self.wait(submitted));
        // Every scope is popped, whatever the first one answers.
        let answers: Vec<_> = (0..3)
            .map(|_| answered(self.device.pop_error_scope()))
            .collect();
        let raised = answers.iter().any(|answer| !matches!(answer, Some(None)));
        let lost = self.lost();
        if lost {
            // Nothing it held outlives its device.
            self.window = None;
        }
        self.figures.in_use = self.window.as_ref().map_or(0, |(_, bytes)| *bytes);
        let (submitted, mapped) = waited?;
        if lost {
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        if raised {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        if !mapped {
            return Err(TileFailure::Unavailable(TileUnavailable::DeviceLost));
        }
        let read = Self::read(&submitted, end);
        self.figures.runs += 1;
        Ok(read)
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
    ) -> Result<GpuSource, TileFailure> {
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
        let inside = cut.0 >= x
            && cut.1 >= y
            && u64::from(cut.0) + u64::from(cut_width) <= u64::from(x) + u64::from(width)
            && u64::from(cut.1) + u64::from(cut_height) <= u64::from(y) + u64::from(height);
        if !inside {
            return Err(TileFailure::PIPELINE_FAILED);
        }
        source.window(window).ok_or(TileFailure::PIPELINE_FAILED)
    }

    /// What the tile's slot takes beside the window of the source, as [`TileRunner::charge`] counts
    /// it: the slot's own figures ([`Shape`], [`super::chain_charge`], each buffer at the capacity
    /// the device gives it) with the output at its exact size in `end`'s format and its readback
    /// copy, row by row padded as a copy lays it out.
    fn slot_bytes(&self, plan: &GpuPlan, end: TileEnd) -> Result<u64, TileFailure> {
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
        let row = width * end.texel_bytes();
        let output = u64::from(row) * u64::from(height);
        let readback = u64::from(padded(row)) * u64::from(height);
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
        end: TileEnd,
    ) -> Result<Vec<(Compiled, u64)>, TileFailure> {
        let chain = chain::chain(&plan.steps);
        let intermediate = intermediate_format(plan.boundary.format());
        let sequences = chain
            .links
            .iter()
            .map(|steps| (*steps, intermediate))
            .chain(std::iter::once((chain.last, end.format())));
        let mut ready = Vec::new();
        for (steps, format) in sequences {
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
            ready.push((compiled.map_err(|_| TileFailure::PIPELINE_FAILED)?, id));
        }
        Ok(ready)
    }

    /// The run's work, encoded and submitted with its mapping asked for: the window of the source
    /// held, or uploaded whole; the boundary cut from it; every link before the last into its
    /// intermediate; the last into the output, through a geometry tail's intermediate when it
    /// holds one; and the output copied out. What it created besides the window and the copy goes
    /// as it returns, before the wait, so the device frees it once the GPU is done with it.
    fn submit(
        &mut self,
        plan: &GpuPlan,
        held: &GpuSource,
        end: TileEnd,
        pipelines: &[(Compiled, u64)],
    ) -> Result<Submitted, TileFailure> {
        let layouts = self.layouts.as_ref().ok_or(TileFailure::PIPELINE_FAILED)?;
        if self.window.is_none() {
            let mut slot = SourceSlot::new(&self.device, held, layouts, |_| Ok(()))
                .map_err(TileFailure::Plan)?;
            while !slot.ready() {
                if slot.upload(&self.queue, held, u64::MAX) == 0 {
                    return Err(TileFailure::Plan(GpuFallback::SourceMissing));
                }
            }
            self.window = Some((slot, held.bytes()));
        }
        let (device, queue) = (&self.device, &self.queue);
        let (window, _) = self.window.as_ref().expect("the window held");
        let shape = Shape::of(plan);
        let size = shape.boundary;
        let origin = texel_origin(plan);
        let texels = plan.texels;
        let chain = chain::chain(&plan.steps);
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
        let buffer = |label, bytes| {
            buffer_capacity(device, bytes)
                .map(|capacity| storage_buffer(device, label, capacity))
                .map_err(TileFailure::Plan)
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.tiles.encoder"),
        });
        // The boundary, cut from the window by the derivation's own pass.
        let boundary = texture(
            "luxforge.tiles.boundary",
            size,
            shape.format.texture(),
            drawn,
        );
        let derivation = plan
            .boundary
            .derivation()
            .map(|(_, derivation)| derivation)
            .ok_or(TileFailure::PIPELINE_FAILED)?;
        let cut = window
            .words(derivation, size)
            .ok_or(TileFailure::PIPELINE_FAILED)?;
        let cut_words = buffer("luxforge.tiles.cut", (cut.len() * 4) as u64)?;
        queue.write_buffer(&cut_words, 0, &le_bytes(&cut));
        window.encode(
            device,
            &mut encoder,
            layouts,
            derivation,
            &cut_words,
            &boundary.create_view(&wgpu::TextureViewDescriptor::default()),
            size,
        );
        // The pool every link's scratch planes are taken from, fitted to every link before any
        // link's planes, as the slot fits its own; charged with the run.
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
        // Each link before the last, from the boundary, into its intermediate.
        let mut links: Vec<chain::LinkSlot> = Vec::with_capacity(chain.links.len());
        let mut input = chain::boundary_key(plan.boundary.version());
        let mut words = Vec::new();
        for (steps, (compiled, id)) in chain.links.iter().copied().zip(pipelines) {
            chain::pack_words(texels, (0, 0), steps, &mut words);
            let word_bytes =
                buffer_capacity(device, (words.len() * 4) as u64).map_err(TileFailure::Plan)?;
            let block_bytes = buffer_capacity(device, (blocks::block_len(steps) * 4) as u64)
                .map_err(TileFailure::Plan)?;
            let link_words = Charged {
                buffer: storage_buffer(device, "luxforge.tiles.link_words", word_bytes),
                bytes: word_bytes,
            };
            let link_blocks = Charged {
                buffer: storage_buffer(device, "luxforge.tiles.link_blocks", block_bytes),
                bytes: block_bytes,
            };
            let reads = links.last().map_or(&boundary, |link| &link.texture);
            let bindings = self.bindings(reads, &link_words.buffer, &link_blocks.buffer);
            let mut link = chain::LinkSlot::new(
                texture(
                    "luxforge.tiles.link",
                    size,
                    intermediate_format(shape.format),
                    drawn,
                ),
                link_words,
                link_blocks,
                bindings,
                intermediate_bytes(size, shape.format),
            );
            link.spatial = spatial::PlanesKey::of(steps, size, origin).map(|key| {
                Box::new(SpatialSlot::new(
                    spatial::Planes::create(device, key),
                    &mut pool,
                ))
            });
            link.write(queue, &words, steps);
            link.encode(
                device,
                queue,
                &mut encoder,
                (compiled, *id),
                &mut pool,
                steps,
                &words,
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
        chain::pack_words(texels, output_offset(plan), chain.last, &mut words);
        let last_words = buffer("luxforge.tiles.words", (words.len() * 4) as u64)?;
        queue.write_buffer(&last_words, 0, &le_bytes(&words));
        let last_blocks = buffer(
            "luxforge.tiles.blocks",
            (blocks::block_len(chain.last) * 4) as u64,
        )?;
        let mut written = blocks::WrittenBlocks::default();
        written.write(queue, &last_blocks, chain.last, BLOCK_CHUNK);
        let reads = links.last().map_or(&boundary, |link| &link.texture);
        let bindings = self.bindings(reads, &last_words, &last_blocks);
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
                (&words, written.words()),
                input,
                (texels, size),
                None,
            );
        }
        let planes = last
            .as_ref()
            .and_then(|spatial| spatial.groups())
            .and_then(|groups| groups.fragment.as_ref());
        let output_size = shape.output;
        let output = texture(
            "luxforge.tiles.output",
            output_size,
            end.format(),
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let as_size = |(width, height): (u32, u32)| (width as f32, height as f32);
        match (shape.intermediate, &compiled.tail) {
            (Some(format), Some(tail)) => {
                let intermediate = texture("luxforge.tiles.intermediate", size, format, drawn);
                let tail_bindings = self.bindings(&intermediate, &last_words, &last_blocks);
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
        // The output copied out, each row padded to the copy's alignment.
        let (width, height) = output_size;
        let padded = padded(width * end.texel_bytes());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.tiles.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
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
        queue.submit([encoder.finish()]);
        let (sender, mapped) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        Ok(Submitted {
            readback,
            mapped,
            size: output_size,
            padded,
        })
    }

    /// Wait, once, on the runner's own device, for its most recent submission — `submitted`'s, as
    /// the runner is its device's one submitter — and answer whether its copy was mapped. A
    /// submission the device refused leaves an earlier one most recent, so the wait still ends,
    /// and a destroyed or lost device's maintenance then runs its lost callback.
    fn wait(&self, submitted: Submitted) -> (Submitted, bool) {
        let waited = self
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .is_ok();
        let mapped = waited && matches!(submitted.mapped.try_recv(), Ok(Ok(())));
        (submitted, mapped)
    }

    /// The mapped copy's rows, unpadded, as `end` reads them; the copy unmapped.
    fn read(submitted: &Submitted, end: TileEnd) -> TilePixels {
        let (width, height) = submitted.size;
        let row = (width * end.texel_bytes()) as usize;
        let mapped = submitted.readback.slice(..).get_mapped_range();
        let rows = mapped
            .chunks_exact(submitted.padded as usize)
            .take(height as usize)
            .map(|line| &line[..row]);
        let pixels = match end {
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
        submitted.readback.unmap();
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
