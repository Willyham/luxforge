//! `luxforge-ctl` across its process boundary, against a real catalog owner serving a live session
//! from this test process, as the desktop serves one: schema and state, edits recorded in history,
//! Auto tone, a conflict with a competing commit, the running session found through the registry,
//! a batch's drafts on one connection, and jobs followed to their end, left running, or stopped as
//! their client leaves. Narrow a run with `cargo test -p luxforge-cli live_session_process`.

use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, LocalServer, LocalSessionInfo, OwnerHandle,
    live_session_file,
};
use luxforge_testbase::{Gate, paths, wait_for, wait_until};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
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
    /// The gate every source job passes before its work, open unless a test shuts it.
    sources: Arc<Gate>,
}

impl Live {
    /// Start the owner and its session, the session file at `session_file` (beside the catalog
    /// when `None`). `None` where the host forbids a loopback listener and the run declares it
    /// ([`luxforge_testbase::loopback_forbidden`]).
    fn start(label: &str, session_file: Option<&Path>) -> Option<Self> {
        let scratch = paths::temp_dir(label);
        let catalog = scratch.join("catalog.sqlite");
        let sources = Arc::new(Gate::new());
        let hold = sources.clone();
        let (owner, join) =
            OwnerHandle::start_holding_sources(&catalog, Arc::new(move || hold.pass())).unwrap();
        let file = session_file.map_or_else(|| live_session_file(&catalog), Path::to_path_buf);
        let server = match LocalServer::start(owner.clone(), &file) {
            Ok(server) => server,
            Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
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
            sources,
        })
    }

    /// The person's own call, as the desktop makes one, answering its result or its failure.
    fn try_ask(&self, method: &str, params: Value) -> Result<Value, Value> {
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
        match response.error {
            Some(error) => Err(serde_json::to_value(error).unwrap()),
            None => Ok(response.result.unwrap()),
        }
    }

    fn ask(&self, method: &str, params: Value) -> Value {
        self.try_ask(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    /// Run `luxforge-ctl --catalog CATALOG args...` to its end.
    fn ctl(&self, args: &[&str]) -> Ran {
        let mut command = Command::new(BINARY);
        command.arg("--catalog").arg(&self.catalog).args(args);
        Ran::of(command, "")
    }

    /// Develop a copy of the JPEG fixture through `luxforge-ctl ... --wait`, answering the copy's
    /// path and the new photograph's identity. Its source is not prepared yet.
    fn develop_only(&self) -> (PathBuf, String) {
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
        (original, asset)
    }

    /// Develop a copy of the JPEG fixture and prepare its photograph, as the desktop prepares the
    /// photograph it opens, each through `luxforge-ctl ... --wait`.
    fn develop(&self) -> (PathBuf, String) {
        let (original, asset) = self.develop_only();
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

    /// Wait until every live client of the session has gone, each having told the owner it left.
    fn clients_gone(&self) {
        let server = self.server.as_ref().unwrap();
        wait_until("every live client to disconnect", || {
            server.connected() == 0
        });
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.sources.open();
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
    fn of(mut command: Command, stdin: &str) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Self {
            status: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    fn printed(&self) -> Value {
        assert_eq!(
            (self.status, self.stderr.as_str()),
            (Some(0), ""),
            "{}",
            self.stdout
        );
        serde_json::from_str(&self.stdout).unwrap()
    }

    /// The failure on standard error, with nothing on standard output and exit status `status`.
    fn failed(&self, status: i32) -> Value {
        assert_eq!(
            (self.status, self.stdout.as_str()),
            (Some(status), ""),
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
    assert_eq!(
        (&entry["mutation"], &entry["revision_of"]),
        (&json!("revision"), &json!("asset"))
    );
    assert_eq!(
        live.ctl(&["schema", "draft.begin"]).printed()["session_scoped"],
        true
    );
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
        .failed(1);
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
        (Some(3), &b""[..])
    );
    let error: Value = serde_json::from_slice(&ran.stderr).unwrap();
    assert_eq!(error["error"]["code"], "no-session", "{error}");
    assert!(!catalog.exists(), "no owner was started for the catalog");
}

/// Where `luxforge-ctl`, run with `home` as its home directory and every platform's configuration
/// variable under it, finds the registry of running live sessions: the configuration directory
/// `luxforge_cli::Paths` resolves there.
fn home_registry(command: &mut Command, home: &Path) -> PathBuf {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"));
    let config = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Luxforge")
    } else if cfg!(windows) {
        home.join("roaming").join("Luxforge")
    } else {
        home.join("xdg").join("luxforge")
    };
    config.join(luxforge_core::LIVE_SESSIONS_DIR)
}

#[test]
fn live_session_process_finds_the_running_session_without_a_catalog() {
    let Some(mut live) = Live::start("ctl-process-registry", None) else {
        return;
    };
    let home = live.scratch.join("home");
    let command = || {
        let mut command = Command::new(BINARY);
        let registry = home_registry(&mut command, &home);
        command.arg("status");
        (command, registry)
    };
    // Nothing registered: no session, whatever catalog a launch would open.
    let (unregistered, registry) = command();
    let error = Ran::of(unregistered, "").failed(3);
    assert_eq!(error["error"]["code"], "no-session", "{error}");
    live.server
        .as_mut()
        .unwrap()
        .register(&registry, &live.catalog)
        .unwrap();
    // catalog.info names the catalog by its canonical path.
    let catalog = json!(std::fs::canonicalize(&live.catalog).unwrap());
    let status = Ran::of(command().0, "").printed();
    assert_eq!(status["catalog"]["path"], catalog, "{status}");
    // A second running desktop, on another catalog, is never guessed between.
    let mut other = Live::start("ctl-process-registry-other", None).unwrap();
    let other_catalog = other.catalog.clone();
    other
        .server
        .as_mut()
        .unwrap()
        .register(&registry, &other_catalog)
        .unwrap();
    let several = Ran::of(command().0, "").failed(2);
    assert_eq!(several["error"]["code"], "usage", "{several}");
    assert_eq!(
        several["error"]["data"]["sessions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // A desktop that quits removes its entry with its session.
    drop(other);
    assert_eq!(
        Ran::of(command().0, "").printed()["catalog"]["path"],
        catalog
    );
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
    assert_eq!(exported["job"]["ownership"], "catalog", "{exported}");
    assert!(destination.is_file());
    assert_eq!(
        std::fs::read(&original).unwrap(),
        source,
        "the original is untouched"
    );
}

#[test]
fn live_session_process_applies_auto_tone_as_one_entry_in_history() {
    let Some(live) = Live::start("ctl-process-auto-tone", None) else {
        return;
    };
    let (original, asset) = live.develop();
    let source = std::fs::read(&original).unwrap();
    let before = live.revision(&asset);
    let applied = live
        .ctl(&["call", "edit.auto-tone", "--asset", &asset])
        .printed();
    assert_eq!(applied["outcome"], "applied", "{applied}");
    assert_eq!(live.revision(&asset), before + 1, "one undoable entry");
    assert_eq!(live.actors(&asset).last().unwrap(), "luxforge-ctl");
    // Auto tone sets its fields absolutely, so the same request again changes nothing.
    let repeated = live
        .ctl(&["call", "edit.auto-tone", "--asset", &asset])
        .printed();
    assert_eq!(repeated["outcome"], "no-op", "{repeated}");
    assert_eq!(live.revision(&asset), before + 1);
    assert_eq!(std::fs::read(&original).unwrap(), source);
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
        .failed(1);
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
fn live_session_process_leaves_a_catalog_job_running_when_it_disconnects() {
    let Some(live) = Live::start("ctl-process-disconnect", None) else {
        return;
    };
    let (_, asset) = live.develop();
    let destination = live.scratch.join("left-running.jpg");
    let params = json!({"asset_id": asset, "destination": destination});
    // Without --wait the process ends, closing its connection, as soon as the job is accepted. An
    // export belongs to the catalog, so nothing warns that it will be released.
    let started = live
        .ctl(&["call", "export.jpeg", "--params", &params.to_string()])
        .printed();
    let job = started["job_id"].as_str().unwrap();
    let finished = wait_for("the export after its client left", || {
        let read = live.ask("job.read", json!({"job_id": job}));
        (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
    });
    assert_eq!(
        (&finished["status"], &finished["ownership"]),
        (&json!("ready"), &json!("catalog")),
        "{finished}"
    );
    assert!(destination.is_file());
}

#[test]
fn live_session_process_stops_a_job_its_clients_own_when_it_exits() {
    let Some(live) = Live::start("ctl-process-released", None) else {
        return;
    };
    let (_, asset) = live.develop_only();
    let params = json!({"asset_id": asset}).to_string();
    // The preparation is held before its work, so it is still running when luxforge-ctl exits.
    live.sources.shut();
    let ran = live.ctl(&["call", "source.prepare", "--params", &params]);
    assert_eq!(ran.status, Some(0), "{}", ran.stderr);
    let started: Value = serde_json::from_str(&ran.stdout).unwrap();
    let job = started["job_id"].as_str().unwrap().to_owned();
    let warning: Value = serde_json::from_str(&ran.stderr).unwrap();
    assert_eq!(warning["warning"]["code"], "job-released", "{warning}");
    assert_eq!(warning["warning"]["data"]["ownership"], "clients");
    live.clients_gone();
    // A later client cannot read it: it belonged to the client that left.
    let unread = live
        .try_ask("job.read", json!({"job_id": job}))
        .unwrap_err();
    assert_eq!(unread["code"], "validation", "{unread}");
    // Nobody wants it any more, so it stopped and no longer joins: the same request starts a new
    // job, which a second request while it runs joins.
    let renewed = live.ask("source.prepare", json!({"asset_id": asset}));
    let renewed_job = renewed["job_id"].as_str().unwrap();
    assert_ne!(renewed_job, job, "the released job was not joined");
    let joined = live.ask("source.prepare", json!({"asset_id": asset}));
    assert_eq!(joined["job_id"], renewed_job, "a wanted job is joined");
    live.sources.open();
    let finished = wait_for("the person's own preparation", || {
        let read = live.ask("job.read", json!({"job_id": renewed_job}));
        (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
    });
    assert_eq!(
        (&finished["status"], &finished["ownership"]),
        (&json!("ready"), &json!("clients")),
        "{finished}"
    );
}

#[test]
fn live_session_process_batch_carries_a_draft_across_requests_on_one_connection() {
    let Some(live) = Live::start("ctl-process-batch", None) else {
        return;
    };
    let (_, asset) = live.develop();
    let before = live.revision(&asset);
    // A draft lives in its client's session, so a single call cannot use one.
    let refused = live
        .ctl(&[
            "call",
            "draft.begin",
            "--params",
            &json!({"asset_id": asset, "action": "set-basic"}).to_string(),
        ])
        .failed(2);
    assert_eq!(refused["error"]["code"], "usage", "{refused}");
    // The draft's identity is the owner's, so a script names it once it knows it: here the batch
    // is fed one line at a time through a pipe, reading each answer before writing the next.
    let mut child = Command::new(BINARY)
        .arg("--catalog")
        .arg(&live.catalog)
        .arg("batch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut send = |line: Value| -> Value {
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
        let mut answer = String::new();
        output.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    };
    let begun = send(json!({"id": "begin", "method": "draft.begin",
                            "params": {"asset_id": asset, "action": "set-basic"}}));
    assert_eq!(begun["id"], "begin", "{begun}");
    let draft = begun["result"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no draft identity in {begun}"))
        .to_owned();
    let set = send(json!({"method": "draft.set",
                          "params": {"draft_id": draft, "fields": {"exposure": 0.75}}}));
    assert!(set.get("error").is_none(), "{set}");
    let committed = send(
        json!({"method": "draft.commit", "params": {"draft_id": draft},
                                "expected_revision": before}),
    );
    assert!(committed.get("error").is_none(), "{committed}");
    drop(input);
    let finished = child.wait_with_output().unwrap();
    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(live.revision(&asset), before + 1);
    assert_eq!(live.actors(&asset).last().unwrap(), "luxforge-ctl");
}
