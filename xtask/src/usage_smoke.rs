//! Usage statistics end to end, against a loopback receiver this process starts, over three
//! background launches that share one isolated configuration.
//!
//! - `ask` starts with a new configuration: the startup notice is claimed and drawn once the opens
//!   are captured, and answered No thanks, which stores the notice as shown and sharing off.
//! - `work` starts from a copy of that configuration: no notice this time; Settings › Privacy shows
//!   the switch off and turns it on, which starts a session. Then a person's operations on the
//!   desktop — the photograph opened again in Develop, a Basic commit, a zoom step to 100% and an
//!   export — and one operation through the live session: `luxforge-ctl call edit.set-basic`, run
//!   by this process while the script waits for its edit to arrive. The statistics clock (an
//!   evidence seam of the launch, never a production setting) is moved on eleven minutes, so the
//!   window falls due and is sent at once; the receiver accepts it. A zoom back to Fit fills the
//!   next window, the clock moves on again, and the receiver holds that request open; while it is
//!   in flight this process runs `luxforge-ctl call usage.set` with `enabled: false`, which must
//!   cancel the request (the receiver sees the connection closed before it answers) and turn the
//!   switch off. A third move of the clock sends nothing.
//! - `after` starts from the same configuration again: no notice, the switch still off.
//!
//! The receiver records every request's method, path, headers and body. The checks decode each
//! body with the core's strict `usage::decode_batch`, match the first batch's events to the
//! operations in the order they were performed — the GUI's Basic commit and the live session's
//! the same `tool.commit` event, with the photograph's image ID on every Develop event — and
//! correlate the frames' notice and Privacy state, `usage.read` through `luxforge-ctl`, the log
//! and the captured bodies. No frame records an identifier, and nothing names the production
//! origin as its destination. The fixture's hash is checked unchanged.
use crate::{
    scenario::{
        Checked, Checks, Frame, Launch, Plan, Run, Step,
        launch::{self as launching, Guard, Poll, Watcher},
    },
    smoke::Scenario,
    *,
};
use luxforge_evidence::{self as script, UsageAwait, UsageStep};
use luxforge_testbase::{Request, TestServer, respond};
use std::{
    io::Write,
    process::ExitStatus,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

pub const SCENARIO: &str = "usage";
/// The three launches, in order.
pub const ASK: &str = "ask";
pub const WORK: &str = "work";
pub const AFTER: &str = "after";
/// What each launch opens, and what `work` opens again from its script.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The receiver's path, as the test receiver URL names it.
const RECEIVER_PATH: &str = "/v1/usage/batches";
/// The production origin, which no frame may name.
const PRODUCTION_ORIGIN: &str = "api.luxforge.app";
/// What the receiver recorded, in the output directory.
const RECEIVED: &str = "receiver.json";
/// What `luxforge-ctl` answered, in the output directory.
const CTL: &str = "ctl.json";
/// Where `work` and `after` keep the configuration copied from `ask`.
const SHARED: &str = "shared/host/config";
/// The export `work` writes.
const EXPORT: &str = "usage-export.jpg";
/// The release an evidence launch with a test receiver reports
/// (`crates/luxforge-app/src/app/usage_supervisor.rs`).
const TEST_RELEASE: &str = "0.0.0+000000e71de5";
/// How far each `advance` moves the statistics clock: past the ten-minute flush and its spacing.
const ADVANCE_S: u64 = 660;
/// The live session's edit.
const API_EDIT: &str = "edit.set-basic";
/// How long the receiver holds a request it does not answer at once, and how often it writes to
/// it meanwhile, which is how it learns the client has closed the connection.
const HOLD: Duration = Duration::from_secs(15);
const TRICKLE: Duration = Duration::from_millis(250);
/// The work launch's process deadline: an export, three deliveries and the receiver's waits.
const WORK_DEADLINE: Duration = Duration::from_secs(120);

pub const NOTE: &str = "Three launches over one isolated configuration, copied from the first launch's `host/config` into `shared/host/config` and passed to the next two with `--evidence-config`, each with `--usage-test-receiver` naming a loopback receiver this process started. The work launch's statistics clock is moved forward by its script's `usage advance` steps, an evidence seam that exists only with a test receiver; nothing waits ten real minutes. `luxforge-ctl` (built beside the editor) is run against the work launch's live session by this process: once for an edit, while the script waits for it, and once to turn statistics off while the receiver holds the second request open. `receiver.json` holds every request the receiver saw, `ctl.json` every `luxforge-ctl` answer.";

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

/// `ask`: the notice, answered No thanks.
pub fn ask_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("decline", UsageStep::Answer(script::NoticeAnswer::Decline)).commits(0),
    ])
}

/// `work`: no notice, opted in through Privacy, the operations, two deliveries and an opt-out
/// while the second is in flight.
pub fn work_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("privacy", script::Step::settings_tab("privacy")).commits(0),
        Step::new("privacy-on", UsageStep::Share(true)).commits(0),
        Step::new("closed", script::Step::key(script::KEY_ESCAPE)).commits(0),
        Step::new("open", open_step()).commits(0),
        Step::new(
            "basic",
            script::SliderStep::new("set-basic", "exposure", vec![0.4]).release(),
        )
        .commits(1),
        Step::new("zoom", script::Step::key("1")).commits(0),
        Step::new("export", script::Step::export(EXPORT, false)).commits(0),
        // `luxforge-ctl`'s edit through the live session, run by the harness once this step has
        // begun.
        Step::new(
            "api",
            awaiting(UsageAwait {
                edited: true,
                ..UsageAwait::default()
            }),
        )
        .commits(1),
        Step::new("flush", UsageStep::Advance(ADVANCE_S)).commits(0),
        Step::new(
            "delivered",
            awaiting(UsageAwait {
                accepted: Some(1),
                ..UsageAwait::default()
            }),
        )
        .commits(0),
        Step::new("fit", script::Step::key("f")).commits(0),
        // The receiver holds this request; the harness opts out through the live session.
        Step::new("flush-held", UsageStep::Advance(ADVANCE_S)).commits(0),
        Step::new(
            "revoked",
            awaiting(UsageAwait {
                attempts: Some(2),
                effective: Some(false),
                ..UsageAwait::default()
            }),
        )
        .commits(0),
        Step::new("privacy-after", script::Step::settings_tab("privacy")).commits(0),
        Step::new("flush-after", UsageStep::Advance(ADVANCE_S)).commits(0),
        Step::new("quiet", script::Step::Wait { ms: 1_500 }).commits(0),
    ])
}

/// `after`: the stored opt-out, with no notice.
pub fn after_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("notice", UsageStep::Notice).commits(0),
        Step::new("privacy", script::Step::settings_tab("privacy")).commits(0),
    ])
}

/// The scripted number of the `work` step named `name`.
pub(crate) fn scripted_number(plan: &Plan, name: &str) -> Result<u64> {
    plan.steps()
        .iter()
        .filter(|step| step.script().is_some())
        .position(|step| step.name() == name)
        .map(|index| index as u64 + 1)
        .ok_or_else(|| format!("The work plan has no {name:?} step").into())
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
        .script(&format!("{name}-script.json"), plan.script());
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    launch
}

/// The row's own run: the receiver, the three launches and the harness's `luxforge-ctl` calls.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let receiver = if run.replaying() {
        None
    } else {
        Some(Receiver::start()?)
    };
    let url = match &receiver {
        Some(receiver) => receiver.url.clone(),
        None => read_json(&run.out().join("result.json"))?["receiver"]
            .as_str()
            .ok_or("The recorded run names no receiver")?
            .to_owned(),
    };
    run.record("receiver", json!(url));
    run.note(NOTE);
    let outcome = (|| -> Result {
        run.hash(&sources)?;
        let ctl = match run.binary() {
            Some(bin) => {
                let ctl =
                    bin.with_file_name(format!("luxforge-ctl{}", std::env::consts::EXE_SUFFIX));
                ensure(
                    ctl.is_file(),
                    format!(
                        "{} is missing: build luxforge-cli beside the editor (`cargo build --release -p luxforge-cli`)",
                        ctl.display()
                    ),
                )?;
                Some(ctl)
            }
            None => None,
        };
        let phase = |phase: &str| {
            if let Some(receiver) = &receiver {
                receiver.phase(phase);
            }
        };

        phase(ASK);
        let plan = ask_plan(&sources);
        let dir = run
            .launch(launch_of(scenario, ASK, &plan, &sources, &url))?
            .dir;
        let ask = plan.check(&dir)?;

        let shared = std::path::absolute(run.out().join(SHARED))?;
        if !run.replaying() {
            copy_dir(&dir.join("host/config"), &shared)?;
        }

        phase(WORK);
        let plan = work_plan(&sources);
        let mut launch = launch_of(scenario, WORK, &plan, &sources, &url)
            .evidence_config(&shared)
            .deadline(WORK_DEADLINE);
        if let (Some(receiver), Some(ctl)) = (&receiver, &ctl) {
            let evidence = std::path::absolute(run.out().join(WORK))?;
            launch = launch
                .watch(watcher(
                    receiver.state.clone(),
                    evidence.clone(),
                    evidence.join("catalog.sqlite"),
                    ctl.clone(),
                    scripted_number(&plan, "api")?,
                ))
                .keep(CTL);
        }
        let dir = run.launch(launch)?.dir;
        let work = plan.check(&dir)?;

        phase(AFTER);
        let plan = after_plan(&sources);
        let launch = launch_of(scenario, AFTER, &plan, &sources, &url).evidence_config(&shared);
        let dir = run.launch(launch)?.dir;
        let after = plan.check(&dir)?;

        if let Some(receiver) = &receiver {
            write_json(&run.out().join(RECEIVED), &receiver.record())?;
        }
        let launches = [ask, work, after];
        (scenario.verify)(&mut run, &launches)?;
        run.sources_unchanged()?;
        run.record(
            "backend",
            launches[1].at("quiet")?.state()["backend"].clone(),
        );
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))
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

// -- The receiver. --

/// The loopback receiver, on the workspace's one test server: it records each request with the
/// launch running when it arrived, answers the first `202` at once, and holds every later one open
/// — its status line sent, then one header line every [`TRICKLE`] — until a write fails because the
/// client closed the connection, as a cancelled request does, or [`HOLD`] passes.
struct Receiver {
    url: String,
    state: Arc<Mutex<Received>>,
    /// Kept for the run: dropping it stops the server.
    _server: TestServer,
}

#[derive(Default)]
struct Received {
    /// The launch running now.
    phase: String,
    requests: Vec<Value>,
}

fn lock(state: &Mutex<Received>) -> std::sync::MutexGuard<'_, Received> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Receiver {
    fn start() -> Result<Self> {
        let state: Arc<Mutex<Received>> = Arc::default();
        let shared = state.clone();
        let started = Instant::now();
        let server = TestServer::http(move |request: &Request, out: &mut dyn Write| {
            serve(request, out, &shared, started);
        })?;
        Ok(Self {
            url: server.url(RECEIVER_PATH),
            state,
            _server: server,
        })
    }

    fn phase(&self, phase: &str) {
        lock(&self.state).phase = phase.into();
    }

    fn record(&self) -> Value {
        json!({"url": self.url, "requests": lock(&self.state).requests})
    }
}

/// Whether the receiver holds a request open now.
fn holding(state: &Mutex<Received>) -> bool {
    lock(state)
        .requests
        .iter()
        .any(|request| request["outcome"] == "holding")
}

fn serve(request: &Request, out: &mut dyn Write, state: &Mutex<Received>, started: Instant) {
    let index = {
        let mut received = lock(state);
        let index = received.requests.len();
        let phase = received.phase.clone();
        received.requests.push(json!({
            "index": index,
            "phase": phase,
            "received_ms": started.elapsed().as_millis() as u64,
            "method": request.method,
            "path": request.path,
            "headers": request.headers,
            "body": String::from_utf8_lossy(&request.body),
            "body_bytes": request.body.len(),
            "outcome": if index == 0 { "answered" } else { "holding" },
        }));
        index
    };
    if index == 0 {
        respond(out, "202 Accepted", "", b"");
        return;
    }
    let held = Instant::now();
    let mut write = |bytes: &[u8]| out.write_all(bytes).and_then(|()| out.flush()).is_ok();
    let mut outcome = "closed-by-client";
    if write(b"HTTP/1.1 202 Accepted\r\n") {
        outcome = "answered-after-hold";
        while held.elapsed() < HOLD {
            std::thread::sleep(TRICKLE);
            if !write(b"X-Held: 1\r\n") {
                outcome = "closed-by-client";
                break;
            }
        }
        if outcome != "closed-by-client" {
            write(b"Content-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
    let mut received = lock(state);
    received.requests[index]["outcome"] = json!(outcome);
    received.requests[index]["held_ms"] = json!(held.elapsed().as_millis() as u64);
}

// -- The harness's live-session client. --

/// Wait for the work launch, running `luxforge-ctl` against its live session: the edit once the
/// script's `api` step has begun, and the opt-out once the receiver holds a request.
fn watcher(
    state: Arc<Mutex<Received>>,
    evidence: PathBuf,
    catalog: PathBuf,
    ctl: PathBuf,
    api_step: u64,
) -> Watcher {
    Box::new(
        move |child: &mut Guard,
              _: Instant,
              deadline: Duration|
              -> Result<(Option<ExitStatus>, Value)> {
            let mut calls = Vec::new();
            let (mut edited, mut revoked) = (false, false);
            let poll = Poll {
                every: Duration::from_millis(50),
                from: Instant::now(),
                deadline,
                late: "The work launch timed out; killed and reaped",
            };
            let status = launching::until_exit(child, poll, |_| {
                if !edited && step_begun(&evidence, api_step) {
                    edited = true;
                    let asset = open_asset(&evidence)?;
                    calls.push(launching::ctl_call(
                        &ctl,
                        &catalog,
                        "edit",
                        API_EDIT,
                        json!({"contrast": 12}),
                        Some(&asset),
                    )?);
                    calls.push(launching::ctl_call(
                        &ctl,
                        &catalog,
                        "read-enabled",
                        "usage.read",
                        json!({}),
                        None,
                    )?);
                }
                if edited && !revoked && holding(&state) {
                    revoked = true;
                    calls.push(launching::ctl_call(
                        &ctl,
                        &catalog,
                        "opt-out",
                        "usage.set",
                        json!({"enabled": false}),
                        None,
                    )?);
                    calls.push(launching::ctl_call(
                        &ctl,
                        &catalog,
                        "read-revoked",
                        "usage.read",
                        json!({}),
                        None,
                    )?);
                }
                Ok(())
            })?;
            Ok((Some(status), json!({"calls": calls})))
        },
    )
}

/// Whether the work launch's log records that its scripted step `number` has begun.
pub(crate) fn step_begun(evidence: &Path, number: u64) -> bool {
    fs::read_to_string(evidence.join("events.jsonl")).is_ok_and(|log| {
        log.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|event| event["event"] == "script_step" && event["detail"]["step"] == number)
    })
}

/// The open photograph's asset, from the newest frame state the launch has written.
pub(crate) fn open_asset(evidence: &Path) -> Result<String> {
    let newest = fs::read_dir(evidence)?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            let number: u64 = name
                .strip_prefix("state-")?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some(number)
        })
        .max()
        .ok_or("The work launch has written no frame state")?;
    let state = read_json(&evidence.join(format!("state-{newest}.json")))?;
    state["state"]["develop"]["document"]["asset_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "The newest frame state names no open photograph".into())
}

// -- The checks. --

fn usage(frame: &Frame) -> &Value {
    &frame.state()["usage"]
}

/// The scenario's checks over the three launches, from the files the run wrote, so a replay checks
/// the recorded run the same way.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [ask, work, after] = launches else {
        return Err("The usage scenario has three launches".into());
    };
    let out = run.out().to_path_buf();
    let receiver_url = run.recorded_value("receiver").as_str().map(str::to_owned);
    let mut checks = Checks::new();
    for launch in launches {
        every_frame(launch, receiver_url.as_deref())?;
    }

    // ask: the notice once, declined; the stored record says so and holds no identifier.
    let notice = ask.at("notice")?;
    ensure(
        usage(notice)["prompt"] == "shown"
            && usage(notice)["notice"]["buttons"] == json!(["Share statistics", "No thanks"])
            && usage(notice)["stored"]["prompt_shown"] == true
            && usage(notice)["stored"]["enabled"] == false
            && usage(notice)["effective"] == false,
        format!(
            "The first launch did not claim and draw the notice: {}",
            usage(notice)
        ),
    )?;
    let declined = ask.at("decline")?;
    ensure(
        usage(declined)["prompt"] == "declined"
            && usage(declined)["notice"].is_null()
            && usage(declined)["stored"]["enabled"] == false,
        format!("No thanks did not leave sharing off: {}", usage(declined)),
    )?;
    let asked = stored_record(&out.join(ASK).join("host/config"))?;
    ensure(
        asked == json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1}),
        format!("The declined record is not the bare refusal: {asked}"),
    )?;
    checks.note(
        notice,
        "the startup notice in a new configuration",
        usage(notice).clone(),
    );
    checks.note(
        declined,
        "No thanks: the notice shown, sharing off",
        json!({"stored": asked}),
    );

    // work: no notice, then the explicit opt-in.
    let notice = work.at("notice")?;
    ensure(
        usage(notice)["prompt"] == "not-shown"
            && usage(notice)["notice"].is_null()
            && usage(notice)["stored"]["prompt_shown"] == true
            && usage(notice)["stored"]["enabled"] == false,
        format!("The second launch asked again: {}", usage(notice)),
    )?;
    let privacy = work.at("privacy")?;
    ensure(
        usage(privacy)["privacy"]["switch"] == false,
        format!("Privacy does not show the refusal: {}", usage(privacy)),
    )?;
    let on = work.at("privacy-on")?;
    ensure(
        usage(on)["privacy"]["switch"] == true
            && usage(on)["effective"] == true
            && usage(on)["stored"]["enabled"] == true,
        format!("The Privacy switch did not opt in: {}", usage(on)),
    )?;
    checks.note(
        notice,
        "no notice in the second launch",
        usage(notice).clone(),
    );
    checks.note(on, "opted in through Settings › Privacy", usage(on).clone());

    // What the receiver saw.
    let received = read_json(&out.join(RECEIVED))?;
    let requests = received["requests"]
        .as_array()
        .ok_or("The receiver recorded nothing")?;
    ensure(
        requests.len() == 2,
        format!(
            "The receiver saw {} requests, not the accepted one and the revoked one",
            requests.len()
        ),
    )?;
    for request in requests {
        ensure(
            request["phase"] == WORK
                && request["method"] == "POST"
                && request["path"] == RECEIVER_PATH,
            format!(
                "A request outside the work launch or to another path: {}",
                request["path"]
            ),
        )?;
        let headers = request["headers"].as_array().ok_or("No headers recorded")?;
        ensure(
            headers
                .iter()
                .any(|header| header[0] == "content-type" && header[1] == "application/json"),
            "A request is not JSON",
        )?;
    }
    let first = batch(&requests[0])?;
    let second = batch(&requests[1])?;
    ensure(
        requests[0]["outcome"] == "answered",
        format!(
            "The first request was not answered: {}",
            requests[0]["outcome"]
        ),
    )?;
    ensure(
        requests[1]["outcome"] == "closed-by-client",
        format!(
            "The held request was not cancelled by the opt-out: {}",
            requests[1]["outcome"]
        ),
    )?;
    for (request, body) in [(&requests[0], &first), (&requests[1], &second)] {
        let headers = request["headers"].to_string();
        for id in [&body["installation_id"], &body["session_id"]] {
            let id = id.as_str().ok_or("A batch has no identifier")?;
            ensure(
                !headers.contains(id) && !request["path"].as_str().unwrap_or_default().contains(id),
                "An identifier is in a request's path or headers",
            )?;
        }
    }
    ensure(
        first["release"] == TEST_RELEASE
            && first["batch_seq"] == 0
            && first["dropped"] == 0
            && second["batch_seq"] == 1
            && second["session_id"] == first["session_id"]
            && second["installation_id"] == first["installation_id"],
        format!(
            "The batches are not one session's first two: release {}, batch_seq {} and {}, dropped {}",
            first["release"], first["batch_seq"], second["batch_seq"], first["dropped"]
        ),
    )?;

    // The first batch's events: the operations, in the order performed.
    let events = first["events"].as_array().ok_or("No events")?;
    let image = events
        .iter()
        .find_map(|event| event["image"].as_str())
        .ok_or("No event carries an image ID")?;
    ensure(
        image.len() == 32 && image.bytes().all(|b| b.is_ascii_hexdigit()),
        format!("The image ID is not 32 hexadecimal digits: {image}"),
    )?;
    ensure(
        events
            .iter()
            .filter_map(|event| event.get("image"))
            .all(|other| other == image),
        "Develop events carry more than one image ID for one photograph",
    )?;
    let performed: Vec<Value> = events
        .iter()
        .filter(|event| {
            !matches!(
                event["name"].as_str(),
                Some("preview.settled" | "process.sample")
            )
        })
        .map(summary)
        .collect();
    let expected = vec![
        json!({"name": "session.start", "trigger": "opt-in"}),
        // Opening a file is a one-file Develop of it: accepted, it finds the photograph already in
        // the catalog, so it adds none, and its preparation and adoption follow.
        json!({"name": "interaction", "feature": "develop-picks"}),
        json!({"name": "operation.end", "operation": "source-preparation", "outcome": "completed", "image": image}),
        json!({"name": "image.open", "image": image}),
        json!({"name": "tool.commit", "tool": "basic", "image": image}),
        json!({"name": "interaction", "feature": "zoom", "image": image}),
        json!({"name": "interaction", "feature": "export", "image": image}),
        json!({"name": "operation.end", "operation": "export", "outcome": "completed", "count": 1, "image": image}),
        json!({"name": "tool.commit", "tool": "basic", "image": image}),
    ];
    ensure(
        performed == expected,
        format!(
            "The first batch's events are not the operations in order:\n  got      {}\n  expected {}",
            json!(performed),
            json!(expected)
        ),
    )?;
    let second_events: Vec<Value> = second["events"]
        .as_array()
        .ok_or("No events in the second batch")?
        .iter()
        .filter(|event| {
            !matches!(
                event["name"].as_str(),
                Some("preview.settled" | "process.sample")
            )
        })
        .map(summary)
        .collect();
    ensure(
        second_events == [json!({"name": "interaction", "feature": "zoom", "image": image})],
        format!(
            "The second batch is not the zoom to Fit: {}",
            json!(second_events)
        ),
    )?;
    let last_seq = events.last().and_then(|event| event["seq"].as_u64());
    let next_seq = second["events"][0]["seq"].as_u64();
    ensure(
        last_seq
            .zip(next_seq)
            .is_some_and(|(last, next)| next == last + 1),
        "The second batch does not continue the first's sequence",
    )?;

    // The live session's calls, as luxforge-ctl answered them.
    let ctl = read_json(&out.join(CTL))?;
    let call = |name: &str| -> Result<Value> {
        ctl["calls"]
            .as_array()
            .and_then(|calls| calls.iter().find(|call| call["name"] == name))
            .cloned()
            .ok_or_else(|| format!("luxforge-ctl made no {name:?} call").into())
    };
    let edit = call("edit")?;
    ensure(
        edit["exit_code"] == 0,
        format!("luxforge-ctl's edit failed: {}", edit["stderr"]),
    )?;
    let enabled = call("read-enabled")?;
    ensure(
        enabled["exit_code"] == 0
            && enabled["result"]["effective"] == true
            && enabled["result"]["session_id"] == first["session_id"]
            && enabled["result"]["installation_id"] == first["installation_id"]
            && enabled["result"]["release"] == TEST_RELEASE
            && enabled["result"]["destination"].as_str() == receiver_url.as_deref(),
        format!(
            "usage.read through the live session does not name the batch's session and installation: {}",
            enabled["result"]
        ),
    )?;
    ensure(
        !enabled["result"].to_string().contains("image_key"),
        "usage.read answered the image key",
    )?;
    let opt_out = call("opt-out")?;
    ensure(
        opt_out["exit_code"] == 0 && opt_out["result"]["state"]["effective"] == false,
        format!(
            "luxforge-ctl could not opt out: {} {}",
            opt_out["result"], opt_out["stderr"]
        ),
    )?;
    let revoked_read = call("read-revoked")?;
    ensure(
        revoked_read["result"]["effective"] == false
            && revoked_read["result"]["enabled"] == false
            && revoked_read["result"]["installation_id"].is_null(),
        format!("usage.read after the opt-out: {}", revoked_read["result"]),
    )?;

    // The frames around delivery and the opt-out.
    let api = work.at("api")?;
    let delivered = work.at("delivered")?;
    ensure(
        usage(delivered)["diagnostics"]["accepted"] == 1
            && usage(delivered)["diagnostics"]["attempts"] == 1,
        format!(
            "The delivered frame does not show one accepted batch: {}",
            usage(delivered)
        ),
    )?;
    let revoked = work.at("revoked")?;
    ensure(
        usage(revoked)["effective"] == false
            && usage(revoked)["stored"]["enabled"] == false
            && usage(revoked)["diagnostics"]["attempts"] == 2
            && usage(revoked)["diagnostics"]["accepted"] == 1,
        format!(
            "The opt-out is not shown after the held attempt: {}",
            usage(revoked)
        ),
    )?;
    let privacy_after = work.at("privacy-after")?;
    ensure(
        usage(privacy_after)["privacy"]["switch"] == false,
        format!(
            "Privacy does not show the live session's opt-out: {}",
            usage(privacy_after)
        ),
    )?;
    let quiet = work.at("quiet")?;
    ensure(
        usage(quiet)["diagnostics"]["attempts"] == 2,
        format!("Another attempt followed the opt-out: {}", usage(quiet)),
    )?;
    ensure(
        out.join(WORK).join(EXPORT).is_file(),
        "The export was not written",
    )?;
    checks.note(
        api,
        "luxforge-ctl's edit through the live session, on screen",
        json!({"call": edit["result"]}),
    );
    checks.note(
        delivered,
        "the first batch accepted by the receiver",
        json!({"diagnostics": usage(delivered)["diagnostics"], "events": events.len(),
               "performed": performed}),
    );
    checks.note(
        revoked,
        "the opt-out through the live session while the second request was held",
        json!({"receiver": requests[1]["outcome"], "held_ms": requests[1]["held_ms"],
               "diagnostics": usage(revoked)["diagnostics"]}),
    );

    // The log of the work launch.
    let log = scenario::events(&out.join(WORK).join("events.jsonl"))?;
    let supervisor = log
        .iter()
        .find(|event| event["event"] == "usage_supervisor")
        .ok_or("The work launch logged no statistics supervisor")?;
    ensure(
        supervisor["detail"]
            == json!({"started": true, "release": TEST_RELEASE, "test_clock": true}),
        format!("The supervisor started otherwise: {}", supervisor["detail"]),
    )?;
    let advanced = log
        .iter()
        .filter(|event| event["event"] == "usage_clock_advanced")
        .count();
    ensure(
        advanced == 3,
        format!("{advanced} clock moves logged, not 3"),
    )?;
    ensure(
        log.iter().any(|event| {
            event["event"] == "usage_set"
                && event["detail"] == json!({"enabled": true, "effective": true})
        }),
        "The opt-in is not logged",
    )?;

    // after: the stored opt-out holds; no notice.
    let notice = after.at("notice")?;
    ensure(
        usage(notice)["prompt"] == "not-shown"
            && usage(notice)["stored"]["enabled"] == false
            && usage(notice)["effective"] == false,
        format!(
            "The third launch asked again or collects: {}",
            usage(notice)
        ),
    )?;
    ensure(
        usage(after.at("privacy")?)["privacy"]["switch"] == false,
        "The third launch's Privacy switch is on",
    )?;
    let stored = stored_record(&out.join(SHARED))?;
    ensure(
        stored == json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1}),
        format!("The configuration after the opt-out keeps more than the refusal: {stored}"),
    )?;
    checks.note(
        notice,
        "the opt-out kept by the next launch",
        json!({"stored": stored}),
    );
    checks.write(
        &out,
        SCENARIO,
        json!({"receiver": receiver_url, "test_release": TEST_RELEASE, "advance_s": ADVANCE_S}),
    )
}

/// The invariants of every frame: eligible under the test receiver, never an identifier, never
/// the production origin.
fn every_frame(launch: &Checked, receiver: Option<&str>) -> Result {
    for name in launch.names() {
        let usage = usage(launch.at(name)?);
        let object = usage
            .as_object()
            .ok_or_else(|| format!("Step {name:?} records no usage statistics"))?;
        ensure(
            usage["eligible"] == true && !object.keys().any(|key| key.ends_with("_id")),
            format!("Step {name:?} is not eligible or records an identifier: {usage}"),
        )?;
        let text = usage.to_string();
        ensure(
            !text.contains("installation_id") && !text.contains("session_id"),
            format!("Step {name:?} records an identifier: {usage}"),
        )?;
        ensure(
            usage["destination"].as_str().is_none_or(|destination| {
                Some(destination) == receiver && !destination.contains(PRODUCTION_ORIGIN)
            }),
            format!("Step {name:?} names another destination: {usage}"),
        )?;
    }
    Ok(())
}

/// A request's body, decoded by the core's strict decoder and read as JSON.
fn batch(request: &Value) -> Result<Value> {
    let body = request["body"].as_str().ok_or("A request has no body")?;
    luxforge_core::usage::decode_batch(body.as_bytes())
        .map_err(|error| format!("The core refuses request {}: {error:?}", request["index"]))?;
    Ok(serde_json::from_str(body)?)
}

/// An event without its sequence number and time, and an operation's duration, which vary.
fn summary(event: &Value) -> Value {
    let mut event = event.clone();
    if let Some(object) = event.as_object_mut() {
        for varying in ["seq", "t", "duration_ms"] {
            object.remove(varying);
        }
    }
    event
}

/// The stored usage statistics record in `config`'s `preferences.json`.
fn stored_record(config: &Path) -> Result<Value> {
    Ok(read_json(&config.join("preferences.json"))?["usage_statistics"].clone())
}
