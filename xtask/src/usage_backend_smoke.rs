//! Usage statistics from the real desktop into the local reference backend: the client's own
//! batches go through the reference stack's ingress, Vector and ClickHouse. The stack is not
//! started here: `services/usage-statistics/test.sh --client` brings it up, runs this scenario with
//! `LUXFORGE_USAGE_INGRESS` naming the ingress it published on loopback, and then checks what was
//! stored, the dashboards and every hop's logs against `receiver.json`. Outside every `verify`
//! tier, so nothing ordinary depends on Docker.
//!
//! Each launch's test receiver is a relay in this process that forwards every request, byte for
//! byte, to the ingress and returns the ingress's reply byte for byte, recording both. Once it
//! withholds a reply: it forwards the batch, which the ingress accepts, and closes the client's
//! connection without answering, an ambiguous failure.
//!
//! - `work` starts with a new configuration and opts in through the startup notice. A Develop
//!   journey is sent and accepted. The next batch's reply is withheld: the client counts a failure,
//!   cools down and never sends that batch again; a window recorded in the cooldown is held back
//!   and then expires unsent.
//! - `again` starts from `work`'s configuration: the same installation, a new session. One batch
//!   is accepted; the next meets the ingress's per-installation limit, whose `429` the client
//!   honours (its `Retry-After` raised to the ten-minute minimum, shorter than the failure
//!   cooldown); the next window is accepted, leaving the refused batch as a gap in the stored
//!   session. Privacy off and on rotates the installation ID and the image key: the same
//!   photograph's next batch carries a new installation, session and image ID, and is accepted.
//! - `rejected` starts with a new configuration whose receiver names a path the ingress does not
//!   serve: its `404` suspends sending for the launch.
//!
//! The evidence clock moves the worker's time, not the events' (`usage_supervisor`), so a window
//! ages by the moves since its session began: once a session's moves pass 30 minutes, every later
//! window of that session expires at once. Each session here stays within that until its last
//! window. The ingress's limiter runs on real time, so the plans wait for it to refill.
use crate::{
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step},
    smoke::Scenario,
    *,
};
use luxforge_evidence::{self as script, NoticeAnswer, UsageAwait, UsageStep};
use luxforge_testbase::{Request, TestServer};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

pub const SCENARIO: &str = "usage-backend";
pub const WORK: &str = "work";
pub const AGAIN: &str = "again";
pub const REJECTED: &str = "rejected";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The variable naming the ingress, `http://127.0.0.1:PORT`.
pub const INGRESS_VAR: &str = "LUXFORGE_USAGE_INGRESS";
const ROUTE: &str = "/v1/usage/batches";
/// A path the ingress does not serve, as a misconfigured destination would name.
const WRONG_ROUTE: &str = "/v1/usage/batch";
const RECEIVED: &str = "receiver.json";
const SHARED: &str = "shared/host/config";
const EXPORT: &str = "usage-backend-export.jpg";
const TEST_RELEASE: &str = "0.0.0+000000e71de5";
/// Past the ten-minute flush and the minute between attempts.
const FLUSH_S: u64 = 660;
/// The minute between attempts: a window already due goes as soon as this has passed.
const ATTEMPT_S: u64 = 60;
/// Short of the ten-minute minimum a `Retry-After` is raised to and of the failure cooldown.
const EARLY_S: u64 = 300;
/// With [`EARLY_S`], past the ten-minute minimum and short of the failure cooldown's 20 minutes.
const LATE_S: u64 = 310;
/// With the moves before it, past a window's 30 minutes.
const EXPIRE_S: u64 = 900;
/// The ingress's per-installation limit the stack is started with: one batch per this many real
/// seconds, no burst.
pub const ID_INTERVAL_S: u64 = 5;
/// Longer than [`ID_INTERVAL_S`], for the limiter to admit the next batch.
const REFILL_MS: u64 = 6_000;
const DEADLINE: Duration = Duration::from_secs(150);
/// The header fields the client sends, in the order its transport writes them, and nothing else.
const CLIENT_HEADERS: [&str; 6] = [
    "content-length",
    "host",
    "user-agent",
    "accept-encoding",
    "connection",
    "content-type",
];

pub const NOTE: &str = "Run by `services/usage-statistics/test.sh --client`, which starts the reference stack with a per-installation limit of one batch per 5 seconds and sets LUXFORGE_USAGE_INGRESS to its loopback ingress; this scenario alone checks only the client's side. Each launch's `--usage-test-receiver` is a relay in the xtask process that forwards each request byte for byte to the ingress and returns its reply byte for byte, except that it withholds the reply to `work`'s second batch after the ingress accepted it. `again` reads `work`'s configuration from `shared/host/config` with `--evidence-config`. `receiver.json` holds every request, the ingress's status and what the relay returned.";

fn open_step() -> script::Step {
    script::Step::Open {
        path: FIXTURE.into(),
    }
}

fn awaiting(wait: UsageAwait) -> UsageStep {
    UsageStep::Await(UsageAwait {
        timeout_ms: Some(20_000),
        ..wait
    })
}

fn accepted(n: u64) -> UsageStep {
    awaiting(UsageAwait {
        accepted: Some(n),
        ..UsageAwait::default()
    })
}

fn attempts(n: u64) -> UsageStep {
    awaiting(UsageAwait {
        attempts: Some(n),
        ..UsageAwait::default()
    })
}

/// A fresh `usage.read`, without waiting for anything else.
fn read_now() -> UsageStep {
    awaiting(UsageAwait {
        effective: Some(true),
        ..UsageAwait::default()
    })
}

fn slider(value: f64) -> script::SliderStep {
    script::SliderStep::new("set-basic", "exposure", vec![value]).release()
}

fn refill() -> script::Step {
    script::Step::Wait { ms: REFILL_MS }
}

pub fn work_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("share", UsageStep::Answer(NoticeAnswer::Share)).commits(0),
        Step::new("open", open_step()).commits(0),
        Step::new("basic", slider(0.4)).commits(1),
        Step::new("zoom", script::Step::key("1")).commits(0),
        Step::new("export", script::Step::export(EXPORT, false)).commits(0),
        Step::new("flush", UsageStep::Advance(FLUSH_S)).commits(0),
        Step::new("delivered", accepted(1)).commits(0),
        // The ingress's limiter admits the next batch, whose reply the relay withholds.
        Step::new("refill", refill()).commits(0),
        Step::new("fit", script::Step::key("f")).commits(0),
        Step::new("flush-withheld", UsageStep::Advance(ATTEMPT_S)).commits(0),
        Step::new("withheld", attempts(2)).commits(0),
        Step::new("zoom-held", script::Step::key("1")).commits(0),
        Step::new("cooling", UsageStep::Advance(EARLY_S)).commits(0),
        Step::new("cooled", read_now()).commits(0),
        Step::new("expire", UsageStep::Advance(EXPIRE_S)).commits(0),
        Step::new("expired", read_now()).commits(0),
    ])
}

pub fn again_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("open", open_step()).commits(0),
        Step::new("basic", slider(0.6)).commits(1),
        // `work`'s last batch may be under five seconds old.
        Step::new("refill-first", refill()).commits(0),
        Step::new("flush", UsageStep::Advance(FLUSH_S)).commits(0),
        Step::new("delivered", accepted(1)).commits(0),
        // Within the limiter's interval: the ingress refuses this one with 429.
        Step::new("fit", script::Step::key("f")).commits(0),
        Step::new("flush-limited", UsageStep::Advance(ATTEMPT_S)).commits(0),
        Step::new("limited", attempts(2)).commits(0),
        Step::new("zoom", script::Step::key("1")).commits(0),
        Step::new("early", UsageStep::Advance(EARLY_S)).commits(0),
        Step::new("held", read_now()).commits(0),
        Step::new("refill", refill()).commits(0),
        Step::new("late", UsageStep::Advance(LATE_S)).commits(0),
        Step::new("resumed", accepted(2)).commits(0),
        Step::new("privacy", script::Step::settings_tab("privacy")).commits(0),
        Step::new("off", UsageStep::Share(false)).commits(0),
        Step::new("on", UsageStep::Share(true)).commits(0),
        Step::new("closed", script::Step::key(script::KEY_ESCAPE)).commits(0),
        Step::new("basic-rotated", slider(0.8)).commits(1),
        Step::new("flush-rotated", UsageStep::Advance(FLUSH_S)).commits(0),
        Step::new("rotated", accepted(3)).commits(0),
    ])
}

pub fn rejected_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("share", UsageStep::Answer(NoticeAnswer::Share)).commits(0),
        Step::new("open", open_step()).commits(0),
        Step::new("basic", slider(0.3)).commits(1),
        Step::new("flush", UsageStep::Advance(FLUSH_S)).commits(0),
        Step::new("refused", attempts(1)).commits(0),
        Step::new("zoom", script::Step::key("1")).commits(0),
        Step::new("flush-after", UsageStep::Advance(FLUSH_S)).commits(0),
        Step::new("quiet", read_now()).commits(0),
    ])
}

fn launch_of(
    scenario: &Scenario,
    name: &str,
    plan: &Plan,
    sources: &[PathBuf],
    receiver: &str,
) -> Launch {
    let mut launch = Launch::named(name)
        .usage_receiver(receiver)
        .open_all(sources)
        .script(&format!("{name}-script.json"), plan.script())
        .deadline(DEADLINE);
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    launch
}

/// The ingress `LUXFORGE_USAGE_INGRESS` names: plain HTTP on a loopback address only, so this
/// scenario can reach nothing but a local stack.
fn ingress() -> Result<SocketAddr> {
    let value = std::env::var(INGRESS_VAR).map_err(|_| {
        format!(
            "{INGRESS_VAR} is not set: run this scenario through `services/usage-statistics/test.sh --client`, which starts the reference stack and names its ingress"
        )
    })?;
    let authority = value
        .strip_prefix("http://")
        .map(|rest| rest.trim_end_matches('/'))
        .filter(|rest| !rest.contains('/'))
        .ok_or_else(|| format!("{INGRESS_VAR} must be http://127.0.0.1:PORT, not {value:?}"))?;
    let address = authority
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| format!("{INGRESS_VAR} names no address: {value:?}"))?;
    ensure(
        address.ip().is_loopback() && authority.contains(':'),
        format!("{INGRESS_VAR} must name a loopback address and port, not {value:?}"),
    )?;
    Ok(address)
}

pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let relay = if run.replaying() {
        None
    } else {
        Some(Relay::start(ingress()?)?)
    };
    let base = match &relay {
        Some(relay) => relay.base.clone(),
        None => read_json(&run.out().join("result.json"))?["relay"]
            .as_str()
            .ok_or("The recorded run names no relay")?
            .to_owned(),
    };
    run.record("relay", json!(base));
    run.note(NOTE);
    let outcome = (|| -> Result {
        run.hash(&sources)?;
        let phase = |phase: &str| {
            if let Some(relay) = &relay {
                relay.phase(phase);
            }
        };
        let route = format!("{base}{ROUTE}");

        phase(WORK);
        let plan = work_plan(&sources);
        let dir = run
            .launch(launch_of(scenario, WORK, &plan, &sources, &route))?
            .dir;
        let work = plan.check(&dir)?;
        let shared = std::path::absolute(run.out().join(SHARED))?;
        if !run.replaying() {
            copy_dir(&dir.join("host/config"), &shared)?;
        }

        phase(AGAIN);
        let plan = again_plan(&sources);
        let launch = launch_of(scenario, AGAIN, &plan, &sources, &route).evidence_config(&shared);
        let dir = run.launch(launch)?.dir;
        let again = plan.check(&dir)?;

        phase(REJECTED);
        let plan = rejected_plan(&sources);
        let wrong = format!("{base}{WRONG_ROUTE}");
        let dir = run
            .launch(launch_of(scenario, REJECTED, &plan, &sources, &wrong))?
            .dir;
        let rejected = plan.check(&dir)?;

        write_relayed(&run, relay.as_ref())?;
        (scenario.verify)(&mut run, &[work, again, rejected])?;
        run.sources_unchanged()
    })();
    // A failed run keeps what the relay saw too.
    let outcome = outcome.or_else(|error| {
        write_relayed(&run, relay.as_ref())?;
        Err(error)
    });
    run.finish(outcome, |_| Ok(()))
}

fn write_relayed(run: &Run, relay: Option<&Relay>) -> Result {
    match relay {
        Some(relay) => write_json(&run.out().join(RECEIVED), &relay.record()),
        None => Ok(()),
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

// -- The relay. --

struct Relay {
    /// `http://127.0.0.1:PORT`, with no path.
    base: String,
    state: Arc<Mutex<Relayed>>,
    _server: TestServer,
}

#[derive(Default)]
struct Relayed {
    phase: String,
    requests: Vec<Value>,
}

fn lock(state: &Mutex<Relayed>) -> std::sync::MutexGuard<'_, Relayed> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Relay {
    fn start(ingress: SocketAddr) -> Result<Self> {
        let state: Arc<Mutex<Relayed>> = Arc::default();
        let shared = state.clone();
        let server = TestServer::http(move |request: &Request, out: &mut dyn Write| {
            relay(request, out, &shared, ingress);
        })?;
        let base = server.url("");
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            state,
            _server: server,
        })
    }

    fn phase(&self, phase: &str) {
        lock(&self.state).phase = phase.into();
    }

    fn record(&self) -> Value {
        json!({"base": self.base, "requests": lock(&self.state).requests})
    }
}

/// The ingress's reply: its status, header fields (lowercased names) and body, and the bytes as
/// they arrived.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    raw: Vec<u8>,
}

fn forward(raw: &[u8], ingress: SocketAddr) -> std::io::Result<Reply> {
    let mut stream = TcpStream::connect_timeout(&ingress, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    stream.write_all(raw)?;
    stream.flush()?;
    // The client asks for `Connection: close`, so the reply ends with the connection; at most
    // 64 KiB of it is kept.
    let mut reply = Vec::new();
    stream.take(65_536).read_to_end(&mut reply)?;
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("the ingress's reply has no complete head"))?;
    let head = String::from_utf8_lossy(&reply[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::other("the ingress's reply has no status"))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: reply[split + 4..].to_vec(),
        raw: reply,
    })
}

/// Forward one request byte for byte and return the ingress's reply byte for byte, except
/// `work`'s second, whose reply is withheld: the connection closes unanswered.
fn relay(request: &Request, out: &mut dyn Write, state: &Mutex<Relayed>, ingress: SocketAddr) {
    let (index, phase, withhold) = {
        let received = lock(state);
        let index = received.requests.len();
        let in_phase = received
            .requests
            .iter()
            .filter(|request| request["phase"] == received.phase.as_str())
            .count();
        (
            index,
            received.phase.clone(),
            received.phase == WORK && in_phase == 1,
        )
    };
    let raw = request.text();
    let mut record = json!({
        "index": index,
        "phase": phase,
        "method": request.method,
        "path": request.path,
        "headers": request.headers,
        "body": String::from_utf8_lossy(&request.body),
        "body_bytes": request.body.len(),
        "raw_bytes": raw.len(),
    });
    match forward(raw.as_bytes(), ingress) {
        Ok(reply) => {
            record["ingress"] = json!({
                "status": reply.status,
                "headers": reply.headers,
                "body": String::from_utf8_lossy(&reply.body),
            });
            if withhold {
                record["relayed"] = json!("withheld");
            } else {
                let _ = out.write_all(&reply.raw).and_then(|()| out.flush());
                record["relayed"] = json!("reply");
            }
        }
        Err(error) => {
            record["ingress"] = json!({"error": error.to_string()});
            record["relayed"] = json!("nothing");
        }
    }
    lock(state).requests.push(record);
}

// -- The checks. --

fn usage(frame: &Frame) -> &Value {
    &frame.state()["usage"]
}

fn diagnostics(frame: &Frame) -> &Value {
    &usage(frame)["diagnostics"]
}

fn delivery(frame: &Frame) -> (String, String) {
    let delivery = &diagnostics(frame)["delivery"];
    (
        delivery["state"].as_str().unwrap_or_default().to_owned(),
        delivery["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

fn batch(request: &Value) -> Result<Value> {
    let body = request["body"].as_str().ok_or("A request has no body")?;
    luxforge_core::usage::decode_batch(body.as_bytes())
        .map_err(|error| format!("The core refuses request {}: {error:?}", request["index"]))?;
    Ok(serde_json::from_str(body)?)
}

/// An event without what varies between runs.
fn summary(event: &Value) -> Value {
    let mut event = event.clone();
    if let Some(object) = event.as_object_mut() {
        for varying in ["seq", "t", "duration_ms"] {
            object.remove(varying);
        }
    }
    event
}

/// A batch's events, setting aside the previews and process samples a run may or may not record.
fn performed(body: &Value) -> Vec<Value> {
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    !matches!(
                        event["name"].as_str(),
                        Some("preview.settled" | "process.sample")
                    )
                })
                .map(summary)
                .collect()
        })
        .unwrap_or_default()
}

fn seqs(body: &Value) -> Vec<u64> {
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["seq"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

fn image_of(body: &Value) -> Option<String> {
    body["events"]
        .as_array()?
        .iter()
        .find_map(|event| event["image"].as_str().map(str::to_owned))
}

fn stored_record(config: &Path) -> Result<Value> {
    Ok(read_json(&config.join("preferences.json"))?["usage_statistics"].clone())
}

/// The client's side of the scenario, from the files the run wrote: what it sent, when, to which
/// path, and how each frame's `usage.read` saw the outcome. What the stack stored is checked by
/// `services/usage-statistics/tests/client_test.py` against the same `receiver.json`.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [work, again, rejected] = launches else {
        return Err("The usage-backend scenario has three launches".into());
    };
    let out = run.out().to_path_buf();
    let mut checks = Checks::new();
    for launch in launches {
        for name in launch.names() {
            let usage = usage(launch.at(name)?);
            let text = usage.to_string();
            ensure(
                !text.contains("installation_id")
                    && !text.contains("session_id")
                    && usage["destination"].as_str().is_none_or(|destination| {
                        destination.starts_with("http://127.0.0.1:")
                            && !destination.contains("luxforge.app")
                    }),
                format!("Step {name:?} records an identifier or another destination: {text}"),
            )?;
        }
    }

    let received = read_json(&out.join(RECEIVED))?;
    let requests = received["requests"]
        .as_array()
        .ok_or("The relay recorded nothing")?;
    let statuses: Vec<(&str, u64, &str)> = requests
        .iter()
        .map(|request| {
            (
                request["phase"].as_str().unwrap_or_default(),
                request["ingress"]["status"].as_u64().unwrap_or_default(),
                request["relayed"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    ensure(
        statuses
            == [
                (WORK, 202, "reply"),
                (WORK, 202, "withheld"),
                (AGAIN, 202, "reply"),
                (AGAIN, 429, "reply"),
                (AGAIN, 202, "reply"),
                (AGAIN, 202, "reply"),
                (REJECTED, 404, "reply"),
            ],
        format!("The requests, the ingress's answers and the relay's were {statuses:?}"),
    )?;

    // Every request: a POST of JSON with exactly the client's header fields and no identifier
    // outside its body.
    let mut bodies = Vec::new();
    for request in requests {
        let names: Vec<&str> = request["headers"]
            .as_array()
            .ok_or("No headers recorded")?
            .iter()
            .filter_map(|header| header[0].as_str())
            .collect();
        ensure(
            request["method"] == "POST"
                && names == CLIENT_HEADERS
                && request["headers"].as_array().is_some_and(|headers| {
                    headers.iter().any(|header| {
                        header[0] == "content-type" && header[1] == "application/json"
                    })
                }),
            format!(
                "Request {} is not a POST of JSON with the client's header fields: {} {names:?}",
                request["index"], request["method"]
            ),
        )?;
        let body = batch(request)?;
        let head = format!("{} {}", request["path"], request["headers"]);
        for id in [&body["installation_id"], &body["session_id"]] {
            let id = id.as_str().ok_or("A batch has no identifier")?;
            ensure(
                !head.contains(id),
                "An identifier is in a request's path or headers",
            )?;
        }
        ensure(
            body["release"] == TEST_RELEASE,
            format!("A batch reports release {}", body["release"]),
        )?;
        bodies.push(body);
    }
    ensure(
        requests[..6].iter().all(|request| request["path"] == ROUTE)
            && requests[6]["path"] == WRONG_ROUTE,
        "A request went to another path than its launch's receiver names",
    )?;
    let (w0, w1) = (&bodies[0], &bodies[1]);
    let (a0, a1, a2, b0) = (&bodies[2], &bodies[3], &bodies[4], &bodies[5]);
    let r0 = &bodies[6];

    // work: one installation and session; the withheld batch never sent again; the window
    // recorded in the cooldown held back and then discarded unsent.
    let installation_a = w0["installation_id"].clone();
    ensure(
        w1["installation_id"] == installation_a && w1["session_id"] == w0["session_id"],
        "work's batches are not one installation's one session",
    )?;
    let image_a = image_of(w0).ok_or("work's first batch carries no image ID")?;
    let expected = vec![
        json!({"name": "session.start", "trigger": "opt-in"}),
        json!({"name": "interaction", "feature": "develop-picks"}),
        json!({"name": "operation.end", "operation": "source-preparation", "outcome": "completed", "image": image_a}),
        json!({"name": "image.open", "image": image_a}),
        json!({"name": "tool.commit", "tool": "basic", "image": image_a}),
        json!({"name": "interaction", "feature": "zoom", "image": image_a}),
        json!({"name": "interaction", "feature": "export", "image": image_a}),
        json!({"name": "operation.end", "operation": "export", "outcome": "completed", "count": 1, "image": image_a}),
    ];
    ensure(
        performed(w0) == expected,
        format!(
            "work's first batch is not the journey in order:\n  got      {}\n  expected {}",
            json!(performed(w0)),
            json!(expected)
        ),
    )?;
    let zoom = |image: &str| json!({"name": "interaction", "feature": "zoom", "image": image});
    ensure(
        performed(w1) == [zoom(&image_a)]
            && w1["batch_seq"] == 1
            && seqs(w0).last().map(|seq| seq + 1) == seqs(w1).first().copied(),
        format!(
            "work's second batch is not the next zoom: {}",
            json!(performed(w1))
        ),
    )?;
    let delivered = work.at("delivered")?;
    let withheld = work.at("withheld")?;
    let cooled = work.at("cooled")?;
    let expired = work.at("expired")?;
    ensure(
        diagnostics(delivered)["accepted"] == 1 && diagnostics(delivered)["attempts"] == 1,
        format!("work's first delivery: {}", diagnostics(delivered)),
    )?;
    ensure(
        diagnostics(withheld)["attempts"] == 2
            && diagnostics(withheld)["accepted"] == 1
            && delivery(withheld) == ("cooldown".into(), "failure".into()),
        format!(
            "The withheld reply is not a failure and a cooldown: {}",
            diagnostics(withheld)
        ),
    )?;
    ensure(
        diagnostics(cooled)["attempts"] == 2
            && delivery(cooled) == ("cooldown".into(), "failure".into()),
        format!(
            "A window fell due in the failure cooldown and was sent: {}",
            diagnostics(cooled)
        ),
    )?;
    ensure(
        diagnostics(expired)["attempts"] == 2
            && diagnostics(expired)["dropped"].as_u64() >= Some(1),
        format!(
            "The window recorded in the cooldown was not discarded unsent: {}",
            diagnostics(expired)
        ),
    )?;
    let work_config = stored_record(&out.join(WORK).join("host/config"))?;
    ensure(
        work_config["enabled"] == true && work_config["installation_id"] == installation_a,
        "work's stored installation ID is not its batches'",
    )?;
    checks.note(
        withheld,
        "an ambiguous failure: the ingress accepted the batch, the client saw no reply and cooled down",
        json!({"diagnostics": diagnostics(withheld)}),
    );
    checks.note(
        expired,
        "the window recorded in the cooldown held back, then discarded unsent",
        json!({"cooled": diagnostics(cooled), "expired": diagnostics(expired)}),
    );

    // again: the same installation in a new session; the ingress's 429 honoured; the refused
    // batch a gap before the next accepted one; off and on rotating the installation, session and
    // image ID.
    ensure(
        a0["installation_id"] == installation_a
            && a0["session_id"] != w0["session_id"]
            && [a1, a2]
                .iter()
                .all(|body| body["session_id"] == a0["session_id"])
            && a0["events"][0]["name"] == "session.start"
            && a0["events"][0]["trigger"] == "launch"
            && a0["batch_seq"] == 0,
        "again's batches are not a new launch session of the same installation",
    )?;
    ensure(
        image_of(a0).as_deref() == Some(image_a.as_str()),
        "The same photograph has another image ID in the same installation's next launch",
    )?;
    let retry_after: u64 = requests[3]["ingress"]["headers"]
        .as_array()
        .and_then(|headers| {
            headers
                .iter()
                .find(|header| header[0] == "retry-after")
                .and_then(|header| header[1].as_str()?.parse().ok())
        })
        .ok_or("The ingress's 429 carries no numeric Retry-After")?;
    ensure(
        (1..=ID_INTERVAL_S).contains(&retry_after),
        format!(
            "The ingress's Retry-After is {retry_after} s, not within its {ID_INTERVAL_S} s interval: was the stack started with that per-installation limit?"
        ),
    )?;
    let limited = again.at("limited")?;
    let held = again.at("held")?;
    let resumed = again.at("resumed")?;
    ensure(
        diagnostics(limited)["attempts"] == 2
            && delivery(limited) == ("cooldown".into(), "rate-limited".into()),
        format!(
            "The 429 did not rate-limit the client: {}",
            diagnostics(limited)
        ),
    )?;
    ensure(
        diagnostics(held)["attempts"] == 2
            && delivery(held) == ("cooldown".into(), "rate-limited".into()),
        format!(
            "The client sent again within ten minutes of the 429: {}",
            diagnostics(held)
        ),
    )?;
    ensure(
        diagnostics(resumed)["attempts"] == 3 && delivery(resumed).0 == "ready",
        format!(
            "The client did not send once ten minutes had passed, short of the failure cooldown: {}",
            diagnostics(resumed)
        ),
    )?;
    let (last_a1, first_a2) = (seqs(a1).last().copied(), seqs(a2).first().copied());
    ensure(
        performed(a1) == [zoom(&image_a)]
            && performed(a2) == [zoom(&image_a)]
            && a1["batch_seq"] == 1
            && a2["batch_seq"] == 2
            && last_a1.zip(first_a2).is_some_and(|(a, b)| b == a + 1),
        format!(
            "again's refused and next batches are not one zoom each in sequence: {} {}",
            json!(performed(a1)),
            json!(performed(a2))
        ),
    )?;
    let installation_b = b0["installation_id"].clone();
    let image_b = image_of(b0).ok_or("again's rotated batch carries no image ID")?;
    ensure(
        installation_b != installation_a
            && b0["session_id"] != a0["session_id"]
            && b0["batch_seq"] == 0
            && b0["events"][0]["name"] == "session.start"
            && b0["events"][0]["trigger"] == "opt-in"
            && image_b != image_a,
        format!(
            "Off and on did not rotate the installation, session and image ID: installation {installation_b}, image {image_b}"
        ),
    )?;
    ensure(
        performed(b0).iter().any(|event| {
            *event == json!({"name": "tool.commit", "tool": "basic", "image": image_b})
        }),
        format!(
            "The rotated batch has no Basic commit: {}",
            json!(performed(b0))
        ),
    )?;
    let rotated = again.at("rotated")?;
    ensure(
        diagnostics(rotated)["attempts"] == 4 && delivery(rotated).0 == "ready",
        format!(
            "The rotated batch was not accepted: {}",
            diagnostics(rotated)
        ),
    )?;
    let shared = stored_record(&out.join(SHARED))?;
    ensure(
        shared["enabled"] == true && shared["installation_id"] == installation_b,
        "The configuration after again does not hold the rotated installation ID",
    )?;
    checks.note(
        held,
        "the ingress's 429 honoured: nothing sent within ten minutes",
        json!({"retry_after_s": retry_after, "diagnostics": diagnostics(held)}),
    );
    checks.note(
        resumed,
        "the next window accepted after ten minutes, short of the failure cooldown; the refused batch a gap",
        json!({"diagnostics": diagnostics(resumed)}),
    );
    checks.note(
        rotated,
        "off and on: a new installation, session and image ID for the same photograph",
        json!({"diagnostics": diagnostics(rotated)}),
    );

    // rejected: the ingress's 404 suspends sending for the launch.
    let refused = rejected.at("refused")?;
    let quiet = rejected.at("quiet")?;
    ensure(
        delivery(refused) == ("suspended".into(), "rejected".into())
            && diagnostics(quiet)["attempts"] == 1
            && delivery(quiet) == ("suspended".into(), "rejected".into()),
        format!(
            "The 404 did not suspend sending: {} then {}",
            diagnostics(refused),
            diagnostics(quiet)
        ),
    )?;
    ensure(
        r0["installation_id"] != installation_a && r0["installation_id"] != installation_b,
        "A new configuration reused an installation ID",
    )?;
    checks.note(
        quiet,
        "a misrouted 404 suspends sending until the next launch",
        json!({"diagnostics": diagnostics(quiet)}),
    );
    ensure(
        out.join(WORK).join(EXPORT).is_file(),
        "work's export was not written",
    )?;
    checks.write(
        &out,
        SCENARIO,
        json!({"relay": received["base"], "test_release": TEST_RELEASE,
               "retry_after_s": retry_after}),
    )
}
