//! The statistics supervisor sending through the real transport to loopback receivers: one bare
//! JSON `POST` per batch with the IDs only in the body, the 64 KiB body and 1 KiB response limits,
//! a refused redirect, and quitting shutting a stalled upload's socket. The supervisor runs its own
//! worker thread on a manual clock; nothing leaves the machine and the production destination is
//! never contacted (its activation switch stays off; these hosts use explicit test eligibility).
use luxforge_core::usage::{
    Arch, Collector, CpuProfile, CpuVendor, DeviceFn, DeviceProfile, DurationMs, EventKind,
    GpuBackend, GpuProfile, GpuType, GpuVendor, Idle, ImageKey, ManualClock, MemoryBand,
    NoSampling, OperationEnd, OsFamily, OsProfile, Outcome, Release, Renderer, Supervisor,
    SupervisorConfig, SystemRandomness, TestReceiver, Usage, Version, decode_batch,
    device_profile::AppleTier,
    eligibility::Eligibility,
    sender::{self, Cooldown, Delivery, Suspension},
};
use luxforge_net::{HttpTransport, TlsTrust, TransportConfig};
use luxforge_testbase::{Gate, Request, TestServer, paths::temp_dir, respond, wait_until};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

const INSTALLATION: &str = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";
/// 2026-10-03T07:20:00Z.
const WALL: Duration = Duration::from_secs(1_791_012_000);
const PATH: &str = "/v1/usage/batches";

fn profile() -> DeviceProfile {
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
            physical_cores: 12,
            logical_cores: 12,
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

/// A desktop process with a stored opt-in and explicit test eligibility for `receiver`, its
/// supervisor started and its window presented.
struct Host {
    dir: PathBuf,
    usage: Arc<Usage>,
    collector: Arc<Collector>,
    clock: Arc<ManualClock>,
    supervisor: Supervisor,
}

impl Host {
    fn new(name: &str, receiver: &str) -> Self {
        let dir = temp_dir(&format!("usage-sender-{name}"));
        fs::write(
            dir.join("preferences.json"),
            format!(
                r#"{{"format": 1, "usage_statistics": {{"prompt_shown": true, "enabled": true, "disclosure_version": 1, "installation_id": "{INSTALLATION}", "image_key": "{}"}}}}"#,
                "ab".repeat(32)
            ),
        )
        .unwrap();
        let usage = Arc::new(Usage::new(Eligibility::test(
            TestReceiver::loopback(receiver).unwrap(),
        )));
        usage.launch(Some(dir.clone()));
        assert!(usage.active().is_some(), "the stored opt-in is effective");
        let collector = Arc::new(Collector::new());
        let clock = Arc::new(ManualClock::new(WALL));
        let transport = Arc::new(HttpTransport::new(TransportConfig {
            trust: TlsTrust::Roots(Vec::new()),
            ..TransportConfig::default()
        }));
        let supervisor = Supervisor::start(SupervisorConfig {
            usage: usage.clone(),
            collector: collector.clone(),
            preferences_dir: Some(dir.clone()),
            transport,
            release: Release::parse("0.0.0+9d4138ca1f2e"),
            device: Box::new(DeviceFn(|| Ok(profile()))),
            sampler: Box::new(NoSampling),
            clock: clock.clone(),
            randomness: Box::new(SystemRandomness::new()),
        });
        supervisor.set_presented(true);
        wait_until("the worker's session", || collector.session().is_some());
        Self {
            dir,
            usage,
            collector,
            clock,
            supervisor,
        }
    }

    /// Let the first window fall due (two minutes plus at most a minute of jitter) and wait for
    /// its one attempt to end.
    fn first_attempt(&self) {
        self.clock.advance(Duration::from_secs(4 * 60));
        wait_until("the attempt's outcome", || {
            let counts = self.usage.diagnostics();
            counts.accepted == 1 || counts.delivery != Delivery::Ready
        });
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.supervisor.shutdown();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn idle() -> EventKind {
    EventKind::AppIdle {
        state: Idle::Active,
    }
}

#[test]
fn usage_a_batch_reaches_a_loopback_receiver_as_one_bare_json_post() {
    let server = TestServer::http(|_: &Request, out: &mut dyn Write| {
        respond(out, "204 No Content", "", b"");
    })
    .unwrap();
    let host = Host::new("accepted", &server.url(PATH));
    host.collector.record(idle());
    host.first_attempt();
    let counts = host.usage.diagnostics();
    assert_eq!((counts.attempts, counts.accepted), (1, 1));
    // The twelve minutes the clock jumps pass the idle deadline first: the window is presented
    // and nobody interacted, so the worker records `app.idle` before it sends.
    assert_eq!(counts.sent, 3, "session.start, one event and app.idle");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, PATH, "no query string");
    assert_eq!(request.header("content-type"), Some("application/json"));
    for absent in ["cookie", "authorization", "content-encoding", "expect"] {
        assert_eq!(request.header(absent), None, "{absent}");
    }
    let batch = decode_batch(&request.body).expect("a valid batch");
    let session = host.collector.session().unwrap().id();
    assert_eq!(batch.installation_id.to_wire(), INSTALLATION.as_bytes());
    assert_eq!(batch.session_id, session);
    assert_eq!(batch.batch_seq, 0);
    assert_eq!(batch.events.len(), 3);
    assert_eq!(
        batch.events[2].kind,
        EventKind::AppIdle { state: Idle::Idle }
    );
    // The IDs travel only in the body: never in the URL or a header.
    let session = String::from_utf8_lossy(&session.to_wire()).into_owned();
    for id in [INSTALLATION, session.as_str()] {
        assert!(!request.path.contains(id));
        assert!(request.headers.iter().all(|(_, value)| !value.contains(id)));
    }
}

#[test]
fn usage_a_full_window_stays_within_the_64_kib_body_limit() {
    let server = TestServer::http(|_: &Request, out: &mut dyn Write| {
        respond(out, "202 Accepted", "", b"");
    })
    .unwrap();
    let host = Host::new("body-limit", &server.url(PATH));
    let image = ImageKey::from_bytes([7; 32]).image_id(&[9; 32]);
    for _ in 0..600 {
        host.collector
            .record(EventKind::OperationEnd(OperationEnd::export(
                Outcome::Completed,
                DurationMs::from_millis(86_400_000),
                Some(1_000_000),
                Some(image),
            )));
    }
    host.first_attempt();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body = &requests[0].body;
    assert!(body.len() <= 64 * 1024, "{} bytes", body.len());
    let batch = decode_batch(body).unwrap();
    // 512 events fit the window; the body limit left out the newest of them, counted as dropped,
    // as is the `app.idle` the clock's jump made due once the window was full.
    assert_eq!(batch.events.len() as u32 + batch.dropped, 600 + 2);
    assert!(batch.dropped > 600 + 2 - 512);
    assert_eq!(host.usage.diagnostics().accepted, 1);
}

#[test]
fn usage_an_oversized_response_is_cut_off_and_cools_down() {
    let server = TestServer::http(|_: &Request, out: &mut dyn Write| {
        respond(out, "200 OK", "", &[b'x'; 4096]);
    })
    .unwrap();
    let host = Host::new("oversized", &server.url(PATH));
    host.first_attempt();
    let counts = host.usage.diagnostics();
    assert_eq!((counts.attempts, counts.accepted), (1, 0));
    assert_eq!(counts.delivery, Delivery::Cooldown(Cooldown::Failure));
    assert_eq!(server.hits(), 1);
}

#[test]
fn usage_a_redirect_is_refused_and_suspends_sending() {
    let target = TestServer::http(|_: &Request, out: &mut dyn Write| {
        respond(out, "204 No Content", "", b"");
    })
    .unwrap();
    let location = target.url(PATH);
    let server = TestServer::http(move |_: &Request, out: &mut dyn Write| {
        respond(
            out,
            "307 Temporary Redirect",
            &format!("Location: {location}\r\n"),
            b"",
        );
    })
    .unwrap();
    let host = Host::new("redirect", &server.url(PATH));
    host.first_attempt();
    let counts = host.usage.diagnostics();
    assert_eq!(counts.delivery, Delivery::Suspended(Suspension::Redirect));
    assert_eq!(counts.accepted, 0);
    assert_eq!(server.hits(), 1);
    assert_eq!(target.hits(), 0, "the redirect was never followed");
}

#[test]
fn usage_quitting_shuts_a_stalled_uploads_socket_without_waiting() {
    let stall = Arc::new(Gate::new());
    stall.shut();
    let held = stall.clone();
    let server = TestServer::http(move |_: &Request, out: &mut dyn Write| {
        held.pass();
        respond(out, "204 No Content", "", b"");
    })
    .unwrap();
    let host = Host::new("quit-stalled", &server.url(PATH));
    host.clock.advance(Duration::from_secs(12 * 60));
    stall.wait_reached(1, "the upload at the receiver");
    let quit = Instant::now();
    host.supervisor.shutdown();
    wait_until("the worker to end", || !host.supervisor.running());
    // The request ended while the receiver still held its answer, sooner than the transport's
    // own idle timeout could have ended it: the cancel shut the socket.
    assert!(stall.holding(), "the receiver never answered");
    assert!(
        quit.elapsed() < sender::IDLE_TIMEOUT,
        "ended after {:?}",
        quit.elapsed()
    );
    assert_eq!(host.usage.diagnostics().accepted, 0);
    assert!(host.collector.session().is_none());
    stall.open();
}

/// Both hosted refusals fit the body cap and drive the same clamped rate-limit policy.
#[test]
fn usage_hosted_worker_and_waf_429s_use_the_rate_limit_cooldown() {
    for (name, retry, body, content_type) in [
        ("worker", "60", "too many requests", "text/plain"),
        ("waf", "10", "error code: 1015", "text/plain; charset=UTF-8"),
    ] {
        assert!(body.len() as u64 <= sender::MAX_RESPONSE_BYTES);
        let server = TestServer::http(move |_: &Request, out: &mut dyn Write| {
            respond(
                out,
                "429 Too Many Requests",
                &format!("Retry-After: {retry}\r\nContent-Type: {content_type}\r\n"),
                body.as_bytes(),
            );
        })
        .unwrap();
        let host = Host::new(name, &server.url(PATH));
        host.first_attempt();
        let counts = host.usage.diagnostics();
        assert_eq!(counts.delivery, Delivery::Cooldown(Cooldown::RateLimited));
        assert_eq!((counts.attempts, counts.accepted), (1, 0));
        assert_eq!(server.requests()[0].header("accept"), None);
    }
}
