//! The `luxforge-ctl` command line against a scripted session: what each command sends, the envelopes
//! it builds from `schema.list`, `--wait` and `batch` on one connection, the warning for a job it
//! leaves to be released, and what reaches standard output and standard error with which exit
//! status.
use super::scripted_session::{Endpoint, Reply, schema};
use crate::live::command::{ACTOR, run};
use crate::live::*;
use luxforge_testbase::paths::{temp_catalog, temp_path};
use serde_json::{Value, json};
use std::ffi::OsString;

struct Ran {
    status: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The one JSON line on standard output, with nothing on standard error.
    fn printed(&self) -> Value {
        assert_eq!(
            (self.status, self.stderr.as_str()),
            (0, ""),
            "{}",
            self.stdout
        );
        assert_eq!(self.stdout.lines().count(), 1, "{}", self.stdout);
        serde_json::from_str(&self.stdout).unwrap()
    }

    /// The one JSON line on standard error, with nothing on standard output, and the exit status
    /// its code calls for.
    fn failed(&self) -> Value {
        assert_eq!(self.stdout, "", "{}", self.stderr);
        assert_eq!(self.stderr.lines().count(), 1, "{}", self.stderr);
        let error: Value = serde_json::from_str(&self.stderr).unwrap();
        let code = error["error"]["code"].as_str().unwrap();
        assert_eq!(self.status, exit_status(code), "{error}");
        error
    }

    /// Each JSON line on standard output, with nothing on standard error.
    fn lines(&self) -> Vec<Value> {
        assert_eq!(self.stderr, "", "{}", self.stdout);
        self.stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn ctl_with(args: &[&str], stdin: &str) -> Ran {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = run(
        args.iter().map(OsString::from),
        &mut stdin.as_bytes(),
        &mut stdout,
        &mut stderr,
        None,
    );
    Ran {
        status,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

fn ctl_in(endpoint: &Endpoint, args: &[&str], stdin: &str) -> Ran {
    let mut all = vec!["--catalog", endpoint.catalog.to_str().unwrap()];
    all.extend_from_slice(args);
    ctl_with(&all, stdin)
}

fn ctl(endpoint: &Endpoint, args: &[&str]) -> Ran {
    ctl_in(endpoint, args, "")
}

/// A session answering each request after its handshake through `then`.
fn session(mut then: impl FnMut(&Value) -> Reply + Send + 'static) -> Endpoint {
    Endpoint::serve(move |_, request| then(request))
}

fn methods(requests: &[Value]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect()
}

fn assert_envelope(mutation: &Value, expected_revision: Option<u64>) {
    let request_id = uuid::Uuid::parse_str(mutation["request_id"].as_str().unwrap()).unwrap();
    assert_eq!(request_id.get_version_num(), 4);
    assert_eq!(mutation["actor"], ACTOR);
    assert_eq!(
        mutation.get("expected_revision").and_then(Value::as_u64),
        expected_revision
    );
}

#[test]
fn luxforge_ctl_commands_refuse_bad_command_lines_without_connecting() {
    for args in [
        &[][..],
        &["frobnicate"],
        &["call"],
        &["status", "extra"],
        &["schema", "a", "b"],
        &["batch", "extra"],
        &["status", "--wait"],
        &["batch", "--params", "{}"],
        &["schema", "--params", "{}"],
        &["call", "m", "--bogus"],
        &["call", "m", "--params", "{}", "--params", "{}"],
        &["call", "m", "--asset", "a", "--expected-revision", "1"],
        &["call", "m", "--asset", "a", "--asset", "b"],
        &["call", "m", "--expected-revision", "-1"],
        &["call", "m", "--expected-revision"],
        &["call", "m", "--wait", "--wait"],
        &["--catalog", "a", "--catalog", "b", "status"],
        // No configuration to find a running session in, and no catalog named.
        &["status"],
    ] {
        // Without a configuration no session can be found, so any connection attempt would fail
        // differently; a usage failure proves none was made.
        let error = ctl_with(args, "").failed();
        assert_eq!(error["error"]["code"], USAGE, "{args:?}: {error}");
        assert_eq!(exit_status(USAGE), 2);
    }
}

#[test]
fn luxforge_ctl_commands_answer_help_and_version_without_a_session() {
    for args in [&["--help"][..], &["call", "m", "-h"], &["status", "--help"]] {
        let ran = ctl_with(args, "");
        assert_eq!((ran.status, ran.stderr.as_str()), (0, ""));
        assert!(
            ran.stdout
                .contains("luxforge-ctl [--catalog PATH] call METHOD")
        );
        assert!(ran.stdout.contains("luxforge-ctl [--catalog PATH] batch"));
    }
    let ran = ctl_with(&["--version"], "");
    assert_eq!(
        (ran.status, ran.stdout.trim()),
        (0, concat!("luxforge-ctl ", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn luxforge_ctl_commands_fail_on_a_catalog_without_a_session_with_the_session_status() {
    let catalog = temp_catalog("ctl-no-session");
    for command in ["status", "batch"] {
        let ran = ctl_with(&["--catalog", catalog.to_str().unwrap(), command], "");
        let error = ran.failed();
        assert_eq!(error["error"]["code"], NO_SESSION, "{error}");
        assert_eq!(ran.status, EXIT_SESSION);
    }
    assert!(!catalog.exists(), "no owner was started for it");
}

#[test]
fn luxforge_ctl_commands_status_reads_the_session_and_the_catalog() {
    let endpoint = session(|request| match request["method"].as_str() {
        Some("session.state") => Reply::Result(json!({"authority": "edit"})),
        Some("catalog.info") => Reply::Result(json!({"format": 13})),
        other => panic!("unexpected {other:?}"),
    });
    let printed = ctl(&endpoint, &["status"]).printed();
    assert_eq!(
        printed,
        json!({"session": {"authority": "edit"}, "catalog": {"format": 13}})
    );
    assert_eq!(
        methods(&endpoint.requests()),
        ["schema.list", "session.state", "catalog.info"]
    );
}

#[test]
fn luxforge_ctl_commands_schema_prints_the_handshakes_list_or_one_method() {
    let endpoint = session(|request| panic!("{} was sent", request["method"]));
    assert_eq!(&ctl(&endpoint, &["schema"]).printed(), schema());
    assert_eq!(methods(&endpoint.requests()), ["schema.list"]);
    let endpoint = session(|request| panic!("{} was sent", request["method"]));
    let entry = ctl(&endpoint, &["schema", "edit.set-basic"]).printed();
    assert_eq!(entry, schema()["methods"]["edit.set-basic"]);
    assert_eq!(entry["revision_of"], "asset");
    drop(endpoint);
    let endpoint = session(|request| panic!("{} was sent", request["method"]));
    let error = ctl(&endpoint, &["schema", "no.such"]).failed();
    assert_eq!(error["error"]["code"], "validation");
}

#[test]
fn luxforge_ctl_commands_pass_a_read_through_and_refuse_what_does_not_apply() {
    let endpoint = session(|_| Reply::Result(json!({"read": true})));
    let printed = ctl(
        &endpoint,
        &["call", "asset.state", "--params", r#"{"asset_id":"a"}"#],
    )
    .printed();
    assert_eq!(printed, json!({"read": true}));
    let requests = endpoint.requests();
    assert_eq!(methods(&requests), ["schema.list", "asset.state"]);
    assert_eq!(requests[1]["params"], json!({"asset_id": "a"}));
    // Each refusal is made after the handshake and before the method is sent.
    for (args, code) in [
        (&["call", "no.such"][..], "validation"),
        (&["call", "catalog.info", "--asset", "a"], "validation"),
        (
            &["call", "catalog.info", "--expected-revision", "1"],
            "validation",
        ),
        (&["call", "preset.create", "--asset", "a"], "validation"),
        (
            &["call", "preset.create", "--params", r#"{"mutation":{}}"#],
            "validation",
        ),
        (&["call", "edit.set-basic"], "validation"),
        (
            &["call", "edit.set-basic", "--expected-revision", "1"],
            "validation",
        ),
        (
            &[
                "call",
                "edit.set-basic",
                "--asset",
                "a",
                "--params",
                r#"{"asset_id":"b"}"#,
            ],
            "validation",
        ),
        (
            &["call", "module.settings.set", "--asset", "a"],
            "validation",
        ),
        (&["call", "module.settings.set"], "validation"),
        // A method whose effect ends with the connection belongs in a batch.
        (
            &[
                "call",
                "draft.begin",
                "--params",
                r#"{"asset_id":"a","action":"set-basic"}"#,
            ],
            USAGE,
        ),
        (&["call", "draft.commit", "--expected-revision", "1"], USAGE),
    ] {
        let endpoint = session(|request| panic!("{} was sent", request["method"]));
        let error = ctl(&endpoint, args).failed();
        assert_eq!(error["error"]["code"], code, "{args:?}: {error}");
        assert_eq!(methods(&endpoint.requests()), ["schema.list"], "{args:?}");
    }
}

#[test]
fn luxforge_ctl_commands_read_params_inline_from_a_file_or_stdin_as_one_object() {
    let file = temp_path("ctl-params.json");
    std::fs::write(&file, r#"{"name": "From file"}"#).unwrap();
    let at_file = format!("@{}", file.display());
    for (params, stdin, name) in [
        (r#"{"name": "Inline"}"#, "", "Inline"),
        (at_file.as_str(), "", "From file"),
        ("-", r#"{"name": "Piped"}"#, "Piped"),
    ] {
        let endpoint = session(|_| Reply::Result(json!({"deduplicated": false})));
        ctl_in(
            &endpoint,
            &["call", "preset.create", "--params", params],
            stdin,
        )
        .printed();
        let requests = endpoint.requests();
        assert_eq!(requests[1]["params"]["name"], name);
        assert_envelope(&requests[1]["params"]["mutation"], None);
    }
    std::fs::remove_file(file).unwrap();
    // Parameters are checked before anything connects: there is no session here.
    let catalog = temp_catalog("ctl-params-unconnected");
    let catalog = catalog.to_str().unwrap();
    for (params, code) in [
        ("[1]", "validation"),
        ("{", USAGE),
        ("@/no/such/file", USAGE),
    ] {
        let error = ctl_with(&["--catalog", catalog, "call", "m", "--params", params], "").failed();
        assert_eq!(error["error"]["code"], code, "{params}: {error}");
    }
}

#[test]
fn luxforge_ctl_commands_send_an_asset_edit_at_its_current_revision_and_never_retry() {
    let endpoint = session(|request| match request["method"].as_str() {
        Some("asset.state") => Reply::Result(json!({"revision": 7})),
        Some("edit.set-basic") => Reply::Result(json!({"outcome": "applied", "revision": 8})),
        other => panic!("unexpected {other:?}"),
    });
    let printed = ctl(
        &endpoint,
        &[
            "call",
            "edit.set-basic",
            "--asset",
            "asset-1",
            "--params",
            r#"{"exposure": 1}"#,
        ],
    )
    .printed();
    assert_eq!(printed["revision"], 8);
    let requests = endpoint.requests();
    assert_eq!(
        methods(&requests),
        ["schema.list", "asset.state", "edit.set-basic"]
    );
    assert_eq!(requests[1]["params"], json!({"asset_id": "asset-1"}));
    let params = &requests[2]["params"];
    assert_eq!(
        (&params["asset_id"], &params["exposure"]),
        (&json!("asset-1"), &json!(1))
    );
    assert_envelope(&params["mutation"], Some(7));

    // A competing edit between the read and the edit is the owner's conflict, reported whole.
    let conflict =
        json!({"code": "conflict", "message": "stale revision 7; current revision is 8"});
    let answer = conflict.clone();
    let endpoint = session(move |request| match request["method"].as_str() {
        Some("asset.state") => Reply::Result(json!({"revision": 7})),
        _ => Reply::Error(answer.clone()),
    });
    let ran = ctl(&endpoint, &["call", "edit.set-basic", "--asset", "asset-1"]);
    assert_eq!(ran.failed(), json!({"error": conflict}));
    assert_eq!(ran.status, EXIT_FAILURE);
    assert_eq!(
        methods(&endpoint.requests()),
        ["schema.list", "asset.state", "edit.set-basic"],
        "sent once, never retried"
    );
}

#[test]
fn luxforge_ctl_commands_send_a_given_revision_and_a_request_envelope() {
    let endpoint = session(|_| Reply::Result(json!({"revision": 4})));
    let values = r#"{"module_id": "m", "values": {"a": 1}}"#;
    ctl(
        &endpoint,
        &[
            "call",
            "module.settings.set",
            "--expected-revision",
            "3",
            "--params",
            values,
        ],
    )
    .printed();
    let requests = endpoint.requests();
    assert_eq!(requests[1]["params"]["values"], json!({"a": 1}));
    assert_envelope(&requests[1]["params"]["mutation"], Some(3));
}

#[test]
fn luxforge_ctl_commands_report_an_unanswered_mutation_as_unknown_and_a_read_as_lost() {
    let endpoint = session(|_| Reply::Close);
    let ran = ctl(
        &endpoint,
        &["call", "preset.create", "--params", r#"{"name":"x"}"#],
    );
    let error = ran.failed();
    assert_eq!(ran.status, EXIT_OUTCOME_UNKNOWN);
    let requests = endpoint.requests();
    assert_eq!(
        methods(&requests),
        ["schema.list", "preset.create"],
        "never resent"
    );
    assert_eq!(error["error"]["code"], OUTCOME_UNKNOWN, "{error}");
    assert_eq!(
        error["error"]["data"],
        json!({"method": "preset.create",
               "request_id": requests[1]["params"]["mutation"]["request_id"]})
    );
    let endpoint = session(|_| Reply::Close);
    let ran = ctl(&endpoint, &["call", "catalog.info"]);
    assert_eq!(ran.failed()["error"]["code"], CONNECTION_LOST);
    assert_eq!(ran.status, EXIT_FAILURE);
}

/// A session whose job is queued, then running, then `last`, each `job.wait` answering the next
/// change, and whose `job.read` names the job's `ownership`.
fn job_session(method: &'static str, ownership: &'static str, last: Value) -> Endpoint {
    let mut change = 0;
    session(move |request| match request["method"].as_str() {
        Some(called) if called == method => {
            Reply::Result(json!({"job_id": "job-1", "status": "queued"}))
        }
        Some("job.wait") => {
            change += 1;
            let job = match change {
                1 => json!({"job_id": "job-1", "status": "running"}),
                _ => last.clone(),
            };
            Reply::Result(json!({"change": change, "job": job}))
        }
        Some("job.read") => Reply::Result(json!({"job_id": "job-1", "kind": "prepare",
                                                 "status": "running", "ownership": ownership})),
        other => panic!("unexpected {other:?}"),
    })
}

fn exporting(last: Value) -> Endpoint {
    job_session("export.jpeg", "catalog", last)
}

const EXPORT: &[&str] = &[
    "call",
    "export.jpeg",
    "--params",
    r#"{"asset_id": "a", "destination": "/tmp/out.jpg"}"#,
    "--wait",
];

#[test]
fn luxforge_ctl_commands_wait_follows_a_job_to_its_end_on_one_connection() {
    let ready = json!({"job_id": "job-1", "status": "ready", "result": {"path": "/tmp/out.jpg"}});
    let endpoint = exporting(ready.clone());
    let printed = ctl(&endpoint, EXPORT).printed();
    assert_eq!(
        printed,
        json!({"call": {"job_id": "job-1", "status": "queued"}, "job": ready})
    );
    // The scripted session accepts one connection only, so every wait was made on it.
    let requests = endpoint.requests();
    assert_eq!(
        methods(&requests),
        ["schema.list", "export.jpeg", "job.wait", "job.wait"]
    );
    let hold = luxforge_core::MAX_EVENT_WAIT_MS;
    assert_eq!(
        requests[2]["params"],
        json!({"job_id": "job-1", "timeout_ms": hold})
    );
    assert_eq!(
        requests[3]["params"],
        json!({"job_id": "job-1", "timeout_ms": hold, "after": 1})
    );
    // Without --wait the call's own answer is printed, and a job that belongs to the catalog
    // needs no warning.
    let endpoint = exporting(ready);
    let printed = ctl(&endpoint, &EXPORT[..4]).printed();
    assert_eq!(printed, json!({"job_id": "job-1", "status": "queued"}));
    assert_eq!(
        methods(&endpoint.requests()),
        ["schema.list", "export.jpeg", "job.read"]
    );
}

#[test]
fn luxforge_ctl_commands_warn_that_a_job_its_requesters_own_is_released_on_exit() {
    let ready = json!({"job_id": "job-1", "status": "ready"});
    let endpoint = job_session("source.prepare", "clients", ready.clone());
    let ran = ctl(
        &endpoint,
        &["call", "source.prepare", "--params", r#"{"asset_id": "a"}"#],
    );
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    let printed: Value = serde_json::from_str(&ran.stdout).unwrap();
    assert_eq!(printed, json!({"job_id": "job-1", "status": "queued"}));
    let warning: Value = serde_json::from_str(&ran.stderr).unwrap();
    assert_eq!(warning["warning"]["code"], JOB_RELEASED, "{warning}");
    assert_eq!(warning["warning"]["job_id"], "job-1");
    assert_eq!(
        warning["warning"]["data"],
        json!({"kind": "prepare", "status": "running", "ownership": "clients"})
    );
    assert!(
        warning["warning"]["message"]
            .as_str()
            .unwrap()
            .contains("--wait")
    );
    // Followed to its end, nothing is left to release.
    let endpoint = job_session("source.prepare", "clients", ready);
    ctl(
        &endpoint,
        &[
            "call",
            "source.prepare",
            "--params",
            r#"{"asset_id": "a"}"#,
            "--wait",
        ],
    )
    .printed();
}

#[test]
fn luxforge_ctl_commands_wait_reports_a_job_that_did_not_finish_ready() {
    let failed = json!({"job_id": "job-1", "status": "failed",
                        "error": {"code": "render", "message": "out of memory", "data": {"n": 1}}});
    let endpoint = exporting(failed.clone());
    let error = ctl(&endpoint, EXPORT).failed();
    assert_eq!(
        error,
        json!({"error": {"code": "render", "message": "out of memory", "job_id": "job-1",
                         "data": {"n": 1}},
               "job": failed})
    );
    let cancelled = json!({"job_id": "job-1", "status": "cancelled"});
    let endpoint = exporting(cancelled);
    let error = ctl(&endpoint, EXPORT).failed();
    assert_eq!(error["error"]["code"], "cancelled");
}

#[test]
fn luxforge_ctl_commands_batch_sends_each_line_over_one_connection_and_carries_on() {
    let endpoint = session(|request| match request["method"].as_str() {
        Some("draft.begin") => Reply::Result(json!({"draft_id": "draft-1"})),
        Some("draft.set") => Reply::Result(json!({"ok": true})),
        Some("asset.state") => Reply::Result(json!({"revision": 2})),
        Some("draft.commit") => Reply::Error(json!({"code": "conflict", "message": "stale"})),
        Some("edit.set-basic") => Reply::Result(json!({"revision": 3})),
        other => panic!("unexpected {other:?}"),
    });
    let input = [
        r#"{"id": 1, "method": "draft.begin", "params": {"asset_id": "a", "action": "set-basic"}}"#,
        "",
        r#"{"method": "draft.set", "params": {"draft_id": "draft-1", "fields": {"exposure": 1}}}"#,
        r#"{"id": "commit", "method": "draft.commit", "params": {"draft_id": "draft-1"}, "expected_revision": 2}"#,
        r#"not json"#,
        r#"{"id": "both", "method": "edit.set-basic", "asset": "a", "expected_revision": 1}"#,
        r#"{"id": "edit", "method": "edit.set-basic", "params": {"exposure": 1}, "asset": "a"}"#,
    ]
    .join("\n");
    let ran = ctl_in(&endpoint, &["batch"], &input);
    let lines = ran.lines();
    assert_eq!(lines.len(), 6, "{}", ran.stdout);
    assert_eq!(
        lines[0],
        json!({"id": 1, "result": {"draft_id": "draft-1"}})
    );
    assert_eq!(lines[1], json!({"result": {"ok": true}}));
    assert_eq!(lines[2]["id"], "commit");
    assert_eq!(lines[2]["error"]["code"], "conflict");
    assert_eq!(lines[3]["error"]["code"], USAGE);
    assert_eq!(
        (&lines[4]["id"], &lines[4]["error"]["code"]),
        (&json!("both"), &json!(USAGE))
    );
    assert_eq!(lines[5], json!({"id": "edit", "result": {"revision": 3}}));
    assert_eq!(ran.status, EXIT_USAGE, "the highest status of its requests");
    let requests = endpoint.requests();
    assert_eq!(
        methods(&requests),
        [
            "schema.list",
            "draft.begin",
            "draft.set",
            "draft.commit",
            "asset.state",
            "edit.set-basic"
        ],
        "one handshake, every request on one connection"
    );
    assert_envelope(&requests[3]["params"]["mutation"], Some(2));
    assert_envelope(&requests[5]["params"]["mutation"], Some(2));
}

#[test]
fn luxforge_ctl_commands_batch_waits_warns_bounds_its_lines_and_stops_on_a_lost_connection() {
    let ready = json!({"job_id": "job-1", "status": "ready"});
    let endpoint = job_session("source.prepare", "clients", ready.clone());
    let input = [
        r#"{"method": "source.prepare", "params": {"asset_id": "a"}, "wait": true}"#.to_owned(),
        format!(
            r#"{{"method": "catalog.info", "params": {{"pad": "{}"}}}}"#,
            "x".repeat(luxforge_core::MAX_REQUEST_BYTES)
        ),
        r#"{"method": "source.prepare", "params": {"asset_id": "a"}}"#.to_owned(),
    ]
    .join("\n");
    let ran = ctl_in(&endpoint, &["batch"], &input);
    let lines = ran.lines();
    assert_eq!(lines.len(), 3, "{}", ran.stdout);
    assert_eq!(lines[0]["result"]["job"], ready);
    assert_eq!(
        lines[1]["error"]["code"], USAGE,
        "a line past a request's length"
    );
    assert_eq!(lines[2]["warning"]["code"], JOB_RELEASED);
    assert!(
        lines[2]["warning"]["message"]
            .as_str()
            .unwrap()
            .contains("this batch ends")
    );
    // A connection lost under a mutation ends the batch, with the outcome unknown.
    let endpoint = session(|_| Reply::Close);
    let input = [
        r#"{"method": "preset.create", "params": {"name": "x", "settings": {}}}"#,
        r#"{"method": "catalog.info"}"#,
    ]
    .join("\n");
    let ran = ctl_in(&endpoint, &["batch"], &input);
    let lines = ran.lines();
    assert_eq!(lines.len(), 1, "nothing is sent after a lost connection");
    assert_eq!(lines[0]["error"]["code"], OUTCOME_UNKNOWN);
    assert_eq!(ran.status, EXIT_OUTCOME_UNKNOWN);
    assert_eq!(
        methods(&endpoint.requests()),
        ["schema.list", "preset.create"]
    );
}
