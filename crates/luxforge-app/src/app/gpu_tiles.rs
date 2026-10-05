//! The desktop's GPU tile worker (`docs/design/gpu-first.md`, stage 4; `docs/design/gpu-preview.md`,
//! "Qualifying a program"): the core's tile contract ([`luxforge_core::tiles`]) answered on a
//! device of the desktop's own through the photo surface's tile runner ([`TileRunner`]), so a
//! pixel read and an export's band are drawn by the GPU as the picture on screen is drawn.
//!
//! Built and not yet wired: no catalog owner is handed this service, so `render.sample`, the
//! modules' queries and the mutations' pixel reads are still answered by the point worker and on
//! the owner, and `export.jpeg` still renders its reference frame. Handing it to the owner through
//! `HostConfig`, and the export lane's stream, are the next step.
//!
//! - **One thread.** `luxforge-gpu-tiles`, started by the first call or stream and asleep on its
//!   condition variable while nothing waits (performance rule 8). It owns the runner, which the
//!   first call or stream that needs it opens on the adapter the window's renderer reports drawing
//!   with, never another ([`TileRunner::open`]): a host without that adapter answers
//!   `tiles-unavailable adapter-mismatch`, a launch with `--no-gpu-render` `refused`, a desktop
//!   that named no adapter `no-adapter`, and a lost device `device-lost` from then on. Submitting a
//!   call or asking for a stream never waits for the worker (rule 12); only the worker waits on its
//!   device.
//! - **Order.** Calls are answered in the order queued, each before any export tile still to be
//!   drawn, so a call waits behind at most the one tile being drawn. An export's tiles are drawn
//!   one at a time, and only while no call waits. A stream whose band channel already holds
//!   [`EXPORT_BANDS_IN_FLIGHT`] bands draws nothing until its encoder takes one, which wakes the
//!   worker ([`BandStream::waking`]), as dropping the stream does; the worker never blocks on a
//!   full channel.
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
//! - **A stream.** [`GpuTiles::stream`] plans the export on its caller's thread ([`plan_stream`]),
//!   so a stack the GPU cannot draw is answered at once and the export lane renders its reference
//!   frame. The worker then draws the output stage at the longest of [`STREAM_TILE_SIDES`] whose
//!   every tile the runner's own charge holds within [`GPU_TILE_BUDGET`] less
//!   [`STREAM_READ_RESERVE`], fixed for the stream from the plan, those constants and the device's
//!   figures alone; row by row, each tile a fresh evaluation, each row of tiles assembled into one
//!   band of `width × side × 4` bytes and sent in order. It stops between tiles once the stream's
//!   cancellation is set or its encoder drops it, and lets go of everything it held for it. A tile
//!   the GPU cannot draw ends the stream with an error naming why in its data (`fallback`, the
//!   reason's code, and `unavailable` for a runner that cannot draw): GPU and reference tiles are
//!   never mixed in one export, and the export lane renders it again with the reference.
//! - **Evidence.** [`GpuTiles::figures`]: the status, the adapter, the reads each renderer
//!   answered, the streams and bands, the tiles drawn, the bytes the runner holds and has held,
//!   and its compiles.
use super::gpu_plan::{self, WarpGrid, surface_plan_over};
use luxforge_core::{
    Cancel, ClientId, CoordinateGrid, Error, GpuFallback, GpuGeometry, GpuPlan, LinearImage,
    PreviewSource, Region, RestTile, STREAM_TILE_SIDES, StreamPlan, TilePlan, plan_read,
    plan_stream,
    tiles::{
        Answered, Band, BandSender, BandStream, EXPORT_BANDS_IN_FLIGHT, ReadAnswer, ReadPixels,
        ReadStage, ReadValues, ReferenceReads, TILE_QUEUE_CAPACITY, TileCall, TileFallback,
        TileReads, TileService, TileSession, TileStatus, TileUnavailable, clipped,
    },
};
use luxforge_ui::{
    adapters::Adapter,
    photo_surface::{
        Derivation, GpuBoundary, GpuSource,
        gpu_preview::tiles::{
            GPU_TILE_BUDGET, TileEnd, TileFailure, TileFigures, TilePixels, TileRunner,
            TileUnavailable as RunnerUnavailable,
        },
    },
};
use serde_json::json;
use std::{
    cell::{Cell, RefCell, RefMut},
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

/// The stack a call or a stream reads, named on this one line (`desktop-keeps-no-stack`): the
/// worker holds one only while it answers the call whose evaluation carries it, and an export's
/// for as long as its stream is drawn.
type Stack = luxforge_core::Evaluation;

/// The photo surface's plan, beside the core's of the same name.
type SurfacePlan = luxforge_ui::photo_surface::GpuPlan;

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
    /// The backend and name of the adapter the window's renderer draws with, which the runner is
    /// opened on.
    adapter: Option<(String, String)>,
}

struct State {
    calls: VecDeque<TileCall>,
    /// The streams asked for, the one being drawn first; the worker takes it out while it draws a
    /// step of it.
    streams: VecDeque<Box<Stream>>,
    stopping: bool,
    /// The client of the call being answered, and that call's cancellation.
    active: Option<(ClientId, Cancel)>,
    /// Why the runner cannot draw: the launch refused the GPU, the desktop named no adapter,
    /// opening it failed or its device was lost.
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

/// What the worker has done and holds, for evidence.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileWorkerFigures {
    /// Which renderer answers the reads now, and why the reference does.
    pub(crate) status: TileStatus,
    /// The adapter the runner's device is on, once it is opened.
    pub(crate) adapter: Option<Adapter>,
    /// What opening the runner was refused with.
    pub(crate) refusal: Option<String>,
    /// Reads the GPU answered, and reads the reference answered naming why.
    pub(crate) reads: u64,
    pub(crate) references: u64,
    /// Streams the worker began drawing, and bands it sent.
    pub(crate) streams: u64,
    pub(crate) bands: u64,
    /// Tiles the runner drew, reads' and streams' alike, and program sequences it compiled.
    pub(crate) tiles: u64,
    pub(crate) compiles: u64,
    /// The bytes the runner holds now, as charged, and the most it has held at once.
    pub(crate) in_use: u64,
    pub(crate) peak: u64,
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
    runner: TileFigures,
}

impl Figures {
    fn report(&self, status: TileStatus) -> TileWorkerFigures {
        TileWorkerFigures {
            status,
            adapter: self.adapter.clone(),
            refusal: self.refusal.clone(),
            reads: self.reads,
            references: self.references,
            streams: self.streams,
            bands: self.bands,
            tiles: self.runner.runs,
            compiles: self.runner.compiles,
            in_use: self.runner.in_use,
            peak: self.runner.peak,
        }
    }
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
    /// answers naming it. Starts no thread before the first call or stream.
    pub(crate) fn new(adapter: Option<(String, String)>, refused: bool) -> Self {
        Self::with_capacity(TILE_QUEUE_CAPACITY, adapter, refused)
    }

    /// [`Self::new`], holding at most `capacity` calls waiting.
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
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
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
                adapter,
            }),
            thread: Mutex::new(None),
            capacity,
        }
    }

    /// What the worker has done and holds, as of the last call or tile it drew.
    pub(crate) fn figures(&self) -> TileWorkerFigures {
        let state = self.shared.lock();
        state.figures.report(status_of(state.unavailable))
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
        let sides = self.sides();
        let plan = plan_stream(evaluation, sides[0])?;
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
            first: Some(plan),
            drawing: None,
            next: 0,
            band: Vec::new(),
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
    first: Option<StreamPlan>,
    /// What the worker draws it with, once it has begun.
    drawing: Option<Drawing>,
    /// The next tile to draw, and the band its row is assembled in, empty between bands.
    next: usize,
    band: Vec<u8>,
    sent: usize,
    /// The error that ends the stream, which waits for room in its channel.
    ending: Option<Error>,
}

/// A stream being drawn: the plan at the side chosen for it, the source its tiles' windows are cut
/// from, and a lens warp's grid of the whole output stage.
struct Drawing {
    plan: StreamPlan,
    source: GpuSource,
    grid: Option<CoordinateGrid>,
}

impl Stream {
    /// Whether its channel has room for one more band.
    fn room(&self) -> bool {
        self.sent.saturating_sub(self.taken.load(Ordering::Acquire)) < EXPORT_BANDS_IN_FLIGHT
    }

    /// Whether the worker has a step of it to take: an abandoned stream to let go of, a cancelled
    /// one to end, one to begin, a tile of the band being assembled, or the first tile of a band
    /// its channel has room for; an ending stream only once its channel has room for the error.
    fn ready(&self) -> bool {
        if self.sender.abandoned() {
            return true;
        }
        if self.ending.is_some() {
            return self.room();
        }
        self.cancel.check().is_err()
            || self.drawing.is_none()
            || !self.band.is_empty()
            || self.room()
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
}

impl Worker {
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
            let Some((backend, name)) = &self.shared.adapter else {
                return Err(TileFallback::Unavailable(TileUnavailable::NoAdapter));
            };
            // A JPEG's cut reads the core's decode table, which the surface holds once handed it.
            gpu_plan::install_output_encoding();
            match TileRunner::open(backend, name) {
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
        drawn.map_err(|failure| {
            let fallback = fallback_of(failure);
            if fallback == TileFallback::Unavailable(TileUnavailable::DeviceLost) {
                self.shared.lock().unavailable = Some(TileUnavailable::DeviceLost);
                *self.runner.borrow_mut() = None;
            }
            fallback
        })
    }

    /// Let go of the window of the source the runner keeps between runs.
    fn release(&self) {
        if let Some(runner) = self.runner.borrow_mut().as_mut() {
            runner.release();
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
            Err(_) => self.end(stream, Error::internal("drawing an export's tile panicked")),
        }
    }

    fn advance(&self, stream: &mut Stream) -> bool {
        if stream.sender.abandoned() {
            // Its export was abandoned: nothing more is drawn for it.
            self.release();
            return false;
        }
        if let Some(error) = stream.ending.take() {
            // The worker steps an ending stream only once its channel has room for the error.
            stream.sender.send(Err(error));
            return false;
        }
        if let Err(cancelled) = stream.cancel.check() {
            return self.end(stream, cancelled);
        }
        let Some(drawing) = &stream.drawing else {
            return match self.begin(stream) {
                Ok(drawing) => {
                    stream.drawing = Some(drawing);
                    self.figures.borrow_mut().streams += 1;
                    true
                }
                Err(fallback) => self.end(stream, ended(&fallback)),
            };
        };
        let tile = drawing.plan.tiles[stream.next];
        let drawn = self
            .convert(
                &drawing.plan.plan,
                &drawing.source,
                drawing.grid.as_ref(),
                tile,
            )
            .and_then(|plan| self.run(&plan, &drawing.source, tile.window, TileEnd::Codes));
        let codes = match drawn {
            Ok(TilePixels::Codes(codes)) => codes,
            Ok(TilePixels::Linear(_)) => {
                return self.end(
                    stream,
                    Error::internal("an export's tile read back no codes"),
                );
            }
            Err(fallback) => return self.end(stream, ended(&fallback)),
        };
        let width = drawing.plan.output.width as usize;
        let rect = tile.rect;
        if stream.band.is_empty() {
            stream.band = vec![0; width * rect.height as usize * 4];
        }
        let row = rect.width as usize * 4;
        for (line, pixels) in codes.chunks_exact(row).enumerate() {
            let at = (line * width + rect.x0 as usize) * 4;
            stream.band[at..at + row].copy_from_slice(pixels);
        }
        stream.next += 1;
        let tiles = &drawing.plan.tiles;
        let last = stream.next == tiles.len();
        if last || tiles[stream.next].rect.y0 != rect.y0 {
            let band = Band {
                y0: rect.y0,
                rows: rect.height,
                rgba: std::mem::take(&mut stream.band),
            };
            // The channel had room when the band's first tile was drawn, and its encoder only
            // makes more: this send never waits.
            if !stream.sender.send(Ok(band)) {
                self.release();
                return false;
            }
            stream.sent += 1;
            self.figures.borrow_mut().bands += 1;
        }
        if last {
            // Every band sent: the stream ends as its sender goes.
            self.release();
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
        let source = gpu_source(self.version(), stream.stack.source())?;
        let grid = match first.warp() {
            Some(warp) => Some(stage_grid(warp)?),
            None => None,
        };
        let budget = GPU_TILE_BUDGET - STREAM_READ_RESERVE;
        let mut requested = 0;
        for &side in &stream.sides {
            let plan = if side == first.side {
                first.clone()
            } else {
                plan_stream(&stream.stack, side)?
            };
            let charge = self.largest_charge(&plan, &source, grid.as_ref())?;
            if charge <= budget {
                return Ok(Drawing { plan, source, grid });
            }
            requested = charge;
        }
        Err(TileFallback::Budget { requested, budget })
    }

    /// The most the runner would hold for any one tile of `plan`, by its own charge.
    fn largest_charge(
        &self,
        plan: &StreamPlan,
        source: &GpuSource,
        grid: Option<&CoordinateGrid>,
    ) -> Result<u64, TileFallback> {
        let runner = self.runner()?;
        let mut largest = 0;
        for tile in &plan.tiles {
            let converted = self.convert(&plan.plan, source, grid, *tile)?;
            let window = tile.window;
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

    /// End `stream` with `error`: everything held for it let go, and the error sent once its
    /// channel has room for it. Whether the stream waits for that room.
    fn end(&self, stream: &mut Stream, error: Error) -> bool {
        self.release();
        stream.drawing = None;
        stream.band = Vec::new();
        if stream.room() {
            stream.sender.send(Err(error));
            false
        } else {
            stream.ending = Some(error);
            true
        }
    }
}

impl TileReads for Worker {
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

/// The error that ends a stream the GPU cannot go on drawing, naming why in its data: `fallback`,
/// the reason's code; `unavailable` for a runner that cannot draw; `requested` and `budget` for a
/// stage no side's tiles fit; `detail` for the plan's own reason.
fn ended(fallback: &TileFallback) -> Error {
    let mut data = json!({ "fallback": fallback.code() });
    match fallback {
        TileFallback::Unavailable(reason) => data["unavailable"] = json!(reason.as_str()),
        TileFallback::Budget { requested, budget } => {
            data["requested"] = json!(requested);
            data["budget"] = json!(budget);
        }
        TileFallback::Plan(reason) => data["detail"] = json!(reason.to_string()),
        TileFallback::Stage(_) => {}
    }
    Error::render(format!(
        "the GPU stopped drawing the export's tiles: {}",
        fallback.code()
    ))
    .with_data(data)
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
}
