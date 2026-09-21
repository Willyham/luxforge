//! Opt-in end-to-end RAW JSON owner coverage.
//!
//! This test deliberately stays ignored in normal CI. It needs the owner's qualified NEF/RAF
//! files and a release-built native RAW adapter. Run it with
//! `LIGHTWELL_RAW_OWNER_DIR=/path/to/private/raw cargo test --release -p lightwell-app
//! --test raw_json_cli -- --ignored --nocapture`.

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
const JOB_TIMEOUT: Duration = Duration::from_secs(180);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const CHILD_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct JsonClient {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<std::io::Result<String>>,
    next_id: u64,
}

fn response_reader(stdout: ChildStdout) -> mpsc::Receiver<std::io::Result<String>> {
    let (sender, receiver) = mpsc::sync_channel(16);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn stop_child(child: &mut Child) {
    let deadline = Instant::now() + CHILD_SHUTDOWN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

impl JsonClient {
    fn start(catalog: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
            .args(["--catalog", catalog.to_str().expect("catalog is UTF-8")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start lightwell-json");
        let mut child = child;
        let input = child.stdin.take().expect("JSON stdin");
        let output = response_reader(child.stdout.take().expect("JSON stdout"));
        Self {
            child: Some(child),
            input: Some(input),
            output,
            next_id: 1,
        }
    }

    fn call_raw(&mut self, method: &str, params: Value) -> Value {
        let id = format!("raw-cli-{}", self.next_id);
        self.next_id += 1;
        let input = self.input.as_mut().expect("JSON process is running");
        writeln!(
            input,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .expect("write JSON request");
        input.flush().expect("flush JSON request");
        let line = self
            .output
            .recv_timeout(RESPONSE_TIMEOUT)
            .unwrap_or_else(|_| panic!("timed out waiting for {method} response"))
            .expect("read JSON response");
        let response: Value = serde_json::from_str(&line).expect("valid JSON response");
        assert_eq!(response["id"], id, "response id for {method}");
        response
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.call_raw(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    fn finish(mut self) {
        drop(self.input.take());
        let mut child = self.child.take().expect("JSON process");
        stop_child(&mut child);
        assert!(
            child
                .try_wait()
                .expect("wait for lightwell-json")
                .expect("lightwell-json did not exit")
                .success()
        );
    }
}

impl Drop for JsonClient {
    fn drop(&mut self) {
        self.input.take();
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child);
        }
    }
}

fn catalog_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-raw-json-{label}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn fixture(root: &Path, names: &[&str]) -> PathBuf {
    names
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "LIGHTWELL_RAW_OWNER_DIR does not contain one of {names:?}: {}",
                root.display()
            )
        })
}

fn asset_id(state: &Value) -> String {
    state["asset"]["id"].as_str().expect("asset id").to_owned()
}

fn current_entry_id(state: &Value) -> String {
    state["current_entry"]["id"]
        .as_str()
        .expect("current entry id")
        .to_owned()
}

fn sample_point(state: &Value) -> (u32, u32) {
    (
        state["asset"]["width"].as_u64().expect("asset width") as u32 / 2,
        state["asset"]["height"].as_u64().expect("asset height") as u32 / 2,
    )
}

fn revision(state: &Value) -> u64 {
    state["revision"].as_u64().expect("revision")
}

fn mutation(state: &Value, request_id: &str) -> Value {
    json!({
        "expected_revision": revision(state),
        "request_id": request_id,
        "actor": "raw-json-cli",
    })
}

fn wait_job(client: &mut JsonClient, job_id: &str) -> Value {
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < JOB_TIMEOUT,
            "RAW source job timed out: {job_id}"
        );
        let response = client.call_raw("job.status", json!({"job_id":job_id}));
        assert!(response.get("error").is_none(), "job.status: {response}");
        let result = &response["result"];
        match result["state"].as_str() {
            Some("queued" | "preparing") => thread::sleep(Duration::from_millis(50)),
            Some("ready") => return result.clone(),
            Some("failed") => panic!("RAW source job failed: {result}"),
            state => panic!("unexpected RAW source job state {state:?}: {result}"),
        }
    }
}

fn import_and_adopt(client: &mut JsonClient, path: &Path) -> Value {
    let imported = client.call(
        "catalog.import",
        json!({"path":path.to_str().expect("fixture is UTF-8")}),
    );
    let job_id = imported["job_id"]
        .as_str()
        .expect("import job id")
        .to_owned();
    let ready = wait_job(client, &job_id);
    let adopted = client.call("job.adopt", json!({"job_id":job_id}));
    assert_eq!(asset_id(&adopted["asset"]), asset_id(&ready["asset"]));
    adopted["asset"].clone()
}

fn sample_after_preparation(
    client: &mut JsonClient,
    asset: &str,
    entry: &str,
    x: u32,
    y: u32,
) -> Value {
    for attempt in 0..4 {
        let sample = client.call_raw("render.sample", json!({"asset_id":asset,"x":x,"y":y}));
        if sample.get("error").is_none() {
            return sample["result"].clone();
        }
        assert_eq!(sample["error"]["code"], "preparation-required", "{sample}");
        // The first request deliberately exercises the public source.prepare path with the
        // entry id. A source file decode and RAW development are separate bounded jobs, so a
        // later preparation-required response may carry the next job id from the owner.
        let job_id = if attempt == 0 {
            let queued = client.call("source.prepare", json!({"asset_id":asset,"entry_id":entry}));
            queued["job_id"]
                .as_str()
                .expect("preparation job id")
                .to_owned()
        } else if let Some(job_id) = sample["error"]["job_id"].as_str() {
            job_id.to_owned()
        } else {
            let queued = client.call("source.prepare", json!({"asset_id":asset,"entry_id":entry}));
            queued["job_id"]
                .as_str()
                .expect("preparation job id")
                .to_owned()
        };
        let ready = wait_job(client, &job_id);
        assert_eq!(ready["state"], "ready");
    }
    panic!("render.sample remained preparation-required after bounded retries")
}

fn raw_action(schema: &Value, keyword: &str, parameter: &str) -> String {
    let methods = schema["methods"].as_object().expect("schema methods");
    let found = methods.iter().filter_map(|(name, method)| {
        if !name.starts_with("edit.") || !name.contains(keyword) {
            return None;
        }
        let parameters = method["parameters"].as_array()?;
        parameters
            .iter()
            .any(|item| item["name"] == parameter)
            .then(|| name.clone())
    });
    let matches: Vec<_> = found.collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one RAW {keyword} action, found {matches:?}"
    );
    matches.into_iter().next().expect("RAW action")
}

fn state(client: &mut JsonClient, asset: &str) -> Value {
    client.call("asset.state", json!({"asset_id":asset}))
}

fn apply_action(
    client: &mut JsonClient,
    method: &str,
    current: &Value,
    request: &str,
    fields: Value,
) -> Value {
    let asset = asset_id(current);
    let mut params = fields.as_object().expect("action fields").clone();
    params.insert("asset_id".into(), Value::String(asset.clone()));
    params.insert("mutation".into(), mutation(current, request));
    let result = client.call(method, Value::Object(params));
    assert!(matches!(
        result["outcome"].as_str(),
        Some("applied" | "no-op")
    ));
    state(client, &asset)
}

fn run_fixture(path: &Path, label: &str) {
    let metadata_before = std::fs::metadata(path).expect("fixture metadata");
    let catalog = catalog_path(label);
    let mut client = JsonClient::start(&catalog);
    let schema = client.call("schema.list", Value::Null);
    let temperature_action = raw_action(&schema, "raw-temperature", "kelvin");
    let tint_action = raw_action(&schema, "raw-tint", "tint");
    assert_eq!(temperature_action, "edit.set-raw-temperature");
    assert_eq!(tint_action, "edit.set-raw-tint");

    let initial = import_and_adopt(&mut client, path);
    let asset = asset_id(&initial);
    let fingerprint = initial["asset"]["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(initial["asset"]["source"]["kind"], "raw");
    let initial_entry = current_entry_id(&initial);
    let (sample_x, sample_y) = sample_point(&initial);
    let initial_sample =
        sample_after_preparation(&mut client, &asset, &initial_entry, sample_x, sample_y);

    let exposed = apply_action(
        &mut client,
        "edit.set-raw-exposure",
        &initial,
        "raw-exposure",
        json!({"ev":1.0}),
    );
    let temperature = apply_action(
        &mut client,
        &temperature_action,
        &exposed,
        "raw-temperature",
        json!({"kelvin":6504.0}),
    );
    let custom_zero = apply_action(
        &mut client,
        &tint_action,
        &temperature,
        "raw-tint-zero",
        json!({"tint":0.0}),
    );
    let custom_zero_entry = current_entry_id(&custom_zero);
    let custom_zero_sample =
        sample_after_preparation(&mut client, &asset, &custom_zero_entry, sample_x, sample_y);
    assert_ne!(
        custom_zero_sample["rgba"], initial_sample["rgba"],
        "RAW controls did not affect sample"
    );

    let positive = apply_action(
        &mut client,
        &tint_action,
        &custom_zero,
        "raw-tint-positive",
        json!({"tint":35.0}),
    );
    let positive_entry = current_entry_id(&positive);
    let positive_sample =
        sample_after_preparation(&mut client, &asset, &positive_entry, sample_x, sample_y);
    let negative = apply_action(
        &mut client,
        &tint_action,
        &positive,
        "raw-tint-negative",
        json!({"tint":-35.0}),
    );
    let negative_entry = current_entry_id(&negative);
    let negative_sample =
        sample_after_preparation(&mut client, &asset, &negative_entry, sample_x, sample_y);
    assert_ne!(
        positive_sample["rgba"], negative_sample["rgba"],
        "Tint signs collapsed"
    );
    let negative_recipe = client.call(
        "recipe.describe",
        json!({"asset_id":asset,"entry_id":negative_entry}),
    );

    let undone_result = client.call(
        "history.undo",
        json!({"asset_id":asset,"mutation":mutation(&negative,"undo-raw-tint")}),
    );
    assert_eq!(undone_result["outcome"], "navigated");
    let undone = state(&mut client, &asset);
    assert_eq!(current_entry_id(&undone), positive_entry);
    let undone_sample =
        sample_after_preparation(&mut client, &asset, &positive_entry, sample_x, sample_y);
    assert_eq!(undone_sample["rgba"], positive_sample["rgba"]);
    let redone_result = client.call(
        "history.redo",
        json!({"asset_id":asset,"mutation":mutation(&undone,"redo-raw-tint")}),
    );
    assert_eq!(redone_result["outcome"], "navigated");
    let redone = state(&mut client, &asset);
    assert_eq!(current_entry_id(&redone), negative_entry);
    let redo_sample =
        sample_after_preparation(&mut client, &asset, &negative_entry, sample_x, sample_y);
    assert_eq!(redo_sample["rgba"], negative_sample["rgba"]);

    let as_shot = apply_action(
        &mut client,
        "edit.use-as-shot-wb",
        &redone,
        "raw-as-shot",
        json!({}),
    );
    let as_shot_entry = current_entry_id(&as_shot);
    let _as_shot_sample =
        sample_after_preparation(&mut client, &asset, &as_shot_entry, sample_x, sample_y);
    let as_shot_recipe = client.call(
        "recipe.describe",
        json!({"asset_id":asset,"entry_id":as_shot_entry}),
    );
    assert_ne!(
        as_shot_recipe["layers"], negative_recipe["layers"],
        "As-shot restore did not change WB"
    );
    let back_to_negative_result = client.call(
        "history.undo",
        json!({"asset_id":asset,"mutation":mutation(&as_shot,"undo-as-shot")}),
    );
    assert_eq!(back_to_negative_result["outcome"], "navigated");
    let back_to_negative = state(&mut client, &asset);
    assert_eq!(current_entry_id(&back_to_negative), negative_entry);
    let baseline_custom = apply_action(
        &mut client,
        &tint_action,
        &back_to_negative,
        "raw-tint-zero-again",
        json!({"tint":0.0}),
    );
    let baseline_entry = current_entry_id(&baseline_custom);
    let baseline_sample =
        sample_after_preparation(&mut client, &asset, &baseline_entry, sample_x, sample_y);

    let version = client.call(
        "version.create",
        json!({"asset_id":asset,"name":"RAW WB","actor":"raw-json-cli","entry_id":baseline_entry}),
    );
    assert_eq!(version["version"]["entry_id"], baseline_entry);
    let wb_recipe = client.call(
        "recipe.describe",
        json!({"asset_id":asset,"entry_id":baseline_entry}),
    );

    let cropped = apply_action(
        &mut client,
        "edit.crop",
        &baseline_custom,
        "raw-crop",
        json!({"x":0.05,"y":0.05,"width":0.8,"height":0.8,"angle":0.0}),
    );
    let rotated = apply_action(
        &mut client,
        "edit.transform",
        &cropped,
        "raw-rotate",
        json!({"transform":"rotate-right"}),
    );
    let rotated_entry = current_entry_id(&rotated);
    let _rotated_sample = sample_after_preparation(&mut client, &asset, &rotated_entry, 0, 0);

    let restore_result = client.call(
        "history.restore",
        json!({"asset_id":asset,"entry_id":baseline_entry,"mutation":mutation(&rotated,"restore-raw-wb")}),
    );
    assert_eq!(restore_result["outcome"], "applied");
    let restored = state(&mut client, &asset);
    assert_ne!(
        current_entry_id(&restored),
        baseline_entry,
        "restore should append history"
    );
    let restored_entry = current_entry_id(&restored);
    let restored_recipe = client.call(
        "recipe.describe",
        json!({"asset_id":asset,"entry_id":restored_entry}),
    );
    assert_eq!(restored_recipe["layers"], wb_recipe["layers"]);
    let restored_sample =
        sample_after_preparation(&mut client, &asset, &restored_entry, sample_x, sample_y);
    assert_eq!(restored_sample["rgba"], baseline_sample["rgba"]);
    let current = client.call("asset.state", json!({"asset_id":asset}));
    assert_eq!(current["asset"]["fingerprint"], fingerprint);
    client.finish();

    // A fresh process has no source cache. Exercise the explicit PreparationRequired ->
    // source.prepare(entry_id) -> job.status -> retry path rather than a direct service call.
    let mut reopened = JsonClient::start(&catalog);
    let reopened_state = reopened.call("asset.state", json!({"asset_id":asset}));
    assert_eq!(asset_id(&reopened_state), asset);
    assert_eq!(current_entry_id(&reopened_state), restored_entry);
    assert_eq!(reopened_state["asset"]["fingerprint"], fingerprint);
    let inspect = reopened.call(
        "source.inspect",
        json!({"asset_id":asset,"entry_id":restored_entry}),
    );
    assert_eq!(inspect["fingerprint"], fingerprint);
    let first_sample = reopened.call_raw(
        "render.sample",
        json!({"asset_id":asset,"x":sample_x,"y":sample_y}),
    );
    assert_eq!(
        first_sample["error"]["code"], "preparation-required",
        "{first_sample}"
    );
    let after_reopen =
        sample_after_preparation(&mut reopened, &asset, &restored_entry, sample_x, sample_y);
    assert_eq!(after_reopen["rgba"], restored_sample["rgba"]);
    let reopened_recipe = reopened.call(
        "recipe.describe",
        json!({"asset_id":asset,"entry_id":restored_entry}),
    );
    assert_eq!(reopened_recipe["layers"], wb_recipe["layers"]);
    let reopened_versions = reopened.call("version.list", json!({"asset_id":asset}));
    assert_eq!(reopened_versions["versions"][0]["entry_id"], baseline_entry);
    reopened.finish();

    let metadata_after = std::fs::metadata(path).expect("fixture metadata after workflow");
    assert_eq!(
        metadata_after.len(),
        metadata_before.len(),
        "RAW source length changed"
    );
    assert_eq!(
        metadata_after.modified().ok(),
        metadata_before.modified().ok()
    );
    std::fs::remove_file(&catalog).expect("remove temporary catalog");
}

#[test]
#[ignore = "requires owner-supplied NEF/RAF fixtures and native RAW support"]
fn raw_json_cli_owner_fixtures_preserve_pipeline_and_history() {
    let owner_dir = PathBuf::from(std::env::var_os("LIGHTWELL_RAW_OWNER_DIR").unwrap_or_else(
        || panic!("LIGHTWELL_RAW_OWNER_DIR is required when running this ignored test"),
    ));
    let z6 = fixture(
        &owner_dir,
        &[
            "nikon_z6.NEF",
            "z6-12-lossless.NEF",
            "z6-14-lossless.NEF",
            "Nikon-Z6-12-lossless.NEF",
        ],
    );
    let fuji = fixture(
        &owner_dir,
        &[
            "fujifilm_x100vi.RAF",
            "x100vi-uncompressed.RAF",
            "x100vi-lossless.RAF",
            "Fujifilm-X100VI-uncompressed.RAF",
        ],
    );
    run_fixture(&z6, "z6");
    run_fixture(&fuji, "fuji");
}
