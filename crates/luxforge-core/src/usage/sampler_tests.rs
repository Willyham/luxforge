//! The sampler against a fake clock and a fake counter source: independently computed rates, at
//! most one sample per minute of presented activity, nothing read while hidden, idle or off,
//! baselines reset by every interruption, a fresh panel reading reused, and invalid data omitted.
use super::*;
use crate::usage::{MemoryKind, Session, SessionClock, SessionStart, Trigger};
use luxforge_process::MemoryKind as Kind;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, atomic::AtomicU32, atomic::Ordering},
};

const GIB: u64 = 1 << 30;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn ns(s: f64) -> u64 {
    (s * 1e9) as u64
}

/// What the fake platform reports at monotonic time `t` seconds.
#[derive(Clone)]
struct Platform {
    /// CPU seconds per second, or unavailable.
    cpu: Option<f64>,
    gpu: Option<f64>,
    memory: Option<u64>,
    gpu_allocated: Option<u64>,
    kind: Kind,
    /// CPU seconds added in one jump (a counter reset is a negative adjustment on `offset`).
    cpu_offset: f64,
    gpu_offset: f64,
}

impl Default for Platform {
    fn default() -> Self {
        Self {
            cpu: Some(0.25),
            gpu: Some(0.10),
            memory: Some(3 * GIB),
            gpu_allocated: Some(512 << 20),
            kind: Kind::Footprint,
            cpu_offset: 100.0,
            gpu_offset: 50.0,
        }
    }
}

struct Fake {
    /// The harness's monotonic clock, in seconds.
    clock: Rc<Cell<f64>>,
    platform: Rc<RefCell<Platform>>,
    /// Real reads made (not reuses).
    reads: Rc<Cell<u32>>,
    /// The Performance panel's latest reading, shared like the process-wide one.
    panel: Rc<Cell<Option<Reading>>>,
    allowance: Rc<Cell<Option<Duration>>>,
}

impl Fake {
    fn reading_at(&self, t: f64) -> Reading {
        let p = self.platform.borrow();
        Reading {
            monotonic_ns: ns(t),
            cpu_time_ns: p.cpu.map(|rate| ns(p.cpu_offset + rate * t)),
            memory_kind: p.kind,
            memory_bytes: p.memory,
            gpu_time_ns: p.gpu.map(|rate| ns(p.gpu_offset + rate * t)),
            gpu_allocated_bytes: p.gpu_allocated,
        }
    }
}

impl Source for Fake {
    fn fetch(&mut self, max_age: Duration) -> Fetched {
        self.allowance.set(Some(max_age));
        let now = self.clock.get();
        if let Some(panel) = self.panel.get()
            && let Some(age) = ns(now).checked_sub(panel.monotonic_ns)
            && u128::from(age) <= max_age.as_nanos()
        {
            return Fetched {
                reading: panel,
                reused: true,
            };
        }
        self.reads.set(self.reads.get() + 1);
        let reading = self.reading_at(now);
        self.panel.set(Some(reading));
        Fetched {
            reading,
            reused: false,
        }
    }
}

/// A worker's view: the sampler, the collector and the session it records into, and the clocks.
struct Rig {
    sampler: Sampler<Fake>,
    collector: Collector,
    session: Arc<Session>,
    millis: Arc<AtomicU32>,
    clock: Rc<Cell<f64>>,
    wall_skew: Duration,
    platform: Rc<RefCell<Platform>>,
    reads: Rc<Cell<u32>>,
    panel: Rc<Cell<Option<Reading>>>,
    allowance: Rc<Cell<Option<Duration>>>,
    next: Next,
    seen: Vec<(u32, ProcessSample)>,
}

fn start() -> SessionStart {
    SessionStart::parse("2026-10-10T10:00:00Z").unwrap()
}

impl Rig {
    /// Collection on, the window presented and the person active, at time zero, with the baseline
    /// taken.
    fn active() -> Self {
        let mut rig = Self::inactive();
        rig.sampler.set_presented(true);
        rig.step();
        rig
    }

    /// Collection on, but the window not yet presented.
    fn inactive() -> Self {
        let clock = Rc::new(Cell::new(0.0));
        let platform = Rc::new(RefCell::new(Platform::default()));
        let reads = Rc::new(Cell::new(0));
        let panel = Rc::new(Cell::new(None));
        let allowance = Rc::new(Cell::new(None));
        let collector = Collector::new();
        let (session_clock, millis) = SessionClock::manual();
        let session = collector.start(Trigger::OptIn, start(), session_clock);
        let sampler = Sampler::with_source(Fake {
            clock: clock.clone(),
            platform: platform.clone(),
            reads: reads.clone(),
            panel: panel.clone(),
            allowance: allowance.clone(),
        });
        Self {
            sampler,
            collector,
            session,
            millis,
            clock,
            wall_skew: Duration::ZERO,
            platform,
            reads,
            panel,
            allowance,
            next: Next::Parked,
            seen: Vec::new(),
        }
    }

    fn stamp(&self) -> Stamp {
        let t = Duration::from_secs_f64(self.clock.get());
        Stamp {
            monotonic: t,
            wall: secs(1_800_000_000) + t + self.wall_skew,
        }
    }

    fn step(&mut self) -> Next {
        self.millis
            .store((self.clock.get() * 1000.0) as u32, Ordering::Release);
        self.next = self.sampler.step(&self.collector, self.stamp());
        self.collect();
        self.next
    }

    fn collect(&mut self) {
        if let Some(window) = self.session.detach() {
            for event in window.events() {
                if let EventKind::ProcessSample(sample) = event.kind {
                    self.seen.push((event.t, sample));
                }
            }
        }
    }

    /// Let time pass as the worker would live it: wake at each deadline up to `seconds` later, and
    /// at no other time. Time only moves forward to a deadline or to the end.
    fn run(&mut self, seconds: f64) {
        let end = self.clock.get() + seconds;
        while let Next::At(due) = self.next {
            if due.as_secs_f64() > end {
                break;
            }
            self.clock.set(due.as_secs_f64());
            self.step();
        }
        self.clock.set(end);
    }

    /// Time passes with no worker wake-up (hidden, idle, or off: there is no deadline).
    fn wait(&mut self, seconds: f64) {
        assert_eq!(self.next, Next::Parked, "a deadline would have woken it");
        self.clock.set(self.clock.get() + seconds);
    }
}

#[test]
fn usage_a_sample_is_cpu_gpu_and_memory_computed_independently() {
    let mut rig = Rig::active();
    assert!(rig.seen.is_empty(), "the first reading is only a baseline");
    assert_eq!(rig.reads.get(), 1);
    assert_eq!(rig.next, Next::At(INTERVAL));
    rig.run(60.0);
    assert_eq!(rig.seen.len(), 1);
    let (t, sample) = rig.seen[0];
    assert_eq!(t, 60_000);
    // 0.25 CPU-seconds per second over 60 s is 15 s of CPU: 100 * 15 / 60 = 25 percent of a core.
    assert_eq!(sample.cpu.get(), 25);
    // 6 s of GPU time over 60 s.
    assert_eq!(sample.gpu.map(Percent::get), Some(10));
    assert_eq!(sample.memory.get(), 3072);
    assert_eq!(sample.memory_kind, MemoryKind::Footprint);
    assert_eq!(sample.gpu_memory.map(MemoryMib::get), Some(512));
}

#[test]
fn usage_rates_come_from_deltas_between_readings_not_totals() {
    let mut a = Reading {
        monotonic_ns: ns(100.0),
        cpu_time_ns: Some(ns(40.0)),
        memory_kind: Kind::Resident,
        memory_bytes: Some(700 << 20),
        gpu_time_ns: Some(ns(9.0)),
        gpu_allocated_bytes: None,
    };
    let b = Reading {
        monotonic_ns: ns(160.0),
        cpu_time_ns: Some(ns(130.0)),
        gpu_time_ns: Some(ns(12.0)),
        memory_bytes: Some((900 << 20) + 5),
        ..a
    };
    let sample = rate(&a, &b).unwrap();
    // 90 s of CPU over 60 s is 150% (a multi-core process), 3 s of GPU is 5%.
    assert_eq!(sample.cpu.get(), 150);
    assert_eq!(sample.gpu.map(Percent::get), Some(5));
    assert_eq!(
        sample.memory.get(),
        900,
        "the level now, rounded down to MiB"
    );
    assert_eq!(sample.memory_kind, MemoryKind::Resident);
    assert_eq!(sample.gpu_memory, None);
    // Rounded to the nearest percent: 0.95 s over 60 s is 1.58%.
    a.cpu_time_ns = Some(ns(130.0) - 950_000_000);
    assert_eq!(rate(&a, &b).unwrap().cpu.get(), 2);
}

#[test]
fn usage_at_most_one_sample_per_minute_of_presented_activity() {
    let mut rig = Rig::active();
    // Ten minutes, with a stream of spurious wake-ups every few seconds in between.
    for _ in 0..200 {
        rig.run(3.0);
        let before = rig.reads.get();
        rig.step();
        assert_eq!(
            rig.reads.get(),
            before,
            "an early step reads nothing before the deadline"
        );
    }
    assert_eq!(rig.seen.len(), 10, "{:?}", rig.seen);
    for pair in rig.seen.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= 60_000, "{pair:?}");
    }
    // One baseline and one read per sample; nothing in between.
    assert_eq!(rig.reads.get(), 11);
}

#[test]
fn usage_nothing_is_read_while_hidden_idle_or_off() {
    let mut rig = Rig::active();
    rig.run(61.0);
    let reads = rig.reads.get();

    rig.sampler.set_presented(false);
    assert_eq!(rig.step(), Next::Parked, "hidden: no timer");
    rig.wait(3600.0);
    assert_eq!(rig.step(), Next::Parked);

    rig.sampler.set_presented(true);
    rig.sampler.set_idle(true);
    assert_eq!(rig.step(), Next::Parked, "idle: no timer");
    rig.wait(3600.0);
    rig.step();

    rig.sampler.set_idle(false);
    rig.collector.stop();
    assert_eq!(rig.step(), Next::Parked, "disabled: no timer");
    rig.wait(3600.0);
    rig.step();

    assert_eq!(rig.reads.get(), reads, "not one read in three idle hours");
    assert_eq!(rig.seen.len(), 1);
}

#[test]
fn usage_hiding_or_idling_resets_the_baseline() {
    for idle in [false, true] {
        let mut rig = Rig::active();
        rig.run(70.0);
        assert_eq!(rig.seen.len(), 1);
        if idle {
            rig.sampler.set_idle(true);
        } else {
            rig.sampler.set_presented(false);
        }
        rig.step();
        // The process burns a lot of CPU while nobody looks; none of it may land in a rate.
        rig.platform.borrow_mut().cpu_offset += 5000.0;
        rig.wait(300.0);
        if idle {
            rig.sampler.set_idle(false);
        } else {
            rig.sampler.set_presented(true);
        }
        let reads = rig.reads.get();
        assert_eq!(
            rig.step(),
            Next::At(Duration::from_secs_f64(rig.clock.get()) + INTERVAL)
        );
        assert_eq!(rig.reads.get(), reads + 1, "a new baseline");
        assert_eq!(rig.seen.len(), 1, "a baseline is not a sample");
        rig.run(60.0);
        assert_eq!(rig.seen.len(), 2);
        assert_eq!(rig.seen[1].1.cpu.get(), 25, "only the minute presented");
    }
}

#[test]
fn usage_a_sleep_is_noticed_by_the_wall_clock_and_not_backfilled() {
    let mut rig = Rig::active();
    rig.run(30.0);
    // Eight hours of sleep: the wall clock moves, the monotonic one does not, the counters do.
    rig.wall_skew += secs(8 * 3600);
    rig.platform.borrow_mut().cpu_offset += 20_000.0;
    rig.run(30.0);
    assert!(rig.seen.is_empty(), "the window spanned a sleep");
    assert_eq!(rig.sampler.stats().sleeps, 1);
    assert_eq!(rig.sampler.stats().samples, 0);
    // A fresh baseline was taken at the wake; the next sample is a clean minute.
    rig.run(60.0);
    assert_eq!(rig.seen.len(), 1);
    assert_eq!(rig.seen[0].1.cpu.get(), 25);
    // A small clock step below the slop is not a sleep.
    rig.wall_skew += secs(2);
    rig.run(60.0);
    assert_eq!(rig.seen.len(), 2);
    assert_eq!(rig.sampler.stats().sleeps, 1);
}

#[test]
fn usage_drift_is_the_gap_between_the_clocks_in_either_direction() {
    let at = |monotonic: u64, wall: u64| Stamp {
        monotonic: secs(monotonic),
        wall: secs(wall),
    };
    let base = at(100, 1_000_000);
    assert_eq!(at(160, 1_000_060).drift_since(&base), Duration::ZERO);
    assert_eq!(at(160, 1_000_064).drift_since(&base), secs(4));
    assert!(at(160, 1_028_800).drift_since(&base) > SLEEP_SLOP, "slept");
    assert!(
        at(160, 996_400).drift_since(&base) > SLEEP_SLOP,
        "clock set back"
    );
}

#[test]
fn usage_consent_changes_reset_the_baseline() {
    // Revoked and given again before the worker looks: a new generation.
    let mut rig = Rig::active();
    rig.run(40.0);
    rig.collector.stop();
    let (session_clock, millis) = SessionClock::manual();
    rig.session = rig.collector.start(Trigger::OptIn, start(), session_clock);
    rig.millis = millis;
    rig.clock.set(61.0);
    let reads = rig.reads.get();
    assert_eq!(rig.step(), Next::At(secs(61) + INTERVAL));
    assert_eq!(rig.reads.get(), reads + 1);
    assert!(rig.seen.is_empty(), "the old baseline did not carry over");
    rig.run(60.0);
    assert_eq!(rig.seen.len(), 1);

    // Revoked: nothing is held, and the same sampler parks.
    rig.collector.stop();
    assert_eq!(rig.step(), Next::Parked);
}

#[test]
fn usage_an_adapter_or_profile_change_starts_a_new_window() {
    let mut rig = Rig::active();
    rig.run(45.0);
    rig.sampler.profile_changed();
    let reads = rig.reads.get();
    rig.step();
    assert_eq!(rig.reads.get(), reads + 1);
    // The old deadline (60 s) passes with nothing recorded for the window across the change.
    rig.run(20.0);
    assert!(rig.seen.is_empty());
    rig.run(40.0);
    assert_eq!(rig.seen.len(), 1);
    assert_eq!(rig.seen[0].0, 105_000, "a minute after the change at 45 s");
}

#[test]
fn usage_a_fresh_panel_reading_is_reused_without_a_second_read() {
    let mut rig = Rig::active();
    assert_eq!(rig.reads.get(), 1);
    // The Performance panel read half a second before the sample is due.
    rig.run(59.5);
    let panel = rig.reading_for_panel(59.5);
    rig.panel.set(Some(panel));
    rig.run(0.5);
    assert_eq!(rig.seen.len(), 1);
    assert_eq!(rig.reads.get(), 1, "no second read: the panel's was used");
    assert_eq!(rig.sampler.stats().reused, 1);
    assert_eq!(rig.allowance.get(), Some(FRESH_FOR));
    // The window was the readings' own, 59.5 s, and the rate is the same 25 percent.
    assert_eq!(rig.seen[0].1.cpu.get(), 25);
}

impl Rig {
    fn reading_for_panel(&self, t: f64) -> Reading {
        Fake {
            clock: self.clock.clone(),
            platform: self.platform.clone(),
            reads: self.reads.clone(),
            panel: self.panel.clone(),
            allowance: self.allowance.clone(),
        }
        .reading_at(t)
    }
}

#[test]
fn usage_a_stale_panel_reading_is_not_reused() {
    let mut rig = Rig::active();
    rig.run(50.0);
    let stale = rig.reading_for_panel(50.0);
    rig.panel.set(Some(stale));
    rig.run(10.0);
    assert_eq!(rig.seen.len(), 1);
    assert_eq!(rig.reads.get(), 2, "ten seconds old: read again");
    assert_eq!(rig.sampler.stats().reused, 0);
    // And the sampler's own read is what the panel finds next.
    assert_eq!(rig.panel.get().unwrap().monotonic_ns, ns(60.0));
}

#[test]
fn usage_a_fresh_reading_can_serve_as_the_baseline_too() {
    let mut rig = Rig::inactive();
    let before = rig.reading_for_panel(0.0);
    rig.panel.set(Some(before));
    rig.sampler.set_presented(true);
    rig.step();
    assert_eq!(rig.reads.get(), 0);
    assert_eq!(rig.sampler.stats().reused, 1);
}

#[test]
fn usage_unavailable_or_reset_counters_are_omitted_never_zero() {
    // GPU time and allocations unavailable: the sample stands without them.
    let mut rig = Rig::active();
    {
        let mut p = rig.platform.borrow_mut();
        p.gpu = None;
        p.gpu_allocated = None;
    }
    rig.run(60.0);
    // The baseline had GPU time, the sample does not: no GPU rate.
    let (_, sample) = rig.seen[0];
    assert_eq!(sample.cpu.get(), 25);
    assert_eq!(sample.gpu, None);
    assert_eq!(sample.gpu_memory, None);
    rig.run(60.0);
    assert_eq!(rig.seen[1].1.gpu, None, "unavailable in both readings");

    // CPU time unavailable: nothing is recorded, not a sample of zeros.
    let mut rig = Rig::active();
    rig.platform.borrow_mut().cpu = None;
    rig.run(60.0);
    assert!(rig.seen.is_empty());
    assert_eq!(rig.sampler.stats().dropped, 1);

    // Memory unavailable: likewise.
    let mut rig = Rig::active();
    rig.platform.borrow_mut().memory = None;
    rig.run(60.0);
    assert!(rig.seen.is_empty());

    // A CPU counter that went backwards (a reset) drops the window; the next one is clean.
    let mut rig = Rig::active();
    rig.platform.borrow_mut().cpu_offset -= 90.0;
    rig.run(60.0);
    assert!(rig.seen.is_empty());
    rig.run(60.0);
    assert_eq!(rig.seen.len(), 1);
    assert_eq!(rig.seen[0].1.cpu.get(), 25);

    // A reset GPU counter loses only the GPU figure.
    let mut rig = Rig::active();
    rig.platform.borrow_mut().gpu_offset -= 45.0;
    rig.run(60.0);
    assert_eq!(rig.seen[0].1.cpu.get(), 25);
    assert_eq!(rig.seen[0].1.gpu, None);
}

#[test]
fn usage_windows_outside_the_valid_span_are_dropped() {
    let reading = |t: f64, cpu: f64| Reading {
        monotonic_ns: ns(t),
        cpu_time_ns: Some(ns(cpu)),
        memory_kind: Kind::Footprint,
        memory_bytes: Some(GIB),
        gpu_time_ns: None,
        gpu_allocated_bytes: None,
    };
    assert!(
        rate(&reading(0.0, 0.0), &reading(29.0, 1.0)).is_none(),
        "too short"
    );
    assert!(rate(&reading(0.0, 0.0), &reading(30.0, 3.0)).is_some());
    assert!(rate(&reading(0.0, 0.0), &reading(180.0, 3.0)).is_some());
    assert!(
        rate(&reading(0.0, 0.0), &reading(181.0, 3.0)).is_none(),
        "too long"
    );
    assert!(
        rate(&reading(60.0, 0.0), &reading(60.0, 0.0)).is_none(),
        "no time passed"
    );
    assert!(
        rate(&reading(60.0, 0.0), &reading(0.0, 0.0)).is_none(),
        "backwards"
    );
}

#[test]
fn usage_the_platform_memory_kind_is_preserved() {
    for (kind, wire) in [
        (Kind::Footprint, MemoryKind::Footprint),
        (Kind::Resident, MemoryKind::Resident),
        (Kind::Private, MemoryKind::Private),
    ] {
        let mut rig = Rig::active();
        rig.platform.borrow_mut().kind = kind;
        rig.run(60.0);
        assert_eq!(rig.seen[0].1.memory_kind, wire);
    }
}

#[test]
fn usage_samples_ignore_a_sampler_that_was_never_presented() {
    let mut rig = Rig::inactive();
    assert_eq!(rig.step(), Next::Parked);
    assert_eq!(rig.reads.get(), 0);
}

/// The production source: one real step on this machine takes a baseline through the process-wide
/// sampler, and records nothing.
#[test]
fn usage_the_real_source_takes_a_baseline_and_no_sample() {
    let collector = Collector::new();
    let (clock, _) = SessionClock::manual();
    let session = collector.start(Trigger::OptIn, start(), clock);
    let mut sampler = Sampler::new();
    sampler.set_presented(true);
    let now = Stamp::now();
    assert_eq!(
        sampler.step(&collector, now),
        Next::At(now.monotonic + INTERVAL)
    );
    assert_eq!(sampler.stats().readings, 1);
    assert_eq!(sampler.stats().samples, 0);
    let window = session.window();
    assert_eq!(window.len, 1, "only session.start");
}
