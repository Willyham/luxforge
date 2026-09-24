//! The preset library and `edit.apply-preset` through `lightwell-json`, the way an agent drives
//! them: import a Lightroom preset, list it, apply it, undo, make the same edit with the individual
//! `edit.set-*` actions and compare stacks and pixels, capture, create, export, re-import, delete
//! and read the event log. Each test runs on a fresh temporary catalog.

use serde_json::{Map, Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
const JOB_TIMEOUT: Duration = Duration::from_secs(60);
const ACTOR: &str = "presets-json-cli";

/// A fresh `{request_id, actor}` envelope, so every call is a new request.
fn request() -> Value {
    json!({
        "request_id": format!("request-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        "actor": ACTOR,
    })
}

fn temp_catalog() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-presets-json-cli-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn repository(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
        .canonicalize()
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn preset_file(name: &str) -> String {
    std::fs::read_to_string(repository(&format!("fixtures/presets/{name}")))
        .expect("a preset fixture")
}

/// One `lightwell-json` process over one catalog, answering one request per line.
struct JsonClient {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl JsonClient {
    fn start(catalog: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
            .args(["--catalog", catalog.to_str().expect("catalog is UTF-8")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start lightwell-json");
        let input = child.stdin.take().expect("JSON stdin");
        let output = BufReader::new(child.stdout.take().expect("JSON stdout"));
        Self {
            child,
            input: Some(input),
            output,
            next_id: 1,
        }
    }

    /// The whole response line, error or not.
    fn call_raw(&mut self, method: &str, params: Value) -> Value {
        let id = format!("presets-{}", self.next_id);
        self.next_id += 1;
        let input = self.input.as_mut().expect("the process is running");
        writeln!(
            input,
            "{}",
            json!({"id": id, "method": method, "params": params})
        )
        .expect("write a request");
        input.flush().expect("flush a request");
        let mut line = String::new();
        self.output.read_line(&mut line).expect("read a response");
        let response: Value = serde_json::from_str(&line).expect("a JSON response");
        assert_eq!(response["id"], json!(id), "response id for {method}");
        response
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.call_raw(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    fn error(&mut self, method: &str, params: Value) -> Value {
        let response = self.call_raw(method, params);
        assert!(response.get("result").is_none(), "{method}: {response}");
        response["error"].clone()
    }

    /// Import a file and wait for its verified source; returns the asset record.
    fn import(&mut self, path: &Path) -> Value {
        let job = self.call(
            "catalog.import",
            json!({"path": path, "mutation": request()}),
        )["job_id"]
            .clone();
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < JOB_TIMEOUT, "import timed out");
            let status = self.call("job.status", json!({"job_id": job}));
            match status["state"].as_str() {
                Some("ready") => return status["asset"]["asset"].clone(),
                Some("queued" | "preparing") => std::thread::sleep(Duration::from_millis(5)),
                other => panic!("unexpected source job {other:?}: {status}"),
            }
        }
    }

    fn state(&mut self, asset: &Value) -> Value {
        self.call("asset.state", json!({"asset_id": asset}))
    }

    fn mutation(&mut self, asset: &Value, request_id: &str) -> Value {
        let revision = self.state(asset)["revision"].clone();
        json!({"expected_revision": revision, "request_id": request_id, "actor": ACTOR})
    }

    fn samples(&mut self, asset: &Value, points: &[(u64, u64)]) -> Vec<Value> {
        points
            .iter()
            .map(|(x, y)| {
                self.call("render.sample", json!({"asset_id": asset, "x": x, "y": y}))["rgba"]
                    .clone()
            })
            .collect()
    }

    fn finish(mut self) {
        drop(self.input.take());
        assert!(self.child.wait().expect("lightwell-json exits").success());
    }
}

impl Drop for JsonClient {
    fn drop(&mut self) {
        if self.input.take().is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A stack as its effects, formats and payloads in order: what two stacks built by different
/// requests share, since every commit mints fresh layer identities.
fn contents(state: &Value) -> Vec<Value> {
    state["current_entry"]["snapshot"]["recipe"]["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .map(|layer| json!([layer["effect_id"], layer["effect_format"], layer["payload"]]))
        .collect()
}

fn history(client: &mut JsonClient, asset: &Value) -> Vec<Value> {
    client.call("history.list", json!({"asset_id": asset, "limit": 100}))["entries"]
        .as_array()
        .expect("entries")
        .clone()
}

fn object(value: &Value) -> &Map<String, Value> {
    value.as_object().expect("a JSON object")
}

#[test]
fn a_preset_imports_applies_like_its_individual_actions_and_round_trips_through_the_library() {
    let catalog = temp_catalog();
    let mut client = JsonClient::start(&catalog);
    let asset_record = client.import(&repository("fixtures/s0/orientation-1.jpg"));
    let asset = asset_record["id"].clone();
    let (width, height) = (
        asset_record["width"].as_u64().expect("width"),
        asset_record["height"].as_u64().expect("height"),
    );
    let points = [
        (0, 0),
        (width / 4, height / 3),
        (width / 2, height / 2),
        (3 * width / 4, height / 5),
        (width - 1, height - 1),
    ];
    let original = client.samples(&asset, &points);

    // 1. Import a Lightroom XMP preset, then list the library.
    let develop = preset_file("develop.xmp");
    let imported = client.call(
        "preset.import",
        json!({"content": develop, "mutation": request(), "file_name": "develop.xmp"}),
    );
    let record = &imported["preset"];
    assert_eq!(imported["report"], record["report"]);
    assert_eq!(record["name"], json!("Soft Film"));
    assert_eq!(record["group"], json!("Synthetic Looks"));
    assert_eq!(record["origin"]["kind"], json!("lightroom-xmp"));
    assert_eq!(record["origin"]["file_name"], json!("develop.xmp"));
    assert_eq!(record["unavailable"], json!([]));
    let listed = client.call("preset.list", json!({}))["presets"].clone();
    assert_eq!(listed.as_array().expect("presets").len(), 1);
    let row = &listed[0];
    assert_eq!(row["id"], record["id"]);
    assert_eq!(row["settings"], record["settings"]);
    let count = |list: &str| record["report"][list].as_array().expect("a list").len();
    assert_eq!(
        row["report"],
        json!({
            "mapped": count("mapped"),
            "neutral": count("neutral"),
            "unsupported": count("unsupported"),
            "refused": count("refused"),
        }),
        "a listing reduces the report to its counts"
    );
    assert!(
        row.get("source_text").is_none(),
        "a listing carries no text"
    );
    let read = client.call("preset.read", json!({"preset_id": row["id"]}))["preset"].clone();
    assert_eq!(read["source_text"], json!(develop), "kept verbatim");
    assert_eq!(read["report"], record["report"]);
    let settings = row["settings"].clone();
    let name = row["name"].clone();
    // The relative white balance maps to Basic, so the preset sets temperature and tint too.
    for field in ["temperature", "tint", "exposure", "contrast"] {
        assert!(
            settings["set-basic"][field].is_number(),
            "{field}: {settings}"
        );
    }
    for action in ["set-mixer", "set-presence", "set-vignette"] {
        assert!(settings.get(action).is_some(), "{action}: {settings}");
    }

    // 2. Apply it with the listed settings, name and identity: one entry, then undo it.
    let mutation = client.mutation(&asset, "apply");
    let applied = client.call(
        "edit.apply-preset",
        json!({
            "asset_id": asset,
            "mutation": mutation,
            "settings": settings,
            "name": name,
            "preset-id": row["id"],
        }),
    );
    assert_eq!(applied["outcome"], json!("applied"));
    let preset_entry = applied["created_entry_id"].clone();
    let entries = history(&mut client, &asset);
    assert_eq!(entries.len(), 2, "the import entry and one preset entry");
    assert_eq!(entries[0]["id"], preset_entry);
    assert_eq!(entries[0]["label"], json!("Preset: Soft Film"));
    assert_eq!(entries[0]["action_id"], json!("apply-preset"));
    assert_eq!(entries[0]["parameters"]["preset-id"], row["id"]);
    assert_eq!(entries[0]["parameters"]["settings"], settings);
    let preset_state = client.state(&asset);
    let preset_stack = contents(&preset_state);
    assert_eq!(preset_stack.len(), 4, "Basic, mixer, Presence and vignette");
    let preset_pixels = client.samples(&asset, &points);
    assert_ne!(preset_pixels, original, "the preset changes the photo");
    let mutation = client.mutation(&asset, "undo");
    client.call(
        "history.undo",
        json!({"asset_id": asset, "mutation": mutation}),
    );
    assert!(contents(&client.state(&asset)).is_empty());
    assert_eq!(client.samples(&asset, &points), original);

    // 3. The same fields through the individual actions give the same stack and pixels.
    for (action, fields) in object(&settings) {
        let mut params = object(fields).clone();
        params.insert("asset_id".into(), asset.clone());
        params.insert(
            "mutation".into(),
            client.mutation(&asset, &format!("individual-{action}")),
        );
        let result = client.call(&format!("edit.{action}"), Value::Object(params));
        assert_eq!(result["outcome"], json!("applied"), "{action}");
    }
    assert_eq!(
        history(&mut client, &asset).len(),
        2 + object(&settings).len()
    );
    assert_eq!(contents(&client.state(&asset)), preset_stack);
    assert_eq!(client.samples(&asset, &points), preset_pixels);

    // 4. Capture the fields back, create a native preset, export it and re-import the export.
    let fields: Map<String, Value> = object(&settings)
        .iter()
        .map(|(action, values)| {
            let names: Vec<Value> = object(values).keys().map(|name| json!(name)).collect();
            (action.clone(), Value::Array(names))
        })
        .collect();
    let captured = client.call(
        "preset.capture",
        json!({"asset_id": asset, "fields": fields}),
    )["settings"]
        .clone();
    for (action, values) in object(&settings) {
        for (field, value) in object(values) {
            assert_eq!(
                captured[action][field].as_f64(),
                value.as_f64(),
                "{action}.{field}"
            );
        }
    }
    // The historical preset entry holds the same values, read by its identity.
    let at_preset_entry = client.call(
        "preset.capture",
        json!({"asset_id": asset, "fields": fields, "entry_id": preset_entry}),
    )["settings"]
        .clone();
    assert_eq!(at_preset_entry, captured);
    let created = client.call(
        "preset.create",
        json!({"name": "Captured film", "settings": captured, "mutation": request()}),
    )["preset"]
        .clone();
    assert_eq!(created["settings"], captured);
    assert_eq!(created["group"], json!("User presets"));
    assert_eq!(created["origin"], json!({"kind": "lightwell"}));
    assert_eq!(created["report"], Value::Null);
    let exported = client.call("preset.export", json!({"preset_id": created["id"]}));
    assert_eq!(exported["file_name"], json!("Captured film.lwpreset"));
    let reimported = client.call(
        "preset.import",
        json!({
            "content": exported["content"],
            "mutation": request(),
            "file_name": exported["file_name"],
            "name": "Captured film copy",
        }),
    );
    assert_eq!(reimported["preset"]["settings"], created["settings"]);
    assert_eq!(reimported["preset"]["group"], created["group"]);
    assert_eq!(reimported["preset"]["name"], json!("Captured film copy"));
    assert_eq!(reimported["preset"]["origin"], json!({"kind": "lightwell"}));
    assert_eq!(reimported["report"]["format"], json!("lightwell"));
    assert_eq!(
        client.call(
            "preset.read",
            json!({"preset_id": reimported["preset"]["id"]})
        )["preset"]["source_text"],
        exported["content"]
    );

    // 5. Delete, then read the event log: every library write and nothing else.
    let delete = json!({"preset_id": created["id"], "mutation": request()});
    let deleted = client.call("preset.delete", delete.clone());
    assert_eq!(
        deleted,
        json!({"outcome": "applied", "deleted": true, "deduplicated": false})
    );
    // A retry of the same request is its first answer, and changes and announces nothing.
    let retried = client.call("preset.delete", delete);
    assert_eq!(
        retried,
        json!({"outcome": "applied", "deleted": true, "deduplicated": true})
    );
    // A new request for a preset that is gone is a no-op.
    let again = client.call(
        "preset.delete",
        json!({"preset_id": created["id"], "mutation": request()}),
    );
    assert_eq!(
        again,
        json!({"outcome": "no-op", "deleted": false, "deduplicated": false})
    );
    let events = client.call("events.since", json!({"after": 0}));
    assert_eq!(events["gap"], json!(false));
    let preset_events: Vec<&str> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["method"].as_str())
        .filter(|method| method.starts_with("preset."))
        .collect();
    assert_eq!(
        preset_events,
        [
            "preset.import",
            "preset.create",
            "preset.import",
            "preset.delete"
        ],
        "reads, capture, export and the no-op delete emit nothing"
    );
    let names: Vec<Value> = client.call("preset.list", json!({}))["presets"]
        .as_array()
        .expect("presets")
        .iter()
        .map(|preset| json!([preset["group"], preset["name"]]))
        .collect();
    assert_eq!(
        names,
        [
            json!(["Synthetic Looks", "Soft Film"]),
            json!(["User presets", "Captured film copy"]),
        ]
    );
    client.finish();
    std::fs::remove_file(catalog).expect("the catalog is removed");
}

#[test]
fn an_unsupported_or_unmappable_file_is_refused_and_nothing_is_stored() {
    let catalog = temp_catalog();
    let mut client = JsonClient::start(&catalog);
    for (file, message) in [
        ("profile.xmp", "Lightroom profiles are not presets"),
        (
            "legacy.xmp",
            "the preset has no setting Lightwell can apply",
        ),
    ] {
        let error = client.error(
            "preset.import",
            json!({"content": preset_file(file), "mutation": request(), "file_name": file}),
        );
        assert_eq!(error["code"], json!("unsupported-input"), "{file}: {error}");
        assert!(
            error["message"]
                .as_str()
                .expect("a message")
                .contains(message),
            "{file}: {error}"
        );
    }
    // The dry run refuses the profile too, but reports a file that maps nothing in full.
    let error = client.error(
        "preset.inspect",
        json!({"content": preset_file("profile.xmp")}),
    );
    assert_eq!(error["code"], json!("unsupported-input"));
    let inspected = client.call(
        "preset.inspect",
        json!({"content": preset_file("legacy.xmp"), "file_name": "legacy.xmp"}),
    );
    assert_eq!(inspected["preset"]["id"], Value::Null);
    assert_eq!(inspected["preset"]["settings"], json!({}));
    assert_eq!(inspected["preset"]["report"], inspected["report"]);
    assert!(
        !inspected["report"]["refused"]
            .as_array()
            .expect("refused")
            .is_empty()
    );
    assert_eq!(
        client.call("preset.list", json!({})),
        json!({"presets": []}),
        "nothing was stored"
    );
    let events = client.call("events.since", json!({"after": 0}));
    assert_eq!(
        events["events"],
        json!([]),
        "a refused import emits nothing"
    );
    client.finish();
    std::fs::remove_file(catalog).expect("the catalog is removed");
}
