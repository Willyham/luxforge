//! The GPU stage's pipelines, compiled off the UI thread.
//!
//! A pipeline is kept per program sequence — each step's kind, entry and source — never per image,
//! size or uniform value, so a sequence compiles at most once per process while it stays cached.
//! Compiling is the one expensive thing the stage does (`naga` validates the assembled WGSL, the
//! backend translates it and the driver builds the pipeline), so it never runs on the UI thread:
//!
//! - **A first-seen sequence** is handed to one compile thread, which holds a clone of the device
//!   and the stage's shared support — its layouts and the spatial passes' cache — and compiles
//!   through the stage's one `compile`. The frame that asked draws the CPU frame and names why
//!   ([`GpuFallback::Compiling`]). When the compile ends the thread wakes the surface, and the
//!   next `prepare` finds the pipeline ready, or failed, and keeps it so.
//! - **Warming.** The desktop names the sequences a gesture is likely to need when the stack
//!   changes ([`super::super::PhotoSurface::gpu_warm`]), so they compile before a drag begins.
//! - **Bounds.** At most [`PIPELINE_CACHE`] sequences are kept, compiling, ready and failed ones
//!   together, the least recently asked for evicted first and a compiling one never. A sequence
//!   that cannot find room is asked for again by the next frame that needs it. The compile queue
//!   therefore holds at most that many sequences, and the thread sleeps on its channel when idle.
//!
//! The compile thread takes wgpu's error scopes around the pipeline it creates. wgpu 27's scopes
//! belong to the device, not to a thread, and the UI thread pushes none of its own, so the stack
//! stays balanced; an error the UI thread raised while a compile's scope is open would be counted
//! as that compile's failure, which takes the CPU path for that sequence and nothing worse.
use super::{Compiled, Figures, GpuFallback, GpuStep, OUTPUT_FORMAT, StepKind, Support, compile};
use crate::photo_surface::wake_surface;
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::Ordering,
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};

/// How many program sequences, compiling, ready or failed, a pipeline keeps.
pub const PIPELINE_CACHE: usize = 16;

/// The program sequences a gesture is likely to need, which the desktop names when the stack
/// changes so they compile before a drag begins ([`super::super::PhotoSurface::gpu_warm`]). Each
/// sequence is a plan's steps; only their kinds and programs matter, never their words. A new
/// `version` is warmed once, so handing the same one to every frame costs nothing.
#[derive(Clone, Debug)]
pub struct GpuWarm {
    version: u64,
    sequences: Arc<[Vec<GpuStep>]>,
}

impl GpuWarm {
    pub fn new(version: u64, sequences: Vec<Vec<GpuStep>>) -> Self {
        Self {
            version,
            sequences: sequences.into(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn sequences(&self) -> &[Vec<GpuStep>] {
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

/// Where one cached sequence is.
enum State {
    /// On the compile thread.
    Compiling,
    /// One pipeline per pass.
    Ready(Compiled),
    /// Refused, with why: kept, so it is not compiled again every frame. The reason is read by the
    /// tests that name each failure.
    Failed(#[cfg_attr(not(test), allow(dead_code))] Arc<str>),
}

struct Entry {
    signature: Signature,
    state: State,
    /// The identity of this compile, which a finished one is matched back to.
    id: u64,
    used: u64,
}

/// One finished compile, from the thread.
struct Done {
    id: u64,
    result: Result<Compiled, String>,
    elapsed: Duration,
}

/// The compile thread's two channels.
struct Worker {
    jobs: Sender<(u64, Vec<GpuStep>)>,
    /// Behind a lock only so the pipeline is `Sync`, as Iced requires: only `prepare` reads it.
    done: Mutex<Receiver<Done>>,
}

impl Worker {
    /// The thread, with a clone of `device` and the stage's shared [`Support`]: its layouts and
    /// the spatial passes' cache, which the one `compile` reads and fills. It sleeps on its
    /// channel while nothing is asked, and ends when the pipeline that owns it is dropped.
    fn spawn(device: &wgpu::Device, support: &Arc<Support>) -> Self {
        let (jobs, requests) = channel::<(u64, Vec<GpuStep>)>();
        let (finished, done) = channel();
        let device = device.clone();
        let support = Arc::clone(support);
        std::thread::Builder::new()
            .name("luxforge-gpu-compile".into())
            .spawn(move || {
                while let Ok((id, steps)) = requests.recv() {
                    let started = Instant::now();
                    let result = compile(&device, &support, &steps, OUTPUT_FORMAT);
                    let done = Done {
                        id,
                        result,
                        elapsed: started.elapsed(),
                    };
                    if finished.send(done).is_err() {
                        break;
                    }
                    wake_surface();
                }
            })
            .expect("the GPU preview compile thread starts");
        Self {
            jobs,
            done: Mutex::new(done),
        }
    }
}

/// The stage's cached pipelines and the thread that compiles them.
#[derive(Default)]
pub(super) struct Pipelines {
    entries: Vec<Entry>,
    clock: u64,
    worker: Option<Worker>,
}

impl Pipelines {
    /// The ready pipeline for `steps` and its identity, or why there is none to draw with yet: a
    /// sequence still compiling, or first seen now and handed to the compile thread, answers
    /// [`GpuFallback::Compiling`]; a failed one [`GpuFallback::PipelineFailed`]. Never compiles.
    pub(super) fn get(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        steps: &[GpuStep],
        figures: &Figures,
    ) -> Result<(Compiled, u64), GpuFallback> {
        self.collect(figures);
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| matches(&entry.signature, steps))
        {
            entry.used = clock;
            return match &entry.state {
                State::Ready(pipeline) => Ok((pipeline.clone(), entry.id)),
                State::Failed(_) => Err(GpuFallback::PipelineFailed),
                State::Compiling => Err(GpuFallback::Compiling),
            };
        }
        self.request(device, support, steps, figures);
        Err(GpuFallback::Compiling)
    }

    /// Hand every sequence of `sequences` this pipeline does not hold to the compile thread, while
    /// there is room, so a gesture that needs one later finds it ready.
    pub(super) fn warm(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        sequences: &[Vec<GpuStep>],
        figures: &Figures,
    ) {
        self.collect(figures);
        for steps in sequences {
            if steps.is_empty()
                || self
                    .entries
                    .iter()
                    .any(|entry| matches(&entry.signature, steps))
            {
                continue;
            }
            self.clock += 1;
            if !self.request(device, support, steps, figures) {
                break;
            }
        }
    }

    /// Start compiling `steps`, making room by evicting the least recently used sequence that is
    /// not compiling. `false` when every kept sequence is still compiling.
    fn request(
        &mut self,
        device: &wgpu::Device,
        support: &Arc<Support>,
        steps: &[GpuStep],
        figures: &Figures,
    ) -> bool {
        if self.entries.len() >= PIPELINE_CACHE {
            let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !matches!(entry.state, State::Compiling))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
            else {
                return false;
            };
            self.entries.swap_remove(oldest);
        }
        let worker = self
            .worker
            .get_or_insert_with(|| Worker::spawn(device, support));
        let id = self.clock;
        if worker.jobs.send((id, steps.to_vec())).is_err() {
            return false;
        }
        figures.compiles.fetch_add(1, Ordering::Relaxed);
        self.entries.push(Entry {
            signature: signature(steps),
            state: State::Compiling,
            id,
            used: id,
        });
        true
    }

    /// Take up every compile the thread has finished.
    fn collect(&mut self, figures: &Figures) {
        let Some(worker) = &self.worker else {
            return;
        };
        let done = worker.done.lock().unwrap_or_else(PoisonError::into_inner);
        while let Ok(done) = done.try_recv() {
            figures.compiled(done.elapsed);
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == done.id) {
                entry.state = match done.result {
                    Ok(pipeline) => State::Ready(pipeline),
                    Err(error) => State::Failed(Arc::from(error)),
                };
            }
        }
    }

    /// Whether `steps` is compiling.
    #[cfg(test)]
    pub(super) fn compiling(&self, steps: &[GpuStep]) -> bool {
        self.entries.iter().any(|entry| {
            matches(&entry.signature, steps) && matches!(entry.state, State::Compiling)
        })
    }

    /// Why `steps` failed, when it did.
    #[cfg(test)]
    pub(super) fn failure(&self, steps: &[GpuStep]) -> Option<Arc<str>> {
        self.entries
            .iter()
            .find(|entry| matches(&entry.signature, steps))
            .and_then(|entry| match &entry.state {
                State::Failed(error) => Some(error.clone()),
                _ => None,
            })
    }

    /// How many sequences are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Take up every finished compile now, as the next `prepare` would.
    #[cfg(test)]
    pub(super) fn settle(&mut self, figures: &Figures) {
        self.collect(figures);
    }
}
