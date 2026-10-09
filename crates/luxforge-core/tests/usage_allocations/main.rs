//! What recording and encoding statistics cost in allocations: none per event, none while off, and
//! none to encode a window into the reserved body.
//!
//! The counter sees every thread of the process, so this binary holds one test and does nothing
//! else while it counts.
use luxforge_core::usage::{
    Arch, BodyBuffer, Collector, CpuProfile, CpuVendor, DeviceProfile, DurationMs, EventKind,
    Feature, Idle, ImageKey, MemoryBand, MemoryKind, MemoryMib, OperationEnd, OsFamily, OsProfile,
    Outcome, Percent, ProcessSample, Recorded, Release, Renderer, ReportId, SessionClock,
    SessionStart, Tool, Trigger, Version,
};
use luxforge_process::allocations::{self, Counting};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

static COLLECTOR: Collector = Collector::new();

const EVENTS: usize = 10_000;

fn flood(image: luxforge_core::usage::ImageId) -> (usize, usize) {
    let mut outcome = (0, 0);
    for index in 0..EVENTS {
        let kind = match index % 4 {
            0 => EventKind::ToolCommit {
                tool: Tool::Basic,
                image,
            },
            1 => EventKind::Interaction {
                feature: Feature::Zoom,
                image: Some(image),
            },
            2 => EventKind::OperationEnd(OperationEnd::export(
                Outcome::Completed,
                DurationMs::from_millis(812),
                Some(1),
                Some(image),
            )),
            _ => EventKind::ProcessSample(ProcessSample {
                cpu: Percent::from_f64(37.0).unwrap(),
                memory: MemoryMib::from_bytes(1 << 31),
                memory_kind: MemoryKind::Footprint,
                gpu: None,
                gpu_memory: None,
            }),
        };
        match COLLECTOR.record(kind) {
            Recorded::Stored(_) => outcome.0 += 1,
            Recorded::Dropped => outcome.1 += 1,
            Recorded::Off => {}
        }
    }
    outcome
}

#[test]
fn usage_recording_and_encoding_allocate_nothing_per_event() {
    let image = ImageKey::from_bytes([3; 32]).image_id(&[4; 32]);

    // Off: producers allocate nothing and the collector owns nothing.
    let before = allocations::allocations();
    assert_eq!(flood(image), (0, 0));
    assert!(COLLECTOR.session().is_none());
    assert_eq!(
        allocations::allocations() - before,
        0,
        "a disabled producer allocated"
    );

    // On: the session's banks are allocated once at start; recording, overflowing and drops after
    // that allocate nothing.
    let session = COLLECTOR.start(
        Trigger::Launch,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::monotonic(),
    );
    let release = Release::parse("0.0.0+9d4138ca1f2e").unwrap();
    let installation = ReportId::random();
    let device = DeviceProfile::new(
        OsProfile {
            family: OsFamily::Macos,
            version: Version::new(&[26, 0, 1]).unwrap(),
            build: None,
            distro: None,
            distro_version: None,
            kernel: None,
        },
        Arch::Arm64,
        CpuProfile {
            vendor: CpuVendor::Apple,
            physical_cores: 10,
            logical_cores: 10,
            generation: Some(4),
            tier: None,
            family: None,
            model: None,
            stepping: None,
        },
        MemoryBand::Gib16,
        &[],
        Renderer::Gpu,
        None,
    )
    .unwrap();
    let mut body = BodyBuffer::new();
    let storage = session.storage_bytes();

    let before = allocations::allocations();
    let (stored, dropped) = flood(image);
    let recorded = allocations::allocations() - before;
    assert_eq!(stored, 511, "the bank holds 512 with session.start");
    assert_eq!(dropped, EVENTS - 511);
    assert_eq!(
        recorded, 0,
        "{recorded} allocations recording {EVENTS} events"
    );

    // Detaching and encoding into the reserved body allocates nothing either.
    let before = allocations::allocations();
    let window = session.detach().unwrap();
    let encoded = window
        .encode(&release, installation, &device, &mut body)
        .unwrap();
    drop(window);
    COLLECTOR.record(EventKind::AppIdle { state: Idle::Idle });
    let encoding = allocations::allocations() - before;
    assert_eq!(encoding, 0, "{encoding} allocations encoding a window");
    assert_eq!(encoded.sent + encoded.omitted, 512);
    assert_eq!(body.storage_bytes(), 65_536);
    assert_eq!(session.storage_bytes(), storage, "the banks never grew");
    assert!(storage + body.storage_bytes() + 2048 <= 256 * 1024);

    // Off again: the generation ends and nothing is recorded or allocated.
    COLLECTOR.stop();
    let before = allocations::allocations();
    assert_eq!(flood(image), (0, 0));
    assert_eq!(allocations::allocations() - before, 0);
    drop(session);
}
