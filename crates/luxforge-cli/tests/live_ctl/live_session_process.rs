use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, LocalServer, LocalSessionInfo, OwnerHandle,
    live_session_file,
};
use luxforge_testbase::{paths, wait_for};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    thread::JoinHandle,
};

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-ctl");

/// A catalog owner with a live session, as the desktop runs one, and a person's own client of it.
struct Live {
    catalog: PathBuf,
    scratch: PathBuf,
    owner: OwnerHandle,
    person: ClientId,
    server: Option<LocalServer>,
    join: Option<JoinHandle<()>>,
}

impl Live {
    /// Start the owner and its session, the session file at `session_file` (beside the catalog
    /// when `None`). `None` where the host forbids a loopback listener.
    fn start(label: &str, session_file: Option<&Path>) -> Option<Self> {
        let scratch = paths::temp_dir(label);
        let catalog = scratch.join("catalog.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let file = session_file.map_or_else(|| live_session_file(&catalog), Path::to_path_buf);
        let server = match LocalServer::start(owner.clone(), &file) {
            Ok(server) => server,
            Err(error) if error.detail.contains("Operation not permitted") => {
                owner.stop();
                join.join().unwrap();
                return None;
            }
            Err(error) => panic!("cannot start the live session: {error}"),
        };
        let person = owner.register_with(ClientAuthority::Edit);
        Some(Self {
            catalog,
            scratch,
            owner,
            person,
            server: Some(server),
            join: Some(join),
        })
    }

    /// The person's own call, as the desktop makes one.
    fn ask(&self, method: &str, params: Value) -> Value {
        let response = self
            .owner
            .call(
                self.person,
                ApiRequest {
                    id: "person".into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    }

    /// Run `luxforge-ctl --catalog CATALOG args...` to its end.
    fn ctl(&self, args: &[&str]) -> Ran {
        let output = Command::new(BINARY)
            .arg("--catalog")
            .arg(&self.catalog)
            .args(args)
            .output()
            .unwrap();
        Ran {
            status: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// Develop a copy of the JPEG fixture and prepare its photograph, as the desktop prepares the
    /// photograph it opens, each through `luxforge-ctl ... --wait`, answering the copy's path and the
    /// new photograph's identity.
    fn develop(&self) -> (PathBuf, String) {
        let original = self.scratch.join("original.jpg");
        std::fs::copy(paths::jpeg(), &original).unwrap();
        let params = json!({
            "targets": {"kind": "paths", "paths": [original]},
            "into": [],
            "confirm_removable": true,
        });
        let developed = self
            .ctl(&[
                "call",
                "pick.develop",
                "--params",
                &params.to_string(),
                "--wait",
            ])
            .printed();
        assert_eq!(developed["job"]["status"], "ready", "{developed}");
        let asset = developed["job"]["result"]["developed"][0]["asset_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let params = json!({"asset_id": asset}).to_string();
        let prepared = self
            .ctl(&["call", "source.prepare", "--params", &params, "--wait"])
            .printed();
        assert_eq!(prepared["job"]["status"], "ready", "{prepared}");
        (original, asset)
    }

    fn revision(&self, asset: &str) -> u64 {
        self.ask("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// The actors of the asset's history, oldest first.
    fn actors(&self, asset: &str) -> Vec<String> {
        let listed = self.ask("history.list", json!({"asset_id": asset, "limit": 50}));
        let mut actors: Vec<String> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["actor"].as_str().unwrap().to_owned())
            .collect();
        actors.reverse();
        actors
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        drop(self.server.take());
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

struct Ran {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn printed(&self) -> Value {
        assert_eq!(
            (self.status, self.stderr.as_str()),
            (Some(0), ""),
            "{}",
            self.stdout
        );
        serde_json::from_str(&self.stdout).unwrap()
    }

    fn failed(&self) -> Value {
        assert_eq!(
            (self.status, self.stdout.as_str()),
            (Some(1), ""),
            "{}",
            self.stderr
        );
        serde_json::from_str(&self.stderr).unwrap()
    }
}

#[test]
fn live_session_process_reads_schema_and_state_and_fails_without_a_session() {
    let Some(live) = Live::start("ctl-process-read", None) else {
        return;
    };
    let status = live.ctl(&["status"]).printed();
    assert_eq!(
        status["session"]["authority"], "edit",
        "an ordinary live client"
    );
    assert_eq!(status["catalog"]["counts"]["photographs"], 0);
    let entry = live.ctl(&["schema", "edit.set-basic"]).printed();
    assert_eq!(entry["mutation"], "revision");
    assert!(
        live.ctl(&["schema"]).printed()["methods"]
            .as_object()
            .unwrap()
            .contains_key("job.wait")
    );
    // A live client cannot grant consent: that authority is the desktop's alone.
    let refused = live
        .ctl(&[
            "call",
            "module.permission.grant",
            "--params",
            r#"{"module_id": "m", "capability": "download", "scope": {}}"#,
        ])
        .failed();
    assert_eq!(refused["error"]["code"], "forbidden", "{refused}");
    // The same catalog after its owner has gone: no session, and nothing starts one.
    let catalog = live.catalog.clone();
    drop(live);
    let ran = Command::new(BINARY)
        .args(["--catalog", catalog.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert_eq!(
        (ran.status.code(), ran.stdout.as_slice()),
        (Some(1), &b""[..])
    );
    let error: Value = serde_json::from_slice(&ran.stderr).unwrap();
    assert_eq!(error["error"]["code"], "no-session", "{error}");
    assert!(!catalog.exists(), "no owner was started for the catalog");
}

#[test]
fn live_session_process_edits_join_history_and_export_through_a_waited_job() {
    let Some(live) = Live::start("ctl-process-edit", None) else {
        return;
    };
    let (original, asset) = live.develop();
    let source = std::fs::read(&original).unwrap();
    let edited = live
        .ctl(&[
            "call",
            "edit.set-basic",
            "--asset",
            &asset,
            "--params",
            r#"{"exposure": 0.5}"#,
        ])
        .printed();
    assert_eq!(edited["outcome"], "applied", "{edited}");
    // The person commits next, then luxforge-ctl edits on top of it: nothing is lost.
    let revision = live.revision(&asset);
    live.ask(
        "edit.set-basic",
        json!({"asset_id": asset, "contrast": 10,
               "mutation": {"expected_revision": revision, "request_id": "person-1", "actor": "person"}}),
    );
    live.ctl(&[
        "call",
        "edit.set-basic",
        "--asset",
        &asset,
        "--params",
        r#"{"exposure": -0.25}"#,
    ])
    .printed();
    assert_eq!(live.revision(&asset), revision + 2);
    let actors = live.actors(&asset);
    assert_eq!(
        actors[actors.len() - 3..],
        ["luxforge-ctl", "person", "luxforge-ctl"],
        "{actors:?}"
    );
    // An export, a job any client may read, followed to its end with --wait.
    let destination = live.scratch.join("exported.jpg");
    let params = json!({"asset_id": asset, "destination": destination});
    let exported = live
        .ctl(&[
            "call",
            "export.jpeg",
            "--params",
            &params.to_string(),
            "--wait",
        ])
        .printed();
    assert_eq!(exported["job"]["status"], "ready", "{exported}");
    assert!(destination.is_file());
    assert_eq!(
        std::fs::read(&original).unwrap(),
        source,
        "the original is untouched"
    );
}

#[test]
fn live_session_process_conflicts_with_a_competing_commit_and_never_retries() {
    // The owner's session file is elsewhere; the catalog's names a proxy in front of it, which
    // lets the person commit after luxforge-ctl has read the revision and before its edit arrives.
    let real_file = paths::temp_path("ctl-real-session.json");
    let Some(live) = Live::start("ctl-process-conflict", Some(&real_file)) else {
        return;
    };
    std::fs::copy(&real_file, live_session_file(&live.catalog)).unwrap();
    let (_, asset) = live.develop();
    let real: LocalSessionInfo =
        serde_json::from_slice(&std::fs::read(&real_file).unwrap()).unwrap();
    let proxy = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let info = LocalSessionInfo {
        address: proxy.local_addr().unwrap(),
        ..real.clone()
    };
    std::fs::write(
        live_session_file(&live.catalog),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    let owner = live.owner.clone();
    let person = live.person;
    let competing = asset.clone();
    let forwarding = std::thread::spawn(move || {
        let (client, _) = proxy.accept().unwrap();
        let mut to_client = client.try_clone().unwrap();
        let upstream = TcpStream::connect(real.address).unwrap();
        let mut to_owner = upstream.try_clone().unwrap();
        let mut from_owner = BufReader::new(upstream);
        let mut methods = Vec::new();
        for line in BufReader::new(client).lines() {
            let line = line.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap().to_owned();
            if method == "edit.set-basic" {
                let state = owner
                    .call(
                        person,
                        request_for("asset.state", json!({"asset_id": competing})),
                    )
                    .unwrap()
                    .result
                    .unwrap();
                let committed = owner
                    .call(
                        person,
                        request_for(
                            "edit.set-basic",
                            json!({"asset_id": competing, "exposure": 1,
                                   "mutation": {"expected_revision": state["revision"],
                                                "request_id": "competing", "actor": "person"}}),
                        ),
                    )
                    .unwrap();
                assert!(committed.error.is_none(), "{:?}", committed.error);
            }
            methods.push(method);
            writeln!(to_owner, "{line}").unwrap();
            let mut answer = String::new();
            from_owner.read_line(&mut answer).unwrap();
            to_client.write_all(answer.as_bytes()).unwrap();
        }
        methods
    });
    let before = live.revision(&asset);
    let error = live
        .ctl(&[
            "call",
            "edit.set-basic",
            "--asset",
            &asset,
            "--params",
            r#"{"exposure": 0.5}"#,
        ])
        .failed();
    assert_eq!(error["error"]["code"], "conflict", "{error}");
    assert_eq!(
        forwarding.join().unwrap(),
        ["schema.list", "asset.state", "edit.set-basic"],
        "the edit was sent once and never retried"
    );
    assert_eq!(
        live.revision(&asset),
        before + 1,
        "only the person's commit"
    );
    assert_eq!(live.actors(&asset).last().unwrap(), "person");
    let _ = std::fs::remove_file(real_file);
}

fn request_for(method: &str, params: Value) -> ApiRequest {
    ApiRequest {
        id: "competing".into(),
        method: method.into(),
        params,
        token: None,
    }
}

#[test]
fn live_session_process_leaves_a_job_running_when_it_disconnects() {
    let Some(live) = Live::start("ctl-process-disconnect", None) else {
        return;
    };
    let (_, asset) = live.develop();
    let destination = live.scratch.join("left-running.jpg");
    let params = json!({"asset_id": asset, "destination": destination});
    // Without --wait the process ends, closing its connection, as soon as the job is accepted.
    let started = live
        .ctl(&["call", "export.jpeg", "--params", &params.to_string()])
        .printed();
    let job = started["job_id"].as_str().unwrap();
    let finished = wait_for("the export after its client left", || {
        let read = live.ask("job.read", json!({"job_id": job}));
        (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
    });
    assert_eq!(finished["status"], "ready", "{finished}");
    assert!(destination.is_file());
}
