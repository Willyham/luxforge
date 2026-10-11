//! The application-owned statistics storage, measured rather than summed: the heap bytes the
//! running supervisor, its worker thread, its collector's session and its sender hold above what
//! the process held before it started, at their high-water mark through typical editing, a long
//! event flood sending once a minute, a day offline and the return online, and what is left once
//! consent ends. The design's bound is 256 KiB of allocation capacity
//! (`docs/design/usage-statistics.md`, "Bounded collection and performance sampling").
//!
//! The transport is a fake that records each request into fixed counters and answers from one
//! small response, so nothing here is the network's: the real transport's TLS and connection
//! allocations, and the worker thread's stack (which the system maps and the allocator never
//! sees), are measured separately (`luxforge-net`'s `usage_overhead` binary) and are outside this
//! bound. Everything the supervisor does for itself is inside it: the session's two banks, the
//! device profile, the reserved body and the request around it, the process sampler, the consent
//! rereads before each handoff and the worker's own thread bookkeeping.
//!
//! The counter sees every thread of the process, so this binary holds one test. It simulates over
//! a day and a half of the worker's clock and takes several seconds, so it is a `slow_` test.
use luxforge_core::{
    Error,
    capabilities::transport::{SendOptions, Transport, TransportRequest, TransportResponse},
    usage::{
        Arch, Collector, CpuProfile, CpuVendor, DeviceFn, DeviceProfile, DurationMs, EventKind,
        Feature, GpuBackend, GpuProfile, GpuType, GpuVendor, ImageKey, ManualClock, MemoryBand,
        OperationEnd, OsFamily, OsProfile, Outcome, Randomness, Release, Renderer, Supervisor,
        SupervisorConfig, TestReceiver, Tool, Usage, Version, device_profile::AppleTier,
        eligibility::Eligibility, registry::MAX_BODY_BYTES, sampler::Sampler,
    },
};
use luxforge_process::allocations::{self, Counting};
use luxforge_testbase::{paths::temp_dir, wait_until};
use std::{
    fmt, fs, io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use url::Url;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const RECEIVER: &str = "http://127.0.0.1:9/v1/usage/batches";
const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
/// 2026-10-03T07:20:00Z.
const WALL: Duration = Duration::from_secs(1_791_012_000);
/// The design's bound on new application-owned statistics storage.
const BOUND: i64 = 256 * 1024;

fn profile() -> DeviceProfile {
    DeviceProfile::new(
        OsProfile {
            family: OsFamily::Macos,
            version: Version::new(&[26, 5, 2]).unwrap(),
            build: None,
            distro: None,
            distro_version: None,
            kernel: None,
        },
        Arch::Arm64,
        CpuProfile {
            vendor: CpuVendor::Apple,
            physical_cores: 14,
            logical_cores: 14,
            generation: Some(4),
            tier: Some(AppleTier::Pro),
            family: None,
            model: None,
            stepping: None,
        },
        MemoryBand::Gib64,
        &[GpuProfile {
            vendor: GpuVendor::Apple,
            backend: GpuBackend::Metal,
            gpu_type: GpuType::Integrated,
            unified_memory: true,
            vendor_id: None,
            device_id: None,
            generation: Some(4),
            tier: Some(AppleTier::Pro),
            memory: None,
            driver_version: None,
        }],
        Renderer::Gpu,
        None,
    )
    .unwrap()
}

/// A transport that counts what it is handed in fixed counters and answers `202`, or fails as a
/// network would while `offline`. Its one response's URL is its only allocation per request.
struct Counted {
    clock: Arc<ManualClock>,
    requests: AtomicUsize,
    largest_body: AtomicUsize,
    body_bytes: AtomicU64,
    /// The shortest gap between two requests on the manual clock, in milliseconds.
    shortest_gap_ms: AtomicU64,
    last_ms: AtomicU64,
    offline: AtomicBool,
    /// Each request's time on the manual clock, in milliseconds, into capacity reserved before
    /// the baseline.
    times: std::sync::Mutex<Vec<u64>>,
}

impl fmt::Debug for Counted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Counted")
    }
}

impl Transport for Counted {
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        _: &mut dyn io::Write,
    ) -> Result<TransportResponse, Error> {
        options.control.checkpoint()?;
        let at = u64::try_from(luxforge_core::usage::Clock::monotonic(&*self.clock).as_millis())
            .unwrap();
        if self.requests.fetch_add(1, Ordering::SeqCst) > 0 {
            let gap = at - self.last_ms.load(Ordering::SeqCst);
            self.shortest_gap_ms.fetch_min(gap, Ordering::SeqCst);
        }
        self.last_ms.store(at, Ordering::SeqCst);
        let mut times = self.times.lock().unwrap();
        if times.len() < times.capacity() {
            times.push(at);
        }
        drop(times);
        self.largest_body
            .fetch_max(request.body.len(), Ordering::SeqCst);
        self.body_bytes
            .fetch_add(request.body.len() as u64, Ordering::SeqCst);
        if self.offline.load(Ordering::SeqCst) {
            return Err(Error::file_access("connection refused"));
        }
        Ok(TransportResponse {
            status: 202,
            headers: Vec::new(),
            final_url: Url::parse(RECEIVER).unwrap(),
            received: 0,
        })
    }
}

/// No jitter, so a flood's attempts fall exactly a minute apart and the test can wait for each.
struct NoJitter;

impl Randomness for NoJitter {
    fn up_to(&mut self, _: Duration) -> Duration {
        Duration::ZERO
    }
}

/// What the process holds above `baseline` now, and at its high-water mark since the last reset.
fn above(baseline: i64) -> (i64, i64) {
    (
        allocations::live_bytes() - baseline,
        allocations::peak_bytes() - baseline,
    )
}

#[test]
fn slow_usage_statistics_storage_stays_within_256_kib_however_long_and_hard_it_runs() {
    let dir = temp_dir("usage-capacity");
    let installation = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";
    fs::write(
        dir.join("preferences.json"),
        format!(
            r#"{{"format": 1, "usage_statistics": {{"prompt_shown": true, "enabled": true, "disclosure_version": 1, "installation_id": "{installation}", "image_key": "{}"}}}}"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();
    let usage = Arc::new(Usage::new(Eligibility::test(
        TestReceiver::loopback(RECEIVER).unwrap(),
    )));
    usage.launch(Some(dir.clone()));
    assert!(usage.active().is_some(), "the stored opt-in is effective");
    let collector = Arc::new(Collector::new());
    let clock = Arc::new(ManualClock::new(WALL));
    let transport = Arc::new(Counted {
        clock: clock.clone(),
        requests: AtomicUsize::new(0),
        largest_body: AtomicUsize::new(0),
        body_bytes: AtomicU64::new(0),
        shortest_gap_ms: AtomicU64::new(u64::MAX),
        last_ms: AtomicU64::new(0),
        offline: AtomicBool::new(false),
        times: std::sync::Mutex::new(Vec::with_capacity(1024)),
    });
    let image = ImageKey::from_bytes([3; 32]).image_id(&[4; 32]);
    let events = [
        EventKind::ToolCommit {
            tool: Tool::Basic,
            image,
        },
        EventKind::Interaction {
            feature: Feature::Zoom,
            image: Some(image),
        },
        EventKind::OperationEnd(OperationEnd::export(
            Outcome::Completed,
            DurationMs::from_millis(86_400_000),
            Some(1_000_000),
            Some(image),
        )),
    ];
    let record = |count: usize| {
        for index in 0..count {
            collector.record(events[index % events.len()]);
        }
    };
    let requests = || transport.requests.load(Ordering::SeqCst);

    // Everything the process holds before the supervisor starts is the baseline: the consent
    // runtime, the test's own parts and the preferences it read.
    let config = SupervisorConfig {
        usage: usage.clone(),
        collector: collector.clone(),
        preferences_dir: Some(dir.clone()),
        transport: transport.clone(),
        release: Release::parse("0.0.0+9d4138ca1f2e"),
        device: Box::new(DeviceFn(|| Ok(profile()))),
        sampler: Box::new(Sampler::new()),
        clock: clock.clone(),
        randomness: Box::new(NoJitter),
    };
    let baseline = allocations::live_bytes();
    allocations::reset_peak();
    let supervisor = Supervisor::start(config);
    supervisor.set_presented(true);
    wait_until("the worker's session", || collector.session().is_some());
    wait_until("the worker to wait", || clock.caught_up());
    let (started, _) = above(baseline);
    // One moment of a person's work: `count` events and an interaction, which the worker acts on
    // (a move by zero wakes it), then `by` passes and the worker acts on that, each before the
    // next, so it never reads one time and acts at another.
    let tick = |by: Duration, count: usize| {
        record(count);
        supervisor.interacted();
        for by in [Duration::ZERO, by] {
            clock.advance(by);
            wait_until("the worker to act and wait again", || clock.caught_up());
        }
    };

    // Typical editing: an event every five seconds for an hour, a window sent every ten minutes.
    for _ in 0..720 {
        tick(5 * SECOND, 1);
    }
    let typical = requests();
    let (_, typical_peak) = above(baseline);

    // A flood: a hundred events a second for an hour, five seconds at a time. Each window is full
    // at once and goes when a minute has passed since the last attempt: a request a minute, with
    // no hourly cap.
    for _ in 0..720 {
        tick(5 * SECOND, 500);
    }
    let flooded = requests() - typical;
    let (_, flood_peak) = above(baseline);

    // A day offline at fifty events a second, a minute at a time: cooldowns, not a request a
    // minute, and windows that expire are replaced rather than kept.
    transport.offline.store(true, Ordering::SeqCst);
    let before_offline = requests();
    for _ in 0..24 * 60 {
        tick(MINUTE, 3000);
    }
    let offline_attempts = requests() - before_offline;
    let (offline_live, offline_peak) = above(baseline);

    // Back online: the last cooldown runs out, then a request a minute again with no catch-up.
    transport.offline.store(false, Ordering::SeqCst);
    let back = requests();
    let mut waited = Duration::ZERO;
    while requests() == back {
        assert!(waited <= 7 * HOUR, "sending never resumed");
        tick(MINUTE, 3000);
        waited += MINUTE;
    }
    for _ in 0..30 {
        tick(MINUTE, 3000);
    }
    let resumed = requests() - back;
    let (_, online_peak) = above(baseline);

    // Another process saves an opt-out: the worker catches it before its next handoff, ends the
    // generation and its thread, and frees the session, the banks, the body and the request.
    fs::write(
        dir.join("preferences.json"),
        r#"{"format": 1, "usage_statistics": {"prompt_shown": true, "enabled": false, "disclosure_version": 1}}"#,
    )
    .unwrap();
    let mut minutes = 0;
    wait_until("the worker to end", || {
        if minutes < 12 {
            record(3000);
            clock.advance(MINUTE);
            minutes += 1;
        }
        !supervisor.running()
    });
    wait_until("the session to go", || collector.session().is_none());
    let (after_live, whole_peak) = above(baseline);
    supervisor.shutdown();
    drop(supervisor);
    let _ = fs::remove_dir_all(&dir);

    let requests = requests();
    let times = transport.times.lock().unwrap().clone();
    for pair in times.windows(2) {
        if pair[1] - pair[0] < 60_000 {
            println!("close requests at {} and {} ms", pair[0], pair[1]);
        }
    }
    println!(
        "usage statistics heap above the process's baseline, allocation capacity in bytes: \
         started {started}; peak through typical editing {typical_peak}, an hour's flood \
         {flood_peak}, a day offline {offline_peak} (live at its end {offline_live}), back online \
         {online_peak}; whole run {whole_peak}; after the opt-out {after_live}. Requests: \
         {requests} ({typical} in an hour of typical editing, {flooded} in an hour's flood, \
         {offline_attempts} in a day offline, {resumed} once back online), largest body {} \
         bytes, {} body bytes in all, shortest gap {} ms.",
        transport.largest_body.load(Ordering::SeqCst),
        transport.body_bytes.load(Ordering::SeqCst),
        transport.shortest_gap_ms.load(Ordering::SeqCst),
    );
    assert!(
        whole_peak <= BOUND,
        "the statistics held {whole_peak} bytes at their peak, over {BOUND}"
    );
    assert!(
        offline_live <= flood_peak,
        "a day offline grew the storage: {offline_live} live against {flood_peak}"
    );
    assert!(
        after_live <= 4 * 1024,
        "the opt-out left {after_live} bytes behind"
    );
    assert!(
        (5..=6).contains(&typical),
        "{typical} windows in an hour of editing"
    );
    assert!(
        (59..=61).contains(&flooded),
        "{flooded} requests in an hour's flood: one a minute"
    );
    assert!(
        resumed >= 30,
        "{resumed} requests in the half hour back online"
    );
    assert!(
        (6..=9).contains(&offline_attempts),
        "{offline_attempts} attempts in a day offline"
    );
    assert!(transport.largest_body.load(Ordering::SeqCst) <= MAX_BODY_BYTES);
    assert!(transport.shortest_gap_ms.load(Ordering::SeqCst) >= 60_000);
}
