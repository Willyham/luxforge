//! The `luxforge-json` process itself: a session over standard input and output that edits,
//! queries and exits cleanly at end of input, permission authority, the developer-only test
//! modules, and a proof-endpoint client installing, running and applying the capability proof.
use luxforge_testbase::{ProofEndpoint, paths, wait_for};
use luxforge_testkit::{JsonProcess, proof_protocol};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-json");

#[test]
fn subprocess_client_edits_queries_and_exits_cleanly_on_eof() {
    let catalog = paths::temp_catalog("json-cli");
    let fixture = paths::jpeg().canonicalize().unwrap();
    // The pixel proof is a test module, served only with --developer.
    let mut child = Command::new(BINARY)
        .args(["--catalog", catalog.to_str().unwrap(), "--developer"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    // Open the fixture as a client does: develop it, then prepare its photograph.
    let asset = {
        let mut next = 0;
        let mut ask = |method: &str, params: Value| -> Value {
            next += 1;
            writeln!(
                input,
                "{}",
                json!({"id": format!("open-{next}"), "method": method, "params": params})
            )
            .unwrap();
            input.flush().unwrap();
            line.clear();
            output.read_line(&mut line).unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert!(response.get("error").is_none(), "{method}: {response}");
            response["result"].clone()
        };
        let started = ask(
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": [fixture]},
                "into": [],
                "confirm_removable": true,
                "mutation": {"request_id": "develop", "actor": "subprocess-test"},
            }),
        );
        let developed = wait_for("the Develop", || {
            let status = ask("job.read", json!({"job_id": started["job_id"]}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        });
        assert_eq!(developed["status"], "ready", "{developed}");
        let asset = developed["result"]["developed"][0]["asset_id"].clone();
        let prepared = ask("source.prepare", json!({"asset_id": asset}));
        let ready = wait_for("the preparation", || {
            let status = ask("job.read", json!({"job_id": prepared["job_id"]}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        });
        assert_eq!(ready["status"], "ready", "{ready}");
        asset
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
    assert!(paths::wal(&catalog).exists(), "the open catalog's log");
    drop(input);
    assert!(child.wait().unwrap().success());
    // The owner closed the catalog on the way out, which checkpointed its log and removed it.
    assert!(!paths::wal(&catalog).exists());
    assert!(!paths::shm(&catalog).exists());
    std::fs::remove_file(catalog).unwrap();
}

/// Run one `luxforge-json` process over an isolated data root and in-memory secrets, send it
/// `requests` and return its responses.
fn session(data_root: &Path, extra: &[&str], requests: &[Value]) -> Vec<Value> {
    let catalog = paths::temp_catalog("json-cli-session");
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
    let data_root = paths::temp_path("json-cli-authority");
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

/// The headless owner draws nothing and has no GPU stage, so every client's session reports the
/// reference renderer with no reason, whatever its authority, and `schema.list` says what the
/// field's values mean. No request can claim another renderer.
#[test]
fn the_headless_owner_reports_the_reference_renderer() {
    let data_root = paths::temp_path("json-cli-renderer");
    let requests = [
        json!({"id": "state", "method": "session.state", "params": {}}),
        json!({"id": "claim", "method": "workspace.set", "params": {
            "renderer": {"record": "gpu", "reason": null},
        }}),
        json!({"id": "again", "method": "session.state", "params": {}}),
        json!({"id": "schema", "method": "schema.list", "params": {}}),
    ];
    for extra in [&[][..], &["--permission-authority"][..]] {
        let answers = session(&data_root, extra, &requests);
        let headless = json!({"record": "reference", "reason": null});
        assert_eq!(answers[0]["result"]["renderer"], headless, "{extra:?}");
        assert_eq!(answers[1]["error"]["code"], json!("validation"));
        assert_eq!(answers[2]["result"]["renderer"], headless);
        assert_eq!(
            answers[3]["result"]["renderer"]["records"],
            json!(["gpu", "reference"])
        );
    }
    assert!(!data_root.exists());
}

/// `luxforge-json` serves the test modules only with `--developer`, whatever its build profile:
/// without it `schema.list` names no method a developer-only module generates and `module.list` no
/// such module; with it the list gains exactly those. A proof endpoint without it is refused
/// before anything starts, in the desktop's words.
#[test]
fn test_modules_are_served_only_with_developer() {
    let data_root = paths::temp_path("json-cli-developer");
    let requests = [
        json!({"id": "schema", "method": "schema.list", "params": {}}),
        json!({"id": "modules", "method": "module.list", "params": {}}),
    ];
    // Every method `schema.list` lists, and every method the developer-only modules generate:
    // `edit.<action>`, `query.<query>` and `task.<task>`.
    let listed = |extra: &[&str]| {
        let responses = session(&data_root, extra, &requests);
        let mut methods: Vec<String> = responses[0]["result"]["methods"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        methods.sort();
        let mut developer_ids = Vec::new();
        let mut generated = Vec::new();
        for module in responses[1]["result"]["modules"].as_array().unwrap() {
            if module["developer"] != json!(true) {
                continue;
            }
            developer_ids.push(module["id"].as_str().unwrap().to_owned());
            for (key, prefix) in [("actions", "edit"), ("queries", "query"), ("tasks", "task")] {
                for declared in module[key].as_array().into_iter().flatten() {
                    generated.push(format!("{prefix}.{}", declared["id"].as_str().unwrap()));
                }
            }
        }
        generated.sort();
        (methods, developer_ids, generated)
    };
    let (ordinary, ordinary_developer, _) = listed(&[]);
    assert!(ordinary_developer.is_empty(), "{ordinary_developer:?}");
    let (developer, developer_ids, generated) = listed(&["--developer"]);
    assert_eq!(developer_ids, ["luxforge.pixel", "luxforge.controls"]);
    assert!(
        generated.contains(&"edit.set-pixel".to_owned()),
        "{generated:?}"
    );
    let mut added: Vec<String> = developer
        .iter()
        .filter(|method| !ordinary.contains(method))
        .cloned()
        .collect();
    added.sort();
    assert_eq!(
        added, generated,
        "developer mode adds exactly the test modules' methods"
    );
    assert!(ordinary.iter().all(|method| developer.contains(method)));

    let refused = Command::new(BINARY)
        .args([
            "--catalog",
            "unused.sqlite",
            "--proof-endpoint",
            "http://127.0.0.1:9",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(
        error,
        json!({"error": {"code": "startup", "message": "--proof-endpoint requires developer mode (--developer)"}})
    );
    assert!(!data_root.exists());
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
        json!({"module_id": "luxforge.capabilities"}),
    )["revision"]
        .clone();
    json!({"expected_revision": revision, "request_id": request, "actor": "cli-test"})
}

/// One sRGB code tinted by a linear gain, computed independently of the editor by the f64
/// reference.
fn tinted_code(code: u64, gain: f64) -> f64 {
    let linear = luxforge_reference::srgb::decode(code as u8) * gain;
    (luxforge_reference::srgb::encode_nonnegative(linear.clamp(0.0, 1.0)) * 255.0).round()
}

#[test]
fn a_proof_endpoint_client_installs_runs_the_task_and_applies_its_tint() {
    let key = format!("CLI-SENTINEL-{}", std::process::id());
    let endpoint = ProofEndpoint::start(&key, proof_protocol()).unwrap();
    let data_root = paths::temp_path("json-cli-proof");
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let fixture = paths::jpeg().canonicalize().unwrap();
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
            "--developer",
            "--proof-endpoint",
            &base,
        ],
        "cli",
    );
    let asset = client.open(&fixture, "cli-test")["asset"]["id"].clone();
    let module = "luxforge.capabilities";
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
        client.settle("job.read", &installed["job_id"])["status"],
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
    let job = client.settle("job.read", &queued["job_id"]);
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
