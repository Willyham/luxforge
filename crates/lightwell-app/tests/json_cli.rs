use lightwell_testkit::{JsonProcess, fixtures};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const BINARY: &str = env!("CARGO_BIN_EXE_lightwell-json");
const JOB_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn subprocess_client_edits_queries_and_exits_cleanly_on_eof() {
    let catalog = fixtures::temp_catalog("json-cli");
    let fixture = fixtures::jpeg().canonicalize().unwrap();
    let mut child = Command::new(BINARY)
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
        json!({"id":"import","method":"catalog.import","params":{"path":fixture,"mutation":{"request_id":"import","actor":"subprocess-test"}}})
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
        match status["result"]["status"].as_str() {
            Some("ready") => break status["result"]["asset"]["asset"]["id"].clone(),
            Some("queued" | "running") => std::thread::sleep(std::time::Duration::from_millis(1)),
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
    let catalog = fixtures::temp_catalog("json-cli-session");
    let mut child = Command::new(BINARY)
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
    let data_root = fixtures::temp_path("json-cli-authority");
    let requests = [
        json!({"id": "state", "method": "session.state", "params": {}}),
        json!({"id": "grant", "method": "module.permission.grant", "params": {
            "module_id": "test.missing", "capability": "echo",
            "scope": {}, "mutation": {"request_id": "cli-grant", "actor": "cli-test"},
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

/// Grant the scope a `consent-required` answer names; the client has permission authority.
fn grant(client: &mut JsonProcess, refused: &Value, request_id: &str) {
    assert_eq!(refused["error"]["code"], "consent-required", "{refused}");
    let consent = refused["error"]["data"]["consent"].clone();
    client.call(
        "module.permission.grant",
        json!({
            "module_id": consent["module_id"], "capability": consent["capability"],
            "scope": consent["scope"],
            "mutation": {"request_id": request_id, "actor": "cli-test"},
        }),
    );
}

/// The envelope of a capability settings change, at the settings' current revision.
fn settings_mutation(client: &mut JsonProcess, request: &str) -> Value {
    let revision = client.call(
        "module.settings.read",
        json!({"module_id": "lightwell.capabilities"}),
    )["revision"]
        .clone();
    json!({"expected_revision": revision, "request_id": request, "actor": "cli-test"})
}

/// One sRGB code tinted by a linear gain, computed independently of the editor by the f64
/// reference.
fn tinted_code(code: u64, gain: f64) -> f64 {
    let linear = lightwell_reference::srgb_to_linear(code as u8) * gain;
    (lightwell_reference::srgb_encode(linear.clamp(0.0, 1.0)) * 255.0).round()
}

#[test]
fn a_proof_endpoint_client_installs_activates_runs_the_task_and_applies_its_tint() {
    let key = format!("CLI-SENTINEL-{}", std::process::id());
    let endpoint = lightwell_testkit::ProofEndpoint::start(&key).unwrap();
    let data_root = fixtures::temp_path("json-cli-proof");
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let fixture = fixtures::jpeg().canonicalize().unwrap();
    let base = endpoint.base_url();
    let mut client = JsonProcess::start(
        BINARY,
        &[
            "--catalog",
            catalog.to_str().unwrap(),
            "--data-root",
            data_root.to_str().unwrap(),
            "--secret-store",
            "memory",
            "--permission-authority",
            "--proof-endpoint",
            &base,
        ],
        "cli",
    );
    let imported = client.call(
        "catalog.import",
        json!({"path": fixture, "mutation": {"request_id": "cli-import", "actor": "cli-test"}}),
    );
    let imported = client.settle("job.status", &imported["job_id"], JOB_TIMEOUT);
    assert_eq!(imported["status"], "ready", "{imported}");
    let asset = imported["asset"]["asset"]["id"].clone();
    let module = "lightwell.capabilities";
    let mutation = settings_mutation(&mut client, "cli-profile");
    let profile = client.call(
        "module.profile.create",
        json!({"module_id": module, "adapter": "proof-echo", "label": "CLI", "mutation": mutation}),
    )["profile"]["id"]
        .clone();
    let mutation = settings_mutation(&mut client, "cli-endpoint");
    client.call(
        "module.settings.set",
        json!({
            "module_id": module, "profile_id": profile,
            "values": {"endpoint": endpoint.generate_url()}, "mutation": mutation,
        }),
    );
    let mutation = settings_mutation(&mut client, "cli-key");
    client.call(
        "module.settings.set-secret",
        json!({
            "module_id": module, "profile_id": profile, "setting": "api-key", "value": key,
            "mutation": mutation,
        }),
    );
    let install = |request_id: &str| {
        json!({
            "module_id": module, "resource_id": "proof-palette",
            "mutation": {"request_id": request_id, "actor": "cli-test"},
        })
    };
    let refused = client.call_raw("module.resource.install", install("cli-install-1"));
    grant(&mut client, &refused, "cli-grant-install");
    // After Allow the install is sent again as a new request: the refused one changed nothing.
    let installed = client.call("module.resource.install", install("cli-install-2"));
    assert_eq!(
        client.settle("module.job.read", &installed["job_id"], JOB_TIMEOUT)["status"],
        "ready"
    );
    let activating = client.call(
        "module.activate",
        json!({"module_id": module, "mutation": {"request_id": "cli-activate", "actor": "cli-test"}}),
    );
    assert_eq!(
        client.settle("module.job.read", &activating["job_id"], JOB_TIMEOUT)["status"],
        "ready"
    );
    // A refused request changed nothing, so asking again after Allow keeps its request_id.
    let task = json!({
        "asset_id": asset, "profile_id": profile,
        "mutation": {"request_id": "cli-generate", "actor": "cli-test"},
    });
    let mut consents = 0;
    let queued = loop {
        let response = client.call_raw("task.generate-proof-tint", task.clone());
        match response["error"]["code"].as_str() {
            None => break response["result"].clone(),
            Some("consent-required") => {
                consents += 1;
                grant(&mut client, &response, &format!("cli-grant-{consents}"));
            }
            Some(_) => panic!("{response}"),
        }
    };
    assert_eq!(consents, 1, "this photo's send");
    let retried = client.call("task.generate-proof-tint", task);
    assert_eq!(
        retried["deduplicated"], true,
        "a retry starts no second job"
    );
    assert_eq!(retried["job_id"], queued["job_id"]);
    let job = client.settle("module.job.read", &queued["job_id"], JOB_TIMEOUT);
    assert_eq!(job["status"], "ready", "{job}");
    let artifact = job["result"]["artifacts"][0].clone();
    let gains = job["result"]["result"]["gains"].clone();
    let sample = json!({"asset_id": asset, "x": 3, "y": 4});
    let before = client.call("render.sample", sample.clone())["rgba"].clone();
    let applied = client.call(
        "edit.apply-proof-tint",
        json!({
            "asset_id": asset, "artifact": artifact,
            "mutation": {"expected_revision": 0, "request_id": "cli-apply", "actor": "cli-test"},
        }),
    );
    assert_eq!(applied["outcome"], "applied");
    let after = client.call("render.sample", sample)["rgba"].clone();
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
    let transcript = client.transcript();
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
