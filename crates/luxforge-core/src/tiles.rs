//! The service that answers pixel reads and renders an export's bands, as the core sees it: a
//! [`TileService`] the host gives the catalog owner ([GPU-first], stage 4).
//!
//! # Shape
//!
//! Every call that reads pixels — `render.sample`, a module query's samples (the neutral picker,
//! `mask.sample-input`) and a mutation's planning reads (a colour-limited stroke's seed) — is
//! planned on the catalog owner in `O(layers)`, which reads no pixel itself ([performance rule
//! 5]), and handed to the service as a [`TileCall`]: the evaluation it planned, behind a closure,
//! and the reply its caller waits on. The service runs the call on a thread of its own with its
//! renderer's reads ([`TileReads`]): the call opens one [`TileSession`] over its evaluation and
//! asks it for rectangles of a stage — the output stage, or the stage a layer receives
//! ([`ReadStage`]) — as 8-bit codes or linear values ([`ReadValues`]). Every answer names the
//! renderer that drew it and why the reference did ([`Answered`]). An export asks the service for
//! its output stage as ordered bands ([`BandStream`]) instead, or is told why the reference
//! renders it ([`TileFallback`]).
//!
//! The contract names no GPU crate (the `gpu-free-core` rule): a host implements it over its own
//! device, as `HostConfig` is given a transport and a secret store. A host with no GPU provider is
//! served by [`ReferenceTiles`], the reference renderer's service, whose reads ([`ReferenceReads`])
//! are also what a GPU provider answers with when it cannot render a read.
//!
//! The catalog owner holds the host's service, or [`ReferenceTiles`] when the host gives none:
//! every `render.sample`, every query that reads a pixel and every mutation's planning read is a
//! call here, and its export lane asks it for each export's bands (`docs/design/export.md`). A
//! mutation's read is answered back to the owner, which replays the mutation once with the pixels
//! it read; nothing else waits on the owner thread.
//!
//! # Bounds
//!
//! At most [`TILE_QUEUE_CAPACITY`] calls wait behind the one a service answers, and a full queue
//! refuses at once with `resource-limit` (rule 6); submitting never blocks (rule 12). A read
//! answers at most the pixels one evaluated frame may hold. An export holds at most
//! [`EXPORT_BANDS_IN_FLIGHT`] rendered bands waiting for its encoder.
//!
//! [GPU-first]: ../../../docs/design/gpu-first.md
//! [performance rule 5]: ../../../docs/engineering/performance-rules.md#rules

use crate::{
    Cancel, ClientId, Error, Evaluation, GpuFallback, Region, Renderer, RendererReason,
    RendererRecord, Stage, editor::pixels::PixelAnswer,
};
use serde_json::{Value, json};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
};

pub(crate) mod grid;
mod reference;
pub use grid::{
    ClipDetection, GridRead, MAX_SIDE as GRID_MAX_SIDE, SampleGrid, SensorClip, read as read_grid,
};

#[cfg(test)]
pub(crate) use reference::Hold;
pub use reference::{ReferenceReads, ReferenceTiles};

#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod tests;

/// How many calls may wait behind the one a service is answering: every live loopback
/// connection's one outstanding call — the transport's live-client limit, `MAX_CLIENTS`, eight —
/// and the desktop's one call in flight. Every waiting call has a caller blocked on it, so none is
/// superseded; past this a call is refused with `resource-limit`.
pub const TILE_QUEUE_CAPACITY: usize = 9;

/// How many rendered bands of an export may wait for its encoder: the provider renders at most
/// this many bands ahead of the one being encoded, so an export holds that many bands, never its
/// whole frame.
pub const EXPORT_BANDS_IN_FLIGHT: usize = 2;

/// The service that answers every pixel read off the catalog owner and renders an export's output
/// stage as bands. The host gives the owner one, shared; a host without a GPU provider is served
/// by [`ReferenceTiles`].
///
/// A service answers the calls it is given in the order they were submitted, each exactly once:
/// by running it with its renderer's reads ([`TileCall::run`]), by refusing it
/// ([`TileCall::refuse`]), or by dropping it unanswered when its client disconnects or the service
/// stops, which closes its caller's reply as every call's closes when the owner stops.
pub trait TileService: Send + Sync {
    /// Which renderer answers this service's reads now, and why the reference does.
    fn status(&self) -> TileStatus;

    /// Queue `call` behind the calls waiting. Never blocks (rule 12): a full queue, at
    /// [`TILE_QUEUE_CAPACITY`] waiting calls, answers it at once with `resource-limit` through
    /// [`TileCall::refuse`], on its own reply.
    fn submit(&self, call: TileCall);

    /// Drop `client`'s waiting calls unanswered, and cancel its call being answered, which stops
    /// before its next row or tile.
    fn disconnect(&self, client: ClientId);

    /// Render `evaluation`'s output stage for an export, under `cancel`, as bands in order, which
    /// the export lane encodes as they arrive; or say why the reference renders it instead, and
    /// the lane renders its own reference frame. The provider renders on its own thread, so this
    /// never waits for the render. A provider that cannot go on drawing a stream ends it naming
    /// why ([`BandSender::fall_back`]), and the lane renders the export again with the reference.
    fn stream(&self, evaluation: &Evaluation, cancel: &Cancel) -> Result<BandStream, TileFallback>;

    /// Stop: cancel the call being answered, drop the waiting calls unanswered, and return once
    /// the service's threads have ended. A call submitted afterwards is dropped unanswered.
    fn stop(&self);
}

/// Which renderer answers a service's reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileStatus {
    /// The GPU renders the reads. One the GPU cannot render is answered by the reference, and its
    /// answer names why.
    Gpu,
    /// The reference renderer answers every read, for this reason. `None` for a host without a GPU
    /// provider, whose only renderer is the reference, as `luxforge-json`'s; it has nothing to
    /// fall back from.
    Reference(Option<TileFallback>),
}

/// Why a read or an export is rendered by the reference rather than the GPU. Each is a code a
/// service's answers name, beside the GPU preview's own fallback codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileFallback {
    /// The provider cannot render on its GPU at all (`tiles-unavailable`).
    Unavailable(TileUnavailable),
    /// The GPU plan of the read or the stream falls back, for the plan's own reason, whose code
    /// it answers.
    Plan(GpuFallback),
    /// The GPU work would hold `requested` bytes, past the provider's `budget` (`tiles-budget`).
    Budget { requested: u64, budget: u64 },
    /// The provider's GPU stage cannot run the plan, for the reason the stage names, whose code it
    /// answers: the photo surface's own `pipeline-failed`, `texture-limit`, `buffer-limit` or
    /// `source-missing`, or one the desktop's conversion of the plan names (`warp-grid`,
    /// `boundary-size`, `position-range`, `region-outside`). The codes are the stage's; this crate
    /// names none of them.
    Stage(&'static str),
}

impl TileFallback {
    /// A stable kebab-case name for the reason: `tiles-unavailable`, `tiles-budget`, or the GPU
    /// plan's own code ([`GpuFallback::code`]).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "tiles-unavailable",
            Self::Plan(fallback) => fallback.code(),
            Self::Budget { .. } => "tiles-budget",
            Self::Stage(code) => code,
        }
    }
}

/// The reason an export's result names for the reference rendering it, in the session's shape: the
/// provider's own unavailability by its reason, its budget as `tiles-budget`, and a plan the GPU
/// cannot draw by the plan's or the stage's code.
impl From<&TileFallback> for RendererReason {
    fn from(fallback: &TileFallback) -> Self {
        match fallback {
            TileFallback::Unavailable(reason) => match reason {
                TileUnavailable::Pending => Self::SurfacePending,
                TileUnavailable::NoAdapter => Self::NoAdapter,
                TileUnavailable::Refused => Self::Refused,
                TileUnavailable::DeviceLost => Self::DeviceLost,
                TileUnavailable::AdapterMismatch => Self::AdapterMismatch,
            },
            TileFallback::Budget { .. } => Self::Budget,
            TileFallback::Plan(_) | TileFallback::Stage(_) => Self::Plan(fallback.code()),
        }
    }
}

/// Why a provider cannot render on its GPU at all, or not yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileUnavailable {
    /// The provider has not been told which adapter the picture is drawn with yet, so it opens
    /// nothing until it is: the desktop names it once its photo surface has checked its GPU stage.
    Pending,
    /// No adapter on this host can run the provider's programs.
    NoAdapter,
    /// The launch refused the GPU (`--no-gpu-render`).
    Refused,
    /// The provider's device was lost; nothing waits for a recovery.
    DeviceLost,
    /// The provider's adapter is not the one the picture is drawn with, so its reads could not be
    /// the picture's own bytes.
    AdapterMismatch,
}

impl TileUnavailable {
    /// A stable kebab-case name for the reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "surface-pending",
            Self::NoAdapter => "no-adapter",
            Self::Refused => "refused",
            Self::DeviceLost => "device-lost",
            Self::AdapterMismatch => "adapter-mismatch",
        }
    }
}

/// One call that reads pixels, planned on the catalog owner and answered by a [`TileService`]:
/// the client it is for, its cancellation, the evaluation that reads through the service's
/// renderer, and the reply its caller waits on.
pub struct TileCall {
    client: ClientId,
    cancel: Cancel,
    evaluate: Evaluate,
    reply: Reply,
}

/// What a call evaluates, with the service's reads and the call's cancellation.
type Evaluate = Box<dyn FnOnce(&dyn TileReads, &Cancel) -> Result<TileAnswer, Error> + Send>;

impl TileCall {
    /// A call whose caller waits for a JSON value — `render.sample`, a module query — evaluated by
    /// `evaluate` and handed to `deliver`, which answers the caller: the owner's response on the
    /// caller's own channel, with the request's identity and the event sequence the owner had when
    /// it planned the call.
    pub fn caller(
        client: ClientId,
        cancel: Cancel,
        evaluate: impl FnOnce(&dyn TileReads, &Cancel) -> Result<Value, Error> + Send + 'static,
        deliver: impl FnOnce(Result<Value, Error>) + Send + 'static,
    ) -> Self {
        Self {
            client,
            cancel,
            evaluate: Box::new(move |reads, cancel| evaluate(reads, cancel).map(TileAnswer::Value)),
            reply: Reply::Caller(Box::new(deliver)),
        }
    }

    /// A mutation's pixel read, evaluated by `evaluate` and handed to `deliver`, which returns the
    /// pixels to the catalog owner for its entry, revision, draft and source checks before the
    /// mutation is replayed once: what the owner submits for each read it parks.
    pub(crate) fn pixels(
        client: ClientId,
        cancel: Cancel,
        evaluate: impl FnOnce(&dyn TileReads, &Cancel) -> Result<PixelAnswer, Error> + Send + 'static,
        deliver: impl FnOnce(Result<PixelAnswer, Error>) + Send + 'static,
    ) -> Self {
        Self {
            client,
            cancel,
            evaluate: Box::new(move |reads, cancel| {
                evaluate(reads, cancel).map(|pixels| TileAnswer::Pixels(Box::new(pixels)))
            }),
            reply: Reply::Pixels(Box::new(deliver)),
        }
    }

    /// The client the call is for: the one whose disconnect drops it, waiting or running.
    pub fn client(&self) -> ClientId {
        self.client
    }

    /// The call's cancellation, which its service cancels when its client disconnects while the
    /// call is being answered.
    pub fn cancel(&self) -> &Cancel {
        &self.cancel
    }

    /// Evaluate the call with `reads`, the service's renderer, and answer it: with `cancelled`
    /// when its cancellation came first, and with `internal` when the evaluation panicked, so the
    /// service lives on for the next call. Runs on the service's own thread, never the owner's.
    pub fn run(self, reads: &dyn TileReads) {
        let Self {
            cancel,
            evaluate,
            reply,
            ..
        } = self;
        let result = catch_unwind(AssertUnwindSafe(|| {
            cancel.check()?;
            evaluate(reads, &cancel)
        }))
        .unwrap_or_else(|_| Err(Error::internal("a tile call's evaluation panicked")));
        reply.answer(result);
    }

    /// Answer the call with `error` without evaluating it, such as a full queue's
    /// `resource-limit`.
    pub fn refuse(self, error: Error) {
        self.reply.answer(Err(error));
    }
}

/// What a call's evaluation answers: a JSON value for a caller, or a mutation's pixels for the
/// catalog owner.
pub(crate) enum TileAnswer {
    Value(Value),
    Pixels(Box<PixelAnswer>),
}

/// Where a call's answer goes: a caller is answered directly; a mutation's pixels return to the
/// catalog owner, which checks what they were read from before it replays the mutation. Each holds
/// the owner's own delivery, so this module names none of the owner's channels.
pub(crate) enum Reply {
    Caller(Box<dyn FnOnce(Result<Value, Error>) + Send>),
    Pixels(Box<dyn FnOnce(Result<PixelAnswer, Error>) + Send>),
}

impl Reply {
    fn answer(self, result: Result<TileAnswer, Error>) {
        match self {
            Self::Caller(deliver) => deliver(result.and_then(|answer| match answer {
                TileAnswer::Value(value) => Ok(value),
                TileAnswer::Pixels(_) => Err(Error::internal(
                    "pixel reads cannot answer a caller directly",
                )),
            })),
            Self::Pixels(deliver) => deliver(result.and_then(|answer| match answer {
                TileAnswer::Pixels(pixels) => Ok(*pixels),
                TileAnswer::Value(_) => Err(Error::internal("the pixel read returned no pixels")),
            })),
        }
    }
}

/// A service's renderer, as a call reads through it.
pub trait TileReads {
    /// Renderer identity for retained sample grids ([`grid::read`]). Providers without a stable
    /// identity do not retain grids; a fallback answer is never retained as a GPU answer.
    fn grid_renderer(&self) -> Option<RendererRecord> {
        None
    }

    /// A session over `evaluation`'s stack and source for one call, under `cancel`. What it holds
    /// between reads lives only as long as the call.
    fn session<'a>(
        &'a self,
        evaluation: &'a Evaluation,
        cancel: &'a Cancel,
    ) -> Box<dyn TileSession + 'a>;
}

/// One call's reads of one evaluation.
pub trait TileSession {
    /// The pixels of `rect` of `stage` as `values`. The rectangle answered is `rect` clipped to the
    /// stage ([`clipped`]), empty where they do not meet, so a read can always say which stage it
    /// missed. Refused with `resource-limit` past the pixels one evaluated frame may hold, and with
    /// `cancelled` once the call's cancellation is set.
    fn read(
        &mut self,
        stage: ReadStage,
        rect: Region,
        values: ReadValues,
    ) -> Result<ReadAnswer, Error>;

    /// Gather at most a 1024-square grid's already-mapped nearest pixels, in caller order.
    /// The default groups reads into bounded 256-square tiles; the reference overrides this to
    /// read its held stage directly. Neither path allocates an output-sized frame for the gather.
    fn gather(
        &mut self,
        stage: ReadStage,
        points: &[[u32; 2]],
        cancel: &Cancel,
    ) -> Result<GatherAnswer, Error> {
        gather_tiles(self, stage, points, cancel)
    }
}

/// Gather nearest pixels through bounded tile reads. Providers may retry this whole gather on
/// their reference fallback so one answer never mixes renderer identities.
pub fn gather_tiles<T: TileSession + ?Sized>(
    session: &mut T,
    stage: ReadStage,
    points: &[[u32; 2]],
    cancel: &Cancel,
) -> Result<GatherAnswer, Error> {
    if points.len() > 1024 * 1024 {
        return Err(Error::resource_limit(
            "analysis grid exceeds 1024 squared points",
        ));
    }
    const SIDE: u32 = 256;
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_unstable_by_key(|&index| (points[index][1] / SIDE, points[index][0] / SIDE));
    let mut linear = vec![[0.; 3]; points.len()];
    let mut answered = None;
    let mut from = 0;
    while from < order.len() {
        cancel.check()?;
        let [x, y] = points[order[from]];
        let (bx, by) = (x / SIDE, y / SIDE);
        let end = from
            + order[from..]
                .partition_point(|&i| points[i][0] / SIDE == bx && points[i][1] / SIDE == by);
        let answer = session.read(
            stage,
            Region {
                x0: bx * SIDE,
                y0: by * SIDE,
                width: SIDE,
                height: SIDE,
            },
            ReadValues::Linear,
        )?;
        if answered
            .as_ref()
            .is_some_and(|held| *held != answer.answered)
        {
            return Err(Error::render(
                "analysis grid changed renderer during its read",
            ));
        }
        answered = Some(answer.answered.clone());
        for &index in &order[from..end] {
            let [x, y] = points[index];
            linear[index] = answer
                .linear(x, y)
                .ok_or_else(|| Error::render("analysis grid point lies outside its stage"))?;
        }
        from = end;
    }
    Ok(GatherAnswer { linear, answered })
}

/// A gather's bounded linear values and the renderer that answered every point.
pub struct GatherAnswer {
    pub linear: Vec<[f32; 3]>,
    pub answered: Option<Answered>,
}

/// Which stage of an evaluation a read reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadStage {
    /// The stack's output stage: what `render.sample` reads.
    Output,
    /// The stage layer `layer` receives — the output of the layers before it, with the boundary
    /// width the whole stack hands it there — read as that layer receives it: what the neutral
    /// picker, a colour-limited stroke's seed and `mask.sample-input` read. `layer` may be the
    /// stack's length, whose prefix is the whole stack.
    Before { layer: usize, mode: MaskInputMode },
}

/// How a layer receives the stage before it, which decides what a [`ReadValues::Linear`] read of
/// that stage answers; a [`ReadValues::Codes`] read answers the stage's bytes either way. The
/// public counterpart of the renderer's own mode, which the catalog owner chooses by the layer's
/// effect stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskInputMode {
    /// A spatial layer's: the encoded boundary the stage is handed on at.
    Boundary,
    /// A colour layer's: the unclamped value inside its colour run.
    ColourRun,
}

impl From<MaskInputMode> for crate::render::MaskInputMode {
    fn from(mode: MaskInputMode) -> Self {
        match mode {
            MaskInputMode::Boundary => Self::Boundary,
            MaskInputMode::ColourRun => Self::ColourRun,
        }
    }
}

impl From<crate::render::MaskInputMode> for MaskInputMode {
    fn from(mode: crate::render::MaskInputMode) -> Self {
        match mode {
            crate::render::MaskInputMode::Boundary => Self::Boundary,
            crate::render::MaskInputMode::ColourRun => Self::ColourRun,
        }
    }
}

/// What a read answers for each pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadValues {
    /// Opaque 8-bit sRGB codes, `[r, g, b, 255]`: the bytes the stage holds, the picture's own for
    /// the output stage.
    Codes,
    /// Linear sRGB values as `f32`, before the stage's bytes are quantized from them.
    Linear,
}

/// The answer to one read.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadAnswer {
    /// The whole stage read, which a point outside it is reported against.
    pub stage: Stage,
    /// The rectangle answered: the one asked for, clipped to the stage ([`clipped`]).
    pub rect: Region,
    /// One value per pixel of `rect`, row by row from its top-left.
    pub pixels: ReadPixels,
    /// The renderer that drew the pixels, and why the reference did.
    pub answered: Answered,
}

impl ReadAnswer {
    /// The code at `(x, y)` of the stage: `None` outside the rectangle answered, and for a read of
    /// linear values.
    pub fn code(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        match &self.pixels {
            ReadPixels::Codes(codes) => self.index(x, y).map(|index| codes[index]),
            ReadPixels::Linear(_) => None,
        }
    }

    /// The linear value at `(x, y)` of the stage: `None` outside the rectangle answered, and for
    /// a read of codes.
    pub fn linear(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        match &self.pixels {
            ReadPixels::Linear(values) => self.index(x, y).map(|index| values[index]),
            ReadPixels::Codes(_) => None,
        }
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        self.rect.contains(x, y).then(|| {
            (y - self.rect.y0) as usize * self.rect.width as usize + (x - self.rect.x0) as usize
        })
    }
}

/// The pixels of a read, as it asked for them ([`ReadValues`]).
#[derive(Clone, Debug, PartialEq)]
pub enum ReadPixels {
    Codes(Vec<[u8; 4]>),
    Linear(Vec<[f32; 3]>),
}

/// Which renderer drew an answer, and why the reference did: `None` when the GPU drew it, and when
/// a host without a GPU provider has only the reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub record: RendererRecord,
    pub reason: Option<TileFallback>,
}

impl Answered {
    /// Drawn by the GPU.
    pub const fn gpu() -> Self {
        Self {
            record: RendererRecord::Gpu,
            reason: None,
        }
    }

    /// Drawn by the reference renderer, for `reason`.
    pub const fn reference(reason: Option<TileFallback>) -> Self {
        Self {
            record: RendererRecord::Reference,
            reason,
        }
    }
}

/// The renderer an answer names, in the session's shape: the GPU; the reference for the reason the
/// GPU did not draw it; or the reference with no reason on a host without a GPU provider, whose
/// only renderer it is.
impl From<&Answered> for Renderer {
    fn from(answered: &Answered) -> Self {
        match (answered.record, &answered.reason) {
            (RendererRecord::Gpu, _) => Self::gpu(),
            (RendererRecord::Reference, Some(reason)) => Self::reference(reason.into()),
            (RendererRecord::Reference, None) => Self::headless(),
        }
    }
}

/// The rectangle a read of `rect` answers over `stage`: `rect` clipped to it, empty at the
/// stage's edge where they do not meet. Every provider answers this rectangle.
pub fn clipped(rect: Region, stage: Stage) -> Region {
    let x0 = rect.x0.min(stage.width);
    let y0 = rect.y0.min(stage.height);
    Region {
        x0,
        y0,
        width: rect.x1().min(stage.width) - x0,
        height: rect.y1().min(stage.height) - y0,
    }
}

/// One band of an export's output stage: `rows` whole rows from row `y0`, each opaque RGBA8, row by
/// row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band {
    pub y0: u32,
    pub rows: u32,
    pub rgba: Vec<u8>,
}

/// An export's output stage as its provider renders it: bands received in order, top to bottom,
/// at most [`EXPORT_BANDS_IN_FLIGHT`] of them waiting for the encoder, with the renderer that drew
/// them. Dropping it abandons the stream: its provider's next send fails, and it renders nothing
/// more for it. A stream its provider could not go on drawing says why ([`Self::fallback`]).
pub struct BandStream {
    bands: Receiver<Result<Band, Ended>>,
    width: u32,
    height: u32,
    /// The first row of the band expected next; the stage's height once the stream has ended.
    next: u32,
    answered: Answered,
    /// Why the provider stopped drawing the stream, once it ended for a reason the reference
    /// renders the export for instead.
    fallback: Option<TileFallback>,
    /// Called each time the stream hands a band to its encoder, which leaves room for one more,
    /// and once when it is dropped ([`Self::waking`]).
    wake: Option<Box<dyn Fn() + Send>>,
    /// Set when the stream is dropped, which its provider reads ([`BandSender::abandoned`]).
    abandoned: Arc<AtomicBool>,
}

/// A provider's end of a [`BandStream`].
pub struct BandSender {
    sender: SyncSender<Result<Band, Ended>>,
    abandoned: Arc<AtomicBool>,
}

/// What ends a stream before its last row: the error its encoder reads, and, when the provider
/// could not go on drawing it, why, for the reference to render the export instead.
struct Ended {
    error: Error,
    fallback: Option<TileFallback>,
}

impl BandStream {
    /// A stream of a `width` × `height` output stage drawn by the renderer `answered` names, and
    /// the end its provider sends the bands into.
    pub fn channel(width: u32, height: u32, answered: Answered) -> (BandSender, Self) {
        Self::with_wake(width, height, answered, None)
    }

    /// [`Self::channel`] for a provider that renders on a thread of its own which must never wait
    /// on its encoder, such as one that answers pixel reads between an export's tiles: `wake` is
    /// called each time the stream hands a band to its encoder, which leaves room for one more,
    /// and once when the stream is dropped, so the provider sends a band only when the stream has
    /// room for it and sleeps, rather than blocking on a full channel, until it does. `wake` runs
    /// on the encoder's thread: it only notes the room and wakes the provider.
    pub fn waking(
        width: u32,
        height: u32,
        answered: Answered,
        wake: impl Fn() + Send + 'static,
    ) -> (BandSender, Self) {
        Self::with_wake(width, height, answered, Some(Box::new(wake)))
    }

    fn with_wake(
        width: u32,
        height: u32,
        answered: Answered,
        wake: Option<Box<dyn Fn() + Send>>,
    ) -> (BandSender, Self) {
        let (sender, bands) = sync_channel(EXPORT_BANDS_IN_FLIGHT);
        let abandoned = Arc::new(AtomicBool::new(false));
        let stream = Self {
            bands,
            width,
            height,
            next: 0,
            answered,
            fallback: None,
            wake,
            abandoned: Arc::clone(&abandoned),
        };
        (BandSender { sender, abandoned }, stream)
    }

    /// The renderer that drew the bands, and why the reference did.
    pub fn answered(&self) -> &Answered {
        &self.answered
    }

    /// Why the provider stopped drawing the stream, once it has ended for a reason the reference
    /// renders the export for instead ([`BandSender::fall_back`]): `None` while it goes on, once
    /// it is whole, and after a cancellation or an error of its own.
    pub fn fallback(&self) -> Option<&TileFallback> {
        self.fallback.as_ref()
    }
}

impl Iterator for BandStream {
    type Item = Result<Band, Error>;

    /// The next band, waiting for its provider to render it: `None` once the stage's last row has
    /// arrived, and after an error. A band that does not start where the last one ended, holds no
    /// row, runs past the stage or holds the wrong number of bytes is an error, as is a provider
    /// that stops before the last row; the provider's own error ends the stream too.
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.height {
            return None;
        }
        let row = self.next;
        self.next = self.height;
        let received = self.bands.recv();
        if received.is_ok()
            && let Some(wake) = &self.wake
        {
            wake();
        }
        let band = match received {
            Ok(Ok(band)) => band,
            Ok(Err(Ended { error, fallback })) => {
                self.fallback = fallback;
                return Some(Err(error));
            }
            Err(_) => {
                return Some(Err(Error::internal(format!(
                    "the tile stream ended at row {row} of {}",
                    self.height
                ))));
            }
        };
        let end = u64::from(band.y0) + u64::from(band.rows);
        let bytes = u64::from(self.width) * u64::from(band.rows) * 4;
        if band.y0 != row
            || band.rows == 0
            || end > u64::from(self.height)
            || band.rgba.len() as u64 != bytes
        {
            return Some(Err(Error::internal(format!(
                "the tile stream sent {} rows from row {} in {} bytes where rows of {} pixels \
                 from row {row} of {} were next",
                band.rows,
                band.y0,
                band.rgba.len(),
                self.width,
                self.height
            ))));
        }
        self.next = end as u32;
        Some(Ok(band))
    }
}

impl BandSender {
    /// Hand the next band, or the error that ends the stream, to the stream, waiting while
    /// [`EXPORT_BANDS_IN_FLIGHT`] bands wait in it. `false` once the stream has been dropped: its
    /// export was abandoned, and the provider renders nothing more for it.
    pub fn send(&self, band: Result<Band, Error>) -> bool {
        let band = band.map_err(|error| Ended {
            error,
            fallback: None,
        });
        self.sender.send(band).is_ok()
    }

    /// End the stream because the GPU cannot go on drawing it, for `fallback`, as [`Self::send`]
    /// ends it with an error: its encoder reads an error naming the reason in its data
    /// (`fallback`, the reason's code; `unavailable` for a provider that cannot draw; `requested`
    /// and `budget` for a stage no side's tiles fit; `detail` for the plan's own reason), and the
    /// stream says why ([`BandStream::fallback`]), so the export lane renders the export again
    /// with the reference, naming it: GPU and reference bands are never mixed in one export.
    pub fn fall_back(&self, fallback: TileFallback) -> bool {
        let error = ended(&fallback);
        let ended = Ended {
            error,
            fallback: Some(fallback),
        };
        self.sender.send(Err(ended)).is_ok()
    }

    /// Whether the stream has been dropped: its export was abandoned, and the provider renders
    /// nothing more for it.
    pub fn abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }
}

impl Drop for BandStream {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::Release);
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

/// The error that ends a stream the GPU cannot go on drawing, naming why in its data
/// ([`BandSender::fall_back`]).
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
