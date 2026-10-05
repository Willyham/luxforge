//! The GPU stage's pipelines, compiled off the UI thread.
//!
//! A pipeline is kept per program sequence — each step's kind, entry and source — never per image,
//! size or uniform value, so a sequence compiles at most once per process while it stays cached.
//! Compiling is the one expensive thing the stage does (`naga` validates the assembled WGSL, the
//! backend translates it and the driver builds the pipeline), so it never runs on the UI thread:
//!
//! - **A first-seen sequence** is queued for one compile thread, which holds a clone of the device
//!   and the stage's shared support — its layouts and the spatial passes' cache — and compiles
//!   through the stage's one `compile`. The frame that asked draws the CPU frame and names why
//!   ([`GpuFallback::Compiling`]).
//! - **The thread drains its queue.** It compiles every queued sequence, one after another,
//!   whether or not a frame is drawn, and keeps each result itself, so the next `prepare` finds the
//!   pipeline ready, or failed, and the figures count it when it finishes. A sequence a frame asked
//!   for goes to the front of the queue, ahead of every warmed one, and its compile's end wakes the
//!   surface so that frame is drawn again; a warmed one's end wakes nothing. With the queue empty
//!   the thread sleeps on its channel, which each queued sequence signals: no timer and no poll, so
//!   an idle editor stays asleep.
//! - **Warming.** The desktop names the sequences a gesture is likely to need when the stack
//!   changes ([`super::super::PhotoSurface::gpu_warm`]), so they compile before a drag begins: the
//!   open stack's first, then the rest of the program set. A frame asks for its own sequences
//!   before it hands a warm list, so the picture on screen compiles ahead of both. A new list lets
//!   go of the warmed sequences an older one left waiting and queues its own in its order.
//! - **The warm-up.** From a warm list handed to an idle thread until its queue drains, with when
//!   the open stack's part had compiled ([`WarmUpFigures`]): the figures the desktop records, and
//!   one wake of the surface at its start and one at its end, so the desktop sees both.
//! - **Bounds.** At most [`PIPELINE_CACHE`] compiled sequences are kept, ready and failed ones
//!   together, the least recently asked for or warmed evicted first when a compile ends, and at
//!   most as many wait in the queue; a warmed sequence that finds the queue full is not queued, and
//!   one a frame asks for takes the place of the newest warmed one waiting.
//!   If a full queue contains only sequences already asked for by frames, a new sequence retries
//!   admission on a later prepare while its frame uses the CPU.
//!
//! The compile thread takes wgpu's error scopes around the pipeline it creates. wgpu 27's scopes
//! belong to the device, not to a thread, and the UI thread pushes none of its own, so the stack
//! stays balanced; an error the UI thread raised while a compile's scope is open would be counted
//! as that compile's failure, which takes the CPU path for that sequence and nothing worse.
use super::{
    BoundaryFormat, CompileFigures, Compiled, Figures, GpuFallback, GpuStep, StepKind, Support,
    compile,
};
use crate::photo_surface::wake_surface;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::Ordering,
        mpsc::{Sender, channel},
    },
    time::Instant,
};

/// How many compiled program sequences, ready or failed, a pipeline keeps, and how many may wait
/// for the compile thread.
///
/// Every tick asks for every link's sequence, so the cache holds every link of the plan a drag
/// draws together, or a compile that ends would evict one of the plan's own and the drag would
/// never draw on the GPU. The largest plan is a link for the colour steps before its first spatial
/// step and one for each of up to 18 spatial steps, Detail and Presence globally and 16 masked
/// layers, and the light link its lights are computed with (the core's `GPU_PLAN_LINKS`); the warm
/// list holds at most the rest (`GPU_WARM_LINKS`), so both fit at once. A compiled sequence holds its render pipeline and its passes' compute
/// pipelines, which sequences that run the same pass share through the pass cache
/// ([`super::spatial::PASS_CACHE`]), so a sequence of a new shape adds little more than its render
/// pipeline.
pub const PIPELINE_CACHE: usize = 64;

/// The program sequences a gesture is likely to need, which the desktop names when the stack
/// changes so they compile before a drag begins ([`super::super::PhotoSurface::gpu_warm`]). Each
/// sequence is a plan's steps and the format its boundary will be held in, which its chain's
/// intermediates take ([`super::chain`]); beside them each light link's steps, one sequence of its
/// own ([`super::light`]). Only the steps' kinds and programs matter, never their words. A new
/// `version` is warmed once, so handing the same one to every frame costs nothing.
#[derive(Clone, Debug)]
pub struct GpuWarm {
    version: u64,
    sequences: Arc<[(Vec<GpuStep>, BoundaryFormat)]>,
    lights: Arc<[Vec<GpuStep>]>,
    /// How many of the sequences, from the first, are the open stack's.
    open: usize,
}

impl GpuWarm {
    /// A warm list whose every sequence is the open stack's.
    pub fn new(version: u64, sequences: Vec<(Vec<GpuStep>, BoundaryFormat)>) -> Self {
        let open = sequences.len();
        Self {
            version,
            sequences: sequences.into(),
            lights: Arc::from([]),
            open,
        }
    }

    /// The list with only its first `open` sequences the open stack's: the rest of the program
    /// set follows them.
    pub fn with_open(mut self, open: usize) -> Self {
        self.open = open.min(self.sequences.len());
        self
    }

    /// How many of the sequences, from the first, are the open stack's.
    pub fn open(&self) -> usize {
        self.open
    }

    /// The warm list with the steps of the light links its plans compute beside it, each warmed as
    /// the sequence a light link compiles ([`super::light::GpuLight::steps`]), with the open
    /// stack's sequences.
    pub fn with_lights(self, lights: Vec<Vec<GpuStep>>) -> Self {
        Self {
            lights: lights.into(),
            ..self
        }
    }

    pub fn lights(&self) -> &[Vec<GpuStep>] {
        &self.lights
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn sequences(&self) -> &[(Vec<GpuStep>, BoundaryFormat)] {
        &self.sequences
    }
}

/// What a frame asks of the GPU stage beside its plan: hold the plan's slot but draw the CPU frame
/// (the CPU frame of the same content has arrived, and it is the reference), the tag the plan's
/// output is reported under, and the sequences to warm.
#[derive(Clone, Debug, Default)]
pub(in crate::photo_surface) struct GpuOptions {
    pub(in crate::photo_surface) hold: bool,
    pub(in crate::photo_surface) tag: Option<u64>,
    pub(in crate::photo_surface) warm: Option<GpuWarm>,
    /// Where the plan draws other values than the one the caller last handed under another tag
    /// ([`super::GpuChange`]).
    pub(in crate::photo_surface) change: Option<super::GpuChange>,
}

/// What a cached sequence is keyed by: each step's signature in order, the shape of a masked
/// step and each program's role, entry and source ([`GpuStep::signature`]).
pub(super) type Signature = Vec<(StepKind, String, String)>;

pub(super) fn signature(steps: &[GpuStep]) -> Signature {
    steps
        .iter()
        .flat_map(GpuStep::signature)
        .map(|(kind, entry, source)| (kind, entry.to_owned(), source.to_owned()))
        .collect()
}

fn matches(signature: &Signature, steps: &[GpuStep]) -> bool {
    signature
        .iter()
        .map(|(kind, entry, source)| (*kind, entry.as_str(), source.as_str()))
        .eq(steps.iter().flat_map(GpuStep::signature))
}

/// One sequence to compile: a link's steps and the format its last pass writes, the output's codes
/// or a chain's intermediate.
pub(super) type Sequence = (Vec<GpuStep>, wgpu::TextureFormat);

/// Where one known sequence is.
enum State {
    /// Waiting for the compile thread, with its steps.
    Queued(Vec<GpuStep>),
    /// On the compile thread.
    Compiling,
    /// One pipeline per pass.
    Ready(Compiled),
    /// Refused, with why: kept, so it is not compiled again every frame. The reason is read by the
    /// tests that name each failure.
    Failed(#[cfg_attr(not(test), allow(dead_code))] Arc<str>),
}

impl State {
    fn compiled(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Failed(_))
    }
}

struct Entry {
    signature: Signature,
    /// The format the sequence's last pass writes.
    format: wgpu::TextureFormat,
    state: State,
    /// The identity of this sequence's compile.
    id: u64,
    /// When a frame last asked for it, on the pipeline's clock.
    used: u64,
    /// A frame asked for it, and waits for its compile's end to be woken.
    asked: bool,
}

/// The warm-up the compile thread is running, or ran last ([`WarmUpFigures`]).
struct WarmUp {
    figures: WarmUpFigures,
    /// When it began: a warm list handed to an idle thread.
    started: Instant,
    /// When the newest warm list was handed.
    handed: Instant,
    /// The sequence whose compile ends the newest list's open part: the last one waiting once its
    /// open part was queued.
    open_after: Option<u64>,
}

/// One warm-up of the compile thread: from a warm list handed to an idle thread until its queue
/// has drained, every sequence a frame asked for meanwhile included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WarmUpFigures {
    /// Which warm-up of the pipeline's, from 1: the launch's first is the first photograph's.
    pub period: u64,
    /// The newest warm list's version it compiles.
    pub version: u64,
    /// The sequences its lists queued, and of them the newest list's open stack's.
    pub sequences: u64,
    pub open_sequences: u64,
    /// From the newest list's handing until its open stack's part, and everything a frame had
    /// asked for before it, had compiled; `None` until then.
    pub open_us: Option<u64>,
    /// From its start until the queue drained; `None` while it runs.
    pub us: Option<u64>,
}

impl WarmUpFigures {
    /// Whether the compile thread is still compiling it.
    pub fn running(&self) -> bool {
        self.us.is_none()
    }
}

fn micros(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// What the UI thread and the compile thread share: every known sequence, and the queue of those
/// waiting, in compile order.
#[derive(Default)]
struct Shared {
    entries: Vec<Entry>,
    queue: VecDeque<u64>,
    /// The sequence on the compile thread.
    compiling: Option<u64>,
    /// The warm-up running, or the last one.
    warm_up: Option<WarmUp>,
    /// Tests only: each sequence's first entry, in the order the thread took them.
    #[cfg(test)]
    taken: Vec<String>,
    clock: u64,
    /// How many warmed sequences a frame's has taken the place of in a full queue: dropped
    /// unqueued, never compiled.
    dropped: u64,
    /// The pipeline that owns the thread is gone: the thread ends.
    closed: bool,
}

impl Shared {
    fn find(&mut self, steps: &[GpuStep], format: wgpu::TextureFormat) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.format == format && matches(&entry.signature, steps))
    }

    /// Whether `steps` writing `format` is known, which a warm list naming it again marks used now.
    fn warmed_again(&mut self, steps: &[GpuStep], format: wgpu::TextureFormat) -> bool {
        self.clock += 1;
        let clock = self.clock;
        self.find(steps, format)
            .map(|entry| entry.used = clock)
            .is_some()
    }

    /// Queue `steps` writing `format` under a new identity, first when a frame `asked` for them
    /// and last when they are warmed. `false` when the bounded queue has no room, including when
    /// it holds only asked sequences and none can be displaced by a new request.
    fn queue(&mut self, steps: &[GpuStep], format: wgpu::TextureFormat, asked: bool) -> bool {
        if self.queue.len() >= PIPELINE_CACHE {
            if !asked {
                return false;
            }
            // A frame's sequence takes the place of the newest warmed one waiting.
            let warmed = self.queue.iter().rposition(|id| {
                self.entries
                    .iter()
                    .any(|entry| entry.id == *id && !entry.asked)
            });
            if let Some(at) = warmed
                && let Some(id) = self.queue.remove(at)
            {
                self.entries.retain(|entry| entry.id != id);
                self.dropped += 1;
            } else {
                return false;
            }
        }
        self.clock += 1;
        let id = self.clock;
        self.entries.push(Entry {
            signature: signature(steps),
            format,
            state: State::Queued(steps.to_vec()),
            id,
            used: id,
            asked,
        });
        if asked {
            self.queue.push_front(id);
        } else {
            self.queue.push_back(id);
        }
        true
    }

    /// The next queued sequence, now compiling, with its identity, steps and format.
    fn next(&mut self) -> Option<(u64, Vec<GpuStep>, wgpu::TextureFormat)> {
        while let Some(id) = self.queue.pop_front() {
            let entry = self.entries.iter_mut().find(|entry| entry.id == id);
            if let Some(entry) = entry
                && let State::Queued(steps) = std::mem::replace(&mut entry.state, State::Compiling)
            {
                #[cfg(test)]
                self.taken
                    .extend(entry.signature.first().map(|step| step.1.clone()));
                self.compiling = Some(id);
                return Some((id, steps, entry.format));
            }
        }
        None
    }

    /// Let go of every warmed sequence still waiting, which a newer warm list queues again in its
    /// own order if it names it: how many.
    fn unqueue_warmed(&mut self) -> u64 {
        let warmed: Vec<u64> = self
            .queue
            .iter()
            .copied()
            .filter(|id| {
                self.entries
                    .iter()
                    .any(|entry| entry.id == *id && !entry.asked)
            })
            .collect();
        self.queue.retain(|id| !warmed.contains(id));
        self.entries.retain(|entry| !warmed.contains(&entry.id));
        warmed.len() as u64
    }

    /// Warm list `version` handed now, having queued `sequences`, of them `open_sequences` the
    /// open stack's, the last of everything then waiting `open_after`: begin a warm-up, or carry on
    /// the one running. Its figures, and whether it began.
    fn warm_up(
        &mut self,
        version: u64,
        sequences: u64,
        open_sequences: u64,
        open_after: Option<u64>,
    ) -> (WarmUpFigures, bool) {
        let now = Instant::now();
        let busy = self.compiling.is_some() || !self.queue.is_empty();
        let running = self
            .warm_up
            .as_ref()
            .is_some_and(|warm_up| warm_up.figures.running());
        let period = self
            .warm_up
            .as_ref()
            .map_or(1, |warm_up| warm_up.figures.period + 1);
        let warm_up = match self.warm_up.as_mut().filter(|_| running) {
            Some(warm_up) => {
                warm_up.figures.sequences += sequences;
                warm_up
            }
            None => self.warm_up.insert(WarmUp {
                figures: WarmUpFigures {
                    period,
                    sequences,
                    ..WarmUpFigures::default()
                },
                started: now,
                handed: now,
                open_after: None,
            }),
        };
        warm_up.handed = now;
        warm_up.open_after = open_after;
        warm_up.figures.version = version;
        warm_up.figures.open_sequences = open_sequences;
        warm_up.figures.open_us = open_after.is_none().then_some(0);
        if !busy {
            warm_up.figures.us = Some(micros(now - warm_up.started));
        }
        (warm_up.figures, !running)
    }

    /// Keep the compile of `id`, evicting the least recently asked for or warmed compiled sequence
    /// when the cache is full: whether a frame waits for it, and the running warm-up's figures
    /// when this compile changed them — its open part done, or its queue drained.
    fn finish(&mut self, id: u64, state: State) -> (bool, Option<WarmUpFigures>) {
        if self.compiling == Some(id) {
            self.compiling = None;
        }
        let drained = self.queue.is_empty();
        let warm_up = self
            .warm_up
            .as_mut()
            .filter(|warm_up| warm_up.figures.running())
            .and_then(|warm_up| {
                let open = warm_up.open_after == Some(id) || drained;
                let changed = open && warm_up.figures.open_us.is_none();
                if changed {
                    warm_up.figures.open_us = Some(micros(warm_up.handed.elapsed()));
                }
                if drained {
                    warm_up.figures.us = Some(micros(warm_up.started.elapsed()));
                }
                (changed || drained).then_some(warm_up.figures)
            });
        (self.keep(id, state), warm_up)
    }

    /// [`Self::finish`]'s keeping of the compile of `id`; `true` when a frame waits for it.
    fn keep(&mut self, id: u64, state: State) -> bool {
        let compiled = self
            .entries
            .iter()
            .filter(|entry| entry.state.compiled())
            .count();
        if compiled >= PIPELINE_CACHE
            && let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.state.compiled())
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
        {
            self.entries.swap_remove(oldest);
        }
        match self.entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => {
                entry.state = state;
                std::mem::take(&mut entry.asked)
            }
            None => false,
        }
    }
}

/// The compile thread's side of the pipeline: what it shares with the UI thread, and the channel
/// that wakes it when a sequence is queued.
struct Worker {
    shared: Arc<Mutex<Shared>>,
    wake: Sender<()>,
}

impl Worker {
    /// The thread, with a clone of `device` and the stage's shared [`Support`]: its layouts and
    /// the spatial passes' cache, which the one `compile` reads and fills. It compiles the queue in
    /// order, sleeps on its channel while the queue is empty, and ends when the pipeline that owns
    /// it is dropped.
    fn spawn(device: &wgpu::Device, support: &Arc<Support>, figures: &Arc<CompileFigures>) -> Self {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let (wake, wakes) = channel::<()>();
        let thread = Arc::clone(&shared);
        let device = device.clone();
        let support = Arc::clone(support);
        let figures = Arc::clone(figures);
        std::thread::Builder::new()
            .name("luxforge-gpu-compile".into())
            .spawn(move || {
                loop {
                    let next = {
                        let mut shared = lock_shared(&thread);
                        if shared.closed {
                            return;
                        }
                        shared.next()
                    };
                    let Some((id, steps, format)) = next else {
                        // Asleep until a sequence is queued, or the pipeline is gone.
                        if wakes.recv().is_err() {
                            return;
                        }
                        continue;
                    };
                    let started = Instant::now();
                    let state = match compile(&device, &support, &steps, format) {
                        Ok(pipeline) => State::Ready(pipeline),
                        Err(error) => State::Failed(Arc::from(error)),
                    };
                    let elapsed = started.elapsed();
                    let (asked, warm_up) = lock_shared(&thread).finish(id, state);
                    // Counted once its pipeline is kept, so nothing waits on a sequence the
                    // figures call done.
                    figures.finished(elapsed);
                    let ended = warm_up.is_some_and(|warm_up| !warm_up.running());
                    if let Some(warm_up) = warm_up {
                        figures.warm_up(warm_up);
                    }
                    if asked || ended {
                        wake_surface();
                    }
                }
            })
            .expect("the GPU preview compile thread starts");
        Self { shared, wake }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        lock_shared(&self.shared)
    }

    /// Wake the thread to take what was queued; it may already be awake.
    fn notify(&self) {
        let _ = self.wake.send(());
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread ends at its next turn, or at once from its sleep when the channel closes.
        self.lock().closed = true;
        let _ = self.wake.send(());
    }
}

fn lock_shared(lock: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stage's cached pipelines and the thread that compiles them.
#[derive(Default)]
pub(super) struct Pipelines {
    worker: Option<Worker>,
}

impl Pipelines {
    fn worker(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        figures: &Figures,
    ) -> &Worker {
        self.worker
            .get_or_insert_with(|| Worker::spawn(device, support, &figures.compile))
    }

    /// The ready pipeline for `steps` writing `format` and its identity, or why there is none to
    /// draw with yet: a sequence still waiting or compiling, or first seen now and queued first for
    /// the compile thread, answers [`GpuFallback::Compiling`]; a failed one
    /// [`GpuFallback::PipelineFailed`]. Never compiles.
    pub(super) fn get(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        steps: &[GpuStep],
        format: wgpu::TextureFormat,
        figures: &Figures,
    ) -> Result<(Compiled, u64), GpuFallback> {
        let worker = self.worker(device, support, figures);
        let mut shared = worker.lock();
        shared.clock += 1;
        let clock = shared.clock;
        let known = shared
            .entries
            .iter()
            .position(|entry| entry.format == format && matches(&entry.signature, steps));
        if let Some(at) = known {
            let entry = &mut shared.entries[at];
            entry.used = clock;
            let id = entry.id;
            match &entry.state {
                State::Ready(pipeline) => return Ok((pipeline.clone(), id)),
                State::Failed(_) => return Err(GpuFallback::PipelineFailed),
                State::Compiling => entry.asked = true,
                State::Queued(_) => {
                    // Asked for now: ahead of every warmed sequence.
                    entry.asked = true;
                    shared.queue.retain(|queued| *queued != id);
                    shared.queue.push_front(id);
                }
            }
            return Err(GpuFallback::Compiling);
        }
        let dropped = shared.dropped;
        if !shared.queue(steps, format, true) {
            return Err(GpuFallback::Compiling);
        }
        // Counted under the lock, before the thread can take it and count its end.
        figures.compile.queued(1);
        figures.compile.leave(shared.dropped - dropped);
        drop(shared);
        figures.compiles.fetch_add(1, Ordering::Relaxed);
        worker.notify();
        Err(GpuFallback::Compiling)
    }

    /// Queue every sequence of `sequences`, warm list `version`'s, this pipeline does not know,
    /// after any a frame asked for, in the list's order — its first `open` the open stack's —
    /// while the queue has room, so a gesture that needs one later finds it ready. The warmed
    /// sequences an older list left waiting are let go first, so this list's are taken next. One
    /// it knows is wanted again: it counts as used now, so a compile that ends evicts what an
    /// older warm list named before it. Begins a warm-up, or carries on the one running, and wakes
    /// the surface when one begins or ends here.
    pub(super) fn warm(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        sequences: &[Sequence],
        (version, open): (u64, usize),
        figures: &Figures,
    ) {
        let worker = self.worker(device, support, figures);
        let mut shared = worker.lock();
        let dropped = shared.unqueue_warmed();
        let (mut queued, mut open_queued, mut open_after) = (0, None, None);
        for (index, (steps, format)) in sequences.iter().enumerate() {
            if index == open {
                open_after = shared.queue.back().copied().or(shared.compiling);
                open_queued = Some(queued);
            }
            if steps.is_empty() || shared.warmed_again(steps, *format) {
                continue;
            }
            if !shared.queue(steps, *format, false) {
                break;
            }
            queued += 1;
        }
        let open_queued = open_queued.unwrap_or_else(|| {
            open_after = shared.queue.back().copied().or(shared.compiling);
            queued
        });
        let (warm_up, began) = shared.warm_up(version, queued, open_queued, open_after);
        // Counted under the lock, before the thread can take one and count its end.
        figures.compile.leave(dropped);
        figures.compile.queued(queued);
        figures.compile.warm_up(warm_up);
        drop(shared);
        figures.compiles.fetch_add(queued, Ordering::Relaxed);
        if queued > 0 {
            worker.notify();
        }
        if began || !warm_up.running() {
            wake_surface();
        }
    }

    /// The sequences' first entries in the order the compile thread took them.
    #[cfg(test)]
    pub(super) fn taken(&self) -> Vec<String> {
        self.worker
            .as_ref()
            .map_or_else(Vec::new, |worker| worker.lock().taken.clone())
    }

    /// The known entry for `steps` writing `format`, read under the lock.
    #[cfg(test)]
    fn read<T>(
        &self,
        steps: &[GpuStep],
        format: wgpu::TextureFormat,
        read: impl FnOnce(&State) -> T,
    ) -> Option<T> {
        let mut shared = self.worker.as_ref()?.lock();
        shared.find(steps, format).map(|entry| read(&entry.state))
    }

    /// Whether `steps` writing `format` is waiting for the compile thread or compiling.
    #[cfg(test)]
    pub(super) fn compiling(&self, steps: &[GpuStep], format: wgpu::TextureFormat) -> bool {
        self.read(steps, format, |state| !state.compiled())
            .unwrap_or(false)
    }

    /// Why `steps` writing `format` failed, when it did.
    #[cfg(test)]
    pub(super) fn failure(
        &self,
        steps: &[GpuStep],
        format: wgpu::TextureFormat,
    ) -> Option<Arc<str>> {
        self.read(steps, format, |state| match state {
            State::Failed(error) => Some(error.clone()),
            _ => None,
        })
        .flatten()
    }

    /// How many compiled sequences are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.worker.as_ref().map_or(0, |worker| {
            worker
                .lock()
                .entries
                .iter()
                .filter(|entry| entry.state.compiled())
                .count()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::GpuProgram;
    use super::*;

    fn steps(entry: &'static str) -> Vec<GpuStep> {
        vec![GpuStep::colour(GpuProgram::new(entry, ""))]
    }

    /// The entries of the queue, front first.
    fn order(shared: &Shared) -> Vec<String> {
        shared
            .queue
            .iter()
            .map(|id| {
                let entry = shared.entries.iter().find(|entry| entry.id == *id).unwrap();
                entry.signature[0].1.clone()
            })
            .collect()
    }

    /// A sequence a frame asks for is queued ahead of every warmed one; when the queue is full a
    /// warmed one is refused and an asked one takes the place of the newest warmed one waiting.
    #[test]
    fn a_frames_sequence_is_queued_ahead_of_the_warm_list() {
        let mut shared = Shared::default();
        let warmed: Vec<&'static str> = (0..PIPELINE_CACHE - 1)
            .map(|index| &*Box::leak(format!("warmed_{index}").into_boxed_str()))
            .collect();
        let format = super::super::OUTPUT_FORMAT;
        for entry in &warmed {
            assert!(shared.queue(&steps(entry), format, false));
        }
        assert!(shared.queue(&steps("asked"), format, true));
        assert_eq!(order(&shared)[0], "asked");
        assert_eq!(shared.queue.len(), PIPELINE_CACHE);
        assert!(
            !shared.queue(&steps("refused"), format, false),
            "a full queue warms no more"
        );
        assert_eq!(shared.dropped, 0);
        assert!(shared.queue(&steps("asked_again"), format, true));
        assert_eq!(shared.dropped, 1, "counted, so no wait counts on it");
        let order = order(&shared);
        assert_eq!(order[..2], ["asked_again", "asked"]);
        assert_eq!(order.len(), PIPELINE_CACHE);
        assert!(
            !order.contains(&warmed[PIPELINE_CACHE - 2].to_owned()),
            "the newest warmed one gave its place"
        );
    }

    /// What waits on the compile thread is counted exactly: a queued sequence adds one, a finished
    /// one or a warmed one dropped from a full queue takes one away, and it never goes below zero.
    #[test]
    fn the_pending_figure_counts_what_waits() {
        let figures = CompileFigures::default();
        figures.queued(3);
        figures.finished(std::time::Duration::from_millis(1));
        figures.leave(1);
        assert_eq!(figures.pending.load(Ordering::Acquire), 1);
        figures.leave(5);
        assert_eq!(figures.pending.load(Ordering::Acquire), 0);
    }

    /// A warm list that names a compiled sequence again keeps it: once the cache is full, the
    /// compile that ends next evicts the oldest sequence no list named since, not the one named
    /// again.
    #[test]
    fn a_sequence_warmed_again_outlives_one_an_older_list_warmed() {
        let mut shared = Shared::default();
        let format = super::super::OUTPUT_FORMAT;
        let named: Vec<&'static str> = (0..=PIPELINE_CACHE)
            .map(|index| &*Box::leak(format!("cached_{index}").into_boxed_str()))
            .collect();
        let compiled = |shared: &mut Shared, entry: &'static str| {
            assert!(shared.queue(&steps(entry), format, false));
            let (id, ..) = shared.next().expect("queued");
            shared.finish(id, State::Failed(Arc::from("kept")));
        };
        for entry in &named[..PIPELINE_CACHE] {
            compiled(&mut shared, entry);
        }
        assert!(shared.warmed_again(&steps(named[0]), format));
        assert!(!shared.warmed_again(&steps("unknown"), format));
        compiled(&mut shared, named[PIPELINE_CACHE]);
        let kept = |entry: &str| {
            shared
                .entries
                .iter()
                .any(|kept| kept.signature[0].1 == entry)
        };
        assert_eq!(shared.entries.len(), PIPELINE_CACHE);
        assert!(kept(named[0]), "warmed again, so kept");
        assert!(!kept(named[1]), "the oldest no list named since");
        assert!(kept(named[PIPELINE_CACHE]));
    }

    /// A new warm list lets go of the warmed sequences an older one left waiting, so its own are
    /// taken next, and keeps every sequence a frame asked for.
    #[test]
    fn a_new_warm_list_lets_go_of_an_older_lists_waiting_sequences() {
        let mut shared = Shared::default();
        let format = super::super::OUTPUT_FORMAT;
        for entry in ["older_1", "older_2"] {
            assert!(shared.queue(&steps(entry), format, false));
        }
        assert!(shared.queue(&steps("asked"), format, true));
        assert_eq!(shared.unqueue_warmed(), 2);
        assert_eq!(order(&shared), ["asked"]);
        assert_eq!(
            shared.entries.len(),
            1,
            "forgotten, so a newer list queues them again"
        );
        assert!(shared.queue(&steps("older_2"), format, false));
        assert_eq!(order(&shared), ["asked", "older_2"]);
    }

    /// A warm-up runs from its list until the queue drains, a frame's sequence taken before it
    /// included: its open part is done when the last sequence waiting as that part was queued
    /// compiles, and it ends when nothing waits. A list handed to an idle thread with nothing to
    /// compile is a warm-up of its own that ends at once.
    #[test]
    fn a_warm_up_runs_from_its_list_until_the_queue_drains() {
        let mut shared = Shared::default();
        let format = super::super::OUTPUT_FORMAT;
        assert!(shared.queue(&steps("picture"), format, true));
        assert!(shared.queue(&steps("open"), format, false));
        let open = shared.queue.back().copied();
        assert!(shared.queue(&steps("rest"), format, false));
        let (figures, began) = shared.warm_up(7, 2, 1, open);
        assert!(began && figures.running());
        assert_eq!((figures.period, figures.version), (1, 7));
        let finish = |shared: &mut Shared| {
            let (id, ..) = shared.next().expect("queued");
            shared.finish(id, State::Failed(Arc::from("kept"))).1
        };
        assert_eq!(finish(&mut shared), None, "the picture: neither part done");
        let opened = finish(&mut shared).expect("the open part done");
        assert!(opened.open_us.is_some() && opened.running());
        let ended = finish(&mut shared).expect("the queue drained");
        assert!(!ended.running());
        assert_eq!(ended.open_us, opened.open_us);
        let (idle, began) = shared.warm_up(8, 0, 0, None);
        assert!(began, "a warm-up of its own");
        assert_eq!((idle.period, idle.open_us), (2, Some(0)));
        assert!(!idle.running(), "ended at once");
    }

    /// A full queue of requested sequences rejects another request and keeps both bounds intact.
    #[test]
    fn requested_sequences_cannot_grow_the_full_queue() {
        let mut shared = Shared::default();
        let format = super::super::OUTPUT_FORMAT;
        for index in 0..PIPELINE_CACHE {
            let entry = Box::leak(format!("asked_{index}").into_boxed_str());
            assert!(shared.queue(&steps(entry), format, true));
        }
        assert_eq!(shared.queue.len(), PIPELINE_CACHE);
        assert_eq!(shared.entries.len(), PIPELINE_CACHE);
        assert!(!shared.queue(&steps("deferred"), format, true));
        assert_eq!(shared.queue.len(), PIPELINE_CACHE);
        assert_eq!(shared.entries.len(), PIPELINE_CACHE);
        assert!(shared.entries.iter().all(|entry| entry.asked));
    }
}
