//! The reference tile service: the reference renderer's answer to every [`TileCall`], for a host
//! without a GPU provider and for whatever a GPU provider cannot render.
//!
//! # Shape
//!
//! One thread, `luxforge-tiles`, takes the calls in the order they were queued and answers each on
//! its own reply, so another client's call never waits behind a read on the catalog owner. This is
//! not the latest-wins primitive ([`crate::latest`]): every queued call was asked for by someone
//! still waiting on it. A client's order is kept by construction: every transport serves one call
//! at a time per connection, so a client has at most one call here.
//!
//! # Reads
//!
//! [`ReferenceReads`] answers from the reference renderer: whole frames, never point tiles. A
//! stage without a spatial segment is evaluated point by point over the rectangle, `O(rect ×
//! layers)`. A stage with spatial segments is evaluated in frame mode: each spatial segment's
//! whole frame is materialized once, on the shared pool as a render materializes it, and the
//! rectangle is evaluated through those frames point by point. A session holds the one evaluation
//! of each stage it has read for the life of its call, so a neutral pick's 25 points cost one
//! evaluation of the stack before Basic, and releases it, frames and all, when the call ends.
//! Through spatial layers a read costs about one render of them: slow and plain, which is what the
//! reference is.
//!
//! # Bounds
//!
//! One thread, started with the first call and blocked on a condition variable while its queue is
//! empty (rule 8). At most [`TILE_QUEUE_CAPACITY`] calls wait behind the one being answered; a full
//! queue answers `resource-limit` at once (rule 6). A disconnect drops that client's waiting calls,
//! whose replies close unanswered, and cancels its call being answered before its next row or
//! tile. A call that panics answers `internal`, and the thread lives on for the next one.
use super::{
    Answered, BandStream, ReadAnswer, ReadPixels, ReadStage, ReadValues, TILE_QUEUE_CAPACITY,
    TileCall, TileFallback, TileReads, TileService, TileSession, TileStatus, TileUnavailable,
    clipped,
};
use crate::{
    Cancel, ClientId, Error, Evaluation, Raster, Region, Render, RenderOptions, Stage,
    editor::prefix, render::StagePixels,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
};

/// The reference tile service. See the [module documentation](self).
pub struct ReferenceTiles {
    shared: Arc<Shared>,
    /// The worker, once the first call has started it.
    thread: Mutex<Option<JoinHandle<()>>>,
    capacity: usize,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a call is queued and when the worker is to stop. Only the worker waits on it.
    queued: Condvar,
}

#[derive(Default)]
struct State {
    queue: VecDeque<TileCall>,
    stopping: bool,
    /// The client of the call being answered, and that call's cancellation.
    active: Option<(ClientId, Cancel)>,
    /// Passed before each call is answered, so a test can hold the worker there.
    #[cfg(test)]
    gate: Option<Arc<luxforge_testbase::Gate>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock: calls are answered outside it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ReferenceTiles {
    /// A service that starts no thread until the first call, and holds at most
    /// [`TILE_QUEUE_CAPACITY`] calls waiting.
    pub fn new() -> Self {
        Self::with_capacity(TILE_QUEUE_CAPACITY)
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                queued: Condvar::new(),
            }),
            thread: Mutex::new(None),
            capacity,
        }
    }

    fn thread(&self) -> MutexGuard<'_, Option<JoinHandle<()>>> {
        self.thread.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the worker, if the first call has not started it already.
    fn start(&self) -> Result<(), Error> {
        let mut thread = self.thread();
        if thread.is_none() {
            let shared = self.shared.clone();
            let started = std::thread::Builder::new()
                .name("luxforge-tiles".into())
                .spawn(move || work(&shared))
                .map_err(|error| {
                    Error::internal(format!("the tile worker could not be started: {error}"))
                })?;
            *thread = Some(started);
        }
        Ok(())
    }
}

impl Default for ReferenceTiles {
    fn default() -> Self {
        Self::new()
    }
}

impl TileService for ReferenceTiles {
    /// The reference with no reason: this service is the only renderer of a host without a GPU
    /// provider.
    fn status(&self) -> TileStatus {
        TileStatus::Reference(None)
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
        if state.queue.len() >= self.capacity {
            drop(state);
            call.refuse(Error::resource_limit(format!(
                "{} calls that read pixels are already waiting; retry after one is answered",
                self.capacity
            )));
            return;
        }
        state.queue.push_back(call);
        drop(state);
        self.shared.queued.notify_one();
    }

    fn disconnect(&self, client: ClientId) {
        let gone: VecDeque<TileCall> = {
            let mut state = self.shared.lock();
            let (gone, kept) = std::mem::take(&mut state.queue)
                .into_iter()
                .partition(|call| call.client() == client);
            state.queue = kept;
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

    /// The reference renders no bands: it answers `no-adapter`, there being no GPU here to render
    /// them on, and the export lane renders its own reference frame.
    fn stream(&self, _: &Evaluation, _: &Cancel) -> Result<BandStream, TileFallback> {
        Err(TileFallback::Unavailable(TileUnavailable::NoAdapter))
    }

    fn stop(&self) {
        let waiting = {
            let mut state = self.shared.lock();
            state.stopping = true;
            if let Some((_, cancel)) = &state.active {
                cancel.cancel();
            }
            std::mem::take(&mut state.queue)
        };
        drop(waiting);
        self.shared.queued.notify_one();
        let thread = self.thread().take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for ReferenceTiles {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
impl ReferenceTiles {
    /// How many calls wait behind the one being answered.
    pub(super) fn waiting(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// Whether the worker thread has started.
    pub(super) fn started(&self) -> bool {
        self.thread().is_some()
    }

    /// Have the worker pass `gate` before it answers each call, or stop passing one.
    pub(super) fn hold(&self, gate: Option<Arc<luxforge_testbase::Gate>>) {
        self.shared.lock().gate = gate;
    }
}

/// The worker: take the oldest call, answer it with the reference's reads, and sleep while the
/// queue is empty.
fn work(shared: &Shared) {
    let reads = ReferenceReads;
    loop {
        let call = {
            let mut state = shared.lock();
            loop {
                if state.stopping {
                    return;
                }
                if let Some(call) = state.queue.pop_front() {
                    state.active = Some((call.client(), call.cancel().clone()));
                    break call;
                }
                state = shared
                    .queued
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        #[cfg(test)]
        {
            let gate = shared.lock().gate.clone();
            if let Some(gate) = gate {
                gate.pass();
            }
        }
        call.run(&reads);
        shared.lock().active = None;
    }
}

/// The reference renderer's reads: whole frames, never point tiles, each answer naming the
/// reference with no reason. It holds nothing between calls; a session holds its call's
/// evaluations. See the [module documentation](self).
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceReads;

impl TileReads for ReferenceReads {
    fn session<'a>(
        &'a self,
        evaluation: &'a Evaluation,
        cancel: &'a Cancel,
    ) -> Box<dyn TileSession + 'a> {
        Box::new(ReferenceSession {
            evaluation,
            cancel,
            held: Vec::new(),
        })
    }
}

/// One call's reads of one evaluation through the reference renderer.
struct ReferenceSession<'a> {
    evaluation: &'a Evaluation,
    cancel: &'a Cancel,
    /// Each stage the call has read, with its size and its one evaluation in frame mode, held until
    /// the call ends.
    held: Vec<(ReadStage, Stage, Box<dyn StagePixels + 'a>)>,
}

impl<'a> ReferenceSession<'a> {
    /// The stage's size and its evaluation, evaluated by the first read of it.
    fn held(&mut self, stage: ReadStage) -> Result<(Stage, &(dyn StagePixels + 'a)), Error> {
        let index = match self.held.iter().position(|(held, ..)| *held == stage) {
            Some(index) => index,
            None => {
                let (size, pixels) = self.evaluate(stage)?;
                self.held.push((stage, size, pixels));
                self.held.len() - 1
            }
        };
        let (_, size, pixels) = &self.held[index];
        Ok((*size, &**pixels))
    }

    /// `stage` of the call's evaluation in frame mode: the stack's own compilation for the output
    /// stage, or the layers before `layer` compiled once and read as `layer` receives them — at the
    /// boundary width the whole stack chooses there, which is what the point path reads too.
    fn evaluate(&self, stage: ReadStage) -> Result<(Stage, Box<dyn StagePixels + 'a>), Error> {
        let evaluation = self.evaluation;
        match stage {
            ReadStage::Output => {
                let render = evaluation.exact(self.cancel)?;
                let (width, height) = render.stage();
                Ok((Stage { width, height }, render.frame_pixels()?))
            }
            ReadStage::Before { layer, mode } => {
                let recipe = evaluation.recipe();
                let (width, height) = evaluation.source().dimensions();
                let before = evaluation.registry().compile_layers(
                    width,
                    height,
                    prefix(&recipe.layers, layer)?,
                    &recipe.masks,
                    &recipe.strokes,
                    &recipe.artifacts,
                )?;
                let wide = evaluation.compiled()?.prefix_spatial_input_wide(&before);
                let size = before.stage();
                let render = Render::compiled(
                    evaluation.source().input(),
                    before,
                    RenderOptions::exact(self.cancel),
                    evaluation.context(),
                )?;
                Ok((size, render.frame_input(wide, mode.into())?))
            }
        }
    }
}

impl TileSession for ReferenceSession<'_> {
    fn read(
        &mut self,
        stage: ReadStage,
        rect: Region,
        values: ReadValues,
    ) -> Result<ReadAnswer, Error> {
        let cancel = self.cancel;
        cancel.check()?;
        let (size, pixels) = self.held(stage)?;
        let rect = clipped(rect, size);
        // A read's codes are bounded as an evaluated frame of its size is, which keeps its linear
        // values inside the planar limit.
        Raster::expected_len(rect.width, rect.height)?;
        let pixels = match values {
            ReadValues::Codes => ReadPixels::Codes(rows(rect, cancel, |x, y| pixels.rgba(x, y))?),
            ReadValues::Linear => ReadPixels::Linear(rows(rect, cancel, |x, y| {
                Ok(pixels
                    .linear(x, y)?
                    .map(|value| value.map(|channel| channel as f32)))
            })?),
        };
        Ok(ReadAnswer {
            stage: size,
            rect,
            pixels,
            answered: Answered::reference(None),
        })
    }
}

/// Each pixel of `rect`, row by row from its top-left, as `read` answers it, checking `cancel`
/// before each row.
fn rows<T>(
    rect: Region,
    cancel: &Cancel,
    read: impl Fn(u32, u32) -> Result<Option<T>, Error>,
) -> Result<Vec<T>, Error> {
    let mut values = Vec::with_capacity(rect.pixels() as usize);
    for y in rect.y0..rect.y1() {
        cancel.check()?;
        for x in rect.x0..rect.x1() {
            values.push(
                read(x, y)?.ok_or_else(|| Error::render("a pixel inside its stage is missing"))?,
            );
        }
    }
    Ok(values)
}
