//! A traffic capture: the statistics supervisor sending through the real transport to a loopback
//! receiver over simulated hours of typical editing, an event flood, a failing receiver and its
//! recovery, with every request the receiver saw recorded at the supervisor's clock. It shows the
//! request and byte volume the schedule allows — one request a minute at most, each at most
//! 64 KiB, with no hourly or daily cap — and the cooldowns and the return without a catch-up burst
//! on the wire, not only at a fake transport.
//!
//! The supervisor's worker runs on a manual clock the test moves only once the worker has acted on
//! the last move (`ManualClock::caught_up`), so each request is captured at the time the worker
//! sent it. `LUXFORGE_USAGE_TRAFFIC_CAPTURE=FILE` also writes the capture there, one JSON object a
//! request. Nothing leaves the machine; the production destination is never contacted.
use luxforge_core::usage::{
    Arch, Collector, CpuProfile, CpuVendor, DeviceFn, DeviceProfile, DurationMs, EventKind,
    Feature, GpuBackend, GpuProfile, GpuType, GpuVendor, ImageKey, ManualClock, MemoryBand,
    NoSampling, OperationEnd, OsFamily, OsProfile, Outcome, Randomness, Release, Renderer,
    Supervisor, SupervisorConfig, TestReceiver, Tool, Usage, Version, decode_batch,
    device_profile::AppleTier, eligibility::Eligibility,
};
use luxforge_net::{HttpTransport, TlsTrust, TransportConfig};
use luxforge_test_macros::slow;
use luxforge_testbase::{Request, TestServer, paths::temp_dir, respond, wait_until};
use serde_json::json;
use std::{
    fs,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const PATH: &str = "/v1/usage/batches";
const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
/// 2026-10-03T07:20:00Z.
const WALL: Duration = Duration::from_secs(1_791_012_000);

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

struct NoJitter;

impl Randomness for NoJitter {
    fn up_to(&mut self, _: Duration) -> Duration {
        Duration::ZERO
    }
}

/// One request the receiver saw.
#[derive(Clone, Debug)]
struct Captured {
    /// The supervisor's clock when it arrived, in seconds.
    at: u64,
    /// The phase of the simulation it arrived in.
    phase: &'static str,
    /// The request as it arrived, head and body.
    wire_bytes: usize,
    body_bytes: usize,
    events: usize,
    dropped: u32,
    batch_seq: u32,
    status: u16,
}

#[test]
#[slow]
fn usage_a_loopback_capture_shows_a_request_a_minute_at_most_and_no_hourly_or_daily_cap() {
    let clock = Arc::new(ManualClock::new(WALL));
    let captured: Arc<Mutex<Vec<Captured>>> = Arc::default();
    let phase: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("typical"));
    let failing = Arc::new(AtomicBool::new(false));
    let server = {
        let (clock, captured, phase, failing) = (
            clock.clone(),
            captured.clone(),
            phase.clone(),
            failing.clone(),
        );
        TestServer::http(move |request: &Request, out: &mut dyn Write| {
            let batch = decode_batch(&request.body).expect("every body is a valid batch");
            let status = if failing.load(Ordering::SeqCst) {
                503
            } else {
                202
            };
            captured.lock().unwrap().push(Captured {
                at: luxforge_core::usage::Clock::monotonic(&*clock).as_secs(),
                phase: *phase.lock().unwrap(),
                wire_bytes: request.text().len(),
                body_bytes: request.body.len(),
                events: batch.events.len(),
                dropped: batch.dropped,
                batch_seq: batch.batch_seq,
                status,
            });
            if status == 503 {
                respond(out, "503 Service Unavailable", "", b"");
            } else {
                respond(out, "202 Accepted", "", b"");
            }
        })
        .unwrap()
    };

    let dir = temp_dir("usage-traffic");
    fs::write(
        dir.join("preferences.json"),
        format!(
            r#"{{"format": 1, "usage_statistics": {{"prompt_shown": true, "enabled": true, "disclosure_version": 1, "installation_id": "c9a10716-1b78-4cbe-8d1c-b8309e5e7247", "image_key": "{}"}}}}"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();
    let usage = Arc::new(Usage::new(Eligibility::test(
        TestReceiver::loopback(&server.url(PATH)).unwrap(),
    )));
    usage.launch(Some(dir.clone()));
    let collector = Arc::new(Collector::new());
    let supervisor = Supervisor::start(SupervisorConfig {
        usage: usage.clone(),
        collector: collector.clone(),
        preferences_dir: Some(dir.clone()),
        transport: Arc::new(HttpTransport::new(TransportConfig {
            trust: TlsTrust::Roots(Vec::new()),
            ..TransportConfig::default()
        })),
        release: Release::parse("0.0.0+9d4138ca1f2e"),
        device: Box::new(DeviceFn(|| Ok(profile()))),
        sampler: Box::new(NoSampling),
        clock: clock.clone(),
        randomness: Box::new(NoJitter),
    });
    supervisor.set_presented(true);
    wait_until("the worker's session", || collector.session().is_some());
    wait_until("the worker to wait", || clock.caught_up());

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
            DurationMs::from_millis(812),
            Some(1),
            Some(image),
        )),
    ];
    let tick = |by: Duration, count: usize| {
        for index in 0..count {
            collector.record(events[index % events.len()]);
        }
        supervisor.interacted();
        for by in [Duration::ZERO, by] {
            clock.advance(by);
            wait_until("the worker to act and wait again", || clock.caught_up());
        }
    };
    let enter = |name: &'static str| *phase.lock().unwrap() = name;

    // An hour of typical editing (an event every ten seconds), two hours of a flood (two hundred
    // events a second), a day of a failing receiver under the same flood, and the return.
    for _ in 0..360 {
        tick(10 * SECOND, 1);
    }
    enter("flood");
    for _ in 0..2 * 360 {
        tick(10 * SECOND, 2000);
    }
    enter("failing");
    failing.store(true, Ordering::SeqCst);
    for _ in 0..24 * 60 {
        tick(MINUTE, 3000);
    }
    enter("recovered");
    failing.store(false, Ordering::SeqCst);
    for _ in 0..7 * 60 {
        tick(MINUTE, 3000);
    }
    supervisor.shutdown();
    let _ = fs::remove_dir_all(&dir);

    let captured = captured.lock().unwrap().clone();
    if let Some(file) = std::env::var_os("LUXFORGE_USAGE_TRAFFIC_CAPTURE") {
        let lines: Vec<String> = captured
            .iter()
            .map(|request| {
                json!({
                    "at_s": request.at,
                    "phase": request.phase,
                    "wire_bytes": request.wire_bytes,
                    "body_bytes": request.body_bytes,
                    "events": request.events,
                    "dropped": request.dropped,
                    "batch_seq": request.batch_seq,
                    "status": request.status,
                })
                .to_string()
            })
            .collect();
        fs::write(file, lines.join("\n") + "\n").unwrap();
    }
    let in_phase = |name: &str| -> Vec<&Captured> {
        captured
            .iter()
            .filter(|request| request.phase == name)
            .collect()
    };
    let hours = |requests: &[&Captured], start: u64| -> Vec<usize> {
        let mut counts = Vec::new();
        for request in requests {
            let hour = usize::try_from((request.at - start) / HOUR.as_secs()).unwrap();
            if counts.len() <= hour {
                counts.resize(hour + 1, 0);
            }
            counts[hour] += 1;
        }
        counts
    };
    let (typical, flood, failing, recovered) = (
        in_phase("typical"),
        in_phase("flood"),
        in_phase("failing"),
        in_phase("recovered"),
    );
    let total_wire: usize = captured.iter().map(|request| request.wire_bytes).sum();
    let largest = captured
        .iter()
        .map(|request| request.wire_bytes)
        .max()
        .unwrap_or(0);
    let shortest = captured
        .windows(2)
        .map(|pair| pair[1].at - pair[0].at)
        .min()
        .unwrap_or(u64::MAX);
    let flood_per_hour = hours(&flood, HOUR.as_secs());
    let failing_start = 3 * HOUR.as_secs();
    println!(
        "usage traffic at a loopback receiver: {} requests, {total_wire} bytes on the wire, the \
         largest {largest} bytes, the shortest gap {shortest} s; typical editing {} requests in \
         an hour ({} bytes); the flood {flood_per_hour:?} requests per hour ({} bytes); a failing \
         receiver {} requests in a day at {:?} s; recovered {} requests in seven hours, the first \
         at {} s",
        captured.len(),
        typical.len(),
        typical
            .iter()
            .map(|request| request.wire_bytes)
            .sum::<usize>(),
        flood
            .iter()
            .map(|request| request.wire_bytes)
            .sum::<usize>(),
        failing.len(),
        failing
            .iter()
            .map(|request| request.at - failing_start)
            .collect::<Vec<_>>(),
        recovered.len(),
        recovered
            .first()
            .map_or(0, |request| request.at - failing_start),
    );

    // Never nearer than a minute and never over 64 KiB in its body. While every request was
    // accepted, every window was sent: no `batch_seq` gap. (During the failures, windows that
    // expire in a cooldown are dropped, which is a gap.)
    assert!(shortest >= 60, "two requests {shortest} s apart");
    assert!(
        captured
            .iter()
            .all(|request| request.body_bytes <= 64 * 1024)
    );
    let accepted = captured.len() - failing.len() - recovered.len();
    for pair in captured[..accepted].windows(2) {
        assert_eq!(pair[1].batch_seq, pair[0].batch_seq + 1, "{pair:?}");
    }
    // Typical editing: a window every ten minutes and a little.
    assert!(
        (5..=6).contains(&typical.len()),
        "{} requests in an hour of editing",
        typical.len()
    );
    // The flood: a request a minute in each of its hours, the second as many as the first (the
    // last request falls on the hour that ends it).
    assert!(
        flood_per_hour[..2]
            .iter()
            .all(|count| (59..=61).contains(count)),
        "{flood_per_hour:?}"
    );
    assert!((119..=121).contains(&flood.len()), "{}", flood.len());
    // Each flood window is full: what the 64 KiB body left out and what came while it was full
    // are counted in its `dropped`.
    assert!(
        flood
            .iter()
            .all(|request| request.events as u32 + request.dropped >= 512)
    );
    // A failing receiver: one attempt, then 20, 40, 80, 160 and 360-minute cooldowns, not a
    // request a minute.
    assert!(
        (6..=9).contains(&failing.len()),
        "{} attempts in a day of failures",
        failing.len()
    );
    // Recovered: the cooldown runs out, then a request a minute with no catch-up burst.
    assert!(recovered.len() > 2, "sending resumed");
    for pair in recovered.windows(2) {
        assert!(pair[1].at - pair[0].at >= 60, "no catch-up burst");
    }
}
