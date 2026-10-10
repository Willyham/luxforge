//! The wire encoder and strict decoder against hand-written fixtures, the body limits, the device
//! profile's rules and the privacy sentinels.
use super::*;
use crate::usage::{
    Collector, CpuProfile, CpuVendor, GpuBackend, GpuProfile, GpuType, GpuVendor, ImageKey,
    LinuxDistro, MacBuild, MemoryBand, OsFamily, OsProfile, RendererFallback, SessionClock,
    Version,
    device_profile::{AppleTier, Arch},
};
use serde_json::Value;
use std::sync::atomic::Ordering;

const DESIGN_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/usage/valid-design-example.json"
));
const EVERY_EVENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/usage/valid-every-event.json"
));
const INVALID: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/usage/invalid-batches.json"
));

fn reencode(batch: &Batch) -> BodyBuffer {
    let mut body = BodyBuffer::new();
    let envelope = Envelope {
        release: &batch.release,
        installation_id: batch.installation_id,
        session_id: batch.session_id,
        session_start: batch.session_start,
        batch_seq: batch.batch_seq,
        device: &batch.device,
    };
    encode_batch(&envelope, &batch.events, batch.dropped, &mut body).expect("encodes");
    body
}

fn apple_profile() -> DeviceProfile {
    DeviceProfile::new(
        OsProfile {
            family: OsFamily::Macos,
            version: Version::new(&[15, 5]).unwrap(),
            build: MacBuild::parse("24F74"),
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

#[test]
fn usage_the_design_example_validates_and_reencodes_to_the_same_document() {
    let batch = decode_batch(DESIGN_EXAMPLE.as_bytes()).expect("the design's example is valid");
    assert_eq!(batch.batch_seq, 2);
    assert_eq!(batch.events.len(), 4);
    let body = reencode(&batch);
    let ours: Value = serde_json::from_slice(body.as_bytes()).unwrap();
    let example: Value = serde_json::from_str(DESIGN_EXAMPLE).unwrap();
    assert_eq!(ours, example);
}

#[test]
fn usage_a_hand_written_batch_of_every_event_kind_reencodes_byte_for_byte() {
    let expected = EVERY_EVENT.trim_end();
    let batch = decode_batch(expected.as_bytes()).expect("valid");
    assert_eq!(batch.dropped, 3);
    assert_eq!(
        batch.events.iter().map(|e| e.seq).collect::<Vec<_>>()[3..6],
        [3, 5, 6]
    );
    let body = reencode(&batch);
    assert_eq!(std::str::from_utf8(body.as_bytes()).unwrap(), expected);
}

#[test]
fn usage_every_invalid_fixture_is_refused_whole() {
    let cases: Value = serde_json::from_str(INVALID).unwrap();
    let cases = cases.as_array().unwrap();
    assert!(cases.len() > 100);
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let body = case["body"].as_str().unwrap();
        assert!(decode_batch(body.as_bytes()).is_err(), "accepted: {name}");
    }
}

#[test]
fn usage_a_recorded_session_encodes_to_the_independently_written_batch() {
    let collector = Collector::new();
    let (clock, millis) = SessionClock::manual();
    let session = collector.start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        clock,
        ImageKey::from_bytes([0x5a; 32]),
    );
    let key = ImageKey::from_bytes([0x5a; 32]);
    let image = key.image_id(&[0x11; 32]);
    millis.store(1_203_400, Ordering::Release);
    collector.record(EventKind::WorkspaceEnter {
        workspace: Workspace::Develop,
    });
    millis.store(1_210_000, Ordering::Release);
    collector.record(EventKind::ImageOpen { image });
    millis.store(1_219_050, Ordering::Release);
    collector.record(EventKind::ToolCommit {
        tool: Tool::Basic,
        image,
    });
    millis.store(1_260_000, Ordering::Release);
    collector.record(EventKind::ProcessSample(ProcessSample {
        cpu: Percent::from_f64(37.2).unwrap(),
        memory: MemoryMib::from_bytes(2140 << 20),
        memory_kind: MemoryKind::Footprint,
        gpu: None,
        gpu_memory: None,
    }));
    millis.store(1_284_711, Ordering::Release);
    collector.record(EventKind::OperationEnd(OperationEnd::export(
        Outcome::Completed,
        DurationMs::from_millis(812),
        Some(1),
        Some(image),
    )));
    let release = Release::parse("0.0.0+9d4138ca1f2e").unwrap();
    let installation = ReportId::parse("c9a10716-1b78-4cbe-8d1c-b8309e5e7247").unwrap();
    let device = apple_profile();
    let mut body = BodyBuffer::new();
    let window = session.detach().unwrap();
    let encoded = window
        .encode(&release, installation, &device, &mut body)
        .unwrap();
    assert_eq!(
        encoded,
        Encoded {
            sent: 6,
            omitted: 0,
            dropped: 0
        }
    );
    let session_id = String::from_utf8(session.id().to_wire().to_vec()).unwrap();
    let image_hex = String::from_utf8(image.to_hex().to_vec()).unwrap();
    let expected = format!(
        concat!(
            r#"{{"schema":1,"disclosure_version":1,"release":"0.0.0+9d4138ca1f2e","#,
            r#""installation_id":"c9a10716-1b78-4cbe-8d1c-b8309e5e7247","session_id":"{sid}","#,
            r#""session_start":"2026-10-09T14:03:12Z","batch_seq":0,"dropped":0,"#,
            r#""device":{{"os":{{"family":"macos","version":[15,5],"build":"24F74"}},"arch":"arm64","#,
            r#""cpu":{{"vendor":"apple","generation":4,"tier":"pro","physical_cores":12,"logical_cores":12}},"#,
            r#""memory_gib_band":32,"gpus":[{{"vendor":"apple","backend":"metal","type":"integrated","#,
            r#""generation":4,"tier":"pro","unified_memory":true}}],"renderer":"gpu"}},"#,
            r#""events":[{{"seq":0,"t":0,"name":"session.start","trigger":"opt-in"}},"#,
            r#"{{"seq":1,"t":1203400,"name":"workspace.enter","workspace":"develop"}},"#,
            r#"{{"seq":2,"t":1210000,"name":"image.open","image":"{img}"}},"#,
            r#"{{"seq":3,"t":1219050,"name":"tool.commit","tool":"basic","image":"{img}"}},"#,
            r#"{{"seq":4,"t":1260000,"name":"process.sample","cpu_percent":37,"memory_mib":2140,"memory_kind":"footprint"}},"#,
            r#"{{"seq":5,"t":1284711,"name":"operation.end","operation":"export","outcome":"completed","duration_ms":812,"count":1,"image":"{img}"}}]}}"#,
        ),
        sid = session_id,
        img = image_hex,
    );
    assert_eq!(std::str::from_utf8(body.as_bytes()).unwrap(), expected);
    assert!(decode_batch(body.as_bytes()).is_ok());
}

/// The largest records the format allows, alternating samples and operation ends, at seq numbers
/// near the top of the range: 512 of them cannot fit 64 KiB.
fn oversized_window(samples: bool) -> Vec<Event> {
    let image = ImageId::from_bytes([0xab; 16]);
    (0..MAX_EVENTS as u32)
        .map(|index| Event {
            seq: u32::MAX - 600 + index,
            t: u32::MAX - 600 + index,
            kind: if samples && index % 2 == 0 {
                EventKind::ProcessSample(ProcessSample {
                    cpu: Percent::exact(102_400).unwrap(),
                    memory: MemoryMib::exact(16_777_216).unwrap(),
                    memory_kind: MemoryKind::Footprint,
                    gpu: Percent::exact(102_400),
                    gpu_memory: MemoryMib::exact(16_777_216),
                })
            } else {
                EventKind::OperationEnd(OperationEnd::source_preparation(
                    Outcome::Cancelled,
                    DurationMs::from_millis(86_400_000),
                    Some(image),
                ))
            },
        })
        .collect()
}

fn envelope_parts() -> (Release, ReportId, ReportId, SessionStart, DeviceProfile) {
    (
        Release::parse("999999999.999999999.999999999+0123456789ab").unwrap(),
        ReportId::random(),
        ReportId::random(),
        SessionStart::parse("2200-12-31T23:59:59Z").unwrap(),
        apple_profile(),
    )
}

#[test]
fn usage_an_oversized_window_omits_samples_first_then_the_newest_and_counts_them() {
    let (release, installation, session, start, device) = envelope_parts();
    let envelope = Envelope {
        release: &release,
        installation_id: installation,
        session_id: session,
        session_start: start,
        batch_seq: u32::MAX,
        device: &device,
    };
    let mut body = BodyBuffer::new();
    let events = oversized_window(true);
    let encoded = encode_batch(&envelope, &events, 7, &mut body).unwrap();
    assert!(body.as_bytes().len() <= MAX_BODY_BYTES);
    assert_eq!(body.storage_bytes(), MAX_BODY_BYTES, "the body never grew");
    let batch = decode_batch(body.as_bytes()).expect("a valid, complete document");
    assert!(encoded.omitted > 0);
    assert_eq!(encoded.sent + encoded.omitted, MAX_EVENTS);
    assert_eq!(batch.dropped, 7 + encoded.omitted as u32);
    // Only samples were omitted, newest first: every operation end is still there.
    let ends = batch
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::OperationEnd(_)))
        .count();
    assert_eq!(ends, MAX_EVENTS / 2);
    let kept_samples: Vec<u32> = batch
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ProcessSample(_)))
        .map(|e| e.seq)
        .collect();
    let all_samples: Vec<u32> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ProcessSample(_)))
        .map(|e| e.seq)
        .collect();
    assert_eq!(
        kept_samples,
        all_samples[..kept_samples.len()],
        "the oldest samples stay"
    );

    // With no samples to give up, the newest events go.
    let events = oversized_window(false);
    let encoded = encode_batch(&envelope, &events, 0, &mut body).unwrap();
    let batch = decode_batch(body.as_bytes()).expect("valid");
    assert!(encoded.omitted > 0);
    assert_eq!(batch.dropped, encoded.omitted as u32);
    assert_eq!(
        batch.events[..],
        events[..encoded.sent],
        "the oldest prefix, in order"
    );
}

#[test]
fn usage_more_than_512_events_send_the_oldest_512() {
    let (release, installation, session, start, device) = envelope_parts();
    let envelope = Envelope {
        release: &release,
        installation_id: installation,
        session_id: session,
        session_start: start,
        batch_seq: 0,
        device: &device,
    };
    let events: Vec<Event> = (0..600)
        .map(|seq| Event {
            seq,
            t: seq,
            kind: EventKind::AppIdle { state: Idle::Idle },
        })
        .collect();
    let mut body = BodyBuffer::new();
    let encoded = encode_batch(&envelope, &events, 0, &mut body).unwrap();
    assert_eq!(
        (encoded.sent, encoded.omitted, encoded.dropped),
        (512, 88, 88)
    );
    assert_eq!(
        decode_batch(body.as_bytes())
            .unwrap()
            .events
            .last()
            .unwrap()
            .seq,
        511
    );
}

#[test]
fn usage_the_encoder_refuses_empty_and_out_of_order_windows() {
    let (release, installation, session, start, device) = envelope_parts();
    let envelope = Envelope {
        release: &release,
        installation_id: installation,
        session_id: session,
        session_start: start,
        batch_seq: 0,
        device: &device,
    };
    let mut body = BodyBuffer::new();
    assert_eq!(
        encode_batch(&envelope, &[], 3, &mut body),
        Err(EncodeError::Empty)
    );
    let event = |seq, t| Event {
        seq,
        t,
        kind: EventKind::AppIdle { state: Idle::Idle },
    };
    for events in [
        [event(2, 0), event(1, 0)],
        [event(1, 0), event(1, 0)],
        [event(1, 5), event(2, 4)],
    ] {
        assert_eq!(
            encode_batch(&envelope, &events, 0, &mut body),
            Err(EncodeError::Order)
        );
    }
}

#[test]
fn usage_ids_release_and_session_start_accept_only_their_canonical_forms() {
    let id = ReportId::random();
    let wire = String::from_utf8(id.to_wire().to_vec()).unwrap();
    assert_eq!(ReportId::parse(&wire), Some(id));
    assert_ne!(ReportId::random(), id);
    for bad in [
        wire.to_uppercase(),
        wire.replace('-', ""),
        format!("{{{wire}}}"),
        "6ba7b810-9dad-11d1-80b4-00c04fd430c8".to_owned(),
        "00000000-0000-0000-0000-000000000000".to_owned(),
    ] {
        assert_eq!(ReportId::parse(&bad), None, "{bad}");
    }
    for good in [
        "0.0.0+9d4138ca1f2e",
        "1.20.300+0123456789ab",
        "999999999.0.0+abcdefabcdef",
    ] {
        assert_eq!(
            Release::parse(good).map(|r| r.as_str().to_owned()),
            Some(good.to_owned())
        );
    }
    for bad in [
        "0.0.0",
        "0.0+9d4138ca1f2e",
        "0.0.0.0+9d4138ca1f2e",
        "01.0.0+9d4138ca1f2e",
        "0.0.0+9d4138ca1f2",
        "0.0.0+9d4138ca1f2eg",
        "1000000000.0.0+9d4138ca1f2e",
        "-1.0.0+9d4138ca1f2e",
        " 0.0.0+9d4138ca1f2e",
    ] {
        assert_eq!(Release::parse(bad), None, "{bad}");
    }
    // Every day from 2020 to 2200 at a varying second formats and parses back.
    let mut seconds = SessionStart::FIRST;
    while seconds <= SessionStart::LAST {
        let start = SessionStart::from_unix_seconds(seconds).unwrap();
        let wire = start.to_wire();
        assert_eq!(
            SessionStart::parse(std::str::from_utf8(&wire).unwrap()),
            Some(start)
        );
        seconds += 86_400 + 3_607;
    }
    assert_eq!(
        &SessionStart::parse("2024-02-29T23:59:59Z")
            .unwrap()
            .to_wire(),
        b"2024-02-29T23:59:59Z"
    );
    assert_eq!(
        SessionStart::parse("2024-02-29T23:59:59Z")
            .unwrap()
            .unix_seconds(),
        1_709_251_199
    );
    for bad in [
        "2100-02-29T00:00:00Z",
        "2020-13-01T00:00:00Z",
        "2020-01-01T00:60:00Z",
        "2020-1-01T00:00:00Z",
    ] {
        assert_eq!(SessionStart::parse(bad), None, "{bad}");
    }
    assert_eq!(
        SessionStart::from_unix_seconds(SessionStart::FIRST - 1),
        None
    );
    assert_eq!(
        SessionStart::from_unix_seconds(SessionStart::LAST + 1),
        None
    );
}

#[test]
fn usage_the_device_profile_refuses_combinations_the_wire_does_not_allow() {
    let good = apple_profile();
    let os = *good.os();
    let cpu = *good.cpu();
    let gpu = *good.gpus().next().unwrap();
    let build = |os: OsProfile, cpu: CpuProfile, gpus: &[GpuProfile], renderer, fallback| {
        DeviceProfile::new(
            os,
            Arch::Arm64,
            cpu,
            MemoryBand::Gib32,
            gpus,
            renderer,
            fallback,
        )
    };
    let linux = OsProfile {
        family: OsFamily::Linux,
        version: Version::new(&[6, 8]).unwrap(),
        build: None,
        distro: Some(LinuxDistro::Fedora),
        distro_version: Version::new(&[40]),
        kernel: Version::new(&[6, 8, 9]),
    };
    assert!(build(linux, cpu, &[gpu], Renderer::Gpu, None).is_ok());
    assert!(
        build(
            OsProfile {
                build: MacBuild::parse("24F74"),
                ..linux
            },
            cpu,
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            OsProfile {
                kernel: Version::new(&[24]),
                ..os
            },
            cpu,
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            CpuProfile {
                physical_cores: 0,
                ..cpu
            },
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            CpuProfile {
                logical_cores: 1025,
                ..cpu
            },
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            CpuProfile {
                generation: Some(65),
                ..cpu
            },
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            CpuProfile {
                vendor: CpuVendor::Intel,
                ..cpu
            },
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    let intel = CpuProfile {
        vendor: CpuVendor::Intel,
        generation: None,
        tier: None,
        family: Some(6),
        model: Some(158),
        stepping: Some(10),
        ..cpu
    };
    assert!(build(os, intel, &[], Renderer::Gpu, None).is_ok());
    assert!(
        build(
            os,
            CpuProfile {
                stepping: Some(4096),
                ..intel
            },
            &[],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(os, cpu, &[gpu, gpu], Renderer::Gpu, None).is_err(),
        "same model twice"
    );
    let other = GpuProfile {
        vendor: GpuVendor::Amd,
        generation: None,
        tier: None,
        vendor_id: Some(0x1002),
        device_id: Some(0x73bf),
        gpu_type: GpuType::Discrete,
        unified_memory: false,
        memory: Some(MemoryBand::Gib16),
        driver_version: Version::new(&[31, 0, 24027, 1012]),
        ..gpu
    };
    assert!(build(os, cpu, &[gpu, other], Renderer::Gpu, None).is_ok());
    assert!(
        build(
            os,
            cpu,
            &[
                gpu,
                other,
                GpuProfile {
                    device_id: Some(1),
                    ..other
                }
            ],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            cpu,
            &[GpuProfile {
                tier: Some(AppleTier::Max),
                ..other
            }],
            Renderer::Gpu,
            None
        )
        .is_err()
    );
    assert!(
        build(
            os,
            cpu,
            &[],
            Renderer::Gpu,
            Some(RendererFallback::NoAdapter)
        )
        .is_err()
    );
    let reference = build(
        os,
        cpu,
        &[],
        Renderer::Reference,
        Some(RendererFallback::NoAdapter),
    )
    .unwrap();
    assert!(reference.encoded_len() <= MAX_DEVICE_BYTES);
}

#[test]
fn usage_probed_facts_convert_to_profile_values() {
    use luxforge_process::system_info as sys;
    let facts = sys::probe();
    let os = OsProfile::from_facts(&facts.os);
    let cpu = CpuProfile::from_facts(&facts.cpu);
    if let (Some(os), Some(cpu), Some(memory)) = (os, cpu, facts.memory) {
        let profile = DeviceProfile::new(
            os,
            facts.arch.into(),
            cpu,
            memory.into(),
            &[],
            Renderer::Reference,
            Some(RendererFallback::Disabled),
        )
        .expect("this machine's probed facts make a valid profile");
        let mut body = Vec::new();
        profile.write_json(&mut body).unwrap();
        assert!(body.len() <= MAX_DEVICE_BYTES);
    }
    let gpu = luxforge_gpu_types::adapter_facts::normalize(
        &luxforge_gpu_types::adapter_facts::AdapterDescription {
            name: "Apple M4 Pro",
            vendor: 0,
            device: 0,
            device_type: "IntegratedGpu",
            backend: "Metal",
            driver: "",
            driver_info: "",
        },
    );
    let gpu = GpuProfile::from_facts(&gpu, None);
    assert_eq!(
        (gpu.vendor, gpu.backend, gpu.gpu_type),
        (GpuVendor::Apple, GpuBackend::Metal, GpuType::Integrated)
    );
}

/// Every string value in an encoded body is a registry value, the release, an ID, the session
/// start or an image ID; the sentinels, the image key and the raw fingerprint appear nowhere.
#[test]
fn usage_no_sentinel_or_secret_can_reach_the_body() {
    let key = ImageKey::from_bytes(*b"image-key-sentinel-32-bytes-long");
    let fingerprint = [0xfe; 32];
    let image = key.image_id(&fingerprint);
    let (release, installation, session, start, device) = envelope_parts();
    let duration = DurationMs::from_millis(5);
    let kinds = [
        EventKind::SessionStart {
            trigger: Trigger::Launch,
        },
        EventKind::AppPresented {
            state: Presented::Hidden,
        },
        EventKind::AppIdle { state: Idle::Idle },
        EventKind::WorkspaceEnter {
            workspace: Workspace::Select,
        },
        EventKind::ImageOpen { image },
        EventKind::Interaction {
            feature: Feature::PresetApply,
            image: Some(image),
        },
        EventKind::ToolCommit {
            tool: Tool::Mixer,
            image,
        },
        EventKind::ImagesAdded {
            count: AddedCount::new(3).unwrap(),
        },
        EventKind::OperationEnd(OperationEnd::auto_tone(
            Outcome::Failed,
            duration,
            Some(image),
        )),
        EventKind::PreviewSettled {
            image,
            renderer: Renderer::Gpu,
            duration,
        },
        EventKind::ProcessSample(ProcessSample {
            cpu: Percent::exact(1).unwrap(),
            memory: MemoryMib::exact(1).unwrap(),
            memory_kind: MemoryKind::Private,
            gpu: None,
            gpu_memory: None,
        }),
    ];
    let events: Vec<Event> = kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| Event {
            seq: index as u32,
            t: index as u32,
            kind: *kind,
        })
        .collect();
    let envelope = Envelope {
        release: &release,
        installation_id: installation,
        session_id: session,
        session_start: start,
        batch_seq: 0,
        device: &device,
    };
    let mut body = BodyBuffer::new();
    encode_batch(&envelope, &events, 0, &mut body).unwrap();
    let text = std::str::from_utf8(body.as_bytes()).unwrap();
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    for secret in [
        hex(key.as_bytes()),
        hex(&fingerprint),
        hex(&fingerprint[..16]),
        hex(&key.as_bytes()[..16]),
    ] {
        assert!(!text.contains(&secret), "a secret reached the body");
    }
    assert!(!text.contains("image-key-sentinel"));
    for sentinel in [
        "DSC_0001",
        "/Users/",
        "NIKON",
        "agent",
        "Exposure",
        "Warm",
        "error",
        "localhost",
        "192.168.",
        "C02XL0GZ",
        "will",
    ] {
        assert!(!text.contains(sentinel), "{sentinel}");
    }
    let allowed = |value: &str| {
        Tool::parse(value).is_some()
            || Feature::parse(value).is_some()
            || EventName::parse(value).is_some()
            || Trigger::parse(value).is_some()
            || Presented::parse(value).is_some()
            || Idle::parse(value).is_some()
            || Workspace::parse(value).is_some()
            || Operation::parse(value).is_some()
            || Outcome::parse(value).is_some()
            || Renderer::parse(value).is_some()
            || MemoryKind::parse(value).is_some()
            || OsFamily::parse(value).is_some()
            || Arch::parse(value).is_some()
            || CpuVendor::parse(value).is_some()
            || AppleTier::parse(value).is_some()
            || GpuVendor::parse(value).is_some()
            || GpuBackend::parse(value).is_some()
            || GpuType::parse(value).is_some()
            || MacBuild::parse(value).is_some()
            || value == release.as_str()
            || ReportId::parse(value).is_some_and(|id| id == installation || id == session)
            || SessionStart::parse(value) == Some(start)
            || ImageId::parse(value) == Some(image)
    };
    fn strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
        match value {
            Value::String(text) => out.push(text),
            Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
            Value::Object(map) => map.values().for_each(|item| strings(item, out)),
            _ => {}
        }
    }
    let parsed: Value = serde_json::from_str(text).unwrap();
    let mut found = Vec::new();
    strings(&parsed, &mut found);
    assert!(found.len() > 20);
    for value in found {
        assert!(
            allowed(value),
            "an unapproved string reached the body: {value}"
        );
    }
    assert!(decode_batch(body.as_bytes()).is_ok());
}
