//! The desktop's GPU tile worker (`docs/design/gpu-first.md`, stage 4; `docs/design/gpu-preview.md`,
//! "Qualifying a program"): the core's tile contract ([`luxforge_core::tiles`]) answered on a
//! device of the desktop's own through the photo surface's tile runner ([`TileRunner`]), so a
//! pixel read and an export's band are drawn by the GPU as the picture on screen is drawn.
//!
//! The launch builds one ([`launch`]) and hands it to the catalog owner through `HostConfig`, which
//! submits every pixel read to it — `render.sample`, the modules' queries that read pixels (the
//! neutral picker, `mask.sample-input`) and a mutation's planning read (a colour-limited stroke's
//! seed) — and whose export lane streams every export through it (`docs/design/export.md`), as its
//! preview lane streams a developed photograph's catalog tiers (`docs/design/catalog.md`).
//!
//! - **The window's adapter.** Iced hands the photo surface a device, not the adapter it came from,
//!   and names that adapter only in its system information. The launch's worker opens nothing until
//!   its adapter is named ([`GpuTiles::adopt_adapter`]), and answers the reference as
//!   `surface-pending` until then. The launch names it as its window opens where the host leaves no
//!   doubt which adapter the window draws with — the one hardware adapter, or the one software
//!   adapter a launch drawing on it is offered (`app::renderer::name_once_open`) — and the desktop
//!   names it again from Iced's system information once the photo surface has checked its GPU
//!   stage, the only naming on a host with several adapters. The window's naming confirms the
//!   launch's, or replaces it: between jobs the worker then lets go of its runner, its tiles in
//!   flight unread, and a stream begun on it ends naming `adapter-mismatch`, so no output mixes two
//!   adapters' tiles.
//! - **One thread.** `luxforge-gpu-tiles`, started by the first call or stream and asleep on its
//!   condition variable while nothing waits (performance rule 8). It owns the runner, which the
//!   first call or stream that needs it opens on the adapter the window's renderer reports drawing
//!   with, never another ([`crate::adapters::tile_runner`]): a host without that adapter answers
//!   `tiles-unavailable adapter-mismatch`, a launch with `--no-gpu-render` `refused`, a desktop
//!   that named no adapter, or a launch that refused the GPU stage on a host whose only adapter is
//!   a software one not adopted, `no-adapter`, and a lost device `device-lost` from then on. A
//!   launch drawing on the software adapter names it as any other, so its worker opens on it. Submitting a
//!   call or asking for a stream never waits for the worker (rule 12); only the worker waits on its
//!   device.
//! - **Order.** Calls are answered in the order queued, each before any export tile still to be
//!   submitted. An export keeps at most one tile in flight between its steps, and a step submits
//!   its next tile before it reads the one before back, so a call waits behind at most two tiles:
//!   the one its step is reading back and the one the GPU then draws. An export's steps are taken
//!   only while no call waits. A stream whose band channel already holds, with the bands it is
//!   assembling, [`EXPORT_BANDS_IN_FLIGHT`] bands submits nothing until its encoder takes one,
//!   which wakes the worker ([`BandStream::waking`]), as dropping the stream does; the worker never
//!   blocks on a full channel.
//! - **A call.** A call reads through one session over its evaluation, which the worker holds only
//!   while it answers that call (`desktop-keeps-no-stack`). A read plans the tile of its rectangle
//!   grown by [`READ_RADIUS`] pixels on every side ([`plan_read`]), cuts that tile's window from the
//!   evaluation's prepared source on the GPU, sharing the source's pixels and never copying them,
//!   and draws it: the output stage as the codes the picture is drawn with, every other stage as
//!   the linear values the core answers it from ([`TilePlan::answer`]). A call keeps the tiles it
//!   drew, at most [`READ_WINDOWS`], and answers a later read inside one from it, so a neutral
//!   pick's 25 points draw one tile; they go with the call, and so does the window of the source
//!   the runner holds.
//! - **The reference, by name.** A read the GPU cannot draw — a plan's own fallback, a tile past
//!   the budget, a stage the runner cannot run, a device never opened or lost — is answered by the
//!   reference renderer's reads ([`ReferenceReads`]), its answer naming why ([`Answered::reason`]),
//!   and so is every later read of that call, so what remains of a call has one renderer. Never
//!   silently.
//! - **A stream.** [`GpuTiles::stream`] plans the export on its caller's thread ([`PreparedStream`]),
//!   so a stack the GPU cannot draw is answered at once and the export lane renders its reference
//!   frame. The worker then draws the output stage at the longest of [`STREAM_TILE_SIDES`] whose
//!   every tile the runner's own charge holds within [`GPU_TILE_BUDGET`] less
//!   [`STREAM_READ_RESERVE`], fixed for the stream from the plan, those constants and the device's
//!   figures alone; row by row, each tile a fresh evaluation, each row of tiles assembled into one
//!   band of `width × side × 4` bytes and sent in order. Every tile of a band is cut from the
//!   band's window, its tiles' windows joined, which the runner uploads once for the band; tiles of
//!   one window shape draw into one slot; and two tiles are in flight, the GPU drawing one while the
//!   worker encodes the next or reads the one before back ([`TileRunner::submit`]). It stops
//!   between tiles once the stream's cancellation is set or its encoder drops it, and lets go of
//!   everything it held for it, its tiles in flight unread. A tile
//!   the GPU cannot draw ends the stream naming why ([`BandSender::fall_back`]): GPU and reference
//!   tiles are never mixed in one export, and the export lane renders it again with the reference.
//! - **Evidence.** [`GpuTiles::figures`]: the status, the adapter, the reads each renderer
//!   answered, the streams and bands, the tiles drawn and in flight, the windows uploaded and the
//!   slots created, the bytes the runner holds and has held, its compiles, the lights it computed
//!   and where its tiles' time went.
use super::gpu_plan::{self, WarpGrid, surface_plan_over, sweep_plan_over};
use crate::adapters::Adapter;
use luxforge_core::{
    Cancel, ClientId, CoordinateGrid, Error, GpuFallback, GpuGeometry, GpuLightSweep, GpuPlan,
    GpuStaging, GpuSweep, GpuSweeps, LinearImage, PreparedStream, PreviewSource, Region, RestTile,
    STREAM_TILE_SIDES, StreamPlan, TilePlan, plan_read, plan_stream, plan_stream_light_sweeps,
    plan_stream_sweeps,
    tiles::{
        Answered, Band, BandSender, BandStream, EXPORT_BANDS_IN_FLIGHT, ReadAnswer, ReadPixels,
        ReadStage, ReadValues, ReferenceReads, TILE_QUEUE_CAPACITY, TileCall, TileFallback,
        TileReads, TileService, TileSession, TileStatus, TileUnavailable, clipped,
    },
};
use luxforge_gpu::{
    Derivation, GpuBoundary, GpuSource, tiles::GPU_TILE_BUDGET, tiles::TILES_IN_FLIGHT,
    tiles::Ticket, tiles::TileEnd, tiles::TileFailure, tiles::TileFigures, tiles::TileInput,
    tiles::TileOutput, tiles::TilePixels, tiles::TileRunner, tiles::TileTimes,
    tiles::TileUnavailable as RunnerUnavailable,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell, RefMut},
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

/// The launch's one worker ([`launch`]).
static LAUNCHED: OnceLock<Arc<GpuTiles>> = OnceLock::new();

/// The launch's one worker, which the catalog owner's export lane streams every export through:
/// waiting for the desktop to name the adapter its window draws with ([`GpuTiles::pending`]);
/// refused for a launch with `--no-gpu-render`; and for a launch that refused the GPU stage on a
/// host whose only adapter is a software one, not adopted, or with no adapter, answering the
/// reference as `no-adapter` ([`GpuTiles::unavailable`]). Made once; a second call answers the
/// first's.
pub(crate) fn launch(launch: crate::adapters::LaunchRenderer) -> Arc<GpuTiles> {
    use crate::adapters::{LaunchRenderer, Refusal};
    Arc::clone(LAUNCHED.get_or_init(|| {
        Arc::new(match launch {
            LaunchRenderer::Gpu { .. } => GpuTiles::pending(false),
            LaunchRenderer::Reference(Refusal::Requested) => GpuTiles::pending(true),
            LaunchRenderer::Reference(Refusal::SoftwareNotAdopted | Refusal::NoAdapter) => {
                GpuTiles::unavailable(TileUnavailable::NoAdapter)
            }
        })
    }))
}

/// The launch's one worker, once the launch has made it; none in a test, which makes its own.
pub(crate) fn launched() -> Option<Arc<GpuTiles>> {
    LAUNCHED.get().cloned()
}

/// The stack a call or a stream reads, named on this one line (`desktop-keeps-no-stack`): the
/// worker holds one only while it answers the call whose evaluation carries it, and an export's
/// for as long as its stream is drawn.
type Stack = luxforge_core::Evaluation;

/// The photo surface's plan, beside the core's of the same name.
type SurfacePlan = luxforge_gpu::GpuPlan;

/// How far a read's tile reaches past the rectangle read, on every side: a point's tile is 17 × 17
/// pixels, so the 5 × 5 patch of a neutral pick, read a point at a time from its corner, lies
/// inside its first point's tile.
pub(crate) const READ_RADIUS: u32 = 8;

/// The most tiles a call keeps to answer its later reads from, the oldest let go first; they go
/// with the call.
pub(crate) const READ_WINDOWS: usize = 16;

/// What a stream's tile leaves of [`GPU_TILE_BUDGET`] for a read's: a stream is drawn at the
/// longest side whose every tile is charged at most the budget less this.
pub(crate) const STREAM_READ_RESERVE: u64 = 256 << 20;

/// The desktop's GPU tile worker. See the [module documentation](self).
pub(crate) struct GpuTiles {
    shared: Arc<Shared>,
    /// The worker, once the first call or stream has started it.
    thread: Mutex<Option<JoinHandle<()>>>,
    capacity: usize,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a call or a stream is queued, when a stream's encoder takes a band or drops
    /// the stream, and when the worker is to stop. Only the worker waits on it.
    wake: Condvar,
}

struct State {
    /// The backend and name of the adapter the window's renderer draws with, which the runner is
    /// opened on: given at the start, or named later ([`GpuTiles::adopt_adapter`]).
    adapter: Option<(String, String)>,
    /// Who named `adapter`, once it is named.
    named: Option<AdapterNaming>,
    calls: VecDeque<TileCall>,
    /// The streams asked for, the one being drawn first; the worker takes it out while it draws a
    /// step of it.
    streams: VecDeque<Box<Stream>>,
    stopping: bool,
    /// The client of the call being answered, and that call's cancellation.
    active: Option<(ClientId, Cancel)>,
    /// Why the runner cannot draw: the desktop has not named its window's adapter yet, the launch
    /// refused the GPU, the desktop named no adapter, opening it failed or its device was lost.
    unavailable: Option<TileUnavailable>,
    figures: Figures,
    #[cfg(test)]
    hooks: Hooks,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock: calls and tiles are drawn outside it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Who named the adapter a launch's worker opens on ([`GpuTiles::adopt_adapter`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdapterNaming {
    /// The launch, from wgpu's enumeration of the window's backend, while the host offers one
    /// candidate the window's request can land on.
    Launch,
    /// The desktop, from Iced's name for the adapter its window draws with.
    Window,
}

impl AdapterNaming {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Window => "window",
        }
    }
}

/// What the worker has done and holds, for evidence.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileWorkerFigures {
    /// Which renderer answers the reads now, and why the reference does.
    pub(crate) status: TileStatus,
    /// The adapter the runner's device is on, once it is opened.
    pub(crate) adapter: Option<Adapter>,
    /// Who named the adapter the runner opens on: the launch or the window.
    pub(crate) named: Option<AdapterNaming>,
    /// What opening the runner was refused with.
    pub(crate) refusal: Option<String>,
    /// Reads the GPU answered, and reads the reference answered naming why.
    pub(crate) reads: u64,
    pub(crate) references: u64,
    /// Streams the worker began drawing, and bands it sent.
    pub(crate) streams: u64,
    pub(crate) bands: u64,
    /// Of the streams, those drawn in staged sweeps, and the stage textures' bytes the runner holds
    /// now.
    pub(crate) staged: u64,
    pub(crate) stage_bytes: u64,
    /// Tiles the runner drew, reads' and streams' alike, program sequences it compiled and lights
    /// it computed, each once for the tiles that read it.
    pub(crate) tiles: u64,
    pub(crate) compiles: u64,
    pub(crate) lights: u64,
    /// The bytes the runner holds now, as charged, and the most it has held at once.
    pub(crate) in_use: u64,
    pub(crate) peak: u64,
    /// An export's tiles submitted and not yet read back; the windows of the source uploaded and
    /// the slots created, each once for the tiles of a band and of a shape.
    pub(crate) in_flight: u32,
    pub(crate) uploads: u64,
    pub(crate) slots: u64,
    /// Where the runner's last tile's time went, and every tile's summed: lights, the window's
    /// upload, encoding, the wait for the device and the readback ([`TileTimes`]).
    pub(crate) last: TileTimes,
    pub(crate) total: TileTimes,
}

/// The worker's own counts, which [`TileWorkerFigures`] reports.
#[derive(Clone, Debug, Default)]
struct Figures {
    adapter: Option<Adapter>,
    refusal: Option<String>,
    reads: u64,
    references: u64,
    streams: u64,
    bands: u64,
    staged: u64,
    runner: TileFigures,
}

impl Figures {
    fn report(&self, status: TileStatus, named: Option<AdapterNaming>) -> TileWorkerFigures {
        TileWorkerFigures {
            status,
            adapter: self.adapter.clone(),
            named,
            refusal: self.refusal.clone(),
            reads: self.reads,
            references: self.references,
            streams: self.streams,
            bands: self.bands,
            staged: self.staged,
            stage_bytes: self.runner.stage_bytes,
            tiles: self.runner.runs,
            compiles: self.runner.compiles,
            lights: self.runner.lights,
            in_use: self.runner.in_use,
            peak: self.runner.peak,
            in_flight: self.runner.in_flight,
            uploads: self.runner.uploads,
            slots: self.runner.slots,
            last: self.runner.last,
            total: self.runner.total,
        }
    }
}

/// A tile's times as evidence records them, in milliseconds.
fn times_record(times: &TileTimes) -> Value {
    let ms = |us: u64| us as f64 / 1000.0;
    json!({
        "light_ms": ms(times.light_us),
        "upload_ms": ms(times.upload_us),
        "encode_ms": ms(times.encode_us),
        "wait_ms": ms(times.wait_us),
        "read_ms": ms(times.read_us),
    })
}

/// The GPU while the runner may draw, and the reference naming why once it cannot.
fn status_of(unavailable: Option<TileUnavailable>) -> TileStatus {
    match unavailable {
        Some(reason) => TileStatus::Reference(Some(TileFallback::Unavailable(reason))),
        None => TileStatus::Gpu,
    }
}

impl GpuTiles {
    /// A worker that opens its runner, when its first call or stream needs it, on `adapter`: the
    /// backend and name the window's renderer reports drawing with, or `None` when the desktop
    /// recorded none. `refused` for a launch with `--no-gpu-render`, whose reads the reference
    /// answers naming it. Starts no thread before the first call or stream. A launch makes its
    /// worker before its window names the adapter ([`Self::pending`]).
    #[cfg(test)]
    pub(crate) fn new(adapter: Option<(String, String)>, refused: bool) -> Self {
        Self::with_capacity(TILE_QUEUE_CAPACITY, adapter, refused)
    }

    /// A launch's worker, which opens nothing until the desktop names the adapter its window draws
    /// with ([`Self::adopt_adapter`]) and answers the reference as `surface-pending` until then;
    /// for a launch with `--no-gpu-render`, `refused`, a worker that never opens anything. Starts
    /// no thread before the first call or stream.
    pub(crate) fn pending(refused: bool) -> Self {
        let reason = if refused {
            TileUnavailable::Refused
        } else {
            TileUnavailable::Pending
        };
        Self::with_state(TILE_QUEUE_CAPACITY, None, Some(reason))
    }

    /// A launch's worker that never opens anything, answering every read and export with the
    /// reference for `reason`: for a launch that refused the GPU stage because its host offers no
    /// adapter it draws on. Starts no thread before the first call or stream.
    pub(crate) fn unavailable(reason: TileUnavailable) -> Self {
        Self::with_state(TILE_QUEUE_CAPACITY, None, Some(reason))
    }

    /// [`Self::new`], holding at most `capacity` calls waiting.
    #[cfg(test)]
    pub(crate) fn with_capacity(
        capacity: usize,
        adapter: Option<(String, String)>,
        refused: bool,
    ) -> Self {
        let unavailable = if refused {
            Some(TileUnavailable::Refused)
        } else if adapter.is_none() {
            Some(TileUnavailable::NoAdapter)
        } else {
            None
        };
        Self::with_state(capacity, adapter, unavailable)
    }

    fn with_state(
        capacity: usize,
        adapter: Option<(String, String)>,
        unavailable: Option<TileUnavailable>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    named: adapter.as_ref().map(|_| AdapterNaming::Window),
                    adapter,
                    calls: VecDeque::new(),
                    streams: VecDeque::new(),
                    stopping: false,
                    active: None,
                    unavailable,
                    figures: Figures::default(),
                    #[cfg(test)]
                    hooks: Hooks::default(),
                }),
                wake: Condvar::new(),
            }),
            thread: Mutex::new(None),
            capacity,
        }
    }

    /// Name the adapter the window's renderer draws with, `name` on `backend`, which a launch's
    /// worker waits for ([`Self::pending`]): from then on its status is the GPU's, and the first
    /// call or stream that needs the runner opens it on that adapter, or names why it cannot.
    /// `naming` says who names it: the launch, from wgpu's enumeration while the host offers one
    /// candidate (`app::renderer::name_at_launch`), or the window, from Iced's own name for it.
    /// The window's naming follows the launch's: the same adapter is confirmed, and another
    /// replaces it, the runner opening again on the window's before its next tile. Whether it was
    /// taken: a worker the window has named, or that the launch refused, keeps what it has. Never
    /// waits for the worker.
    pub(crate) fn adopt_adapter(&self, backend: &str, name: &str, naming: AdapterNaming) -> bool {
        let mut state = self.shared.lock();
        let follows = naming == AdapterNaming::Window
            && state.named == Some(AdapterNaming::Launch)
            && state.unavailable.is_none();
        if state.unavailable != Some(TileUnavailable::Pending) && !follows {
            return false;
        }
        state.adapter = Some((backend.to_owned(), name.to_owned()));
        state.named = Some(naming);
        state.unavailable = None;
        true
    }

    /// What the worker has done and holds, as of the last call or tile it drew.
    pub(crate) fn figures(&self) -> TileWorkerFigures {
        let state = self.shared.lock();
        state
            .figures
            .report(status_of(state.unavailable), state.named)
    }

    fn thread(&self) -> MutexGuard<'_, Option<JoinHandle<()>>> {
        self.thread.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the worker, if the first call or stream has not started it already.
    fn start(&self) -> Result<(), Error> {
        let mut thread = self.thread();
        if thread.is_none() {
            let shared = Arc::clone(&self.shared);
            let started = std::thread::Builder::new()
                .name("luxforge-gpu-tiles".into())
                .spawn(move || work(&shared))
                .map_err(|error| {
                    Error::internal(format!("the GPU tile worker could not be started: {error}"))
                })?;
            *thread = Some(started);
        }
        Ok(())
    }

    /// The sides a stream may be drawn at, longest first: never none.
    fn sides(&self) -> Vec<u32> {
        #[cfg(test)]
        if let Some(sides) = self
            .shared
            .lock()
            .hooks
            .sides
            .clone()
            .filter(|sides| !sides.is_empty())
        {
            return sides;
        }
        STREAM_TILE_SIDES.to_vec()
    }

    /// The sides a stream's staged sweeps are tried at in place of [`STREAM_TILE_SIDES`]: a test's
    /// own; `None` for those.
    fn sweep_sides(&self) -> Option<Vec<u32>> {
        #[cfg(test)]
        if let Some(sides) = self
            .shared
            .lock()
            .hooks
            .sides
            .clone()
            .filter(|sides| !sides.is_empty())
        {
            return Some(sides);
        }
        None
    }
}

impl TileService for GpuTiles {
    /// The GPU, and the reference naming why once the runner cannot draw: the launch refused it,
    /// the desktop named no adapter, opening it failed or its device was lost. Before the first
    /// call or stream opens the runner the GPU is answered: that call opens it, and a read it then
    /// cannot draw names why, as does the status from then on.
    fn status(&self) -> TileStatus {
        status_of(self.shared.lock().unavailable)
    }

    fn submit(&self, call: TileCall) {
        if self.shared.lock().stopping {
            // Dropped unanswered, as every waiting call is when the service stops.
            return;
        }
        if let Err(error) = self.start() {
            call.refuse(error);
            return;
        }
        let mut state = self.shared.lock();
        if state.calls.len() >= self.capacity {
            drop(state);
            call.refuse(Error::resource_limit(format!(
                "{} calls that read pixels are already waiting; retry after one is answered",
                self.capacity
            )));
            return;
        }
        state.calls.push_back(call);
        drop(state);
        self.shared.wake.notify_one();
    }

    fn disconnect(&self, client: ClientId) {
        let gone: VecDeque<TileCall> = {
            let mut state = self.shared.lock();
            let (gone, kept) = std::mem::take(&mut state.calls)
                .into_iter()
                .partition(|call| call.client() == client);
            state.calls = kept;
            if let Some((active, cancel)) = &state.active
                && *active == client
            {
                cancel.cancel();
            }
            gone
        };
        // Their replies close unanswered, outside the lock.
        drop(gone);
    }

    /// The export's stream, planned here, on the caller's thread, at the longest side, so a stack
    /// the GPU cannot draw, and a runner that cannot draw, are answered at once; the worker chooses
    /// the side its every tile fits at when it begins drawing it. A worker that could not start,
    /// has stopped or already holds as many streams as calls ends the stream at once with why.
    fn stream(&self, evaluation: &Stack, cancel: &Cancel) -> Result<BandStream, TileFallback> {
        if let TileStatus::Reference(Some(reason)) = self.status() {
            return Err(reason);
        }
        let (sides, sweep_sides) = (self.sides(), self.sweep_sides());
        let prepared = PreparedStream::of(evaluation)?;
        let plan = prepared.stream(sides[0])?;
        let taken = Arc::new(AtomicUsize::new(0));
        let wake = {
            let (shared, taken) = (Arc::clone(&self.shared), Arc::clone(&taken));
            move || {
                taken.fetch_add(1, Ordering::AcqRel);
                // Through the lock, so a worker deciding to sleep has either seen the band taken
                // or is waiting for this signal.
                drop(shared.lock());
                shared.wake.notify_one();
            }
        };
        let (sender, bands) =
            BandStream::waking(plan.output.width, plan.output.height, Answered::gpu(), wake);
        // Nothing has been sent, so the channel has room for the error that ends it.
        if let Err(error) = self.start() {
            sender.send(Err(error));
            return Ok(bands);
        }
        let mut state = self.shared.lock();
        let refusal = if state.stopping {
            Some(Error::internal("the GPU tile worker has stopped"))
        } else if state.streams.len() >= self.capacity {
            Some(Error::resource_limit(format!(
                "{} exports are already drawn by the GPU tile worker; retry after one ends",
                self.capacity
            )))
        } else {
            None
        };
        if let Some(error) = refusal {
            drop(state);
            sender.send(Err(error));
            return Ok(bands);
        }
        state.streams.push_back(Box::new(Stream {
            stack: evaluation.clone(),
            cancel: cancel.clone(),
            sender,
            taken,
            sides,
            sweep_sides,
            first: Some(plan),
            prepared: Some(prepared),
            drawing: None,
            next: 0,
            open: VecDeque::new(),
            sent: 0,
            ending: None,
        }));
        drop(state);
        self.shared.wake.notify_one();
        Ok(bands)
    }

    fn stop(&self) {
        let (calls, streams) = {
            let mut state = self.shared.lock();
            state.stopping = true;
            if let Some((_, cancel)) = &state.active {
                cancel.cancel();
            }
            (
                std::mem::take(&mut state.calls),
                std::mem::take(&mut state.streams),
            )
        };
        // Their replies close unanswered and their encoders find their streams ended, outside the
        // lock; the stream the worker holds goes with it.
        drop((calls, streams));
        self.shared.wake.notify_one();
        let thread = self.thread().take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for GpuTiles {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for GpuTiles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuTiles")
            .field("figures", &self.figures())
            .finish()
    }
}

/// An export's output stage as the worker draws it.
struct Stream {
    stack: Stack,
    cancel: Cancel,
    sender: BandSender,
    /// Bands its encoder has taken, which the stream's wake counts on the encoder's thread.
    taken: Arc<AtomicUsize>,
    /// The sides it may be drawn at, longest first, and the plan [`GpuTiles::stream`] made at the
    /// first.
    sides: Vec<u32>,
    /// The sides its staged sweeps are tried at, in place of [`STREAM_TILE_SIDES`]; `None` for
    /// those.
    sweep_sides: Option<Vec<u32>>,
    first: Option<StreamPlan>,
    /// Source-derived preparation only until strategy and side selection have finished.
    prepared: Option<PreparedStream>,
    /// What the worker draws it with, once it has begun.
    drawing: Option<Drawing>,
    /// The next tile to submit, and the bands its tiles in flight and read back are assembled in,
    /// oldest first: at most two, each counted against the band channel's room from its first
    /// tile's submission, so sending it never waits.
    next: usize,
    open: VecDeque<Assembling>,
    sent: usize,
    /// What ends the stream, which waits for room in its channel.
    ending: Option<End>,
}

/// What ends a stream before its last band: an error of its own, a cancellation among them, or the
/// reason the GPU cannot go on drawing it, for which the export lane renders it again with the
/// reference.
enum End {
    Error(Error),
    Fallback(TileFallback),
}

impl End {
    /// Hand it to the stream through `sender`.
    fn send(self, sender: &BandSender) {
        match self {
            Self::Error(error) => sender.send(Err(error)),
            Self::Fallback(fallback) => sender.fall_back(fallback),
        };
    }
}

/// A stream being drawn: the plan at the side chosen for it, the source its tiles' windows are cut
/// from, a lens warp's grid of the whole output stage, each band's window of the source, which the
/// runner uploads once for every tile of the band, and the tiles in flight.
struct Drawing {
    plan: StreamPlan,
    source: GpuSource,
    grid: Option<CoordinateGrid>,
    /// Each band's window: every window of its tiles, joined.
    windows: Vec<Region>,
    /// Each tile's band.
    band_of: Vec<usize>,
    /// The tiles submitted and not yet read back, oldest first, with their index.
    pending: VecDeque<(Ticket, usize)>,
    /// A stream drawn in staged sweeps: its sweeps, `plan.tiles` the last's.
    staged: Option<Staged>,
    /// Its light sweeps, drawn first, for a chained stream reading a light behind a spatial layer.
    lit: Option<LightSweeps>,
    /// The runner it began on ([`Worker::runners`]): its tiles in flight and its window are that
    /// runner's, so it ends if the worker opens another.
    runner: u64,
}

/// A stream's staged sweeps (`docs/design/gpu-first.md`, "Staged sweeps"): every sweep before the
/// last drawn into the runner's stage textures, tile by tile, two in flight, before the last
/// sweep's tiles are streamed as bands, each cut from the stage texture the sweep before it wrote.
struct Staged {
    sweeps: Box<GpuSweeps>,
    /// The sweep before the last being drawn, and its next tile.
    sweep: usize,
    next: usize,
    /// That sweep's tiles in flight, oldest first.
    pending: VecDeque<Ticket>,
    /// When it reads the source, each band's window and each tile's band, as a chained stream's.
    windows: Vec<Region>,
    band_of: Vec<usize>,
}

impl Staged {
    fn new(sweeps: Box<GpuSweeps>) -> Self {
        let (windows, band_of) = bands(&sweeps.sweeps[0].tiles);
        Self {
            sweeps,
            sweep: 0,
            next: 0,
            pending: VecDeque::new(),
            windows,
            band_of,
        }
    }

    /// Whether a sweep before the last remains to be drawn.
    fn drawing_stages(&self) -> bool {
        self.sweep + 1 < self.sweeps.sweeps.len()
    }

    /// The last sweep, whose tiles are the stream's.
    fn last(&self) -> &GpuSweep {
        self.sweeps.sweeps.last().expect("at least two sweeps")
    }
}

/// A band whose tiles are being read back into its rows.
struct Assembling {
    band: usize,
    y0: u32,
    rows: u32,
    rgba: Vec<u8>,
    /// Its tiles not read back yet.
    left: usize,
}

/// A chained stream's light sweeps (`docs/design/gpu-preview.md`, "The global estimate"): each
/// light behind a spatial layer whose stage texture does not fit computed first, its sweep's tiles
/// drawing the layers before it over their windows, two in flight, each tile's rectangle reduced
/// into the light the runner keeps; the stream's tiles then read it kept.
struct LightSweeps {
    sweeps: Vec<GpuLightSweep>,
    /// The sweep being drawn, its next tile, whether its light was begun, and its tiles in flight.
    sweep: usize,
    next: usize,
    begun: bool,
    pending: VecDeque<Ticket>,
}

impl LightSweeps {
    fn new(sweeps: Vec<GpuLightSweep>) -> Self {
        Self {
            sweeps,
            sweep: 0,
            next: 0,
            begun: false,
            pending: VecDeque::new(),
        }
    }

    /// Whether a light sweep remains to be drawn.
    fn drawing(&self) -> bool {
        self.sweep < self.sweeps.len()
    }
}

impl Drawing {
    /// `plan` drawn over `source`, its tiles grouped in bands by their row.
    fn new(plan: StreamPlan, source: GpuSource, grid: Option<CoordinateGrid>, runner: u64) -> Self {
        let (windows, band_of) = bands(&plan.tiles);
        Self {
            plan,
            source,
            grid,
            windows,
            band_of,
            pending: VecDeque::new(),
            staged: None,
            lit: None,
            runner,
        }
    }

    /// Whether a sweep before the last remains to be drawn into the stage textures, or a light
    /// sweep before the stream's tiles.
    fn drawing_stages(&self) -> bool {
        self.staged.as_ref().is_some_and(Staged::drawing_stages)
            || self.lit.as_ref().is_some_and(LightSweeps::drawing)
    }
}

/// The bands of `tiles`, row by row: each band's window, every window of its tiles joined, and each
/// tile's band.
fn bands(tiles: &[RestTile]) -> (Vec<Region>, Vec<usize>) {
    let mut windows: Vec<Region> = Vec::new();
    let mut band_of = Vec::with_capacity(tiles.len());
    let mut row = None;
    for tile in tiles {
        let window = tile.window;
        if row != Some(tile.rect.y0) {
            row = Some(tile.rect.y0);
            windows.push(window);
        } else if let Some(joined) = windows.last_mut() {
            let (x0, y0) = (joined.x0.min(window.x0), joined.y0.min(window.y0));
            let (x1, y1) = (joined.x1().max(window.x1()), joined.y1().max(window.y1()));
            *joined = Region {
                x0,
                y0,
                width: x1 - x0,
                height: y1 - y0,
            };
        }
        band_of.push(windows.len() - 1);
    }
    (windows, band_of)
}

impl Stream {
    /// Whether its channel has room for one more band beside the ones it holds and assembles.
    fn room(&self) -> bool {
        self.sent.saturating_sub(self.taken.load(Ordering::Acquire)) + self.open.len()
            < EXPORT_BANDS_IN_FLIGHT
    }

    /// Whether its next tile can be submitted: one remains, and its band is being assembled
    /// already or the channel has room for it.
    fn can_submit(&self) -> bool {
        let Some(drawing) = &self.drawing else {
            return false;
        };
        if drawing.drawing_stages() {
            return false;
        }
        let Some(&band) = drawing.band_of.get(self.next) else {
            return false;
        };
        self.open.back().is_some_and(|open| open.band == band) || self.room()
    }

    /// Whether the worker has a step of it to take: an abandoned stream to let go of, a cancelled
    /// one to end, one to begin, a tile in flight to read back, or a tile to submit; an ending
    /// stream only once its channel has room for the error.
    fn ready(&self) -> bool {
        if self.sender.abandoned() {
            return true;
        }
        if self.ending.is_some() {
            return self.room();
        }
        self.cancel.check().is_err()
            || self.drawing.is_none()
            || self
                .drawing
                .as_ref()
                .is_some_and(|drawing| !drawing.pending.is_empty() || drawing.drawing_stages())
            || self.can_submit()
    }
}

/// What the worker takes next.
enum Job {
    Call(TileCall),
    Step(Box<Stream>),
}

/// The worker: the oldest call first, else a step of the stream being drawn when it has one to
/// take, and asleep while neither waits.
fn work(shared: &Arc<Shared>) {
    let worker = Worker {
        shared: Arc::clone(shared),
        runner: RefCell::new(None),
        reference: ReferenceReads,
        versions: Cell::new(0),
        figures: RefCell::new(Figures::default()),
        runners: Cell::new(0),
    };
    loop {
        let job = {
            let mut state = shared.lock();
            loop {
                if state.stopping {
                    return;
                }
                if let Some(call) = state.calls.pop_front() {
                    state.active = Some((call.client(), call.cancel().clone()));
                    break Job::Call(call);
                }
                if state.streams.front().is_some_and(|stream| stream.ready()) {
                    break Job::Step(state.streams.pop_front().expect("a stream ready"));
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        #[cfg(test)]
        worker.hooks(&job);
        worker.follow_adapter();
        match job {
            Job::Call(call) => {
                call.run(&worker);
                let mut state = shared.lock();
                state.active = None;
                worker.publish(&mut state);
            }
            Job::Step(mut stream) => {
                let more = worker.step(&mut stream);
                let finished = {
                    let mut state = shared.lock();
                    worker.publish(&mut state);
                    if more && !state.stopping {
                        state.streams.push_front(stream);
                        None
                    } else {
                        Some(stream)
                    }
                };
                // Its evaluation and its sender go outside the lock.
                drop(finished);
            }
        }
    }
}

/// The worker's renderer: the runner, opened by the first read or stream that needs it, the
/// reference reads it answers with when the GPU cannot draw, and its counts.
struct Worker {
    shared: Arc<Shared>,
    runner: RefCell<Option<TileRunner>>,
    reference: ReferenceReads,
    /// What every source and boundary handed the runner is told apart by.
    versions: Cell<u64>,
    figures: RefCell<Figures>,
    /// How many runners the worker has let go of for another adapter: the runner a stream began on
    /// is this count then ([`Drawing::runner`]).
    runners: Cell<u64>,
}

impl Worker {
    /// Between jobs, never within one: when the window has named another adapter than the one the
    /// runner is open on (it replaced the launch's), let go of the runner, its tiles in flight
    /// abandoned unread, so the next job opens one on the window's. A stream drawn on the old one
    /// ends at its next step naming `adapter-mismatch`, and its export or tiers are drawn again by
    /// the reference: no output mixes two adapters' tiles.
    fn follow_adapter(&self) {
        let named = self.shared.lock().adapter.clone();
        let mut runner = self.runner.borrow_mut();
        let moved = match (runner.as_ref(), &named) {
            (Some(opened), Some((backend, name))) => {
                opened.adapter().backend != *backend || opened.adapter().name != *name
            }
            _ => false,
        };
        if moved {
            if let Some(old) = runner.as_mut() {
                old.abandon();
                old.release();
                self.figures.borrow_mut().runner = old.figures();
            }
            *runner = None;
            self.runners.set(self.runners.get() + 1);
        }
    }

    /// Why the runner cannot draw, when it cannot.
    fn unavailable(&self) -> Option<TileUnavailable> {
        self.shared.lock().unavailable
    }

    /// The runner, opened by the first read or stream that needs it on the adapter the window's
    /// renderer draws with; why it cannot draw otherwise, from then on.
    fn runner(&self) -> Result<RefMut<'_, TileRunner>, TileFallback> {
        if let Some(reason) = self.unavailable() {
            return Err(TileFallback::Unavailable(reason));
        }
        let mut runner = self.runner.borrow_mut();
        if runner.is_none() {
            let Some((backend, name)) = self.shared.lock().adapter.clone() else {
                return Err(TileFallback::Unavailable(TileUnavailable::NoAdapter));
            };
            // A JPEG's cut reads the core's decode table, which the surface holds once handed it.
            gpu_plan::install_output_encoding();
            match crate::adapters::tile_runner(&backend, &name) {
                Ok(opened) => {
                    self.figures.borrow_mut().adapter = Some(opened.adapter().clone());
                    *runner = Some(opened);
                    #[cfg(test)]
                    if let Some(opened) = runner.as_mut() {
                        opened.set_poison(self.shared.lock().hooks.poison);
                    }
                }
                Err(refusal) => {
                    let reason = unavailable(refusal.reason);
                    self.figures.borrow_mut().refusal = Some(refusal.detail);
                    self.shared.lock().unavailable = Some(reason);
                    return Err(TileFallback::Unavailable(reason));
                }
            }
        }
        Ok(RefMut::map(runner, |runner| {
            runner.as_mut().expect("the runner was opened")
        }))
    }

    /// `plan` drawn over `window` of `source` and read back as `end`, or why the GPU cannot draw
    /// it. A lost device is lost for good: the runner goes, and the status names it.
    fn run(
        &self,
        plan: &SurfacePlan,
        source: &GpuSource,
        window: Region,
        end: TileEnd,
    ) -> Result<TilePixels, TileFallback> {
        let mut runner = self.runner()?;
        let drawn = runner.run(
            plan,
            source,
            [window.x0, window.y0, window.width, window.height],
            end,
        );
        self.figures.borrow_mut().runner = runner.figures();
        drop(runner);
        drawn.map_err(|failure| self.failed(failure))
    }

    /// Submit `plan`'s tile over `window` of `source`, read back as `end` once it is finished
    /// ([`Worker::finish`]), or why the GPU cannot draw it.
    fn submit(
        &self,
        plan: &SurfacePlan,
        source: &GpuSource,
        window: Region,
        (input, output): (TileInput, TileOutput),
    ) -> Result<Ticket, TileFallback> {
        let mut runner = self.runner()?;
        let submitted = runner.submit_to(
            plan,
            source,
            [window.x0, window.y0, window.width, window.height],
            input,
            output,
        );
        self.figures.borrow_mut().runner = runner.figures();
        drop(runner);
        submitted.map_err(|failure| self.failed(failure))
    }

    /// `ticket`'s staged sweep tile done on the device, or why it could not be.
    fn finish_stage(&self, ticket: Ticket) -> Result<(), TileFallback> {
        let mut runner = self.runner()?;
        let finished = runner.finish_stage(ticket);
        self.figures.borrow_mut().runner = runner.figures();
        drop(runner);
        finished.map_err(|failure| self.failed(failure))
    }

    /// `ticket`'s tile read back, or why it could not be.
    fn finish(&self, ticket: Ticket) -> Result<TilePixels, TileFallback> {
        let mut runner = self.runner()?;
        let finished = runner.finish(ticket);
        self.figures.borrow_mut().runner = runner.figures();
        drop(runner);
        finished.map_err(|failure| self.failed(failure))
    }

    /// The fallback of `failure`: a lost device is lost for good, the runner going with it.
    fn failed(&self, failure: TileFailure) -> TileFallback {
        let fallback = fallback_of(failure);
        if fallback == TileFallback::Unavailable(TileUnavailable::DeviceLost) {
            self.shared.lock().unavailable = Some(TileUnavailable::DeviceLost);
            *self.runner.borrow_mut() = None;
        }
        fallback
    }

    /// The lights behind a spatial layer that `plan`, a read's plan of a stage of `stack`, reads
    /// ([`luxforge_core::GpuLightInput::Stage`]), which a read's one tile cannot compute: kept by
    /// the runner already, or computed now by drawing `stack`'s staged sweeps before them into stage
    /// textures, every tile of each into its stage texture, each light reduced from the texture its
    /// reading sweep reads before that sweep's first tile, and kept under its input's key, the stage
    /// textures released after; or, where those do not fit or a staged export holds the runner's,
    /// by light sweeps that hold none. The read's tile then reads them kept. Refused, naming
    /// `light-stage`, while an export's light sweep is drawn, or where no light sweep fits. Blocks
    /// the worker for those sweeps, once a stack and source.
    fn stage_lights(
        &self,
        stack: &Stack,
        plan: &GpuPlan,
        converted: &SurfacePlan,
        source: &GpuSource,
    ) -> Result<(), TileFallback> {
        let Some(light) = plan.lights.iter().find(|light| light.staged()) else {
            return Ok(());
        };
        let refused = |why: &str| {
            TileFallback::Plan(GpuFallback::LightStage {
                layer: light.layer,
                why: why.to_owned(),
            })
        };
        let held;
        {
            let mut runner = self.runner()?;
            if runner.keeps_lights(converted, source) {
                return Ok(());
            }
            if runner.light_sweeping() {
                return Err(refused("an export's light sweep holds the light reducer"));
            }
            held = runner.stages().0 > 0;
        }
        let budget = GPU_TILE_BUDGET - STREAM_READ_RESERVE;
        // A staged export holds the stage textures: the light by light sweeps, which hold none.
        if held {
            return self.read_light_sweeps(stack, source, budget);
        }
        let staging = match self.chains_streams() {
            true => GpuStaging::Chained(luxforge_core::Chained::OneSweep),
            false => plan_stream_sweeps(stack, budget)?,
        };
        let GpuStaging::Staged(sweeps) = staging else {
            // The stage textures do not fit: each light by a light sweep that holds none.
            return self.read_light_sweeps(stack, source, budget);
        };
        let Some(last) = sweeps
            .sweeps
            .iter()
            .rposition(|sweep| !sweep.lights.is_empty())
        else {
            return Err(refused("no sweep reads it"));
        };
        let whole = plan_stream(stack, sweeps.sweeps[0].side)?;
        // A reading sweep may be the last, whose tiles draw through a lens warp's grid.
        let grid = match whole.warp() {
            Some(warp) => Some(stage_grid(warp)?),
            None => None,
        };
        {
            let mut runner = self.runner()?;
            let held = runner.hold_stages(
                (sweeps.stage.width, sweeps.stage.height),
                sweeps.format,
                sweeps.textures,
            );
            self.figures.borrow_mut().runner = runner.figures();
            drop(runner);
            held.map_err(|failure| self.failed(failure))?;
        }
        let drawn = (|| {
            for index in 0..last {
                let sweep = &sweeps.sweeps[index];
                let writes = sweep
                    .writes
                    .ok_or_else(|| unplannable("a sweep writes nothing"))?;
                let mut pending = VecDeque::new();
                for tile in &sweep.tiles {
                    let input = sweep.reads.map_or(TileInput::Source, TileInput::Stage);
                    let output = TileOutput::Stage {
                        writes,
                        rect: [tile.rect.x0, tile.rect.y0, tile.rect.x1(), tile.rect.y1()],
                    };
                    let converted = self.convert_sweep(&whole.plan, sweep, source, None, *tile)?;
                    let ticket = self.submit(&converted, source, tile.window, (input, output))?;
                    pending.push_back(ticket);
                    if pending.len() >= TILES_IN_FLIGHT {
                        self.finish_stage(pending.pop_front().expect("a tile in flight"))?;
                    }
                }
                while let Some(ticket) = pending.pop_front() {
                    self.finish_stage(ticket)?;
                }
                let next = &sweeps.sweeps[index + 1];
                if !next.lights.is_empty() {
                    let tile = *next
                        .tiles
                        .first()
                        .ok_or_else(|| unplannable("a sweep of no tiles"))?;
                    let converted =
                        self.convert_sweep(&whole.plan, next, source, grid.as_ref(), tile)?;
                    let mut runner = self.runner()?;
                    let computed = runner.compute_lights(&converted, source);
                    self.figures.borrow_mut().runner = runner.figures();
                    drop(runner);
                    computed.map_err(|failure| self.failed(failure))?;
                }
            }
            Ok(())
        })();
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.release();
            runner.release_stages();
            self.figures.borrow_mut().runner = runner.figures();
        }
        drawn
    }

    /// The lights behind a spatial layer a read of `stack` reads, computed by light sweeps with no
    /// stage texture ([`plan_stream_light_sweeps`]), each tile drawing the layers before a light
    /// over its window and reducing its rectangle into it, two tiles in flight, and kept by the
    /// runner under its input's key. The reference's where no sweep fits.
    fn read_light_sweeps(
        &self,
        stack: &Stack,
        source: &GpuSource,
        budget: u64,
    ) -> Result<(), TileFallback> {
        let sweeps = plan_stream_light_sweeps(stack, budget, &luxforge_core::STREAM_TILE_SIDES)?;
        let whole = plan_stream(stack, luxforge_core::STREAM_TILE_SIDES[0])?;
        let drawn = (|| {
            for planned in &sweeps {
                let light = gpu_plan::light_sweep_light(&whole.plan, planned)
                    .map_err(|unrunnable| TileFallback::Stage(unrunnable.code()))?;
                {
                    let mut runner = self.runner()?;
                    let begun = runner.begin_light(source, &light, planned.side);
                    self.figures.borrow_mut().runner = runner.figures();
                    drop(runner);
                    if !begun.map_err(|failure| self.failed(failure))? {
                        continue;
                    }
                }
                let sweep = gpu_plan::light_sweep_as_sweep(&whole.plan, planned);
                let mut pending = VecDeque::new();
                for tile in &planned.tiles {
                    let converted = self.convert_sweep(&whole.plan, &sweep, source, None, *tile)?;
                    let output = TileOutput::Light {
                        rect: [tile.rect.x0, tile.rect.y0, tile.rect.x1(), tile.rect.y1()],
                    };
                    let ticket =
                        self.submit(&converted, source, tile.window, (TileInput::Source, output))?;
                    pending.push_back(ticket);
                    if pending.len() >= TILES_IN_FLIGHT {
                        self.finish_stage(pending.pop_front().expect("a tile in flight"))?;
                    }
                }
                while let Some(ticket) = pending.pop_front() {
                    self.finish_stage(ticket)?;
                }
                let mut runner = self.runner()?;
                let finished = runner.finish_light();
                self.figures.borrow_mut().runner = runner.figures();
                drop(runner);
                finished.map_err(|failure| self.failed(failure))?;
            }
            Ok(())
        })();
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.abandon_light();
            runner.release();
            self.figures.borrow_mut().runner = runner.figures();
        }
        drawn
    }

    /// Let go of the window of the source and the slot the runner keeps between runs.
    fn release(&self) {
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.release();
            self.figures.borrow_mut().runner = runner.figures();
        }
    }

    /// Let go of everything the runner holds for a stream: its tiles in flight, unread, the window,
    /// the slot and a staged stream's stage textures.
    fn release_stream(&self) {
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.abandon();
            runner.release();
            runner.release_stages();
            runner.abandon_light();
            self.figures.borrow_mut().runner = runner.figures();
        }
    }

    fn version(&self) -> u64 {
        let version = self.versions.get() + 1;
        self.versions.set(version);
        version
    }

    /// `plan` as the photo surface's plain data over `tile`: its boundary cut on the GPU from the
    /// tile's window of `source`, a lens warp's tail through the part of `grid`, the whole output
    /// stage's, over the tile's rectangle, and the rectangle drawn at full scale.
    fn convert(
        &self,
        plan: &GpuPlan,
        source: &GpuSource,
        grid: Option<&CoordinateGrid>,
        tile: RestTile,
    ) -> Result<SurfacePlan, TileFallback> {
        let window = tile.window;
        let boundary = GpuBoundary::derived(
            source,
            Derivation::Cut {
                origin: (window.x0, window.y0),
            },
            window.width,
            window.height,
            self.version(),
        )
        .ok_or(TileFallback::Stage("boundary-size"))?;
        let part = match grid {
            None => None,
            Some(grid) => Some(WarpGrid::new(
                &grid
                    .part(tile.rect)
                    .ok_or(TileFallback::Stage("warp-grid"))?,
            )),
        };
        surface_plan_over(
            plan,
            boundary,
            (window.x0, window.y0),
            part.as_ref(),
            Some(tile.rect),
        )
        .map_err(|unrunnable| TileFallback::Stage(unrunnable.code()))
    }

    /// [`Worker::convert`] for a tile of `sweep`, a staged sweep of `plan`: its own links over its
    /// window of the content stage, cut from `source` or copied out of a stage texture; a sweep
    /// before the last draws no region and no tail, and only the last takes its part of `grid`.
    fn convert_sweep(
        &self,
        plan: &GpuPlan,
        sweep: &GpuSweep,
        source: &GpuSource,
        grid: Option<&CoordinateGrid>,
        tile: RestTile,
    ) -> Result<SurfacePlan, TileFallback> {
        let window = tile.window;
        let boundary = GpuBoundary::derived(
            source,
            Derivation::Cut {
                origin: (window.x0, window.y0),
            },
            window.width,
            window.height,
            self.version(),
        )
        .ok_or(TileFallback::Stage("boundary-size"))?;
        let part = match grid.filter(|_| sweep.last) {
            None => None,
            Some(grid) => Some(WarpGrid::new(
                &grid
                    .part(tile.rect)
                    .ok_or(TileFallback::Stage("warp-grid"))?,
            )),
        };
        sweep_plan_over(
            plan,
            sweep,
            boundary,
            (window.x0, window.y0),
            part.as_ref(),
            tile.rect,
        )
        .map_err(|unrunnable| TileFallback::Stage(unrunnable.code()))
    }

    /// One step of a staged stream's sweeps before the last: its next tile submitted, drawing its
    /// rectangle into the stage texture the sweep writes, and its oldest tile in flight waited for
    /// once two are or none more can be; at a sweep's last tile done, the next sweep. Why the GPU
    /// cannot go on drawing it.
    fn stage_step(&self, drawing: &mut Drawing) -> Result<(), TileFallback> {
        let staged = drawing.staged.as_mut().expect("a staged stream");
        let sweep = &staged.sweeps.sweeps[staged.sweep];
        let tiles = sweep.tiles.len();
        let submitted = staged.next < tiles && staged.pending.len() < TILES_IN_FLIGHT;
        if submitted {
            let tile = sweep.tiles[staged.next];
            let writes = sweep
                .writes
                .ok_or_else(|| unplannable("a sweep writes nothing"))?;
            let (input, window) = match sweep.reads {
                None => (
                    TileInput::Source,
                    staged.windows[staged.band_of[staged.next]],
                ),
                Some(k) => (TileInput::Stage(k), tile.window),
            };
            let rect = tile.rect;
            let output = TileOutput::Stage {
                writes,
                rect: [rect.x0, rect.y0, rect.x1(), rect.y1()],
            };
            let plan =
                self.convert_sweep(&drawing.plan.plan, sweep, &drawing.source, None, tile)?;
            let ticket = self.submit(&plan, &drawing.source, window, (input, output))?;
            staged.pending.push_back(ticket);
            staged.next += 1;
        }
        let finish = staged.pending.len() >= TILES_IN_FLIGHT
            || ((!submitted || staged.next == tiles) && !staged.pending.is_empty());
        if finish {
            let ticket = staged.pending.pop_front().expect("a tile in flight");
            self.finish_stage(ticket)?;
        }
        if staged.next == tiles && staged.pending.is_empty() {
            staged.sweep += 1;
            staged.next = 0;
            if staged.drawing_stages() {
                (staged.windows, staged.band_of) = bands(&staged.sweeps.sweeps[staged.sweep].tiles);
            }
        }
        Ok(())
    }

    /// One step of a chained stream's light sweeps: its light begun at a sweep's first, skipped
    /// where the runner keeps it; its next tile submitted, reducing its rectangle into the light;
    /// its oldest tile in flight waited for once two are or none more can be; and at a sweep's last
    /// tile done, the light selected and kept, and the next sweep. Why the GPU cannot go on.
    fn light_step(&self, drawing: &mut Drawing) -> Result<(), TileFallback> {
        let lit = drawing.lit.as_mut().expect("light sweeps");
        let planned = &lit.sweeps[lit.sweep];
        if !lit.begun {
            let light = gpu_plan::light_sweep_light(&drawing.plan.plan, planned)
                .map_err(|unrunnable| TileFallback::Stage(unrunnable.code()))?;
            let mut runner = self.runner()?;
            let begun = runner.begin_light(&drawing.source, &light, planned.side);
            self.figures.borrow_mut().runner = runner.figures();
            drop(runner);
            if !begun.map_err(|failure| self.failed(failure))? {
                lit.sweep += 1;
                return Ok(());
            }
            lit.begun = true;
        }
        let tiles = planned.tiles.len();
        let submitted = lit.next < tiles && lit.pending.len() < TILES_IN_FLIGHT;
        if submitted {
            let tile = planned.tiles[lit.next];
            let sweep = gpu_plan::light_sweep_as_sweep(&drawing.plan.plan, planned);
            let plan =
                self.convert_sweep(&drawing.plan.plan, &sweep, &drawing.source, None, tile)?;
            let output = TileOutput::Light {
                rect: [tile.rect.x0, tile.rect.y0, tile.rect.x1(), tile.rect.y1()],
            };
            let ticket = self.submit(
                &plan,
                &drawing.source,
                tile.window,
                (TileInput::Source, output),
            )?;
            lit.pending.push_back(ticket);
            lit.next += 1;
        }
        let finish = lit.pending.len() >= TILES_IN_FLIGHT
            || ((!submitted || lit.next == tiles) && !lit.pending.is_empty());
        if finish {
            let ticket = lit.pending.pop_front().expect("a tile in flight");
            self.finish_stage(ticket)?;
        }
        if lit.next == tiles && lit.pending.is_empty() {
            let mut runner = self.runner()?;
            let finished = runner.finish_light();
            self.figures.borrow_mut().runner = runner.figures();
            drop(runner);
            finished.map_err(|failure| self.failed(failure))?;
            lit.sweep += 1;
            lit.next = 0;
            lit.begun = false;
        }
        Ok(())
    }

    /// Copy what the worker has done and holds into the shared state, for the figures.
    fn publish(&self, state: &mut State) {
        let mut figures = self.figures.borrow_mut();
        if let Some(runner) = self.runner.borrow().as_ref() {
            figures.runner = runner.figures();
        }
        state.figures = figures.clone();
    }

    /// One step of `stream`: its beginning, one tile, or its end. Whether it goes on; a step that
    /// panics ends it with `internal`.
    fn step(&self, stream: &mut Stream) -> bool {
        match catch_unwind(AssertUnwindSafe(|| self.advance(stream))) {
            Ok(more) => more,
            Err(_) => self.end(
                stream,
                End::Error(Error::internal("drawing an export's tile panicked")),
            ),
        }
    }

    fn advance(&self, stream: &mut Stream) -> bool {
        if stream.sender.abandoned() {
            // Its export was abandoned: nothing more is drawn for it.
            self.release_stream();
            return false;
        }
        if let Some(end) = stream.ending.take() {
            // The worker steps an ending stream only once its channel has room for its end.
            end.send(&stream.sender);
            return false;
        }
        if let Err(cancelled) = stream.cancel.check() {
            return self.end(stream, End::Error(cancelled));
        }
        if stream
            .drawing
            .as_ref()
            .is_some_and(|drawing| drawing.runner != self.runners.get())
        {
            // Begun on a runner let go of for the window's adapter: its tiles in flight went with
            // it, and the rest is not drawn on another adapter.
            stream.drawing = None;
            return self.end(
                stream,
                End::Fallback(TileFallback::Unavailable(TileUnavailable::AdapterMismatch)),
            );
        }
        if stream.drawing.is_none() {
            return match self.begin(stream) {
                Ok(drawing) => {
                    stream.drawing = Some(drawing);
                    self.figures.borrow_mut().streams += 1;
                    true
                }
                Err(fallback) => self.end(stream, End::Fallback(fallback)),
            };
        }
        // A staged stream draws its sweeps before the last into the stage textures first.
        if stream.drawing.as_ref().is_some_and(Drawing::drawing_stages) {
            let drawing = stream.drawing.as_mut().expect("drawing");
            let step = match drawing.lit.as_ref().is_some_and(LightSweeps::drawing) {
                true => self.light_step(drawing),
                false => self.stage_step(drawing),
            };
            return match step {
                Ok(()) => true,
                Err(fallback) => self.end(stream, End::Fallback(fallback)),
            };
        }
        // The next tile submitted, its band opened with its first; the GPU draws it while the
        // tile before it is read back.
        let submitted = stream.can_submit();
        if submitted {
            let drawing = stream.drawing.as_mut().expect("drawing");
            let index = stream.next;
            let tile = drawing.plan.tiles[index];
            let band = drawing.band_of[index];
            if stream.open.back().is_none_or(|open| open.band != band) {
                let width = drawing.plan.output.width as usize;
                stream.open.push_back(Assembling {
                    band,
                    y0: tile.rect.y0,
                    rows: tile.rect.height,
                    rgba: vec![0; width * tile.rect.height as usize * 4],
                    left: drawing.band_of.iter().filter(|of| **of == band).count(),
                });
            }
            let output = TileOutput::Read(TileEnd::Codes);
            let ticket = match &drawing.staged {
                // The last sweep's tile, cut from the stage texture the sweep before it wrote.
                Some(staged) => {
                    let last = staged.last();
                    let input = last
                        .reads
                        .map(TileInput::Stage)
                        .ok_or_else(|| unplannable("the last sweep reads no stage"));
                    input.and_then(|input| {
                        self.convert_sweep(
                            &drawing.plan.plan,
                            last,
                            &drawing.source,
                            drawing.grid.as_ref(),
                            tile,
                        )
                        .and_then(|plan| {
                            self.submit(&plan, &drawing.source, tile.window, (input, output))
                        })
                    })
                }
                None => self
                    .convert(
                        &drawing.plan.plan,
                        &drawing.source,
                        drawing.grid.as_ref(),
                        tile,
                    )
                    .and_then(|plan| {
                        self.submit(
                            &plan,
                            &drawing.source,
                            drawing.windows[band],
                            (TileInput::Source, output),
                        )
                    }),
            };
            match ticket {
                Ok(ticket) => drawing.pending.push_back((ticket, index)),
                Err(fallback) => return self.end(stream, End::Fallback(fallback)),
            }
            stream.next += 1;
        }
        // The oldest tile read back once the next is on its way, or once none can be submitted.
        let drawing = stream.drawing.as_mut().expect("drawing");
        let tiles = drawing.plan.tiles.len();
        let finish = drawing.pending.len() >= TILES_IN_FLIGHT
            || ((!submitted || stream.next == tiles) && !drawing.pending.is_empty());
        if finish {
            let (ticket, index) = drawing.pending.pop_front().expect("a tile in flight");
            let codes = match self.finish(ticket) {
                Ok(TilePixels::Codes(codes)) => codes,
                Ok(TilePixels::Linear(_)) => {
                    return self.end(
                        stream,
                        End::Error(Error::internal("an export's tile read back no codes")),
                    );
                }
                Err(fallback) => return self.end(stream, End::Fallback(fallback)),
            };
            let drawing = stream.drawing.as_ref().expect("drawing");
            let width = drawing.plan.output.width as usize;
            let rect = drawing.plan.tiles[index].rect;
            let band = drawing.band_of[index];
            let Some(open) = stream.open.iter_mut().find(|open| open.band == band) else {
                return self.end(
                    stream,
                    End::Error(Error::internal("an export's tile read back into no band")),
                );
            };
            let row = rect.width as usize * 4;
            for (line, pixels) in codes.chunks_exact(row).enumerate() {
                let at = (line * width + rect.x0 as usize) * 4;
                open.rgba[at..at + row].copy_from_slice(pixels);
            }
            open.left -= 1;
            // Every band whose tiles are all read back, in order.
            while stream.open.front().is_some_and(|open| open.left == 0) {
                let open = stream.open.pop_front().expect("a band");
                let band = Band {
                    y0: open.y0,
                    rows: open.rows,
                    rgba: open.rgba,
                };
                // The channel had room for the band when its first tile was submitted, and its
                // encoder only makes more: this send never waits.
                if !stream.sender.send(Ok(band)) {
                    self.release_stream();
                    return false;
                }
                stream.sent += 1;
                self.figures.borrow_mut().bands += 1;
            }
        }
        let drawing = stream.drawing.as_ref().expect("drawing");
        let last = stream.next == drawing.plan.tiles.len() && drawing.pending.is_empty();
        if last {
            // Every band sent: the stream ends as its sender goes.
            self.release_stream();
        }
        !last
    }

    /// Begin drawing `stream`: the runner opened, the source its tiles' windows are cut from, a
    /// lens warp's grid of the whole output stage, and the longest of its sides whose every tile
    /// the runner's own charge holds within [`GPU_TILE_BUDGET`] less [`STREAM_READ_RESERVE`].
    fn begin(&self, stream: &mut Stream) -> Result<Drawing, TileFallback> {
        let first = stream
            .first
            .take()
            .ok_or_else(|| unplannable("the stream was begun twice"))?;
        let prepared = stream
            .prepared
            .take()
            .ok_or_else(|| unplannable("the stream was begun twice"))?;
        let source = gpu_source(self.version(), stream.stack.source())?;
        let grid = match first.warp() {
            Some(warp) => Some(stage_grid(warp)?),
            None => None,
        };
        let budget = GPU_TILE_BUDGET - STREAM_READ_RESERVE;
        // A stack whose layers together reach far is drawn in staged sweeps, the runner holding
        // their stage textures, charged before they are created, for as long as the stream is
        // drawn; any other is drawn chained.
        let sides = stream.sweep_sides.as_deref().unwrap_or(&STREAM_TILE_SIDES);
        let (staging, lit) = if self.chains_streams() {
            (None, prepared.light_sweeps(budget, sides)?)
        } else {
            let (staging, lit) = prepared.strategy(budget, sides)?;
            (Some(staging), lit)
        };
        if let Some(GpuStaging::Staged(mut sweeps)) = staging {
            let last = sweeps.sweeps.last_mut().expect("at least two sweeps");
            // The selected sweep already owns exactly the final windows/order. Only its
            // metadata is needed by Staged; Drawing owns this tile list from here on.
            let mut plan = first;
            plan.tiles = std::mem::take(&mut last.tiles);
            plan.side = last.side;
            {
                let mut runner = self.runner()?;
                let held = runner.hold_stages(
                    (sweeps.stage.width, sweeps.stage.height),
                    sweeps.format,
                    sweeps.textures,
                );
                self.figures.borrow_mut().runner = runner.figures();
                drop(runner);
                held.map_err(|failure| self.failed(failure))?;
            }
            self.figures.borrow_mut().staged += 1;
            let mut drawing = Drawing::new(plan, source, grid, self.runners.get());
            drawing.staged = Some(Staged::new(sweeps));
            return Ok(drawing);
        }
        // A light behind a spatial layer whose stage textures do not fit is computed by light
        // sweeps first, which hold none; the stream's chained tiles read it kept. A stack no light
        // sweep fits is the reference's, never drawn with a stand-in.
        let mut requested = 0;
        for &side in &stream.sides {
            let plan = if side == first.side {
                first.clone()
            } else {
                prepared.stream(side)?
            };
            let charge = self.largest_charge(&plan, &source, grid.as_ref())?;
            if charge <= budget {
                let mut drawing = Drawing::new(plan, source, grid, self.runners.get());
                drawing.lit = (!lit.is_empty()).then(|| LightSweeps::new(lit));
                return Ok(drawing);
            }
            requested = charge;
        }
        Err(TileFallback::Budget { requested, budget })
    }

    /// Whether a test asked for every stream drawn chained.
    fn chains_streams(&self) -> bool {
        #[cfg(test)]
        return self.shared.lock().hooks.chained;
        #[cfg(not(test))]
        false
    }

    /// The most the runner would hold for any one tile of `plan`, by its own charge, over its
    /// band's window.
    fn largest_charge(
        &self,
        plan: &StreamPlan,
        source: &GpuSource,
        grid: Option<&CoordinateGrid>,
    ) -> Result<u64, TileFallback> {
        let runner = self.runner()?;
        let (windows, band_of) = bands(&plan.tiles);
        let mut largest = 0;
        for (tile, band) in plan.tiles.iter().zip(band_of) {
            let converted = self.convert(&plan.plan, source, grid, *tile)?;
            let window = windows[band];
            let charge = runner
                .charge(
                    &converted,
                    source,
                    [window.x0, window.y0, window.width, window.height],
                    TileEnd::Codes,
                )
                .map_err(fallback_of)?;
            largest = largest.max(charge);
        }
        Ok(largest)
    }

    /// End `stream` with `end`: everything held for it let go, and the end sent once its channel
    /// has room for it. Whether the stream waits for that room.
    fn end(&self, stream: &mut Stream, end: End) -> bool {
        self.release_stream();
        stream.drawing = None;
        stream.open.clear();
        if stream.room() {
            end.send(&stream.sender);
            false
        } else {
            stream.ending = Some(end);
            true
        }
    }
}

impl TileReads for Worker {
    fn grid_renderer(&self) -> Option<luxforge_core::RendererRecord> {
        self.unavailable()
            .is_none()
            .then_some(luxforge_core::RendererRecord::Gpu)
    }

    fn session<'a>(
        &'a self,
        evaluation: &'a Stack,
        cancel: &'a Cancel,
    ) -> Box<dyn TileSession + 'a> {
        Box::new(Session {
            worker: self,
            stack: evaluation,
            cancel,
            source: None,
            grids: Vec::new(),
            tiles: VecDeque::new(),
            fallback: None,
            reference: None,
        })
    }
}

/// One call's reads through the GPU: its source, a lens warp's grid of each stage it read, the
/// tiles it drew and, once a read fell back, the reference's session and why.
struct Session<'a> {
    worker: &'a Worker,
    stack: &'a Stack,
    cancel: &'a Cancel,
    /// The evaluation's prepared source as the runner cuts windows from it, built by the first
    /// read the GPU draws.
    source: Option<GpuSource>,
    grids: Vec<(ReadStage, Arc<CoordinateGrid>)>,
    /// The tiles the call drew, oldest first, at most [`READ_WINDOWS`].
    tiles: VecDeque<Drawn>,
    /// Why the reference answers the rest of the call.
    fallback: Option<TileFallback>,
    reference: Option<Box<dyn TileSession + 'a>>,
}

/// A tile a call drew, kept to answer its later reads.
struct Drawn {
    plan: TilePlan,
    /// The codes, for a read of the output stage's codes, or the linear values every other read is
    /// answered from.
    pixels: TilePixels,
}

impl Drawn {
    /// Whether it answers a read of `rect` of `stage` as `values`: the same stage read back as
    /// that read reads it, holding every pixel of the rectangle the stage holds.
    fn answers(&self, stage: ReadStage, rect: Region, values: ReadValues) -> bool {
        let read = clipped(rect, self.plan.size);
        let tile = self.plan.tile.rect;
        self.plan.stage == stage
            && self.plan.reads_codes(values) == matches!(self.pixels, TilePixels::Codes(_))
            && (read.is_empty()
                || (tile.x0 <= read.x0
                    && tile.y0 <= read.y0
                    && read.x1() <= tile.x1()
                    && read.y1() <= tile.y1()))
    }

    /// The read of `rect` as `values`, from the tile's pixels.
    fn answer(&self, rect: Region, values: ReadValues) -> ReadAnswer {
        let rect = clipped(rect, self.plan.size);
        let tile = self.plan.tile.rect;
        let at = move |(x, y): (u32, u32)| {
            (y - tile.y0) as usize * tile.width as usize + (x - tile.x0) as usize
        };
        let points =
            (rect.y0..rect.y1()).flat_map(move |y| (rect.x0..rect.x1()).map(move |x| (x, y)));
        let pixels = match &self.pixels {
            TilePixels::Codes(codes) => ReadPixels::Codes(
                points
                    .map(|point| {
                        let at = at(point) * 4;
                        [codes[at], codes[at + 1], codes[at + 2], codes[at + 3]]
                    })
                    .collect(),
            ),
            TilePixels::Linear(linear) => self
                .plan
                .answer(values, points.map(|point| linear[at(point)])),
        };
        ReadAnswer {
            stage: self.plan.size,
            rect,
            pixels,
            answered: Answered::gpu(),
        }
    }
}

impl Session<'_> {
    /// The read drawn on the GPU, or why the GPU cannot draw it.
    fn draw(
        &mut self,
        stage: ReadStage,
        rect: Region,
        values: ReadValues,
    ) -> Result<ReadAnswer, TileFallback> {
        // A runner that cannot draw names why before anything is planned.
        if let Some(reason) = self.worker.unavailable() {
            return Err(TileFallback::Unavailable(reason));
        }
        let plan = plan_read(self.stack, stage, grown(rect))?;
        let read = clipped(rect, plan.size);
        if read.is_empty() {
            // The rectangle misses the stage: nothing is drawn, and the answer holds nothing.
            return Ok(ReadAnswer {
                stage: plan.size,
                rect: read,
                pixels: plan.answer(values, std::iter::empty()),
                answered: Answered::gpu(),
            });
        }
        let source = self.source()?;
        let grid = self.grid(&plan)?;
        let converted = self
            .worker
            .convert(&plan.plan, &source, grid.as_deref(), plan.tile)?;
        // A light behind a spatial layer, computed by the stack's staged sweeps and kept.
        self.worker
            .stage_lights(self.stack, &plan.plan, &converted, &source)?;
        let end = if plan.reads_codes(values) {
            TileEnd::Codes
        } else {
            TileEnd::Linear
        };
        let pixels = self
            .worker
            .run(&converted, &source, plan.tile.window, end)?;
        let drawn = Drawn { plan, pixels };
        let answer = drawn.answer(rect, values);
        if self.tiles.len() == READ_WINDOWS {
            self.tiles.pop_front();
        }
        self.tiles.push_back(drawn);
        Ok(answer)
    }

    /// The evaluation's prepared source as the runner cuts windows from it.
    fn source(&mut self) -> Result<GpuSource, TileFallback> {
        if let Some(source) = &self.source {
            return Ok(source.clone());
        }
        let source = gpu_source(self.worker.version(), self.stack.source())?;
        self.source = Some(source.clone());
        Ok(source)
    }

    /// A lens warp's grid of the whole of `plan`'s stage, which every tile of it takes its part of;
    /// computed once a stage for the call. `None` for an affine or projective tail.
    fn grid(&mut self, plan: &TilePlan) -> Result<Option<Arc<CoordinateGrid>>, TileFallback> {
        let Some(warp) = plan.warp() else {
            return Ok(None);
        };
        if let Some((_, grid)) = self.grids.iter().find(|(stage, _)| *stage == plan.stage) {
            return Ok(Some(Arc::clone(grid)));
        }
        let grid = Arc::new(stage_grid(warp)?);
        self.grids.push((plan.stage, Arc::clone(&grid)));
        Ok(Some(grid))
    }
}

impl TileSession for Session<'_> {
    fn gather(
        &mut self,
        stage: ReadStage,
        points: &[[u32; 2]],
        cancel: &Cancel,
    ) -> Result<luxforge_core::tiles::GatherAnswer, Error> {
        match luxforge_core::tiles::gather_tiles(self, stage, points, cancel) {
            Err(_) if self.fallback.is_some() => {
                cancel.check()?;
                let reference = self
                    .reference
                    .get_or_insert_with(|| self.worker.reference.session(self.stack, self.cancel));
                let mut answer = reference.gather(stage, points, cancel)?;
                answer.answered = Some(Answered::reference(self.fallback.clone()));
                Ok(answer)
            }
            result => result,
        }
    }

    fn read(
        &mut self,
        stage: ReadStage,
        rect: Region,
        values: ReadValues,
    ) -> Result<ReadAnswer, Error> {
        self.cancel.check()?;
        if self.fallback.is_none() {
            let held = self
                .tiles
                .iter()
                .find(|drawn| drawn.answers(stage, rect, values))
                .map(|drawn| drawn.answer(rect, values));
            match held.map_or_else(|| self.draw(stage, rect, values), Ok) {
                Ok(answer) => {
                    self.worker.figures.borrow_mut().reads += 1;
                    return Ok(answer);
                }
                Err(fallback) => self.fallback = Some(fallback),
            }
        }
        let (worker, stack, cancel) = (self.worker, self.stack, self.cancel);
        let reference = self
            .reference
            .get_or_insert_with(|| worker.reference.session(stack, cancel));
        let mut answer = reference.read(stage, rect, values)?;
        answer.answered = Answered::reference(self.fallback.clone());
        worker.figures.borrow_mut().references += 1;
        Ok(answer)
    }
}

impl Drop for Session<'_> {
    /// The call's tiles go with it, and so does the window of the source the runner holds.
    fn drop(&mut self) {
        self.worker.release();
    }
}

/// `rect` grown by [`READ_RADIUS`] pixels on every side, which the planner clips to the stage.
fn grown(rect: Region) -> Region {
    let (x0, y0) = (
        rect.x0.saturating_sub(READ_RADIUS),
        rect.y0.saturating_sub(READ_RADIUS),
    );
    Region {
        x0,
        y0,
        width: rect.x1().saturating_add(READ_RADIUS) - x0,
        height: rect.y1().saturating_add(READ_RADIUS) - y0,
    }
}

/// The reference renders it, for `reason`, under the plan's `unplannable`.
fn unplannable(reason: &str) -> TileFallback {
    TileFallback::Plan(GpuFallback::Unplannable(reason.to_owned()))
}

/// A lens warp's grid of its whole output stage at one display pixel an output pixel, which every
/// tile takes its part of, as the picture at rest's tiles do; `warp-grid` when it needs more nodes
/// than a grid holds.
fn stage_grid(warp: &GpuGeometry) -> Result<CoordinateGrid, TileFallback> {
    match warp.stage_grid(1.0) {
        Ok(Some(grid)) => Ok(grid),
        _ => Err(TileFallback::Stage("warp-grid")),
    }
}

/// A RAW development's planes as the runner uploads a window of them, borrowed through a clone of
/// the image, which shares its planes.
struct Planes(LinearImage);

impl AsRef<[f32]> for Planes {
    fn as_ref(&self) -> &[f32] {
        self.0.shared_planes().0
    }
}

/// `source` as the runner cuts windows from it, under `version`: a JPEG's upright codes, or a RAW
/// development's planes through its view, shared with the evaluation and never copied, as the
/// desktop hands the photo surface the source on screen (`app::gpu_preview`).
fn gpu_source(version: u64, source: &PreviewSource) -> Result<GpuSource, TileFallback> {
    match source {
        PreviewSource::Jpeg(image) => {
            GpuSource::codes(version, Arc::clone(&image.rgba), image.width, image.height)
        }
        PreviewSource::Raw { image, .. } => {
            let (_, base, crop, orientation) = image.shared_planes();
            GpuSource::planes(
                version,
                Arc::new(Planes(image.clone())),
                base,
                crop,
                orientation,
            )
        }
    }
    .ok_or_else(|| unplannable("the prepared source cannot be held on the GPU"))
}

/// The runner's reason it cannot draw at all, as the core's contract names it.
fn unavailable(reason: RunnerUnavailable) -> TileUnavailable {
    match reason {
        RunnerUnavailable::NoAdapter => TileUnavailable::NoAdapter,
        RunnerUnavailable::Refused => TileUnavailable::Refused,
        RunnerUnavailable::DeviceLost => TileUnavailable::DeviceLost,
        RunnerUnavailable::AdapterMismatch => TileUnavailable::AdapterMismatch,
    }
}

/// Why a run drew nothing, as the core's contract names it: the runner's unavailability, its
/// budget, or the stage's own reason under its own code.
fn fallback_of(failure: TileFailure) -> TileFallback {
    match failure {
        TileFailure::Unavailable(reason) => TileFallback::Unavailable(unavailable(reason)),
        TileFailure::Budget { requested, budget } => TileFallback::Budget { requested, budget },
        TileFailure::Plan(stage) => TileFallback::Stage(stage.as_str()),
    }
}

impl TileWorkerFigures {
    /// The figures as evidence records them: the status, `gpu` or the reference's reason code
    /// (`tiles-unavailable` naming the unavailability as `surface-pending`, `refused` and so on);
    /// the adapter the runner's device is on, as an adapter is recorded, or why opening it was
    /// refused; and the counts and bytes.
    pub(crate) fn record(&self) -> Value {
        let status = match &self.status {
            TileStatus::Gpu => json!("gpu"),
            TileStatus::Reference(None) => json!({"reference": null}),
            TileStatus::Reference(Some(TileFallback::Unavailable(reason))) => {
                json!({"reference": "tiles-unavailable", "unavailable": reason.as_str()})
            }
            TileStatus::Reference(Some(reason)) => json!({"reference": reason.code()}),
        };
        json!({
            "status": status,
            "adapter": self.adapter.as_ref().map(|adapter| {
                super::renderer::adapter_record(&adapter.backend, &adapter.name, Some(adapter))
            }),
            "named": self.named.map(AdapterNaming::as_str),
            "refusal": self.refusal,
            "reads": self.reads,
            "references": self.references,
            "streams": self.streams,
            "bands": self.bands,
            "staged_streams": self.staged,
            "stage_bytes": self.stage_bytes,
            "tiles": self.tiles,
            "compiles": self.compiles,
            "lights": self.lights,
            "in_use_bytes": self.in_use,
            "peak_bytes": self.peak,
            "in_flight": self.in_flight,
            "uploads": self.uploads,
            "slots": self.slots,
            // Where the runner's tiles' time went: the last tile's, and every tile's summed.
            "last_tile": times_record(&self.last),
            "tiles_total": times_record(&self.total),
        })
    }
}

/// What a test asks of the worker.
#[cfg(test)]
#[derive(Default)]
struct Hooks {
    /// Passed before each call is answered.
    calls: Option<Arc<luxforge_testbase::Gate>>,
    /// Passed before each step of a stream: its beginning, each tile and its end.
    steps: Option<Arc<luxforge_testbase::Gate>>,
    /// Every later run starts each link's scratch planes from NaN.
    poison: bool,
    /// Lose the runner's device before the next job, once past its gate.
    lose: bool,
    /// The sides streams are drawn at, in place of [`STREAM_TILE_SIDES`].
    sides: Option<Vec<u32>>,
    /// Every later stream is drawn chained, whatever its stack.
    chained: bool,
}

#[cfg(test)]
impl Worker {
    /// Before `job`: pass the test's gate for its kind, then poison the runner's scratch or lose
    /// its device, as the test asked.
    fn hooks(&self, job: &Job) {
        let gate = {
            let state = self.shared.lock();
            match job {
                Job::Call(_) => state.hooks.calls.clone(),
                Job::Step(_) => state.hooks.steps.clone(),
            }
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        let (poison, lose) = {
            let mut state = self.shared.lock();
            (state.hooks.poison, std::mem::take(&mut state.hooks.lose))
        };
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.set_poison(poison);
            if lose {
                runner.simulate_device_loss();
            }
        }
    }
}

#[cfg(test)]
impl GpuTiles {
    /// How many calls wait behind the one being answered.
    pub(crate) fn waiting(&self) -> usize {
        self.shared.lock().calls.len()
    }

    /// Whether the worker thread has started.
    pub(crate) fn started(&self) -> bool {
        self.thread().is_some()
    }

    /// Have the worker pass `gate` before it answers each call, or stop passing one.
    pub(crate) fn hold_calls(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.shared.lock().hooks.calls = gate;
    }

    /// Have the worker pass `gate` before each step of a stream, or stop passing one.
    pub(crate) fn hold_steps(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.shared.lock().hooks.steps = gate;
    }

    /// Start every later run's scratch planes from NaN, or stop.
    pub(crate) fn poison(&self, poisoned: bool) {
        self.shared.lock().hooks.poison = poisoned;
    }

    /// Lose the runner's device before the worker's next job, once past its gate.
    pub(crate) fn lose_device(&self) {
        self.shared.lock().hooks.lose = true;
    }

    /// Draw streams at `sides` in place of [`STREAM_TILE_SIDES`].
    pub(crate) fn draw_streams_at(&self, sides: Vec<u32>) {
        self.shared.lock().hooks.sides = Some(sides);
    }

    /// Draw every later stream chained, never in staged sweeps, or stop.
    pub(crate) fn chain_streams(&self, chained: bool) {
        self.shared.lock().hooks.chained = chained;
    }
}
