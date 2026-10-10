//! Consented, low-frequency `process.sample` events: CPU, memory and, where the platform reports
//! them, GPU use of this process, at most one per [`INTERVAL`] of presented activity.
//!
//! The [`Sampler`] is a state machine the statistics worker drives. It owns no thread and no timer:
//! the worker calls [`Sampler::step`] when a control message arrives (consent, visibility, idle,
//! adapter change) and when the deadline the last step returned ([`Next::At`]) passes, and blocks
//! on its own channel otherwise. [`Next::Parked`] means no deadline at all: collection is off, the
//! window is hidden or minimized, or the person is idle. Nothing here wakes the GUI.
//!
//! - **Baseline.** CPU and GPU time are cumulative, so a rate needs two readings. The first reading
//!   after the sampler becomes active is a baseline and emits nothing; a sample is taken
//!   [`INTERVAL`] later and becomes the next baseline. A sample therefore covers one uninterrupted
//!   minute of presented activity, and a hidden or idle period is never inside a rate.
//! - **Resets.** The baseline is dropped (and the next active step takes a new one) when the window
//!   is hidden or the person goes idle ([`Sampler::set_presented`], [`Sampler::set_idle`]); when
//!   consent ends or a new consent generation begins; when the device profile changes
//!   ([`Sampler::profile_changed`], so a rate never spans two adapters and nothing observed on one
//!   is attributed to the other); and when the machine slept. macOS's monotonic clock does not
//!   advance while it sleeps, so a sleep is found by comparing how far the wall clock and the
//!   monotonic clock moved since the baseline: more than [`SLEEP_SLOP`] apart is a sleep or a
//!   clock change, and the window is not backfilled.
//! - **Invalid readings are dropped, never zeroed.** A counter that is unavailable, went backwards
//!   (a reset) or sits in a window shorter than [`MIN_WINDOW`] or longer than [`MAX_WINDOW`] is
//!   omitted. A sample without a valid CPU rate or a memory figure is not recorded at all; GPU
//!   time and GPU memory are optional fields. Unavailability reasons are never reported.
//! - **No duplicated system calls.** A reading comes from [`Source::fetch`] with a freshness
//!   allowance of [`FRESH_FOR`]. The production source ([`ProcessCounters`]) is
//!   `luxforge_core::resources::reading`, which shares one process-wide sampler and remembers the
//!   newest read by any consumer: when the Performance panel (expanded, one read a second) has
//!   just read, the sampler uses that reading and makes no system call; otherwise it reads through
//!   the same sampler, on the calling worker, and the panel can reuse that read in turn.
use super::{
    event::{Collector, EventKind, MemoryMib, Percent, ProcessSample},
    gate::Generation,
};
use crate::resources::{self, Fetched, Reading};
use std::{
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Presented activity between one sample and the next.
pub const INTERVAL: Duration = Duration::from_secs(60);

/// How old a reading from another consumer may be and still stand for "now": twice the
/// Performance panel's one-second cadence.
pub const FRESH_FOR: Duration = Duration::from_secs(2);

/// How far the wall clock and the monotonic clock may drift apart over a window before it is
/// treated as a sleep or a clock change.
pub const SLEEP_SLOP: Duration = Duration::from_secs(5);

/// The shortest and longest span between two readings a rate is computed over. The span is the
/// readings' own, which a reused reading shifts by up to [`FRESH_FOR`].
pub const MIN_WINDOW: Duration = Duration::from_secs(30);
pub const MAX_WINDOW: Duration = Duration::from_secs(180);

/// A moment on both clocks. The monotonic part orders the sampler's schedule; the wall part exists
/// only to notice that the monotonic clock stood still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// Since an arbitrary fixed origin (the first call in this process), not counting a sleep on
    /// platforms whose monotonic clock stops.
    pub monotonic: Duration,
    /// Since the Unix epoch; zero if the clock reads before it.
    pub wall: Duration,
}

impl Stamp {
    pub fn now() -> Self {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        Self {
            monotonic: ORIGIN.get_or_init(Instant::now).elapsed(),
            wall: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default(),
        }
    }

    /// How far apart the two clocks moved between `earlier` and `self`.
    fn drift_since(&self, earlier: &Stamp) -> Duration {
        let monotonic = self.monotonic.saturating_sub(earlier.monotonic);
        let wall = self.wall.saturating_sub(earlier.wall);
        monotonic.abs_diff(wall)
    }
}

/// Where readings come from: a seam so tests count reads and control freshness.
pub trait Source {
    /// The counters now, or a reading at most `max_age` old.
    fn fetch(&mut self, max_age: Duration) -> Fetched;
}

/// The process-wide resource sampler the Performance panel also reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCounters;

impl Source for ProcessCounters {
    fn fetch(&mut self, max_age: Duration) -> Fetched {
        resources::reading(max_age)
    }
}

/// What the worker does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// No deadline: wait for a control message. Collection is off, the window is hidden or the
    /// person is idle.
    Parked,
    /// Call [`Sampler::step`] again at this monotonic time ([`Stamp::monotonic`]), or earlier on a
    /// control message.
    At(Duration),
}

/// Counts for local diagnostics and tests; never uploaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Readings taken from the source, and how many of them were another consumer's.
    pub readings: u64,
    pub reused: u64,
    /// Samples recorded, and windows whose rate was dropped as invalid.
    pub samples: u64,
    pub dropped: u64,
    /// Baselines abandoned for a sleep or clock change.
    pub sleeps: u64,
}

#[derive(Clone, Copy, Debug)]
struct Baseline {
    reading: Reading,
    at: Stamp,
    /// The monotonic time the next sample is due.
    due: Duration,
}

/// The process sampler. See the [module documentation](self).
#[derive(Debug)]
pub struct Sampler<S = ProcessCounters> {
    source: S,
    presented: bool,
    idle: bool,
    generation: Option<Generation>,
    baseline: Option<Baseline>,
    stats: Stats,
}

impl Sampler<ProcessCounters> {
    /// A sampler on the process-wide resource sampler, not presented until told so.
    pub fn new() -> Self {
        Self::with_source(ProcessCounters)
    }
}

impl Default for Sampler<ProcessCounters> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Source> Sampler<S> {
    pub fn with_source(source: S) -> Self {
        Self {
            source,
            presented: false,
            idle: false,
            generation: None,
            baseline: None,
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The window became visible (`true`) or hidden or minimized (`false`). Hiding drops the
    /// baseline now; showing needs a [`Self::step`] to take a new one.
    pub fn set_presented(&mut self, presented: bool) {
        if self.presented != presented {
            self.presented = presented;
            self.baseline = None;
        }
    }

    /// The person went idle (`true`) or became active again (`false`).
    pub fn set_idle(&mut self, idle: bool) {
        if self.idle != idle {
            self.idle = idle;
            self.baseline = None;
        }
    }

    /// The device profile changed (an adapter or renderer-path change): the window being measured
    /// belongs to the old profile, so it is dropped and a new baseline is taken at the next step.
    pub fn profile_changed(&mut self) {
        self.baseline = None;
    }

    fn active(&self) -> bool {
        self.presented && !self.idle
    }

    /// Do whatever is due at `now`: take a baseline, record a sample into `collector`'s current
    /// session, or drop a window that is no longer valid. Blocking only in the source's system
    /// calls; call it from the statistics worker, never the UI or catalog owner thread.
    pub fn step(&mut self, collector: &Collector, now: Stamp) -> Next {
        let Some(generation) = collector.gate().current() else {
            self.generation = None;
            self.baseline = None;
            return Next::Parked;
        };
        if self.generation != Some(generation) {
            // A new consent period shares nothing with the last one.
            self.generation = Some(generation);
            self.baseline = None;
        }
        if !self.active() {
            self.baseline = None;
            return Next::Parked;
        }
        if let Some(base) = self.baseline
            && now.drift_since(&base.at) > SLEEP_SLOP
        {
            self.stats.sleeps += 1;
            self.baseline = None;
        }
        match self.baseline {
            None => Next::At(self.take_baseline(now)),
            Some(base) if now.monotonic < base.due => Next::At(base.due),
            Some(base) => {
                let current = self.read();
                match rate(&base.reading, &current) {
                    Some(sample) => {
                        collector.record_in(generation, EventKind::ProcessSample(sample));
                        self.stats.samples += 1;
                    }
                    None => self.stats.dropped += 1,
                }
                self.baseline = Some(Baseline {
                    reading: current,
                    at: now,
                    due: now.monotonic + INTERVAL,
                });
                Next::At(now.monotonic + INTERVAL)
            }
        }
    }

    fn take_baseline(&mut self, now: Stamp) -> Duration {
        let reading = self.read();
        let due = now.monotonic + INTERVAL;
        self.baseline = Some(Baseline {
            reading,
            at: now,
            due,
        });
        due
    }

    fn read(&mut self) -> Reading {
        let fetched = self.source.fetch(FRESH_FOR);
        self.stats.readings += 1;
        if fetched.reused {
            self.stats.reused += 1;
        }
        fetched.reading
    }
}

/// The sample over the window from `earlier` to `current`, if every required figure in it is
/// valid: CPU as percent of one core from the CPU-time delta over the monotonic delta, memory as
/// the current level in the platform's own kind, and (each optional, each independently valid) GPU
/// time as the same kind of rate and the GPU's allocated bytes.
pub fn rate(earlier: &Reading, current: &Reading) -> Option<ProcessSample> {
    let window = current
        .monotonic_ns
        .checked_sub(earlier.monotonic_ns)
        .filter(|ns| (nanos(MIN_WINDOW)..=nanos(MAX_WINDOW)).contains(ns))?;
    let percent = |before: Option<u64>, after: Option<u64>| {
        let delta = after?.checked_sub(before?)?;
        Percent::from_f64(100.0 * delta as f64 / window as f64)
    };
    Some(ProcessSample {
        cpu: percent(earlier.cpu_time_ns, current.cpu_time_ns)?,
        memory: MemoryMib::from_bytes(current.memory_bytes?),
        memory_kind: current.memory_kind.into(),
        gpu: percent(earlier.gpu_time_ns, current.gpu_time_ns),
        gpu_memory: current.gpu_allocated_bytes.map(MemoryMib::from_bytes),
    })
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "sampler_tests.rs"]
mod tests;
