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
//!   changes ([`super::super::PhotoSurface::gpu_warm`]), so they compile before a drag begins.
//! - **Bounds.** At most [`PIPELINE_CACHE`] compiled sequences are kept, ready and failed ones
//!   together, the least recently asked for evicted first when a compile ends, and at most as many
//!   wait in the queue; a warmed sequence that finds the queue full is not queued, and one a frame
//!   asks for takes the place of the newest warmed one waiting.
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
pub const PIPELINE_CACHE: usize = 16;

/// The program sequences a gesture is likely to need, which the desktop names when the stack
/// changes so they compile before a drag begins ([`super::super::PhotoSurface::gpu_warm`]). Each
/// sequence is a plan's steps and the format its boundary will be held in, which its chain's
/// intermediates take ([`super::chain`]); only the steps' kinds and programs matter, never their
/// words. A new `version` is warmed once, so handing the same one to every frame costs nothing.
#[derive(Clone, Debug)]
pub struct GpuWarm {
    version: u64,
    sequences: Arc<[(Vec<GpuStep>, BoundaryFormat)]>,
}

impl GpuWarm {
    pub fn new(version: u64, sequences: Vec<(Vec<GpuStep>, BoundaryFormat)>) -> Self {
        Self {
            version,
            sequences: sequences.into(),
        }
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
type Signature = Vec<(StepKind, String, String)>;

fn signature(steps: &[GpuStep]) -> Signature {
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

/// What the UI thread and the compile thread share: every known sequence, and the queue of those
/// waiting, in compile order.
#[derive(Default)]
struct Shared {
    entries: Vec<Entry>,
    queue: VecDeque<u64>,
    clock: u64,
    /// The pipeline that owns the thread is gone: the thread ends.
    closed: bool,
}

impl Shared {
    fn find(&mut self, steps: &[GpuStep], format: wgpu::TextureFormat) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.format == format && matches(&entry.signature, steps))
    }

    /// Queue `steps` writing `format` under a new identity, first when a frame `asked` for them
    /// and last when they are warmed. `false` when the queue has no room for a warmed sequence.
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
                return Some((id, steps, entry.format));
            }
        }
        None
    }

    /// Keep the compile of `id`, evicting the least recently asked for compiled sequence when the
    /// cache is full; `true` when a frame waits for it.
    fn finish(&mut self, id: u64, state: State) -> bool {
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
                    figures.finished(started.elapsed());
                    let asked = lock_shared(&thread).finish(id, state);
                    if asked {
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
        shared.queue(steps, format, true);
        drop(shared);
        figures.compiles.fetch_add(1, Ordering::Relaxed);
        worker.notify();
        Err(GpuFallback::Compiling)
    }

    /// Queue every sequence of `sequences` this pipeline does not know, after any a frame asked
    /// for, while the queue has room, so a gesture that needs one later finds it ready.
    pub(super) fn warm(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        sequences: &[Sequence],
        figures: &Figures,
    ) {
        let worker = self.worker(device, support, figures);
        let mut shared = worker.lock();
        let mut queued = 0;
        for (steps, format) in sequences {
            if steps.is_empty() || shared.find(steps, *format).is_some() {
                continue;
            }
            if !shared.queue(steps, *format, false) {
                break;
            }
            queued += 1;
        }
        drop(shared);
        figures.compiles.fetch_add(queued, Ordering::Relaxed);
        if queued > 0 {
            worker.notify();
        }
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
        assert!(shared.queue(&steps("asked_again"), format, true));
        let order = order(&shared);
        assert_eq!(order[..2], ["asked_again", "asked"]);
        assert_eq!(order.len(), PIPELINE_CACHE);
        assert!(
            !order.contains(&warmed[PIPELINE_CACHE - 2].to_owned()),
            "the newest warmed one gave its place"
        );
    }
}
