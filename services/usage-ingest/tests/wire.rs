//! The strict decoder, against the wire format's every key, enum, range and ordering rule.

mod support;

use luxforge_usage_ingest::wire::{self, MAX_BODY};
use serde_json::{Value, json};
use support::{IMAGE, batch};

fn accepts(value: &Value) -> bool {
    wire::parse(value.to_string().as_bytes()).is_ok()
}

type Mutation = (&'static str, Box<dyn Fn(&mut Value)>);

fn m(label: &'static str, f: impl Fn(&mut Value) + 'static) -> Mutation {
    (label, Box::new(f))
}

#[test]
fn the_design_example_is_accepted() {
    let parsed = wire::parse(batch().to_string().as_bytes()).unwrap();
    assert_eq!(parsed.events.len(), 4);
    assert_eq!(parsed.batch_seq, 2);
}

#[test]
fn whitespace_and_key_order_do_not_matter() {
    let pretty = serde_json::to_vec_pretty(&batch()).unwrap();
    assert!(wire::parse(&pretty).is_ok());
}

#[test]
fn every_event_row_is_accepted_with_its_fields() {
    let mut b = batch();
    b["events"] = json!([
        {"seq": 0, "t": 0, "name": "session.start", "trigger": "launch"},
        {"seq": 1, "t": 0, "name": "session.start", "trigger": "opt-in"},
        {"seq": 2, "t": 1, "name": "app.presented", "state": "hidden"},
        {"seq": 3, "t": 2, "name": "app.idle", "state": "idle"},
        {"seq": 4, "t": 3, "name": "workspace.enter", "workspace": "select"},
        {"seq": 5, "t": 4, "name": "image.open", "image": IMAGE},
        {"seq": 6, "t": 5, "name": "interaction", "feature": "zoom"},
        {"seq": 7, "t": 6, "name": "interaction", "feature": "export", "image": IMAGE},
        {"seq": 8, "t": 7, "name": "tool.commit", "tool": "curve", "image": IMAGE},
        {"seq": 9, "t": 8, "name": "images.added", "count": 1000000},
        {"seq": 10, "t": 9, "name": "operation.end", "operation": "source-preparation",
         "outcome": "failed", "duration_ms": 0, "image": IMAGE},
        {"seq": 11, "t": 9, "name": "operation.end", "operation": "catalog-scan",
         "outcome": "cancelled", "duration_ms": 86400000},
        {"seq": 12, "t": 9, "name": "operation.end", "operation": "export",
         "outcome": "refused", "duration_ms": 5, "count": 0},
        {"seq": 13, "t": 9, "name": "operation.end", "operation": "auto-tone",
         "outcome": "completed", "duration_ms": 5, "image": IMAGE},
        {"seq": 4294967295u64, "t": 4294967295u64, "name": "preview.settled", "image": IMAGE,
         "renderer": "reference", "duration_ms": 12},
    ]);
    assert!(accepts(&b));
    let mut sample = batch();
    sample["events"] = json!([{"seq": 0, "t": 0, "name": "process.sample", "cpu_percent": 102400,
        "memory_mib": 16777216, "memory_kind": "private", "gpu_percent": 0, "gpu_memory_mib": 16777216}]);
    assert!(accepts(&sample));
}

#[test]
fn device_variants_are_accepted() {
    let with = |device: Value| {
        let mut b = batch();
        b["device"] = device;
        b
    };
    assert!(accepts(&with(json!({
        "os": {"family": "linux", "version": [6, 8, 0, 1], "distro": "ubuntu",
               "distro_version": [24, 4], "kernel": [6, 8, 0, 49]},
        "arch": "x86_64",
        "cpu": {"vendor": "amd", "physical_cores": 8, "logical_cores": 16,
                "family": 25, "model": 97, "stepping": 2},
        "memory_gib_band": "over1024",
        "gpus": [
            {"vendor": "nvidia", "backend": "vulkan", "type": "discrete", "unified_memory": false,
             "vendor_id": 4318, "device_id": 9876, "memory_gib_band": 16,
             "driver_version": [550, 54, 14]},
            {"vendor": "amd", "backend": "vulkan", "type": "integrated", "unified_memory": false,
             "vendor_id": 4098, "device_id": 5000}
        ],
        "renderer": "reference", "renderer_fallback": "adapter-lost"
    }))));
    assert!(accepts(&with(json!({
        "os": {"family": "macos", "version": [15, 5], "build": "24F74"},
        "arch": "arm64",
        "cpu": {"vendor": "apple", "physical_cores": 1, "logical_cores": 1024},
        "memory_gib_band": 8, "gpus": [], "renderer": "reference"
    }))));
    assert!(accepts(&with(json!({
        "os": {"family": "windows", "version": [10, 0, 26100]},
        "arch": "other",
        "cpu": {"vendor": "intel", "physical_cores": 4, "logical_cores": 8, "family": 0, "model": 4095},
        "memory_gib_band": 1024,
        "gpus": [{"vendor": "apple", "backend": "other", "type": "other", "unified_memory": true,
                  "generation": 64, "tier": "ultra"},
                 {"vendor": "apple", "backend": "other", "type": "other", "unified_memory": true,
                  "generation": 64, "tier": "max"}],
        "renderer": "gpu"
    }))));
}

#[test]
fn the_batch_may_start_mid_session_and_times_may_repeat() {
    let mut b = batch();
    b["events"] = json!([
        {"seq": 900, "t": 5, "name": "app.idle", "state": "active"},
        {"seq": 905, "t": 5, "name": "app.idle", "state": "idle"},
    ]);
    assert!(accepts(&b));
}

#[test]
fn the_event_count_limits() {
    let event = |seq: u64| json!({"seq": seq, "t": 0, "name": "app.idle", "state": "idle"});
    let mut b = batch();
    b["events"] = (0..512).map(event).collect();
    assert!(accepts(&b));
    b["events"] = (0..513).map(event).collect();
    assert!(!accepts(&b));
    b["events"] = json!([]);
    assert!(!accepts(&b));
}

#[test]
fn the_body_limit_is_exact() {
    let mut text = batch().to_string();
    text.push_str(&" ".repeat(MAX_BODY - text.len()));
    assert_eq!(text.len(), MAX_BODY);
    assert!(wire::parse(text.as_bytes()).is_ok());
    text.push(' ');
    assert!(wire::parse(text.as_bytes()).is_err());
}

#[test]
fn depth_five_containers_is_the_most() {
    // Envelope, device, gpus, gpu object and its driver_version array: five containers deep.
    let mut b = batch();
    b["device"]["gpus"][0]["driver_version"] = json!([1, 2]);
    assert!(accepts(&b));
    // Anything deeper never reaches the parser.
    let mut b = batch();
    b["device"]["gpus"][0]["driver_version"] = json!([[1]]);
    assert!(!accepts(&b));
    let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
    assert!(wire::parse(deep.as_bytes()).is_err());
    let deep = format!("{}1{}", "{\"a\":".repeat(10_000), "}".repeat(10_000));
    assert!(wire::parse(deep.as_bytes()).is_err());
}

#[test]
fn not_json_or_not_an_object_is_refused() {
    for body in [
        &b""[..],
        b"null",
        b"[]",
        b"1",
        b"\"x\"",
        b"{",
        b"\xef\xbb\xbf{}",
        b"\xff\xfe",
        b"{} {}",
        b"{}garbage",
    ] {
        assert!(
            wire::parse(body).is_err(),
            "{:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn duplicate_keys_are_refused() {
    let text = batch()
        .to_string()
        .replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
    assert!(wire::parse(text.as_bytes()).is_err());
    let text = batch().to_string().replacen(
        "\"arch\":\"arm64\"",
        "\"arch\":\"arm64\",\"arch\":\"arm64\"",
        1,
    );
    assert!(wire::parse(text.as_bytes()).is_err());
    let text = batch()
        .to_string()
        .replacen("\"seq\":41", "\"seq\":41,\"seq\":41", 1);
    assert!(wire::parse(text.as_bytes()).is_err());
}

#[test]
fn every_invalid_class_is_refused() {
    let big = |key: &'static str, value: Value| m(key, move |b| b[key] = value.clone());
    let cases: Vec<Mutation> = vec![
        // Envelope.
        m("missing schema", |b| {
            drop(b.as_object_mut().unwrap().remove("schema"))
        }),
        m("missing disclosure", |b| {
            drop(b.as_object_mut().unwrap().remove("disclosure_version"))
        }),
        m("missing release", |b| {
            drop(b.as_object_mut().unwrap().remove("release"))
        }),
        m("missing installation", |b| {
            drop(b.as_object_mut().unwrap().remove("installation_id"))
        }),
        m("missing session", |b| {
            drop(b.as_object_mut().unwrap().remove("session_id"))
        }),
        m("missing start", |b| {
            drop(b.as_object_mut().unwrap().remove("session_start"))
        }),
        m("missing batch_seq", |b| {
            drop(b.as_object_mut().unwrap().remove("batch_seq"))
        }),
        m("missing dropped", |b| {
            drop(b.as_object_mut().unwrap().remove("dropped"))
        }),
        m("missing device", |b| {
            drop(b.as_object_mut().unwrap().remove("device"))
        }),
        m("missing events", |b| {
            drop(b.as_object_mut().unwrap().remove("events"))
        }),
        m("extra envelope key", |b| b["extra"] = json!(1)),
        big("schema", json!(2)),
        big("schema", json!(0)),
        big("schema", json!("1")),
        big("schema", json!(1.0)),
        big("schema", json!(true)),
        big("disclosure_version", json!(2)),
        big("release", json!("0.0.0+9D4138CA1F2E")),
        big("release", json!("1.2.3")),
        big("release", json!("01.2.3+9d4138ca1f2e")),
        big("release", json!(1)),
        big(
            "installation_id",
            json!("C9A10716-1B78-4CBE-8D1C-B8309E5E7247"),
        ),
        big(
            "installation_id",
            json!("c9a10716-1b78-1cbe-8d1c-b8309e5e7247"),
        ),
        big(
            "installation_id",
            json!("c9a10716-1b78-4cbe-0d1c-b8309e5e7247"),
        ),
        big("installation_id", json!(null)),
        big("session_id", json!("not-a-uuid")),
        big("session_id", json!("00000000-0000-0000-0000-000000000000")),
        big("session_start", json!("2026-10-09 14:03:12")),
        big("session_start", json!("2026-02-30T14:03:12Z")),
        big("session_start", json!("2019-12-31T23:59:59Z")),
        big("session_start", json!("2201-01-01T00:00:00Z")),
        big("session_start", json!("2026-10-09T14:03:12+00:00")),
        big("session_start", json!(1760018592)),
        big("batch_seq", json!(-1)),
        big("batch_seq", json!(4294967296u64)),
        big("batch_seq", json!(1.5)),
        big("batch_seq", json!("2")),
        big("dropped", json!(-1)),
        big("dropped", json!(4294967296u64)),
        big("events", json!("x")),
        big("events", json!({})),
        big("device", json!([])),
        // Device.
        m("device extra key", |b| b["device"]["hostname"] = json!("x")),
        m("os extra key", |b| b["device"]["os"]["name"] = json!("x")),
        m("cpu extra key", |b| {
            b["device"]["cpu"]["brand"] = json!("x")
        }),
        m("gpu extra key", |b| {
            b["device"]["gpus"][0]["name"] = json!("x")
        }),
        m("device missing os", |b| {
            drop(b["device"].as_object_mut().unwrap().remove("os"))
        }),
        m("device missing arch", |b| {
            drop(b["device"].as_object_mut().unwrap().remove("arch"))
        }),
        m("device missing cpu", |b| {
            drop(b["device"].as_object_mut().unwrap().remove("cpu"))
        }),
        m("device missing memory", |b| {
            drop(
                b["device"]
                    .as_object_mut()
                    .unwrap()
                    .remove("memory_gib_band"),
            )
        }),
        m("device missing gpus", |b| {
            drop(b["device"].as_object_mut().unwrap().remove("gpus"))
        }),
        m("device missing renderer", |b| {
            drop(b["device"].as_object_mut().unwrap().remove("renderer"))
        }),
        m("os family", |b| {
            b["device"]["os"]["family"] = json!("freebsd")
        }),
        m("os family case", |b| {
            b["device"]["os"]["family"] = json!("MacOS")
        }),
        m("os version empty", |b| {
            b["device"]["os"]["version"] = json!([])
        }),
        m("os version long", |b| {
            b["device"]["os"]["version"] = json!([1, 2, 3, 4, 5])
        }),
        m("os version negative", |b| {
            b["device"]["os"]["version"] = json!([-1])
        }),
        m("os version float", |b| {
            b["device"]["os"]["version"] = json!([1.5])
        }),
        m("os version big", |b| {
            b["device"]["os"]["version"] = json!([4294967296u64])
        }),
        m("os version string", |b| {
            b["device"]["os"]["version"] = json!(["15"])
        }),
        m("os version scalar", |b| {
            b["device"]["os"]["version"] = json!(15)
        }),
        m("os build on macos bad", |b| {
            b["device"]["os"]["build"] = json!("bad")
        }),
        m(
            "os build on windows",
            |b| b["device"]["os"] = json!({"family": "windows", "version": [10], "build": "24F74"}),
        ),
        m("distro on macos", |b| {
            b["device"]["os"]["distro"] = json!("ubuntu")
        }),
        m(
            "distro unknown",
            |b| b["device"]["os"] = json!({"family": "linux", "version": [6], "distro": "gentoo"}),
        ),
        m(
            "kernel on windows",
            |b| b["device"]["os"] = json!({"family": "windows", "version": [10], "kernel": [6]}),
        ),
        m("distro_version on macos", |b| {
            b["device"]["os"]["distro_version"] = json!([1])
        }),
        m("arch", |b| b["device"]["arch"] = json!("riscv")),
        m("cpu vendor", |b| {
            b["device"]["cpu"]["vendor"] = json!("via")
        }),
        m("cpu physical 0", |b| {
            b["device"]["cpu"]["physical_cores"] = json!(0)
        }),
        m("cpu logical 1025", |b| {
            b["device"]["cpu"]["logical_cores"] = json!(1025)
        }),
        m("cpu cores string", |b| {
            b["device"]["cpu"]["logical_cores"] = json!("12")
        }),
        m("cpu generation 0", |b| {
            b["device"]["cpu"]["generation"] = json!(0)
        }),
        m("cpu generation 65", |b| {
            b["device"]["cpu"]["generation"] = json!(65)
        }),
        m("cpu tier", |b| b["device"]["cpu"]["tier"] = json!("mini")),
        m("cpu generation on intel", |b| {
            b["device"]["cpu"]["vendor"] = json!("intel")
        }),
        m("cpu family on apple", |b| {
            b["device"]["cpu"]["family"] = json!(6)
        }),
        m(
            "cpu model out of range",
            |b| {
                b["device"]["cpu"] = json!({"vendor": "intel", "physical_cores": 1, "logical_cores": 1, "model": 4096})
            },
        ),
        m(
            "cpu stepping on qualcomm",
            |b| b["device"]["cpu"] = json!({"vendor": "qualcomm", "physical_cores": 1, "logical_cores": 1, "stepping": 1}),
        ),
        m("memory 12", |b| b["device"]["memory_gib_band"] = json!(12)),
        m("memory 2048", |b| {
            b["device"]["memory_gib_band"] = json!(2048)
        }),
        m("memory over2048", |b| {
            b["device"]["memory_gib_band"] = json!("over2048")
        }),
        m("memory string number", |b| {
            b["device"]["memory_gib_band"] = json!("32")
        }),
        m("memory float", |b| {
            b["device"]["memory_gib_band"] = json!(32.0)
        }),
        m("gpus three", |b| {
            let g = b["device"]["gpus"][0].clone();
            let mut second = g.clone();
            second["tier"] = json!("max");
            let mut third = g.clone();
            third["tier"] = json!("ultra");
            b["device"]["gpus"] = json!([g, second, third]);
        }),
        m("gpus duplicate", |b| {
            let g = b["device"]["gpus"][0].clone();
            b["device"]["gpus"] = json!([g.clone(), g]);
        }),
        m("gpus not array", |b| b["device"]["gpus"] = json!({})),
        m("gpu vendor", |b| {
            b["device"]["gpus"][0]["vendor"] = json!("matrox")
        }),
        m("gpu backend", |b| {
            b["device"]["gpus"][0]["backend"] = json!("opengl")
        }),
        m("gpu type", |b| {
            b["device"]["gpus"][0]["type"] = json!("external")
        }),
        m("gpu unified string", |b| {
            b["device"]["gpus"][0]["unified_memory"] = json!("true")
        }),
        m("gpu unified missing", |b| {
            drop(
                b["device"]["gpus"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("unified_memory"),
            )
        }),
        m("gpu vendor_id big", |b| {
            b["device"]["gpus"][0]["vendor_id"] = json!(65536)
        }),
        m("gpu device_id negative", |b| {
            b["device"]["gpus"][0]["device_id"] = json!(-1)
        }),
        m("gpu tier on nvidia", |b| {
            b["device"]["gpus"][0]["vendor"] = json!("nvidia")
        }),
        m("gpu generation on amd", |b| {
            b["device"]["gpus"][0] = json!({"vendor": "amd", "backend": "vulkan", "type": "discrete",
                                            "unified_memory": false, "generation": 2})
        }),
        m("gpu generation 65", |b| {
            b["device"]["gpus"][0]["generation"] = json!(65)
        }),
        m("gpu memory band", |b| {
            b["device"]["gpus"][0]["memory_gib_band"] = json!(3)
        }),
        m("gpu driver empty", |b| {
            b["device"]["gpus"][0]["driver_version"] = json!([])
        }),
        m("gpu driver long", |b| {
            b["device"]["gpus"][0]["driver_version"] = json!([1, 2, 3, 4, 5])
        }),
        m("renderer", |b| b["device"]["renderer"] = json!("cpu")),
        m("fallback with gpu", |b| {
            b["device"]["renderer_fallback"] = json!("disabled")
        }),
        m("fallback value", |b| {
            b["device"]["renderer"] = json!("reference");
            b["device"]["renderer_fallback"] = json!("because");
        }),
        // Events.
        m("event not object", |b| b["events"][0] = json!(1)),
        m("event unknown name", |b| {
            b["events"][0]["name"] = json!("workspace.leave")
        }),
        m("event missing name", |b| {
            drop(b["events"][0].as_object_mut().unwrap().remove("name"))
        }),
        m("event missing seq", |b| {
            drop(b["events"][0].as_object_mut().unwrap().remove("seq"))
        }),
        m("event missing t", |b| {
            drop(b["events"][0].as_object_mut().unwrap().remove("t"))
        }),
        m("event extra field", |b| b["events"][0]["note"] = json!("x")),
        m("event field of another event", |b| {
            b["events"][0]["image"] = json!(IMAGE)
        }),
        m("event missing field", |b| {
            drop(b["events"][0].as_object_mut().unwrap().remove("workspace"))
        }),
        m("event wrong enum", |b| {
            b["events"][0]["workspace"] = json!("library")
        }),
        m("event null field", |b| {
            b["events"][0]["workspace"] = json!(null)
        }),
        m("seq negative", |b| b["events"][0]["seq"] = json!(-1)),
        m("seq big", |b| b["events"][0]["seq"] = json!(4294967296u64)),
        m("seq float", |b| b["events"][0]["seq"] = json!(41.0)),
        m("seq equal", |b| b["events"][1]["seq"] = json!(41)),
        m("seq decreasing", |b| b["events"][2]["seq"] = json!(40)),
        m("t decreasing", |b| b["events"][1]["t"] = json!(1203399)),
        m("t big", |b| b["events"][3]["t"] = json!(4294967296u64)),
        m("image uppercase", |b| {
            b["events"][1]["image"] = json!(IMAGE.to_uppercase())
        }),
        m("image short", |b| {
            b["events"][1]["image"] = json!(&IMAGE[..31])
        }),
        m("image long", |b| {
            b["events"][1]["image"] = json!(format!("{IMAGE}0"))
        }),
        m("image not hex", |b| {
            b["events"][1]["image"] = json!("g".repeat(32))
        }),
        m("image missing on tool.commit", |b| {
            drop(b["events"][1].as_object_mut().unwrap().remove("image"))
        }),
        m("tool unknown", |b| {
            b["events"][1]["tool"] = json!("sharpen-everything")
        }),
        m("tool case", |b| b["events"][1]["tool"] = json!("Basic")),
        m("feature as tool", |b| {
            b["events"][1]["tool"] = json!("zoom")
        }),
        m("cpu_percent big", |b| {
            b["events"][2]["cpu_percent"] = json!(102401)
        }),
        m("cpu_percent negative", |b| {
            b["events"][2]["cpu_percent"] = json!(-1)
        }),
        m("memory_mib big", |b| {
            b["events"][2]["memory_mib"] = json!(16777217)
        }),
        m("memory_kind", |b| {
            b["events"][2]["memory_kind"] = json!("virtual")
        }),
        m("gpu_percent big", |b| {
            b["events"][2]["gpu_percent"] = json!(102401)
        }),
        m("gpu_memory_mib big", |b| {
            b["events"][2]["gpu_memory_mib"] = json!(16777217)
        }),
        m("operation", |b| {
            b["events"][3]["operation"] = json!("import")
        }),
        m("outcome", |b| b["events"][3]["outcome"] = json!("done")),
        m("duration big", |b| {
            b["events"][3]["duration_ms"] = json!(86400001)
        }),
        m("count big", |b| b["events"][3]["count"] = json!(1000001)),
        m("count on non-export", |b| {
            b["events"][3]["operation"] = json!("auto-tone")
        }),
        m("image on catalog-scan", |b| {
            b["events"][3]["operation"] = json!("catalog-scan")
        }),
        m(
            "images.added zero",
            |b| {
                b["events"][0] =
                    json!({"seq": 41, "t": 1203400, "name": "images.added", "count": 0})
            },
        ),
        m(
            "images.added big",
            |b| {
                b["events"][0] =
                    json!({"seq": 41, "t": 1203400, "name": "images.added", "count": 1000001})
            },
        ),
        m(
            "trigger",
            |b| {
                b["events"][0] =
                    json!({"seq": 41, "t": 1203400, "name": "session.start", "trigger": "resume"})
            },
        ),
        m("preview renderer", |b| {
            b["events"][0] = json!({"seq": 41, "t": 1203400, "name": "preview.settled",
                                    "image": IMAGE, "renderer": "cpu", "duration_ms": 1})
        }),
        m(
            "interaction unknown feature",
            |b| {
                b["events"][0] =
                    json!({"seq": 41, "t": 1203400, "name": "interaction", "feature": "teleport"})
            },
        ),
        m(
            "interaction extra",
            |b| b["events"][0] = json!({"seq": 41, "t": 1203400, "name": "interaction", "feature": "zoom", "tool": "basic"}),
        ),
    ];
    assert!(cases.len() > 120);
    for (label, mutate) in &cases {
        let mut b = batch();
        mutate(&mut b);
        assert!(!accepts(&b), "{label} was accepted");
    }
}
