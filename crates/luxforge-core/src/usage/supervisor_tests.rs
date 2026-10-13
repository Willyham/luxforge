//! The supervisor and its sender against a manual clock, scripted jitter and a fake transport: the
//! schedule (jitter, the 60-second spacing under floods, no hourly or daily cap, no empty batch, no
//! catch-up burst), bounded storage while offline, every response class, opt-outs at each stage of
//! an attempt, another process's opt-out, hidden windows, sleep, device changes, and quitting.
//! Nothing here opens a socket; `luxforge-net`'s usage tests send through the real transport.
use super::*;
use crate::{
    ClientAuthority, Error,
    capabilities::transport::TransportResponse,
    preferences::PreferenceStore,
    usage::{
        Batch, CpuProfile, CpuVendor, DISCLOSURE_VERSION, EventKind, GpuBackend, GpuProfile,
        GpuType, GpuVendor, Idle, MemoryBand, MemoryKind, MemoryMib, OsFamily, OsProfile, Percent,
        ProcessSample, Renderer, Version, decode_batch,
        device_profile::{AppleTier, Arch},
        eligibility::{Eligibility, TestReceiver},
        event::{Event, Recorded},
        registry::{MAX_BODY_BYTES, MAX_EVENTS},
    },
};
use luxforge_test_macros::slow;
use luxforge_testbase::{Gate, paths::temp_dir, wait_until};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64},
};
use url::Url;

const RECEIVER: &str = "http://127.0.0.1:9/v1/usage/batches";
const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
/// 2026-10-03T07:20:00Z.
const WALL: Duration = Duration::from_secs(1_791_012_000);

fn release() -> Release {
    Release::parse("0.0.0+9d4138ca1f2e").unwrap()
}

/// A valid profile; `cores` tells two apart.
fn profile(cores: u16) -> DeviceProfile {
    DeviceProfile::new(
        OsProfile {
            family: OsFamily::Macos,
            version: Version::new(&[15, 5]).unwrap(),
            build: None,
            distro: None,
            distro_version: None,
            kernel: None,
        },
        Arch::Arm64,
        CpuProfile {
            vendor: CpuVendor::Apple,
            physical_cores: cores,
            logical_cores: cores,
            generation: Some(4),
            tier: Some(AppleTier::Pro),
            family: None,
            model: None,
            stepping: None,
        },
        MemoryBand::Gib32,
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

fn idle() -> EventKind {
    EventKind::AppIdle {
        state: Idle::Active,
    }
}

fn sample() -> EventKind {
    EventKind::ProcessSample(ProcessSample {
        cpu: Percent::from_f64(12.0).unwrap(),
        memory: MemoryMib::from_bytes(1 << 30),
        memory_kind: MemoryKind::Footprint,
        gpu: None,
        gpu_memory: None,
    })
}

/// Replace the stored record under `dir`, as another process sharing the configuration would.
fn write_record(dir: &Path, record: &Value) {
    fs::write(
        dir.join("preferences.json"),
        serde_json::to_vec(&json!({"format": 1, "usage_statistics": record})).unwrap(),
    )
    .unwrap();
}

/// A preference directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        Self(temp_dir(&format!("usage-supervisor-{name}")))
    }

    fn store(&self) -> PreferenceStore {
        PreferenceStore::new(Some(self.0.clone()))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Jitter as a fixed fraction of its range, in permille, and the ranges asked for.
#[derive(Clone, Default)]
struct Jitter {
    permille: Arc<AtomicU64>,
    asked: Arc<Mutex<Vec<Duration>>>,
}

impl Randomness for Jitter {
    fn up_to(&mut self, max: Duration) -> Duration {
        self.asked.lock().unwrap().push(max);
        let millis = max.as_millis() as u64 * self.permille.load(Ordering::SeqCst) / 1000;
        Duration::from_millis(millis)
    }
}

/// What the fake transport was handed.
#[derive(Clone, Debug)]
struct Sent {
    /// When, on the manual clock.
    at: Duration,
    /// The decoded body; its events only for the first 16 requests.
    batch: Batch,
    url: String,
    headers: Vec<(String, String)>,
    body_bytes: usize,
    limits: (u64, u64, Duration, Duration, Duration, u8),
    events: usize,
    first_seq: u32,
    last_seq: u32,
    /// The oldest event's time, in milliseconds since its session started.
    first_t: Option<u32>,
}

type Reply = Box<dyn FnOnce(&Arc<JobControl>) -> Result<TransportResponse, Error> + Send>;

/// A transport that records what it is handed and answers from a script: `204` once the script
/// is empty, or a network failure while `offline`.
#[derive(Default)]
struct FakeTransport {
    clock: Mutex<Option<Arc<ManualClock>>>,
    replies: Mutex<VecDeque<Reply>>,
    sent: Mutex<Vec<Sent>>,
    offline: AtomicBool,
}

impl fmt::Debug for FakeTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeTransport")
    }
}

impl FakeTransport {
    fn push(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl Transport for FakeTransport {
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        _: &mut dyn io::Write,
    ) -> Result<TransportResponse, Error> {
        // As the real transport: a cancelled job sends nothing.
        options.control.checkpoint()?;
        let mut batch = decode_batch(&request.body).expect("every body is a valid batch");
        let at = self
            .clock
            .lock()
            .unwrap()
            .as_ref()
            .map_or(Duration::ZERO, |clock| clock.monotonic());
        let reply = {
            let mut sent = self.sent.lock().unwrap();
            let events = batch.events.len();
            let first_seq = batch.events.first().map_or(0, |event| event.seq);
            let last_seq = batch.events.last().map_or(0, |event| event.seq);
            let first_t = batch.events.first().map(|event| event.t);
            if sent.len() >= 16 {
                batch.events = Vec::new();
            }
            sent.push(Sent {
                at,
                batch,
                url: request.endpoint.url.to_string(),
                headers: request.headers.clone(),
                body_bytes: request.body.len(),
                limits: (
                    options.max_request_bytes,
                    options.max_response_bytes,
                    options.connect_timeout,
                    options.read_timeout,
                    options.total_timeout,
                    options.redirects.max,
                ),
                events,
                first_seq,
                last_seq,
                first_t,
            });
            self.replies.lock().unwrap().pop_front()
        };
        match reply {
            Some(reply) => reply(options.control),
            None if self.offline.load(Ordering::SeqCst) => {
                Err(Error::file_access("connection refused"))
            }
            None => Ok(answer(204, &[])),
        }
    }
}

fn answer(status: u16, headers: &[(&str, &str)]) -> TransportResponse {
    TransportResponse {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        final_url: Url::parse(RECEIVER).unwrap(),
        received: 0,
    }
}

fn status(code: u16) -> Reply {
    Box::new(move |_| Ok(answer(code, &[])))
}

fn status_with(code: u16, headers: &'static [(&'static str, &'static str)]) -> Reply {
    Box::new(move |_| Ok(answer(code, headers)))
}

fn fail(error: Error) -> Reply {
    Box::new(move |_| Err(error))
}

/// Run `during` while the request is in flight (resolving, connecting, writing or reading), then
/// end as the real transport does: cancelled if its job was cancelled meanwhile.
fn during(during: impl FnOnce(&Arc<JobControl>) + Send + 'static) -> Reply {
    Box::new(move |control| {
        during(control);
        control.checkpoint()?;
        Ok(answer(204, &[]))
    })
}

/// Run `during` and answer `code` whatever happened: a response that arrives after an opt-out.
fn late(code: u16, during: impl FnOnce() + Send + 'static) -> Reply {
    Box::new(move |_| {
        during();
        Ok(answer(code, &[]))
    })
}

/// Hold the request at `gate`; give up when its job is cancelled if `cancellable`, as the real
/// transport's shut socket does.
fn hold(gate: Arc<Gate>, cancellable: bool) -> Reply {
    Box::new(move |control| {
        if cancellable {
            gate.pass_unless(|| control.is_cancelled());
        } else {
            gate.pass();
        }
        control.checkpoint()?;
        Ok(answer(204, &[]))
    })
}

fn eligible() -> Usage {
    Usage::new(Eligibility::test(TestReceiver::loopback(RECEIVER).unwrap()))
}

fn enable(usage: &Usage, dir: &Dir, request_id: &str) {
    usage
        .set(
            &dir.store(),
            ClientAuthority::Permissions,
            true,
            Some(DISCLOSURE_VERSION),
            request_id,
        )
        .unwrap();
}

fn revoke(usage: &Usage, store: &PreferenceStore, request_id: &str) {
    usage
        .set(store, ClientAuthority::Edit, false, None, request_id)
        .unwrap();
}

/// What a harness is made of, before the supervisor exists.
struct Parts {
    dir: Dir,
    usage: Arc<Usage>,
    collector: Arc<Collector>,
    clock: Arc<ManualClock>,
    transport: Arc<FakeTransport>,
    jitter: Jitter,
    device: Arc<Mutex<Result<DeviceProfile, DeviceUnavailable>>>,
}

impl Parts {
    /// A fresh process over its own preferences, after its launch read.
    fn new(name: &str, usage: Usage) -> Self {
        Self::over(Dir::new(name), usage)
    }

    /// A fresh process over `dir`, after its launch read.
    fn over(dir: Dir, usage: Usage) -> Self {
        let usage = Arc::new(usage);
        usage.launch(Some(dir.0.clone()));
        let clock = Arc::new(ManualClock::new(WALL));
        let transport = Arc::new(FakeTransport::default());
        *transport.clock.lock().unwrap() = Some(clock.clone());
        Self {
            dir,
            usage,
            collector: Arc::new(Collector::new()),
            clock,
            transport,
            jitter: Jitter::default(),
            device: Arc::new(Mutex::new(Ok(profile(12)))),
        }
    }

    fn config(&self, sampler: Box<dyn Sampling>) -> Config {
        let device = self.device.clone();
        Config {
            usage: self.usage.clone(),
            collector: self.collector.clone(),
            preferences_dir: Some(self.dir.0.clone()),
            transport: self.transport.clone(),
            release: Some(release()),
            device: Box::new(DeviceFn(move || *device.lock().unwrap())),
            sampler,
            clock: self.clock.clone(),
            randomness: Box::new(self.jitter.clone()),
        }
    }
}

/// A supervisor whose machine the test steps itself, as the worker would: no thread.
struct Harness {
    parts: Parts,
    shared: Arc<Shared>,
    machine: Machine,
    deadline: Option<Duration>,
}

impl std::ops::Deref for Harness {
    type Target = Parts;
    fn deref(&self) -> &Parts {
        &self.parts
    }
}

impl Harness {
    /// Consent given, the window presented, and the first step taken at time zero.
    fn new(name: &str) -> Self {
        let mut harness = Self::off(name);
        harness.enable("on");
        harness.present(true);
        harness.step();
        harness
    }

    /// Nothing consented yet.
    fn off(name: &str) -> Self {
        Self::with(Parts::new(name, eligible()), Box::new(NoSampling))
    }

    fn with(parts: Parts, sampler: Box<dyn Sampling>) -> Self {
        let shared = Shared::new(parts.config(sampler), false);
        Shared::watch(&shared);
        let machine = shared.lock().machine.take().expect("a release");
        Self {
            parts,
            shared,
            machine,
            deadline: None,
        }
    }

    fn step(&mut self) -> Step {
        let step = self.machine.step(&self.shared);
        self.deadline = match step {
            Step::Wait(deadline) => deadline,
            Step::Off => None,
        };
        step
    }

    fn present(&mut self, presented: bool) {
        self.shared.signal(|state| state.presented = presented);
    }

    fn now(&self) -> Duration {
        self.clock.monotonic()
    }

    fn session(&self) -> Option<Arc<Session>> {
        self.collector.session()
    }

    fn record(&self, count: usize) {
        for _ in 0..count {
            self.collector.record(idle());
        }
    }

    fn revoke(&self, request_id: &str) {
        revoke(&self.usage, &self.dir.store(), request_id);
    }

    fn enable(&self, request_id: &str) {
        enable(&self.usage, &self.dir, request_id);
    }

    /// Step whenever the worker would wake on its own deadlines, until `end`.
    fn run_until(&mut self, end: Duration) {
        loop {
            let step = self.step();
            let now = self.now();
            match step {
                Step::Wait(Some(deadline)) if deadline <= end => {
                    self.clock.advance(deadline.saturating_sub(now));
                }
                _ => {
                    self.clock.advance(end.saturating_sub(now));
                    return;
                }
            }
        }
    }

    /// Record `per_second` events each second until `end`, stepping exactly when the worker would
    /// wake: a window becoming nonempty or full, or its deadline (to the second).
    fn simulate(&mut self, end: Duration, per_second: usize) {
        while self.now() < end {
            let before = self.session().map_or(0, |session| session.window().len);
            self.record(per_second);
            let after = self.session().map_or(0, |session| session.window().len);
            let woken = (before == 0 && after > 0)
                || (before < MAX_EVENTS && after >= MAX_EVENTS)
                || self.deadline.is_some_and(|deadline| deadline <= self.now());
            if woken {
                self.step();
            }
            self.clock.advance(SECOND);
        }
    }

    /// Simulate a minute at a time until the transport has been handed one more request.
    fn until_next_attempt(&mut self, per_second: usize) {
        let count = self.transport.count();
        while self.transport.count() == count {
            self.simulate(self.now() + MINUTE, per_second);
        }
    }

    /// What `usage.read` answers now.
    fn read(&self) -> Value {
        serde_json::to_value(self.usage.read(&self.dir.store())).unwrap()
    }

    /// Statistics storage this process owns now: the session's banks and the request buffers.
    fn storage(&self) -> usize {
        let session = self.session().map_or(0, |session| session.storage_bytes());
        let buffers = self.machine.buffers.as_ref().map_or(0, |buffers| {
            buffers.body.storage_bytes() + buffers.request.body.capacity()
        });
        session + buffers
    }
}

fn session_id(session: &Session) -> Value {
    json!(String::from_utf8_lossy(&session.id().to_wire()))
}

#[test]
fn usage_nothing_runs_before_consent_or_in_an_ineligible_launch() {
    // Before consent: no worker, no session, no request, no buffer.
    let parts = Parts::new("before-consent", eligible());
    let supervisor = Supervisor::start(parts.config(Box::new(NoSampling)));
    supervisor.set_presented(true);
    parts.clock.advance(HOUR);
    assert!(!supervisor.running());
    assert!(parts.collector.session().is_none());
    assert_eq!(parts.collector.gate().current(), None);
    assert_eq!(parts.transport.count(), 0);
    drop(supervisor);

    // An ineligible launch over a stored opt-in: the launch read turns nothing on.
    for usage in [
        Usage::headless(),
        Usage::new(Eligibility::Ineligible(
            crate::usage::Reason::InactiveDestination,
        )),
    ] {
        let dir = Dir::new("ineligible");
        enable(&eligible(), &dir, "stored");
        let parts = Parts::over(dir, usage);
        assert!(parts.usage.active().is_none());
        let supervisor = Supervisor::start(parts.config(Box::new(NoSampling)));
        supervisor.set_presented(true);
        parts.clock.advance(HOUR);
        assert!(!supervisor.running());
        assert!(parts.collector.session().is_none());
        assert_eq!(parts.transport.count(), 0);
    }

    // A build without a release collects and sends nothing even with consent, and says why.
    let parts = Parts::new("no-release", eligible());
    enable(&parts.usage, &parts.dir, "on");
    let supervisor = Supervisor::start(Config {
        release: None,
        ..parts.config(Box::new(NoSampling))
    });
    assert!(!supervisor.running());
    assert!(parts.collector.session().is_none());
    assert_eq!(
        parts.usage.diagnostics().delivery,
        Delivery::Suspended(Suspension::Build)
    );
}

#[test]
fn usage_the_first_window_is_sent_ten_minutes_after_its_first_event_plus_jitter() {
    let mut harness = Harness::off("first-window");
    harness.jitter.permille.store(500, Ordering::SeqCst);
    harness.enable("on");
    harness.present(true);
    harness.step();
    let session = harness.session().expect("a session started on opt-in");
    assert_eq!(harness.read()["session_id"], session_id(&session));
    // Due 10 minutes after the session's first event, plus half of the 60-second jitter range,
    // which is chosen once for the window.
    assert_eq!(harness.deadline, Some(10 * MINUTE + 30 * SECOND));
    assert_eq!(
        *harness.jitter.asked.lock().unwrap(),
        [sender::FLUSH_JITTER]
    );
    harness.clock.advance(4 * MINUTE);
    harness.record(3);
    harness.run_until(10 * MINUTE + 29 * SECOND);
    assert_eq!(harness.transport.count(), 0, "no request before it is due");
    harness.run_until(11 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    let first = &sent[0];
    assert_eq!(first.at, 10 * MINUTE + 30 * SECOND);
    assert_eq!(first.url, RECEIVER);
    assert_eq!(
        first.headers,
        [("Content-Type".to_owned(), "application/json".to_owned())]
    );
    assert_eq!(
        first.limits,
        (
            65_536,
            1024,
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_secs(5),
            0
        )
    );
    let batch = &first.batch;
    assert_eq!(batch.batch_seq, 0);
    assert_eq!(batch.session_id, session.id());
    assert_eq!(batch.release, release());
    assert_eq!(batch.device, profile(12));
    assert_eq!(
        Some(batch.installation_id),
        harness
            .usage
            .active()
            .map(|active| active.installation_id.report_id())
    );
    assert_eq!(batch.events.len(), 4);
    assert_eq!(
        batch.events[0].kind,
        EventKind::SessionStart {
            trigger: Trigger::OptIn
        }
    );
    assert_eq!(batch.events[1].t, 4 * 60_000);
    // The next window has its own jitter: the whole range, this time.
    harness.jitter.permille.store(1000, Ordering::SeqCst);
    harness.clock.advance(MINUTE);
    harness.record(1);
    harness.step();
    assert_eq!(harness.deadline, Some(12 * MINUTE + 10 * MINUTE + MINUTE));
    let read = harness.read();
    assert_eq!(read["diagnostics"]["attempts"], json!(1));
    assert_eq!(read["diagnostics"]["accepted"], json!(1));
    assert_eq!(read["diagnostics"]["sent"], json!(4));
    assert_eq!(
        read["diagnostics"]["delivery"],
        json!({"state": "ready", "reason": null})
    );
}

#[test]
fn usage_a_full_window_is_due_at_once_but_never_within_a_minute_of_the_start_or_the_last_attempt() {
    let mut harness = Harness::new("full-window");
    // 511 more events fill the window a second after opt-in; it is due at once, but no request
    // is made within a minute of the session's start.
    harness.clock.advance(SECOND);
    harness.record(MAX_EVENTS - 1);
    harness.step();
    assert_eq!(harness.deadline, Some(MINUTE));
    harness.record(10);
    harness.run_until(MINUTE + SECOND);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].at, MINUTE);
    assert_eq!(sent[0].events, MAX_EVENTS);
    assert_eq!(
        sent[0].batch.dropped, 10,
        "events after the window filled are dropped"
    );
    // The window fills again at once; it goes a minute after the last attempt.
    harness.record(MAX_EVENTS);
    harness.run_until(5 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].at, 2 * MINUTE);
    assert_eq!(sent[1].batch.batch_seq, 1);
    assert_eq!(
        sent[1].first_seq,
        sent[0].last_seq + 11,
        "a visible seq gap"
    );
}

#[test]
fn usage_the_minute_between_attempts_counts_from_the_handoff_not_from_the_steps_start() {
    // The checks before a handoff (the stored consent reread, serializing) take time: here 30 s
    // of it before the first, as a stalled disk might. The next attempt is a minute after that
    // handoff itself, not a minute after its step began.
    let mut harness = Harness::new("stamp");
    let clock = harness.clock.clone();
    let mut first = true;
    harness.machine.before_handoff = Some(Box::new(move || {
        if std::mem::take(&mut first) {
            clock.advance(30 * SECOND);
        }
    }));
    harness.simulate(10 * MINUTE, 20);
    let sent = harness.transport.sent();
    assert!(sent.len() >= 3, "{} attempts", sent.len());
    for pair in sent.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= MINUTE,
            "handoffs {:?} apart",
            pair[1].at - pair[0].at
        );
    }
}

#[test]
#[slow]
fn usage_floods_send_once_a_minute_with_no_hourly_or_daily_cap() {
    let mut harness = Harness::new("flood");
    harness.jitter.permille.store(500, Ordering::SeqCst);
    harness.simulate(25 * HOUR, 20);
    let sent = harness.transport.sent();
    // One attempt a minute from the first minute to the last: 25 hours of them.
    assert!(sent.len() >= 25 * 60 - 2, "{} attempts", sent.len());
    for pair in sent.windows(2) {
        let gap = pair[1].at - pair[0].at;
        assert!(
            (MINUTE..=MINUTE + 2 * SECOND).contains(&gap),
            "attempts {gap:?} apart at {:?}",
            pair[1].at
        );
        assert_eq!(pair[1].batch.batch_seq, pair[0].batch.batch_seq + 1);
        assert_eq!(pair[1].events, MAX_EVENTS);
    }
    let per_hour = |hour: u32| {
        sent.iter()
            .filter(|sent| sent.at >= hour * HOUR && sent.at < (hour + 1) * HOUR)
            .count()
    };
    assert!(per_hour(0) >= 58);
    assert!(
        per_hour(24) >= 59,
        "no daily cap: the 25th hour sends as often as the first"
    );
    assert!(sent.iter().all(|sent| sent.body_bytes <= MAX_BODY_BYTES));
}

#[test]
#[slow]
fn usage_a_week_of_mixed_use_keeps_every_bound_with_no_daily_cap_and_no_backlog() {
    // Each day: two hours of a flood, three of typical editing, an hour offline under a flood, two
    // hours hidden with background outcomes, a rate limit, then the rest of the day untouched.
    let mut harness = Harness::new("week");
    harness.jitter.permille.store(500, Ordering::SeqCst);
    let mut hidden: Vec<(Duration, Duration)> = Vec::new();
    let mut per_day = Vec::new();
    let mut bounded = None;
    for day in 0..7u32 {
        let start = day * 24 * HOUR;
        let before = harness.transport.count();
        harness.simulate(start + 2 * HOUR, 20);
        // Typical editing: six events a minute. The worker wakes when its window becomes
        // nonempty, as a producer's event wakes it.
        for _ in 0..3 * 60 {
            harness.record(6);
            harness.step();
            let end = harness.now() + MINUTE;
            harness.simulate(end, 0);
        }
        harness.transport.offline.store(true, Ordering::SeqCst);
        harness.simulate(start + 6 * HOUR, 20);
        harness.transport.offline.store(false, Ordering::SeqCst);
        harness.present(false);
        let from = harness.now();
        harness.step();
        for _ in 0..4 {
            harness.record(5);
            harness.step();
            let end = harness.now() + 30 * MINUTE;
            harness.simulate(end, 0);
        }
        hidden.push((from, harness.now()));
        harness.present(true);
        harness.step();
        harness
            .transport
            .push(status_with(429, &[("retry-after", "900")]));
        harness.simulate(start + 9 * HOUR, 5);
        harness.run_until(start + 24 * HOUR);
        per_day.push(harness.transport.count() - before);
        let storage = harness.storage();
        assert!(storage <= 256 * 1024, "day {day}: {storage} bytes");
        assert_eq!(
            *bounded.get_or_insert(storage),
            storage,
            "day {day}: storage does not grow"
        );
    }
    let sent = harness.transport.sent();
    for pair in sent.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= MINUTE,
            "attempts {:?} apart at {:?}",
            pair[1].at - pair[0].at,
            pair[1].at
        );
    }
    for request in &sent {
        assert!(request.body_bytes <= MAX_BODY_BYTES);
        assert!(request.events <= MAX_EVENTS);
        assert!(
            hidden
                .iter()
                .all(|(from, to)| request.at <= *from || request.at >= *to),
            "a request at {:?} while hidden",
            request.at
        );
        // No backlog: nothing in a request is older than a window's 30 minutes (the session
        // began at time zero and no sleep started another).
        if let Some(first_t) = request.first_t {
            let age = request.at - Duration::from_millis(u64::from(first_t));
            assert!(age <= 30 * MINUTE, "{age:?} old at {:?}", request.at);
        }
    }
    // No daily cap: the last day sends as much as the first.
    assert!(per_day.iter().all(|count| *count >= 120), "{per_day:?}");
    assert!(
        per_day[6].abs_diff(per_day[0]) <= 3,
        "the week's days send alike: {per_day:?}"
    );
}

#[test]
fn usage_empty_windows_are_never_sent_and_an_idle_session_keeps_no_timer() {
    let mut harness = Harness::new("idle");
    harness.run_until(12 * MINUTE);
    assert_eq!(harness.transport.count(), 1, "the session.start window");
    // Nothing recorded since: no deadline, no request, however long.
    harness.step();
    assert_eq!(harness.deadline, None);
    harness.run_until(30 * HOUR);
    assert_eq!(harness.transport.count(), 1);
}

#[test]
fn usage_a_hidden_window_starts_no_upload_keeps_no_timer_and_its_expired_data_is_dropped() {
    let mut harness = Harness::new("hidden");
    harness.present(false);
    harness.record(5);
    harness.step();
    assert_eq!(harness.deadline, None, "no timer while hidden");
    harness.clock.advance(20 * MINUTE);
    harness.step();
    assert_eq!(
        harness.transport.count(),
        0,
        "a due window waits while hidden"
    );
    // Shown again within the window's age: it goes at once.
    harness.present(true);
    harness.step();
    assert_eq!(harness.transport.count(), 1);
    assert_eq!(harness.transport.sent()[0].at, 20 * MINUTE);

    // Hidden past the window's age: it is dropped before anything is scheduled.
    harness.present(false);
    harness.clock.advance(SECOND);
    harness.record(5);
    harness.step();
    harness.clock.advance(31 * MINUTE);
    harness.present(true);
    harness.step();
    assert_eq!(
        harness.transport.count(),
        1,
        "the expired window is dropped"
    );
    assert_eq!(harness.session().unwrap().window().len, 0);
    harness.record(1);
    harness.run_until(harness.now() + 12 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].batch.batch_seq, 2,
        "the dropped window is a batch_seq gap"
    );
    assert_eq!(harness.read()["diagnostics"]["dropped"], json!(5));
}

#[test]
fn usage_each_batch_is_attempted_once_and_discarded_whatever_happened() {
    let mut harness = Harness::new("once");
    harness
        .transport
        .push(fail(Error::file_access("connection reset")));
    harness.record(3);
    harness.run_until(11 * MINUTE);
    assert_eq!(harness.transport.count(), 1);
    // Fresh events after the failure; the failed batch's events are never sent again.
    harness.clock.advance(SECOND);
    harness.record(2);
    harness.run_until(harness.now() + 2 * HOUR);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].batch.batch_seq, 1);
    assert_eq!(sent[1].first_seq, sent[0].last_seq + 1);
    assert_eq!(sent[1].events, 2);
    let read = harness.read();
    assert_eq!(read["diagnostics"]["attempts"], json!(2));
    assert_eq!(read["diagnostics"]["accepted"], json!(1));
    assert_eq!(read["diagnostics"]["sent"], json!(2));
}

#[test]
fn usage_failures_cool_down_from_twenty_to_three_hundred_sixty_minutes_and_a_success_resets() {
    let mut harness = Harness::new("cooldown");
    harness.jitter.permille.store(1000, Ordering::SeqCst);
    for reply in [
        fail(Error::file_access("connection refused")),
        fail(Error::file_access("the request timed out")),
        fail(Error::file_access("TLS handshake failed")),
        fail(Error::resource_limit(
            "the response from 127.0.0.1 is larger than 1024 bytes",
        )),
        fail(Error::file_access("connection refused")),
        fail(Error::file_access("connection refused")),
        status(408),
        status(503),
    ] {
        harness.transport.push(reply);
    }
    let expected: [u32; 8] = [20, 40, 80, 160, 360, 360, 360, 360];
    for (index, minutes) in expected.iter().enumerate() {
        harness.until_next_attempt(1);
        let at = harness.transport.sent()[index].at;
        // The cooldown, lengthened by the whole 10% jitter range.
        assert_eq!(
            harness.machine.not_before,
            Some(at + *minutes * MINUTE * 11 / 10),
            "failure {index}"
        );
        assert_eq!(
            harness.read()["diagnostics"]["delivery"],
            json!({"state": "cooldown", "reason": "failure"})
        );
    }
    let sent = harness.transport.sent();
    for (index, pair) in sent.windows(2).enumerate() {
        let gap = pair[1].at - pair[0].at;
        assert!(
            gap >= expected[index] * MINUTE * 11 / 10,
            "attempt {} came {gap:?} after the last",
            index + 1
        );
    }
    // The next attempt succeeds and resets the cooldown: a later failure waits 20 minutes again.
    harness.until_next_attempt(1);
    assert_eq!(harness.machine.failures, 0);
    assert_eq!(harness.machine.not_before, None);
    harness.transport.push(status(500));
    harness.until_next_attempt(1);
    let at = harness.transport.sent().last().unwrap().at;
    assert_eq!(harness.machine.not_before, Some(at + 22 * MINUTE));
}

#[test]
fn usage_429_honours_a_clamped_retry_after_and_otherwise_cools_down() {
    type Headers = &'static [(&'static str, &'static str)];
    let cases: [(Headers, Duration, &str); 5] = [
        (&[("retry-after", "120")], 10 * MINUTE, "rate-limited"),
        (&[("retry-after", "7200")], 2 * HOUR, "rate-limited"),
        (&[("retry-after", "99999999")], 24 * HOUR, "rate-limited"),
        (
            &[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")],
            20 * MINUTE,
            "failure",
        ),
        (&[], 20 * MINUTE, "failure"),
    ];
    for (header, wait, reason) in cases {
        let mut harness = Harness::new("429");
        harness.transport.push(status_with(429, header));
        harness.run_until(11 * MINUTE);
        let at = harness.transport.sent()[0].at;
        assert_eq!(harness.machine.not_before, Some(at + wait), "{header:?}");
        assert_eq!(
            harness.read()["diagnostics"]["delivery"],
            json!({"state": "cooldown", "reason": reason})
        );
        // Fresh data waits out the whole period, then goes on its window's schedule.
        harness.simulate(at + wait + 11 * MINUTE, 1);
        let sent = harness.transport.sent();
        assert!(sent.len() >= 2, "{header:?}");
        assert!(
            (at + wait..=at + wait + 11 * MINUTE).contains(&sent[1].at),
            "{header:?}"
        );
    }
}

#[test]
fn usage_redirects_other_4xx_and_refused_requests_suspend_sending_until_the_next_launch() {
    let cases: Vec<(&str, Reply, &str)> = vec![
        (
            "refused redirect",
            fail(Error::validation("redirect refused")),
            "redirect",
        ),
        ("304", status(304), "redirect"),
        ("404", status(404), "rejected"),
        ("400", status(400), "rejected"),
        ("413", status(413), "rejected"),
        (
            "refused address",
            fail(Error::validation("10.1.2.3 is not a public address")),
            "refused",
        ),
        (
            "no transport",
            fail(Error::not_ready("no network transport is configured")),
            "unavailable",
        ),
    ];
    for (what, reply, reason) in cases {
        let mut harness = Harness::new("suspend");
        harness.transport.push(reply);
        harness.run_until(11 * MINUTE);
        assert_eq!(harness.transport.count(), 1, "{what}");
        harness.simulate(harness.now() + 3 * HOUR, 1);
        assert_eq!(harness.transport.count(), 1, "{what}: nothing more is sent");
        let read = harness.read();
        assert_eq!(
            read["diagnostics"]["delivery"],
            json!({"state": "suspended", "reason": reason}),
            "{what}"
        );
        assert_eq!(
            (&read["enabled"], &read["effective"]),
            (&json!(true), &json!(true)),
            "{what}: consent is unchanged"
        );
        // The next launch over the same preferences sends again.
        let Harness { parts, .. } = harness;
        let Parts { dir, .. } = parts;
        let mut relaunched = Harness::with(Parts::over(dir, eligible()), Box::new(NoSampling));
        relaunched.present(true);
        relaunched.run_until(12 * MINUTE);
        let sent = relaunched.transport.sent();
        assert_eq!(sent.len(), 1, "{what}");
        assert_eq!(
            sent[0].batch.events[0].kind,
            EventKind::SessionStart {
                trigger: Trigger::Launch
            }
        );
    }
}

#[test]
#[slow]
fn usage_storage_stays_bounded_while_offline_and_there_is_no_catch_up_burst_when_back() {
    let mut harness = Harness::new("offline");
    harness.jitter.permille.store(500, Ordering::SeqCst);
    harness.transport.offline.store(true, Ordering::SeqCst);
    harness.simulate(MINUTE + SECOND, 50);
    let bounded = harness.storage();
    assert!(bounded <= 256 * 1024, "{bounded} bytes");
    let end = 24 * HOUR;
    while harness.now() < end {
        harness.simulate(harness.now() + 10 * MINUTE, 50);
        assert_eq!(
            harness.storage(),
            bounded,
            "storage does not grow with time offline"
        );
        let buffers = harness.machine.buffers.as_ref().unwrap();
        assert_eq!(buffers.body.storage_bytes(), MAX_BODY_BYTES);
        assert_eq!(buffers.request.body.capacity(), 0, "one body, handed back");
    }
    // A day offline: 20, 40, 80, 160 and then 360-minute cooldowns, not a request a minute.
    let offline = harness.transport.count();
    assert!((6..=9).contains(&offline), "{offline} attempts");
    // Back online: one window goes when the cooldown ends, then at most one a minute.
    harness.transport.offline.store(false, Ordering::SeqCst);
    harness.simulate(end + 7 * HOUR, 50);
    let sent = harness.transport.sent();
    let back = &sent[offline - 1..];
    assert!(back.len() > 2, "sending resumed");
    for pair in back.windows(2) {
        assert!(pair[1].at - pair[0].at >= MINUTE, "no catch-up burst");
    }
    assert_eq!(harness.storage(), bounded);
}

#[test]
fn usage_an_opt_out_before_detaching_or_during_serialization_sends_nothing() {
    // Before the window is detached.
    let mut harness = Harness::new("revoke-before");
    harness.record(3);
    harness.clock.advance(11 * MINUTE);
    harness.revoke("off");
    // Before the worker wakes: the collector's gate is off and producers record nothing.
    assert!(harness.session().is_none(), "producers stop at once");
    assert_eq!(harness.collector.gate().current(), None);
    assert_eq!(harness.collector.record(idle()), Recorded::Off);
    assert_eq!(harness.step(), Step::Off);
    assert_eq!(harness.transport.count(), 0);
    assert_eq!(harness.storage(), 0, "every buffer is freed");
    assert_eq!(harness.read()["session_id"], json!(null));

    // After serialization, before the handoff.
    let mut harness = Harness::new("revoke-serializing");
    let (usage, store) = (harness.usage.clone(), harness.dir.store());
    harness.machine.before_handoff = Some(Box::new(move || revoke(&usage, &store, "off")));
    harness.run_until(11 * MINUTE);
    assert_eq!(harness.transport.count(), 0, "the handoff never happens");
    assert_eq!(harness.step(), Step::Off);
    assert_eq!(harness.storage(), 0);
}

#[test]
fn usage_an_opt_out_while_the_request_is_in_flight_cancels_it_and_nothing_restarts() {
    // The transport resolves, connects, writes and reads inside one call: an opt-out at any of
    // them cancels the job, which the transport sees at its next checkpoint or shut socket.
    let mut harness = Harness::new("revoke-in-flight");
    let (usage, store) = (harness.usage.clone(), harness.dir.store());
    let cancelled = Arc::new(AtomicBool::new(false));
    let seen = cancelled.clone();
    harness.transport.push(during(move |control| {
        revoke(&usage, &store, "off");
        seen.store(control.is_cancelled(), Ordering::SeqCst);
    }));
    harness.record(4);
    harness.run_until(11 * MINUTE);
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the opt-out cancelled the request's job"
    );
    assert_eq!(harness.step(), Step::Off);
    assert!(harness.session().is_none());
    assert_eq!(
        harness.machine.not_before, None,
        "a cancel is not a failure"
    );
    assert_eq!(harness.read()["diagnostics"]["accepted"], json!(0));
    // Off and on again: a new session and installation ID, and none of the revoked events.
    let before = harness.transport.sent()[0].batch.clone();
    harness.enable("on-again");
    harness.clock.advance(MINUTE);
    harness.run_until(harness.now() + 12 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 2);
    let after = &sent[1].batch;
    assert_ne!(after.session_id, before.session_id);
    assert_ne!(after.installation_id, before.installation_id);
    assert_eq!(after.batch_seq, 0);
    assert_eq!(
        after
            .events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        [EventKind::SessionStart {
            trigger: Trigger::OptIn
        }]
    );
}

#[test]
fn usage_a_late_response_after_an_opt_out_changes_nothing() {
    for code in [204, 503, 404] {
        let mut harness = Harness::new("late");
        let (usage, store) = (harness.usage.clone(), harness.dir.store());
        harness
            .transport
            .push(late(code, move || revoke(&usage, &store, "off")));
        harness.run_until(11 * MINUTE);
        assert_eq!(harness.transport.count(), 1);
        assert_eq!(harness.step(), Step::Off);
        assert!(harness.session().is_none(), "{code}: nothing restarted");
        assert_eq!(
            (harness.machine.not_before, harness.machine.suspended),
            (None, None),
            "{code}: the late outcome set no cooldown or suspension"
        );
        assert_eq!(harness.read()["diagnostics"]["accepted"], json!(0));
        // A later opt-in sends on the ordinary schedule.
        harness.enable("on-again");
        harness.run_until(harness.now() + 12 * MINUTE);
        assert_eq!(harness.transport.count(), 2, "{code}");
    }
}

#[test]
fn usage_another_processs_opt_out_is_caught_before_detaching_and_before_the_handoff() {
    let opted_out = json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1});
    let other_id = json!({
        "prompt_shown": true,
        "enabled": true,
        "disclosure_version": 1,
        "installation_id": "c9a10716-1b78-4cbe-8d1c-b8309e5e7247",
        "image_key": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    });
    for (what, record) in [
        ("opt-out", opted_out),
        ("another installation ID", other_id),
    ] {
        // Saved before the window is due.
        let mut harness = Harness::new("other-process");
        harness.record(2);
        write_record(&harness.dir.0, &record);
        harness.run_until(11 * MINUTE);
        assert_eq!(harness.transport.count(), 0, "{what}");
        assert!(
            harness.usage.active().is_none(),
            "{what}: the generation ended"
        );
        assert!(harness.session().is_none(), "{what}");
        assert_eq!(harness.step(), Step::Off);
        assert_eq!(harness.storage(), 0);

        // Saved between serialization and the handoff.
        let mut harness = Harness::new("other-process-handoff");
        let (dir, saved) = (harness.dir.0.clone(), record.clone());
        harness.machine.before_handoff = Some(Box::new(move || write_record(&dir, &saved)));
        harness.run_until(11 * MINUTE);
        assert_eq!(harness.transport.count(), 0, "{what}: no handoff");
        assert!(harness.usage.active().is_none());
        assert_eq!(harness.step(), Step::Off);
    }
}

#[test]
fn usage_a_system_sleep_starts_a_new_session() {
    let mut harness = Harness::new("sleep");
    let first = harness.session().unwrap().id();
    harness.clock.advance(2 * MINUTE);
    harness.record(3);
    harness.step();
    // A short wall-clock step is not a sleep.
    harness.clock.suspend(5 * SECOND);
    harness.step();
    assert_eq!(harness.session().unwrap().id(), first);
    // Asleep for two hours: the monotonic clock stood still. The next wake starts a new session,
    // discarding the window recorded before the sleep.
    harness.clock.suspend(2 * HOUR);
    harness.record(1);
    harness.step();
    let second = harness.session().unwrap();
    assert_ne!(second.id(), first);
    assert_eq!(harness.read()["session_id"], session_id(&second));
    assert_eq!(second.window().len, 1, "only its own session.start");
    harness.record(2);
    harness.run_until(harness.now() + 12 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    let batch = &sent[0].batch;
    assert_eq!(batch.session_id, second.id());
    assert_eq!(batch.batch_seq, 0);
    assert_eq!(
        batch.events[0],
        Event {
            seq: 0,
            t: 0,
            kind: EventKind::SessionStart {
                trigger: Trigger::Launch
            }
        }
    );
    assert_eq!(
        batch.session_start.unix_seconds(),
        (WALL + 2 * MINUTE + 5 * SECOND + 2 * HOUR).as_secs()
    );
    assert_eq!(batch.events.len(), 3);
    assert_eq!(harness.read()["diagnostics"]["dropped"], json!(5));
}

#[test]
fn usage_a_device_change_drops_the_old_window_without_an_early_request() {
    let mut harness = Harness::new("device");
    harness.record(3);
    harness.clock.advance(MINUTE);
    *harness.device.lock().unwrap() = Ok(profile(8));
    harness.shared.signal(|state| state.device_changed = true);
    harness.step();
    assert_eq!(harness.transport.count(), 0, "no early request");
    assert_eq!(harness.session().unwrap().window().len, 0);
    harness.record(2);
    harness.run_until(harness.now() + 12 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].batch.device, profile(8));
    assert_eq!(sent[0].events, 2, "no old event under the new profile");
    assert_eq!(sent[0].batch.batch_seq, 1, "the dropped window is a gap");
    // The same profile again changes nothing.
    harness.record(1);
    harness.shared.signal(|state| state.device_changed = true);
    harness.step();
    assert_eq!(harness.session().unwrap().window().len, 1);
}

/// A sampler in [`sampler::Sampler`]'s shape: a baseline, then a sample a minute while presented
/// and active, with what it was told logged.
#[derive(Default)]
struct FakeSampler {
    presented: bool,
    idle: bool,
    next: Option<Duration>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Sampling for FakeSampler {
    fn set_presented(&mut self, presented: bool) {
        if self.presented != presented {
            self.presented = presented;
            self.next = None;
            self.log
                .lock()
                .unwrap()
                .push(if presented { "shown" } else { "hidden" });
        }
    }

    fn set_idle(&mut self, idle: bool) {
        if self.idle != idle {
            self.idle = idle;
            self.next = None;
            self.log
                .lock()
                .unwrap()
                .push(if idle { "idle" } else { "active" });
        }
    }

    fn profile_changed(&mut self) {
        self.next = None;
        self.log.lock().unwrap().push("profile");
    }

    fn step(&mut self, collector: &Collector, now: Duration, _: Duration) -> Option<Duration> {
        if !self.presented || self.idle {
            self.next = None;
            return None;
        }
        match self.next {
            Some(due) if now >= due => {
                collector.record(sample());
                self.next = Some(now + MINUTE);
            }
            Some(_) => {}
            None => self.next = Some(now + MINUTE),
        }
        self.next
    }
}

#[test]
fn usage_the_sampler_runs_only_while_presented_and_active_and_hears_every_change() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::with(
        Parts::new("sampler", eligible()),
        Box::new(FakeSampler {
            log: log.clone(),
            ..FakeSampler::default()
        }),
    );
    harness.enable("on");
    harness.present(true);
    harness.step();
    assert_eq!(harness.deadline, Some(MINUTE), "the sampler's deadline");
    harness.run_until(5 * MINUTE + SECOND);
    // Every event but session.start is a sample.
    let samples = |harness: &Harness| harness.session().unwrap().diagnostics().recorded - 1;
    assert_eq!(samples(&harness), 5);
    harness.present(false);
    harness.step();
    assert_eq!(harness.deadline, None, "no sampling timer while hidden");
    harness.clock.advance(HOUR);
    harness.step();
    assert_eq!(samples(&harness), 5);
    harness.present(true);
    harness.run_until(harness.now() + MINUTE + SECOND);
    assert_eq!(
        samples(&harness),
        6,
        "a fresh baseline a minute after restore"
    );
    // Idle pauses it; a device change resets its window.
    harness.shared.signal(|state| state.idle = true);
    harness.run_until(harness.now() + 5 * MINUTE);
    assert_eq!(samples(&harness), 6);
    harness.shared.signal(|state| state.idle = false);
    *harness.device.lock().unwrap() = Ok(profile(8));
    harness.shared.signal(|state| state.device_changed = true);
    harness.step();
    assert_eq!(
        *log.lock().unwrap(),
        ["shown", "hidden", "shown", "idle", "active", "profile"]
    );
}

#[test]
fn usage_visibility_changes_and_a_minute_without_interaction_are_recorded_in_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::with(
        Parts::new("presence", eligible()),
        Box::new(FakeSampler {
            log: log.clone(),
            ..FakeSampler::default()
        }),
    );
    harness.enable("on");
    // The first report is the launch's state: nothing is recorded for it.
    harness.shared.set_presented(true);
    harness.step();
    harness.clock.advance(30 * SECOND);
    harness.shared.interacted();
    harness.step();
    assert_eq!(
        harness.deadline,
        Some(MINUTE),
        "the sampler's minute comes first"
    );
    harness.run_until(MINUTE + 30 * SECOND - SECOND);
    assert!(
        !harness.shared.lock().idle,
        "a minute since the interaction has not passed"
    );
    harness.run_until(MINUTE + 31 * SECOND);
    assert!(harness.shared.lock().idle, "idle at its own deadline");
    assert_eq!(
        harness.deadline,
        Some(10 * MINUTE),
        "idle: only the window's send time is left, no sampling or idle timer"
    );
    harness.clock.advance(5 * SECOND);
    harness.shared.interacted();
    harness.shared.interacted();
    harness.shared.set_presented(false);
    harness.shared.set_presented(false);
    harness.step();
    assert_eq!(harness.deadline, None, "hidden: no idle deadline");
    harness.clock.advance(15 * MINUTE);
    harness.step();
    assert!(
        !harness.shared.lock().idle,
        "hidden time never becomes idle by itself"
    );
    assert_eq!(harness.transport.count(), 0);
    harness.shared.set_presented(true);
    harness.step();
    // Shown after a quarter of an hour with no interaction: idle at once, and the due window,
    // which holds every transition in order, goes.
    assert!(harness.shared.lock().idle);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]
            .batch
            .events
            .iter()
            .map(|event| event.kind)
            .filter(|kind| !matches!(kind, EventKind::ProcessSample(_)))
            .collect::<Vec<_>>(),
        [
            EventKind::SessionStart {
                trigger: Trigger::OptIn
            },
            EventKind::AppIdle { state: Idle::Idle },
            EventKind::AppIdle {
                state: Idle::Active
            },
            EventKind::AppPresented {
                state: Presented::Hidden
            },
            EventKind::AppPresented {
                state: Presented::Visible
            },
            EventKind::AppIdle { state: Idle::Idle },
        ]
    );
    assert_eq!(
        *log.lock().unwrap(),
        // The interaction and the hide reached the sampler in one step, visibility first.
        ["shown", "idle", "hidden", "active", "shown", "idle"]
    );
}

#[test]
fn usage_read_answers_the_release_the_batches_carry() {
    // An unrevisioned test build has no release of its own; the supervisor's is what it reports.
    let harness = Harness::new("release");
    harness.clock.advance(11 * MINUTE);
    assert_eq!(harness.read()["release"], json!(release().as_str()));
}

#[test]
fn usage_a_host_without_a_window_records_no_visibility_or_idleness() {
    let mut harness = Harness::new("windowless");
    harness.shared.interacted();
    harness.run_until(5 * MINUTE);
    assert!(!harness.shared.lock().idle);
    assert_eq!(
        harness.session().unwrap().window().len,
        1,
        "session.start only"
    );
}

#[test]
fn usage_the_process_sampler_plugs_in_and_parks_while_off_or_hidden() {
    let collector = Collector::new();
    let mut sampler: Box<dyn Sampling> = Box::new(sampler::Sampler::new());
    assert_eq!(
        sampler.step(&collector, MINUTE, WALL),
        None,
        "collection off"
    );
    let (clock, _) = SessionClock::manual();
    collector.start(
        Trigger::OptIn,
        SessionStart::from_unix_seconds(WALL.as_secs()).unwrap(),
        clock,
        crate::usage::ImageKey::from_bytes([1; 32]),
    );
    assert_eq!(
        sampler.step(&collector, MINUTE, WALL),
        None,
        "not presented"
    );
    sampler.set_presented(true);
    sampler.set_idle(true);
    assert_eq!(sampler.step(&collector, MINUTE, WALL), None, "idle");
}

#[test]
fn usage_no_reportable_device_stops_collecting_until_the_next_opt_in() {
    let mut harness = Harness::off("no-device");
    *harness.device.lock().unwrap() = Err(DeviceUnavailable::Unreportable);
    harness.enable("on");
    harness.present(true);
    assert_eq!(harness.step(), Step::Off);
    assert!(harness.session().is_none(), "nothing is buffered");
    assert!(
        harness.machine.dormant(&harness.shared),
        "the worker may end"
    );
    assert_eq!(
        harness.read()["diagnostics"]["delivery"],
        json!({"state": "suspended", "reason": "device"})
    );
    assert_eq!(
        harness.read()["effective"],
        json!(true),
        "consent is unchanged"
    );
    // Not asked again in this generation, even if the device could now be gathered.
    *harness.device.lock().unwrap() = Ok(profile(12));
    harness.run_until(HOUR);
    assert!(harness.session().is_none());
    assert_eq!(harness.transport.count(), 0);
    // The next opt-in gathers again and collects.
    harness.revoke("off");
    harness.step();
    harness.enable("on-again");
    harness.run_until(harness.now() + 12 * MINUTE);
    assert_eq!(harness.transport.count(), 1);
    assert_eq!(
        harness.read()["diagnostics"]["delivery"],
        json!({"state": "ready", "reason": null})
    );

    // A device change that cannot be gathered stops collecting too.
    let mut harness = Harness::new("no-device-change");
    harness.record(2);
    *harness.device.lock().unwrap() = Err(DeviceUnavailable::Unreportable);
    harness.shared.signal(|state| state.device_changed = true);
    assert_eq!(harness.step(), Step::Off);
    assert!(harness.session().is_none());
    assert_eq!(harness.storage(), 0);
}

#[test]
fn usage_a_pending_device_sends_nothing_until_it_is_known() {
    let mut harness = Harness::off("pending-device");
    *harness.device.lock().unwrap() = Err(DeviceUnavailable::Pending);
    harness.enable("on");
    harness.present(true);
    harness.step();
    assert!(harness.session().is_some(), "collection runs");
    harness.record(2);
    harness.run_until(11 * MINUTE);
    assert_eq!(
        harness.transport.count(),
        0,
        "a window due without a profile"
    );
    assert_eq!(harness.session().unwrap().window().len, 0, "is discarded");
    *harness.device.lock().unwrap() = Ok(profile(12));
    harness.record(1);
    harness.run_until(harness.now() + 12 * MINUTE);
    let sent = harness.transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].batch.batch_seq, 1);
    assert_eq!(sent[0].events, 1);
}

#[test]
fn usage_the_gathered_device_waits_for_facts_and_needs_consent() {
    let gate = crate::usage::Gate::new();
    let mut pending = GatheredDevice::new(|| None);
    assert_eq!(
        pending.profile(&gate, false),
        Err(DeviceUnavailable::Pending)
    );
    let mut off = GatheredDevice::new(|| Some((Vec::new(), device::RendererPath::Gpu)));
    assert_eq!(
        off.profile(&gate, false),
        Err(DeviceUnavailable::NotConsented),
        "nothing is probed while collection is off"
    );
}

#[test]
fn usage_read_reports_the_session_and_the_delivery_state() {
    let mut harness = Harness::new("read");
    harness.record(2);
    harness.step();
    let read = harness.read();
    assert_eq!(read["effective"], json!(true));
    assert_eq!(read["session_id"], session_id(&harness.session().unwrap()));
    assert_eq!(
        read["diagnostics"],
        json!({
            "recorded": 3,
            "dropped": 0,
            "sent": 0,
            "attempts": 0,
            "accepted": 0,
            "delivery": {"state": "ready", "reason": null},
        })
    );
    harness.transport.push(status(404));
    harness.run_until(11 * MINUTE);
    let read = harness.read();
    assert_eq!(read["diagnostics"]["attempts"], json!(1));
    assert_eq!(
        read["diagnostics"]["delivery"],
        json!({"state": "suspended", "reason": "rejected"})
    );
    harness.revoke("off");
    harness.step();
    let read = harness.read();
    assert_eq!(read["session_id"], json!(null));
    assert_eq!(read["effective"], json!(false));
}

#[test]
fn usage_a_failed_opt_out_write_stays_off() {
    let mut harness = Harness::new("failed-off");
    harness.record(2);
    // The preferences file becomes a directory: the off write fails, but this process is off.
    let file = harness.dir.0.join("preferences.json");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    harness
        .usage
        .set(
            &harness.dir.store(),
            ClientAuthority::Edit,
            false,
            None,
            "off",
        )
        .unwrap_err();
    assert!(harness.session().is_none());
    assert_eq!(harness.step(), Step::Off);
    harness.run_until(HOUR);
    assert_eq!(harness.transport.count(), 0);
    assert!(harness.session().is_none());
}

/// A real supervisor with its worker thread, on the manual clock.
fn threaded(name: &str) -> (Parts, Supervisor) {
    let parts = Parts::new(name, eligible());
    let supervisor = Supervisor::start(parts.config(Box::new(NoSampling)));
    supervisor.set_presented(true);
    (parts, supervisor)
}

/// Opt in and hold the first upload at a shut gate, which gives up on a cancel if `cancellable`.
fn held_upload(parts: &Parts, cancellable: bool) -> Arc<Gate> {
    let stall = Arc::new(Gate::new());
    stall.shut();
    parts.transport.push(hold(stall.clone(), cancellable));
    enable(&parts.usage, &parts.dir, "on");
    wait_until("the worker's session", || {
        parts.collector.session().is_some()
    });
    parts.clock.advance(11 * MINUTE);
    stall.wait_reached(1, "the upload");
    stall
}

#[test]
fn usage_the_worker_starts_on_consent_and_ends_when_consent_ends() {
    let (parts, supervisor) = threaded("worker-lifetime");
    assert!(!supervisor.running());
    enable(&parts.usage, &parts.dir, "on");
    wait_until("the worker's session", || {
        parts.collector.session().is_some()
    });
    assert!(supervisor.running());
    parts.clock.advance(11 * MINUTE);
    wait_until("the first batch", || parts.transport.count() == 1);
    revoke(&parts.usage, &parts.dir.store(), "off");
    assert_eq!(parts.collector.record(idle()), Recorded::Off);
    assert!(
        parts.collector.session().is_none(),
        "producers stop at once"
    );
    wait_until("the worker to end", || !supervisor.running());
    assert_eq!(parts.usage.read(&parts.dir.store()).session_id, None);
    // A later opt-in starts a new worker and session.
    enable(&parts.usage, &parts.dir, "on-again");
    wait_until("a new session", || parts.collector.session().is_some());
    assert!(supervisor.running());
}

#[test]
fn usage_an_opt_out_cancels_a_blocked_upload_and_the_worker_ends() {
    let (parts, supervisor) = threaded("revoke-blocked");
    let stall = held_upload(&parts, true);
    // The owner's opt-out cancels the request's job before its write; only that can end the
    // request, since the gate stays shut.
    revoke(&parts.usage, &parts.dir.store(), "off");
    wait_until("the worker to end", || !supervisor.running());
    assert!(stall.is_shut());
    assert!(parts.collector.session().is_none());
    assert_eq!(parts.usage.diagnostics().accepted, 0);
    stall.open();
}

#[test]
fn usage_quitting_cancels_and_discards_without_waiting_for_the_worker() {
    // A transport that ignores its cancel: quitting still returns at once, leaving the worker to
    // end on its own.
    let (parts, supervisor) = threaded("quit");
    let stall = held_upload(&parts, false);
    supervisor.shutdown();
    assert!(stall.holding(), "quitting did not wait for the request");
    assert!(supervisor.running());
    assert!(parts.collector.session().is_none());
    assert_eq!(parts.collector.gate().current(), None);
    stall.open();
    wait_until("the worker to end", || !supervisor.running());
    assert_eq!(parts.transport.count(), 1);
    assert_eq!(
        parts.usage.diagnostics().accepted,
        0,
        "the late answer is ignored"
    );

    // A transport that honours its cancel ends the request without the gate opening.
    let (parts, supervisor) = threaded("quit-cancel");
    let stall = held_upload(&parts, true);
    supervisor.shutdown();
    wait_until("the cancelled upload to end", || !supervisor.running());
    assert!(stall.is_shut());
    assert!(parts.collector.session().is_none());
    stall.open();
}
