use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp_catalog() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-json-cli-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn subprocess_client_edits_queries_and_exits_cleanly_on_eof() {
    let catalog = temp_catalog();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg")
        .canonicalize()
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
        .args(["--catalog", catalog.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        input,
        "{}",
        json!({"id":"import","method":"catalog.import","params":{"path":fixture}})
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let imported: Value = serde_json::from_str(&line).unwrap();
    assert!(imported.get("error").is_none());
    let job_id = imported["result"]["job_id"].as_str().unwrap();
    let asset = loop {
        writeln!(
            input,
            "{}",
            json!({"id":"status","method":"job.status","params":{"job_id":job_id}})
        )
        .unwrap();
        input.flush().unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let status: Value = serde_json::from_str(&line).unwrap();
        assert!(status.get("error").is_none(), "{status}");
        match status["result"]["state"].as_str() {
            Some("ready") => break status["result"]["asset"]["asset"]["id"].clone(),
            Some("queued" | "preparing") => std::thread::sleep(std::time::Duration::from_millis(1)),
            other => panic!("unexpected source job {other:?}: {status}"),
        }
    };
    writeln!(
        input,
        "{}",
        json!({"id":"pixel","method":"edit.set-pixel","params":{"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"subprocess-pixel","actor":"subprocess-test"},"x":0,"y":0,"rgb":[12,34,56]}})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"id":"sample","method":"render.sample","params":{"asset_id":asset,"x":0,"y":0}})
    )
    .unwrap();
    input.flush().unwrap();
    for (id, expected) in [("pixel", None), ("sample", Some(json!([12, 34, 56, 255])))] {
        line.clear();
        output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none());
        if let Some(pixel) = expected {
            assert_eq!(response["result"]["rgba"], pixel);
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    std::fs::remove_file(catalog).unwrap();
}

/// Run one `lightwell-json` process over an isolated data root and in-memory secrets, send it
/// `requests` and return its responses.
fn session(data_root: &Path, extra: &[&str], requests: &[Value]) -> Vec<Value> {
    let catalog = temp_catalog();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
        .args(["--catalog", catalog.to_str().unwrap()])
        .args(["--data-root", data_root.to_str().unwrap()])
        .args(["--secret-store", "memory"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in requests {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let _ = std::fs::remove_file(catalog);
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn only_a_client_started_with_permission_authority_may_grant() {
    let data_root = std::env::temp_dir().join(format!(
        "lightwell-json-cli-authority-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let requests = [
        json!({"id": "state", "method": "session.state", "params": {}}),
        json!({"id": "grant", "method": "module.permission.grant", "params": {
            "module_id": "test.missing", "capability": "echo",
            "scope": {}, "request_id": "cli-grant",
        }}),
    ];
    let plain = session(&data_root, &[], &requests);
    assert_eq!(plain[0]["result"]["authority"], json!("edit"));
    assert_eq!(plain[1]["error"]["code"], json!("forbidden"));
    assert_eq!(
        plain[1]["error"]["message"],
        json!("granting a permission needs permission authority")
    );
    // With the flag the same request passes the authority check and reaches the next one.
    let permitted = session(&data_root, &["--permission-authority"], &requests);
    assert_eq!(permitted[0]["result"]["authority"], json!("permissions"));
    assert_eq!(permitted[1]["error"]["code"], json!("validation"));
    assert_eq!(
        permitted[1]["error"]["message"],
        json!("unknown module test.missing")
    );
    assert!(
        !data_root.exists(),
        "neither process created a directory under the data root"
    );
}

/// One interactive `lightwell-json` process: each call writes one request line and reads its
/// answer, so a test can wait on capability jobs between requests.
struct Client {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: BufReader<std::process::ChildStdout>,
    next: u64,
    transcript: Vec<String>,
}

impl Client {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 0,
            transcript: Vec::new(),
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let input = self.input.as_mut().unwrap();
        writeln!(
            input,
            "{}",
            json!({"id": format!("cli-{}", self.next), "method": method, "params": params})
        )
        .unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        self.transcript.push(line.clone());
        serde_json::from_str(&line).unwrap()
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    /// Wait for a capability job to finish; the client polls, the owner never does.
    fn finished(&mut self, job_id: &Value) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let job = self.ok("module.job.read", json!({"job_id": job_id}));
            if !matches!(job["status"].as_str(), Some("queued" | "running")) {
                return job;
            }
            assert!(Instant::now() < deadline, "{job}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Grant the scope a `consent-required` answer names; this client has permission authority.
    fn grant(&mut self, refused: &Value) {
        assert_eq!(refused["error"]["code"], "consent-required", "{refused}");
        let consent = refused["error"]["data"]["consent"].clone();
        let request_id = format!("cli-grant-{}", self.next);
        self.ok(
            "module.permission.grant",
            json!({
                "module_id": consent["module_id"], "capability": consent["capability"],
                "scope": consent["scope"], "request_id": request_id,
            }),
        );
    }

    fn mutation(&mut self, request: &str) -> Value {
        let revision = self.ok(
            "module.settings.read",
            json!({"module_id": "lightwell.capabilities"}),
        )["revision"]
            .clone();
        json!({"expected_revision": revision, "request_id": request, "actor": "cli-test"})
    }

    /// Close standard input, wait for a clean exit and return what the process wrote to stderr.
    fn finish(mut self) -> String {
        drop(self.input.take());
        let output = self.child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

/// One sRGB code tinted by a linear gain, computed independently of the editor in f64.
fn tinted_code(code: u64, gain: f64) -> f64 {
    let encoded = code as f64 / 255.0;
    let linear = if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    };
    let linear = (linear * gain).clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round()
}

#[test]
fn a_proof_endpoint_client_installs_activates_runs_the_task_and_applies_its_tint() {
    let key = format!(
        "CLI-SENTINEL-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let endpoint = lightwell_core::ProofEndpoint::start(&key).unwrap();
    let data_root = std::env::temp_dir().join(format!(
        "lightwell-json-cli-proof-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg")
        .canonicalize()
        .unwrap();
    let base = endpoint.base_url();
    let mut client = Client::start(&[
        "--catalog",
        catalog.to_str().unwrap(),
        "--data-root",
        data_root.to_str().unwrap(),
        "--secret-store",
        "memory",
        "--permission-authority",
        "--proof-endpoint",
        &base,
    ]);
    let imported = client.ok("catalog.import", json!({"path": fixture}));
    let asset = loop {
        let status = client.ok("job.status", json!({"job_id": imported["job_id"]}));
        match status["state"].as_str() {
            Some("ready") => break status["asset"]["asset"]["id"].clone(),
            Some("queued" | "preparing") => std::thread::sleep(Duration::from_millis(1)),
            other => panic!("unexpected source job {other:?}: {status}"),
        }
    };
    let module = "lightwell.capabilities";
    let mutation = client.mutation("cli-profile");
    let profile = client.ok(
        "module.profile.create",
        json!({"module_id": module, "adapter": "proof-echo", "label": "CLI", "mutation": mutation}),
    )["profile"]["id"]
        .clone();
    let mutation = client.mutation("cli-endpoint");
    client.ok(
        "module.settings.set",
        json!({
            "module_id": module, "profile_id": profile,
            "values": {"endpoint": endpoint.generate_url()}, "mutation": mutation,
        }),
    );
    let mutation = client.mutation("cli-key");
    client.ok(
        "module.settings.set-secret",
        json!({
            "module_id": module, "profile_id": profile, "setting": "api-key", "value": key,
            "mutation": mutation,
        }),
    );
    let install = json!({"module_id": module, "resource_id": "proof-palette"});
    let refused = client.call("module.resource.install", install.clone());
    client.grant(&refused);
    let installed = client.ok("module.resource.install", install);
    assert_eq!(client.finished(&installed["job_id"])["status"], "succeeded");
    let activating = client.ok("module.activate", json!({"module_id": module}));
    assert_eq!(
        client.finished(&activating["job_id"])["status"],
        "succeeded"
    );
    let task = json!({"asset_id": asset, "profile_id": profile});
    let mut consents = 0;
    let queued = loop {
        let response = client.call("task.generate-proof-tint", task.clone());
        match response["error"]["code"].as_str() {
            None => break response["result"].clone(),
            Some("consent-required") => {
                consents += 1;
                client.grant(&response);
            }
            Some(_) => panic!("{response}"),
        }
    };
    assert_eq!(consents, 1, "this photo's send");
    let job = client.finished(&queued["job_id"]);
    assert_eq!(job["status"], "succeeded", "{job}");
    let artifact = job["result"]["artifacts"][0].clone();
    let gains = job["result"]["result"]["gains"].clone();
    let sample = json!({"asset_id": asset, "x": 3, "y": 4});
    let before = client.ok("render.sample", sample.clone())["rgba"].clone();
    let applied = client.ok(
        "edit.apply-proof-tint",
        json!({
            "asset_id": asset, "artifact": artifact,
            "mutation": {"expected_revision": 0, "request_id": "cli-apply", "actor": "cli-test"},
        }),
    );
    assert_eq!(applied["outcome"], "applied");
    let after = client.ok("render.sample", sample)["rgba"].clone();
    for channel in 0..3 {
        let expected = tinted_code(
            before[channel].as_u64().unwrap(),
            gains[channel].as_f64().unwrap(),
        );
        let rendered = after[channel].as_f64().unwrap();
        assert!(
            (rendered - expected).abs() <= 1.0,
            "channel {channel}: {rendered} for {expected}"
        );
    }
    assert!(endpoint.requests().iter().any(|request| request.authorized));
    let transcript = client.transcript.join("");
    let stderr = client.finish();
    assert!(!transcript.contains(&key), "a response carries the key");
    assert!(!stderr.contains(&key), "stderr carries the key");
    let mut pending = vec![data_root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(&key),
                    "{} holds the key",
                    path.display()
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(data_root);
}
