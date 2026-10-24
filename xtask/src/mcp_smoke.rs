//! The hidden desktop driven through `luxforge-mcp`, as an agent drives it. One background launch
//! opens a synthetic RAW this run writes and the generated 24 MP JPEG, in an isolated data root,
//! and this process runs two `luxforge-mcp` processes, built beside the editor, against its live
//! session as scripted MCP clients:
//!
//! - **modern**: the 2026-07-28 era and the compact profile, calling itself Claude Code;
//! - **legacy**: the 2025-11-25 era (`initialize`) and a typed profile (`--tools` with the
//!   namespaces an editing agent needs), calling itself Codex.
//!
//! Each lists its tools, reads both photographs' state, edits the JPEG's exposure, reads the
//! history entry its edit made (actor `mcp:<client>`) and fetches the large preview tier, whose
//! attached JPEG must decode at 2048 px. The desktop waits for each edit and captures it changed
//! on screen; Settings › Agents lists both clients (and the typed one's tool-list watch); the
//! modern client is disconnected from the tab while a call of its holds on the owner, which must
//! report `connection-lost`, and its next call attaches again with the `session-replaced`
//! warning, listed again. The desktop then undoes both edits, back to the opened picture, and the
//! originals' hashes are unchanged.
//!
//! The steps are ordered so the desktop only ever waits on the harness through steps without a
//! deadline of their own (an agents list it waits to see) or right after the harness has nothing
//! left before the edit it waits for: the modern client's edit while the desktop awaits it, its
//! preview while the desktop waits for the legacy client to be listed, its held call sent before
//! the legacy client starts, so the list the desktop waits for before Disconnect follows the hold,
//! and the legacy client's edit while the desktop awaits it.
//!
//! Every line written to and read from each adapter is kept (`mcp-<era>.jsonl`), and the checks
//! hold every message the adapters wrote to the vendored MCP schema of its era
//! (`crates/luxforge-cli/tests/mcp/spec/`), as the adapter's own process tests do.
use crate::{
    scenario::{
        Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance,
        launch::{self as launching, Guard, Poll, Watcher},
    },
    smoke::Scenario,
    usage_smoke::{scripted_number, step_begun},
    *,
};
use luxforge_evidence::{self as script, AgentsStep, PaletteStep, UsageAwait, UsageStep};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{Receiver, RecvTimeoutError, channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const SCENARIO: &str = "mcp";
/// The one launch.
pub const DESKTOP: &str = "desktop";
/// The photograph both clients edit: the generated 24 MP JPEG, whose large tier is 2048 px.
pub const JPEG: &str = "fixtures/generated/24mp.jpg";
/// Where the run writes its synthetic RAW: one frame of `luxforge_testkit::hdr_bracket`'s still
/// bracket, a DNG of a RAW test camera.
const GENERATED: &str = "generated";
/// The adapters' record: their exits, standard error and the request IDs the checks read.
const MCP: &str = "mcp.json";
const MODERN: &str = "modern";
const LEGACY: &str = "legacy";
/// The labels and actors `luxforge-mcp` gives the two clients.
const MODERN_LABEL: &str = "Claude Code via MCP";
const LEGACY_LABEL: &str = "Codex via MCP";
const LEGACY_WATCH: &str = "Codex via MCP (tool list)";
const MODERN_ACTOR: &str = "mcp:claude-code";
const LEGACY_ACTOR: &str = "mcp:codex";
/// The typed profile's namespaces.
const NAMESPACES: &str = "asset,edit,history,job,preview,session";
/// What each client sets the JPEG's exposure to.
const MODERN_EXPOSURE: f64 = 1.0;
const LEGACY_EXPOSURE: f64 = -1.0;
/// The large preview tier's long side.
const LARGE: u32 = 2048;
/// How long the harness waits for one answer: longer than a held `job.wait`'s 30 s.
const ANSWER: Duration = Duration::from_secs(90);
/// The most `job.wait` rounds a preview is followed for, each at most 30 s.
const WAIT_ROUNDS: usize = 10;
/// The desktop's mean luminance moves at least this far for a one-stop edit, and stays within
/// `SAME` of a frame that shows the same entry.
const MOVED: f64 = 8.0;
const SAME: f64 = 3.0;
/// The process deadline: two previews of a 24 MP photograph and the waits between the clients,
/// bounded by the editor's own five-minute script deadline.
const DEADLINE: Duration = Duration::from_secs(300);

pub const NOTE: &str = "One launch over a synthetic RAW the run writes (`generated/LXF_0001.DNG`, frame 0 of `luxforge_testkit::hdr_bracket`'s still bracket, so the editor starts with `LUXFORGE_TEST_CAMERAS=1`) and the generated 24 MP JPEG. This process runs two `luxforge-mcp --catalog <desktop>/catalog.sqlite` beside the editor as scripted MCP clients: `modern` (2026-07-28, compact profile, clientInfo Claude Code) from the `modern-edit` step, and `legacy` (2025-11-25 `initialize`, `--tools asset,edit,history,job,preview,session`, clientInfo Codex) from the `listed` step, after the modern client's `events.wait` is held on the owner; the modern client's next call is made from the `reattached` step and the legacy client's edit from the `legacy-edit` step. `mcp-modern.jsonl` and `mcp-legacy.jsonl` hold every line written to and read from each, `mcp-*-stderr.log` their diagnostics and `mcp.json` their exits and the request IDs the checks read.";

/// The edit an agent makes elsewhere, awaited: captured once the open photograph's revision moves.
fn awaited() -> UsageStep {
    UsageStep::Await(UsageAwait {
        edited: true,
        timeout_ms: Some(script::MAX_USAGE_AWAIT_MS),
        ..UsageAwait::default()
    })
}

fn listed(labels: &[&str]) -> AgentsStep {
    AgentsStep::Listed(labels.iter().map(|label| (*label).to_owned()).collect())
}

/// The desktop's frames.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("raw").label("Original"),
        Step::opened("jpeg").label("Original"),
        // The modern client's edit, made once this step has begun.
        Step::new("modern-edit", awaited())
            .commits(1)
            .label("Exposure +1.00 EV"),
        Step::new("agents", PaletteStep::Run("settings agents".into())).commits(0),
        // The legacy client connects once the modern client's call is held, which is what this
        // list waits for.
        Step::new(
            "listed",
            listed(&[MODERN_LABEL, LEGACY_LABEL, LEGACY_WATCH]),
        )
        .commits(0),
        Step::new("disconnected", AgentsStep::Disconnect(MODERN_LABEL.into())).commits(0),
        // The modern client's next call, made once this step has begun, attaches again.
        Step::new(
            "reattached",
            listed(&[LEGACY_LABEL, LEGACY_WATCH, MODERN_LABEL]),
        )
        .commits(0),
        Step::new("closed", script::Step::key(script::KEY_ESCAPE)).commits(0),
        // The legacy client's edit, made once this step has begun.
        Step::new("legacy-edit", awaited())
            .commits(1)
            .label("Exposure -1.00 EV"),
        Step::new("undo-legacy", script::Step::api("history.undo")).commits(1),
        Step::new("undo-modern", script::Step::api("history.undo"))
            .commits(1)
            .label("Original"),
    ])
}

/// The run's synthetic RAW, written into `dir`.
fn write_raw(dir: &Path) -> Result<PathBuf> {
    use luxforge_testkit::hdr_bracket::{Bracket, BracketSpec};
    fs::create_dir_all(dir)?;
    let bracket = Bracket::new(&BracketSpec {
        frames: 2,
        seed: 1,
        ..BracketSpec::default()
    });
    let path = dir.join(&bracket.frames()[0].file_name);
    fs::write(&path, bracket.dng(0))?;
    Ok(path)
}

/// The row's own run: the synthetic RAW, the one launch and the harness's two adapters.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    run.note(NOTE);
    let outcome = (|| -> Result {
        let generated = std::path::absolute(run.out().join(GENERATED))?;
        let raw = if run.replaying() {
            generated.join("LXF_0001.DNG")
        } else {
            for source in &sources {
                ensure(
                    source.is_file(),
                    format!(
                        "{} is missing; run `cargo xtask generate-fixtures --output fixtures/generated`",
                        source.display()
                    ),
                )?;
            }
            write_raw(&generated)?
        };
        // The RAW first, so the JPEG is the photograph open in Develop.
        let opened: Vec<PathBuf> = std::iter::once(raw).chain(sources).collect();
        if !run.replaying() {
            run.hash(&opened)?;
        }
        let mcp = match run.binary() {
            Some(bin) => {
                let mcp =
                    bin.with_file_name(format!("luxforge-mcp{}", std::env::consts::EXE_SUFFIX));
                ensure(
                    mcp.is_file(),
                    format!(
                        "{} is missing: build luxforge-cli beside the editor (`cargo build --release -p luxforge-cli`)",
                        mcp.display()
                    ),
                )?;
                Some(mcp)
            }
            None => None,
        };

        let plan = plan(&opened);
        let mut launch = Launch::named(DESKTOP)
            .open_all(&opened)
            .script(&format!("{DESKTOP}-script.json"), plan.script())
            .test_cameras()
            .deadline(DEADLINE);
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        if let Some(mcp) = mcp {
            let evidence = std::path::absolute(run.out().join(DESKTOP))?;
            let steps = Steps {
                modern: scripted_number(&plan, "modern-edit")?,
                listed: scripted_number(&plan, "listed")?,
                reattached: scripted_number(&plan, "reattached")?,
                legacy: scripted_number(&plan, "legacy-edit")?,
            };
            launch = launch.watch(watcher(
                mcp,
                evidence,
                std::path::absolute(run.out())?,
                steps,
            ));
        }
        let dir = run.launch(launch)?.dir;
        let desktop = plan.check(&dir)?;
        let launches = [desktop];
        (scenario.verify)(&mut run, &launches)?;
        if !run.replaying() {
            run.sources_unchanged()?;
        }
        let backend = launches[0]
            .frames
            .last()
            .map_or(Value::Null, |frame| frame.state()["backend"].clone());
        run.record("backend", backend);
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))
}

// -- The harness's agents. --

/// Which MCP revision a client speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Era {
    Modern,
    Legacy,
}

impl Era {
    fn name(self) -> &'static str {
        match self {
            Self::Modern => MODERN,
            Self::Legacy => LEGACY,
        }
    }
}

/// The transcript one adapter's lines are kept in, shared by the writer and the reader thread.
type Transcript = Arc<Mutex<fs::File>>;

fn keep(transcript: &Transcript, started: Instant, direction: &str, line: &str) {
    let message = serde_json::from_str::<Value>(line).unwrap_or_else(|_| json!(line));
    let record = json!({
        "t_ms": started.elapsed().as_secs_f64() * 1000.0,
        "direction": direction,
        "message": message,
    });
    let mut file = transcript.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = writeln!(file, "{record}");
}

/// One `luxforge-mcp` as this process runs it, as an MCP client starts a stdio server: killed if
/// the run ends before it is finished.
struct Agent {
    era: Era,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    reader: Option<JoinHandle<()>>,
    transcript: Transcript,
    started: Instant,
    next: u64,
    /// The messages read while waiting for another.
    unread: Vec<Value>,
    /// The request IDs the checks read, by name.
    ids: serde_json::Map<String, Value>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Agent {
    fn start(mcp: &Path, catalog: &Path, era: Era, args: &[&str], out: &Path) -> Result<Self> {
        let name = era.name();
        let mut child = launching::mcp_command(mcp, catalog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(
                out.join(format!("mcp-{name}-stderr.log")),
            )?)
            .spawn()?;
        let stdout = child.stdout.take().ok_or("luxforge-mcp has no stdout")?;
        let stdin = child.stdin.take().ok_or("luxforge-mcp has no stdin")?;
        let transcript: Transcript = Arc::new(Mutex::new(fs::File::create(
            out.join(format!("mcp-{name}.jsonl")),
        )?));
        let started = Instant::now();
        let (send, lines) = channel();
        let kept = transcript.clone();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                keep(&kept, started, "received", &line);
                let message = serde_json::from_str::<Value>(&line).unwrap_or_else(|_| json!(line));
                if send.send(message).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            era,
            child,
            stdin: Some(stdin),
            lines,
            reader: Some(reader),
            transcript,
            started,
            next: 0,
            unread: Vec::new(),
            ids: serde_json::Map::new(),
        })
    }

    fn write(&mut self, message: &Value) -> Result {
        let line = message.to_string();
        keep(&self.transcript, self.started, "sent", &line);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("luxforge-mcp's input is closed")?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a request named `name` for the checks, a modern one with its `_meta`, and answer its
    /// ID.
    fn send(&mut self, name: &str, method: &str, mut params: Value) -> Result<Value> {
        if self.era == Era::Modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo":
                    {"name": "claude-code", "title": "Claude Code", "version": "2.0.0"},
            });
        }
        self.next += 1;
        let id = json!(self.next);
        self.ids.insert(name.into(), id.clone());
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str) -> Result {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": {}}))
    }

    /// The message answering `id`, keeping any other read meanwhile.
    fn answer(&mut self, id: &Value) -> Result<Value> {
        if let Some(at) = self.unread.iter().position(|message| &message["id"] == id) {
            return Ok(self.unread.remove(at));
        }
        let deadline = Instant::now() + ANSWER;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(message) if &message["id"] == id => return Ok(message),
                Ok(message) => self.unread.push(message),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "luxforge-mcp ({}) did not answer request {id} in {ANSWER:?}",
                        self.era.name()
                    )
                    .into());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "luxforge-mcp ({}) closed its output before answering request {id}",
                        self.era.name()
                    )
                    .into());
                }
            }
        }
    }

    /// Send a request and read its result, which must be one.
    fn ask(&mut self, name: &str, method: &str, params: Value) -> Result<Value> {
        let id = self.send(name, method, params)?;
        let answer = self.answer(&id)?;
        answer.get("result").cloned().ok_or_else(|| {
            format!(
                "luxforge-mcp ({}) answered {name} ({method}) with {answer}",
                self.era.name()
            )
            .into()
        })
    }

    /// Call `tool` and answer its successful result's structured content.
    fn call(&mut self, name: &str, tool: &str, arguments: Value) -> Result<Value> {
        let result = self.ask(
            name,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )?;
        ensure(
            result["isError"] == false,
            format!(
                "luxforge-mcp ({}) {name} ({tool}) failed: {result}",
                self.era.name()
            ),
        )?;
        Ok(result["structuredContent"].clone())
    }

    /// Call `method` through this client's profile: `luxforge_call` in the compact one, the
    /// method's own tool in the typed one.
    fn method(&mut self, name: &str, method: &str, params: Value) -> Result<Value> {
        match self.era {
            Era::Modern => self.call(
                name,
                "luxforge_call",
                json!({"method": method, "params": params}),
            ),
            Era::Legacy => self.call(name, &method.replace('.', "_"), params),
        }
    }

    /// `preview.read`'s large tier of `asset`, followed through `job.wait` to its end when a job
    /// makes it: the request whose result attaches the picture is named `picture`.
    fn picture(&mut self, asset: &str) -> Result {
        let read = self.method(
            "preview",
            "preview.read",
            json!({"item": {"kind": "photo", "asset_id": asset}, "tier": "large"}),
        )?;
        let Some(job) = read["job_id"].as_str().map(str::to_owned) else {
            let id = self.ids["preview"].clone();
            self.ids.insert("picture".into(), id);
            return Ok(());
        };
        let mut after = Value::Null;
        for round in 0..WAIT_ROUNDS {
            let mut params = json!({"job_id": job, "timeout_ms": 30_000});
            if !after.is_null() {
                params["after"] = after.clone();
            }
            let name = format!("job-wait-{round}");
            let waited = self.method(&name, "job.wait", params)?;
            let status = waited["job"]["status"].as_str().unwrap_or_default();
            if !matches!(status, "queued" | "running") {
                ensure(
                    status == "ready",
                    format!("The large preview's job ended {status}: {waited}"),
                )?;
                let id = self.ids[&name].clone();
                self.ids.insert("picture".into(), id);
                return Ok(());
            }
            after = waited["change"].clone();
        }
        Err(format!("The large preview's job {job} did not end in {WAIT_ROUNDS} waits").into())
    }

    /// Close its input, as a client that quits does, and wait for it to exit: its record.
    fn finish(mut self, out: &Path) -> Result<Value> {
        drop(self.stdin.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break Some(status);
            }
            if started.elapsed() > Duration::from_secs(15) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if status.is_some()
            && let Some(reader) = self.reader.take()
        {
            let _ = reader.join();
        }
        let name = self.era.name();
        Ok(json!({
            "exit_code": status.and_then(|status| status.code()),
            "stopped": status.is_none(),
            "ids": self.ids,
            "stderr": fs::read_to_string(out.join(format!("mcp-{name}-stderr.log")))?,
        }))
    }
}

/// The scripted numbers of the steps the harness acts on.
#[derive(Clone, Copy)]
struct Steps {
    modern: u64,
    listed: u64,
    reattached: u64,
    legacy: u64,
}

/// The open photographs' assets: the RAW's from the first frame, the JPEG's from the second.
fn assets(evidence: &Path) -> Result<(String, String)> {
    let asset = |frame: u64| -> Result<String> {
        let state = read_json(&evidence.join(format!("state-{frame}.json")))?;
        state["state"]["develop"]["document"]["asset_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("Frame {frame} names no open photograph").into())
    };
    Ok((asset(1)?, asset(2)?))
}

/// The modern client: discovery, its three tools, both photographs read, the JPEG's exposure set
/// with the revision it read, the history row the edit made and the large preview.
fn modern(agent: &mut Agent, raw: &str, jpeg: &str) -> Result {
    agent.ask("discover", "server/discover", json!({}))?;
    agent.ask("tools", "tools/list", json!({}))?;
    agent.call("methods", "luxforge_methods", json!({"namespace": "edit"}))?;
    agent.call(
        "describe",
        "luxforge_describe",
        json!({"methods": ["edit.set-basic"]}),
    )?;
    agent.method("state-raw", "asset.state", json!({"asset_id": raw}))?;
    let state = agent.method("state", "asset.state", json!({"asset_id": jpeg}))?;
    let mut edit = json!({
        "method": "edit.set-basic",
        "params": {"asset_id": jpeg, "exposure": MODERN_EXPOSURE},
    });
    if let Some(revision) = state["revision"].as_u64() {
        edit["expected_revision"] = json!(revision);
    }
    agent.call("edit", "luxforge_call", edit)?;
    agent.method(
        "history",
        "history.list",
        json!({"asset_id": jpeg, "limit": 5}),
    )?;
    agent.picture(jpeg)
}

/// Hold a call of the modern client on the owner: an `events.wait` for an event naming the RAW,
/// which nothing makes, sent without waiting for its answer.
fn hold(agent: &mut Agent, raw: &str) -> Result {
    let since = agent.method("since", "events.since", json!({"after": 0}))?;
    let current = since["current_sequence"]
        .as_u64()
        .ok_or_else(|| format!("events.since answered no current_sequence: {since}"))?;
    agent.send(
        "held",
        "tools/call",
        json!({"name": "luxforge_call", "arguments": {"method": "events.wait",
            "params": {"after": current, "timeout_ms": 30_000, "asset_id": raw}}}),
    )?;
    Ok(())
}

/// The legacy client's handshake and reads: `initialize`, its typed tools, `ping`, both
/// photographs read and the large preview of the modern client's edit.
fn legacy_reads(agent: &mut Agent, raw: &str, jpeg: &str) -> Result {
    agent.ask(
        "initialize",
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "codex", "title": "Codex", "version": "1.0.0"}}),
    )?;
    agent.notify("notifications/initialized")?;
    agent.ask("tools", "tools/list", json!({}))?;
    agent.ask("ping", "ping", json!({}))?;
    agent.method("state-raw", "asset.state", json!({"asset_id": raw}))?;
    agent.method("state", "asset.state", json!({"asset_id": jpeg}))?;
    agent.picture(jpeg)
}

/// Wait for the launch, acting as the two clients once the steps they serve have begun, and
/// finishing both once it has exited. Their record is written whatever happens.
fn watcher(mcp: PathBuf, evidence: PathBuf, out: PathBuf, steps: Steps) -> Watcher {
    Box::new(
        move |child: &mut Guard,
              _: Instant,
              deadline: Duration|
              -> Result<(Option<ExitStatus>, Value)> {
            let catalog = evidence.join("catalog.sqlite");
            let mut agents: HashMap<Era, Agent> = HashMap::new();
            let mut photographs: Option<(String, String)> = None;
            let mut stage = 0;
            let poll = Poll {
                every: Duration::from_millis(50),
                from: Instant::now(),
                deadline,
                late: "The desktop launch timed out; killed and reaped",
            };
            let outcome = launching::until_exit(child, poll, |_| {
                let begun = |step| step_begun(&evidence, step);
                if stage == 0 && begun(steps.modern) {
                    stage = 1;
                    let (raw, jpeg) = assets(&evidence)?;
                    let agent = Agent::start(&mcp, &catalog, Era::Modern, &[], &out)?;
                    let agent = agents.entry(Era::Modern).or_insert(agent);
                    modern(agent, &raw, &jpeg)?;
                    photographs = Some((raw, jpeg));
                }
                let Some((raw, jpeg)) = &photographs else {
                    return Ok(());
                };
                if stage == 1 && begun(steps.listed) {
                    stage = 2;
                    hold(agents.get_mut(&Era::Modern).ok_or("no modern agent")?, raw)?;
                    let agent =
                        Agent::start(&mcp, &catalog, Era::Legacy, &["--tools", NAMESPACES], &out)?;
                    let agent = agents.entry(Era::Legacy).or_insert(agent);
                    legacy_reads(agent, raw, jpeg)?;
                }
                if stage == 2 && begun(steps.reattached) {
                    stage = 3;
                    let agent = agents.get_mut(&Era::Modern).ok_or("no modern agent")?;
                    let held = agent.ids["held"].clone();
                    agent.answer(&held)?;
                    agent.send(
                        "replaced",
                        "tools/call",
                        json!({"name": "luxforge_call", "arguments":
                            {"method": "asset.state", "params": {"asset_id": jpeg}}}),
                    )?;
                    let replaced = agent.ids["replaced"].clone();
                    agent.answer(&replaced)?;
                }
                if stage == 3 && begun(steps.legacy) {
                    stage = 4;
                    let agent = agents.get_mut(&Era::Legacy).ok_or("no legacy agent")?;
                    agent.method(
                        "edit",
                        "edit.set-basic",
                        json!({"asset_id": jpeg, "exposure": LEGACY_EXPOSURE}),
                    )?;
                    agent.method(
                        "history",
                        "history.list",
                        json!({"asset_id": jpeg, "limit": 5}),
                    )?;
                }
                Ok(())
            });
            let mut record = serde_json::Map::new();
            record.insert("stage".into(), json!(stage));
            for era in [Era::Modern, Era::Legacy] {
                if let Some(agent) = agents.remove(&era) {
                    record.insert(era.name().into(), agent.finish(&out)?);
                }
            }
            if let Err(error) = &outcome {
                record.insert("error".into(), json!(error.to_string()));
            }
            let record = Value::Object(record);
            write_json(&out.join(MCP), &record)?;
            Ok((Some(outcome?), record))
        },
    )
}

// -- The checks. --

/// The vendored MCP schemas, compiled per era and definition as the checks ask for them.
struct Schemas {
    dir: PathBuf,
    documents: HashMap<Era, Value>,
    validators: HashMap<(Era, String), jsonschema::Validator>,
}

impl Schemas {
    fn new(root: &Path) -> Self {
        Self {
            dir: root.join("crates/luxforge-cli/tests/mcp/spec"),
            documents: HashMap::new(),
            validators: HashMap::new(),
        }
    }

    /// Hold `value` to the era's definition `name`.
    fn conforms(&mut self, era: Era, name: &str, value: &Value, what: &str) -> Result {
        let key = (era, name.to_owned());
        if !self.validators.contains_key(&key) {
            if !self.documents.contains_key(&era) {
                let file = match era {
                    Era::Modern => "2026-07-28.schema.json",
                    Era::Legacy => "2025-11-25.schema.json",
                };
                self.documents.insert(era, read_json(&self.dir.join(file))?);
            }
            let document = &self.documents[&era];
            ensure(
                document["$defs"].get(name).is_some(),
                format!("The {} schema defines no {name}", era.name()),
            )?;
            let schema = json!({
                "$schema": document["$schema"],
                "$defs": document["$defs"],
                "$ref": format!("#/$defs/{name}"),
            });
            let validator = jsonschema::draft202012::new(&schema)
                .map_err(|error| format!("The {} {name} schema: {error}", era.name()))?;
            self.validators.insert(key.clone(), validator);
        }
        let errors: Vec<String> = self.validators[&key]
            .iter_errors(value)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        ensure(
            errors.is_empty(),
            format!("{what} is not a {} {name}: {errors:?}", era.name()),
        )
    }

    /// Hold every message one adapter wrote to its era's schema, by the request it answers:
    /// answers as wire tests hold them, and the only notification a client may hear here, a
    /// legacy tool list change. Answers how many messages it held.
    fn transcript(&mut self, era: Era, lines: &[Value]) -> Result<usize> {
        let mut asked: HashMap<String, String> = HashMap::new();
        let mut held = 0;
        for line in lines {
            let message = &line["message"];
            if line["direction"] == "sent" {
                if let Some(method) = message["method"].as_str()
                    && let Some(id) = message.get("id")
                {
                    asked.insert(id.to_string(), method.to_owned());
                }
                continue;
            }
            let what = format!("{} message {message}", era.name());
            ensure(message.is_object(), format!("{what} is not JSON"))?;
            self.conforms(era, "JSONRPCMessage", message, &what)?;
            held += 1;
            if message.get("error").is_some() {
                return Err(format!("{what} is a protocol error").into());
            }
            if let Some(method) = message["method"].as_str() {
                ensure(
                    era == Era::Legacy && method == "notifications/tools/list_changed",
                    format!("{what} is a notification the client never asked for"),
                )?;
                self.conforms(era, "ToolListChangedNotification", message, &what)?;
                continue;
            }
            let method = message
                .get("id")
                .and_then(|id| asked.remove(&id.to_string()))
                .ok_or_else(|| format!("{what} answers no request"))?;
            match era {
                Era::Modern => {
                    let definition = match method.as_str() {
                        "server/discover" => "DiscoverResultResponse",
                        "tools/list" => "ListToolsResultResponse",
                        "tools/call" => "CallToolResultResponse",
                        other => return Err(format!("{what} answers {other}").into()),
                    };
                    self.conforms(era, definition, message, &what)?;
                    ensure(
                        message["result"]["resultType"] == "complete"
                            && message["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"]
                                == "luxforge-mcp",
                        format!("{what} names no complete result or server"),
                    )?;
                }
                Era::Legacy => {
                    self.conforms(era, "JSONRPCResultResponse", message, &what)?;
                    let definition = match method.as_str() {
                        "initialize" => "InitializeResult",
                        "tools/list" => "ListToolsResult",
                        "tools/call" => "CallToolResult",
                        "ping" => "EmptyResult",
                        other => return Err(format!("{what} answers {other}").into()),
                    };
                    self.conforms(era, definition, &message["result"], &what)?;
                    ensure(
                        message["result"].get("resultType").is_none(),
                        format!("{what} carries a resultType"),
                    )?;
                }
            }
        }
        Ok(held)
    }
}

/// One adapter's kept lines and the answers to its named requests.
struct Kept {
    era: Era,
    lines: Vec<Value>,
    record: Value,
}

impl Kept {
    fn read(out: &Path, adapters: &Value, era: Era) -> Result<Self> {
        let name = era.name();
        let lines = fs::read_to_string(out.join(format!("mcp-{name}.jsonl")))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let record = adapters[name].clone();
        ensure(
            record["exit_code"] == 0,
            format!(
                "luxforge-mcp ({name}) exited {} ({})",
                record["exit_code"], record["stderr"]
            ),
        )?;
        Ok(Self { era, lines, record })
    }

    /// The answer to the request named `name`: its result.
    fn result(&self, name: &str) -> Result<&Value> {
        let id = &self.record["ids"][name];
        ensure(
            !id.is_null(),
            format!("The {} client made no {name} request", self.era.name()),
        )?;
        self.lines
            .iter()
            .filter(|line| line["direction"] == "received")
            .map(|line| &line["message"])
            .find(|message| &message["id"] == id)
            .map(|message| &message["result"])
            .ok_or_else(|| format!("The {} client's {name} has no answer", self.era.name()).into())
    }

    /// A tool call's structured result, which must have succeeded.
    fn structured(&self, name: &str) -> Result<&Value> {
        let result = self.result(name)?;
        ensure(
            result["isError"] == false,
            format!("The {} client's {name} failed: {result}", self.era.name()),
        )?;
        Ok(&result["structuredContent"])
    }
}

/// The codes of a tool result's warnings, from its trailing `{"warnings": [...]}` text block.
fn warnings(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .filter_map(|value| value.get("warnings").cloned())
        .flat_map(|warnings| warnings.as_array().cloned().unwrap_or_default())
        .filter_map(|warning| warning["code"].as_str().map(str::to_owned))
        .collect()
}

/// The picture a tool result attaches, decoded: its size and byte count.
fn attached(kept: &Kept) -> Result<Value> {
    use base64::Engine;
    let result = kept.result("picture")?;
    let blocks: Vec<&Value> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "image")
        .collect();
    let [block] = blocks.as_slice() else {
        return Err(format!(
            "The {} client's picture attaches {} images",
            kept.era.name(),
            blocks.len()
        )
        .into());
    };
    ensure(
        block["mimeType"] == "image/jpeg",
        format!("The picture is {}", block["mimeType"]),
    )?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(block["data"].as_str().unwrap_or_default())?;
    let picture = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)?;
    let (width, height) = (picture.width(), picture.height());
    ensure(
        width.max(height) == LARGE,
        format!(
            "The {} client's picture is {width} × {height}, not the {LARGE} px large tier",
            kept.era.name()
        ),
    )?;
    Ok(json!({"width": width, "height": height, "bytes": bytes.len()}))
}

/// The newest history row of the JPEG, as a client's `history` read answered it.
fn newest_row<'a>(kept: &'a Kept, actor: &str) -> Result<&'a Value> {
    let row = &kept.structured("history")?["entries"][0];
    ensure(
        row["actor"] == actor,
        format!(
            "The {} client's edit is not in history under {actor}: {row}",
            kept.era.name()
        ),
    )?;
    let edit = kept.structured("edit")?;
    ensure(
        edit["created_entry_id"].is_null() || edit["created_entry_id"] == row["id"],
        format!("The newest row {row} is not the edit's entry {edit}"),
    )?;
    Ok(row)
}

/// Mean Rec. 709 luminance over a 5 × 5 grid of patches across the photograph.
fn brightness(frame: &Frame) -> Result<f64> {
    let mut sum = 0.0;
    for row in 0..5 {
        for column in 0..5 {
            let at = [0.1 + 0.2 * f64::from(column), 0.1 + 0.2 * f64::from(row)];
            sum += frame.luminance_at(at, 4)?;
        }
    }
    Ok(sum / 25.0)
}

fn tab(frame: &Frame) -> &Value {
    &frame.state()["settings"]["agents"]
}

/// The tab's row labelled `label`.
fn row<'a>(frame: &'a Frame, label: &str) -> Result<&'a Value> {
    tab(frame)["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["label"] == label)
        .ok_or_else(|| format!("The Agents tab lists no {label:?}: {}", tab(frame)).into())
}

/// The scenario's checks over the launch and the adapters' record, from the files the run wrote,
/// so a replay checks the recorded run the same way.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [desktop] = launches else {
        return Err(format!("Expected one launch, found {}", launches.len()).into());
    };
    let out = run.out().to_path_buf();
    let adapters = read_json(&out.join(MCP))?;
    let modern = Kept::read(&out, &adapters, Era::Modern)?;
    let legacy = Kept::read(&out, &adapters, Era::Legacy)?;
    let mut checks = Checks::new();

    // Every message each adapter wrote, held to its era's schema.
    let mut schemas = Schemas::new(run.root());
    let held = [
        schemas.transcript(Era::Modern, &modern.lines)?,
        schemas.transcript(Era::Legacy, &legacy.lines)?,
    ];

    // The photographs: the synthetic RAW, then the JPEG both clients edit.
    let raw_frame = desktop.at("raw")?;
    let jpeg_frame = desktop.at("jpeg")?;
    let document = |frame: &Frame| frame.state()["develop"]["document"].clone();
    ensure(
        document(raw_frame)["file"] == "LXF_0001.DNG" && document(jpeg_frame)["file"] == "24mp.jpg",
        format!(
            "Opened {} then {}",
            document(raw_frame),
            document(jpeg_frame)
        ),
    )?;
    let jpeg = document(jpeg_frame)["asset_id"].clone();
    let raw = document(raw_frame)["asset_id"].clone();

    // The modern client: discovery, the compact tools and the live registry.
    let discovered = modern.result("discover")?;
    ensure(
        discovered["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&json!("2026-07-28"))),
        format!("server/discover answered {discovered}"),
    )?;
    let names = |kept: &Kept| -> Result<Vec<String>> {
        Ok(kept.result("tools")?["tools"]
            .as_array()
            .ok_or("tools/list listed no tools")?
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect())
    };
    let mut compact = names(&modern)?;
    compact.sort();
    ensure(
        compact == ["luxforge_call", "luxforge_describe", "luxforge_methods"],
        format!("The compact profile lists {compact:?}"),
    )?;
    ensure(
        modern.structured("methods")?["source"] == "live",
        "luxforge_methods did not read the live registry",
    )?;
    for (kept, name) in [(&modern, "state-raw"), (&legacy, "state-raw")] {
        ensure(
            kept.structured(name)?["revision"].is_u64(),
            format!(
                "The {} client's RAW state: {}",
                kept.era.name(),
                kept.structured(name)?
            ),
        )?;
    }
    let modern_row = newest_row(&modern, MODERN_ACTOR)?;
    let modern_picture = attached(&modern)?;

    // The legacy client: the handshake and the typed tools.
    let initialized = legacy.result("initialize")?;
    ensure(
        initialized["protocolVersion"] == "2025-11-25"
            && initialized["capabilities"]["tools"]["listChanged"] == true,
        format!("initialize answered {initialized}"),
    )?;
    let typed = names(&legacy)?;
    let namespaces: Vec<&str> = NAMESPACES.split(',').collect();
    for tool in [
        "asset_state",
        "edit_set-basic",
        "history_list",
        "preview_read",
        "job_wait",
    ] {
        ensure(
            typed.iter().any(|name| name == tool),
            format!("The typed profile does not list {tool}"),
        )?;
    }
    ensure(
        typed.iter().all(|name| {
            namespaces
                .iter()
                .any(|namespace| name.split('_').next() == Some(namespace))
        }) && !typed
            .iter()
            .any(|name| name.starts_with("luxforge_") || name == "session_disconnect"),
        format!("The typed profile lists {typed:?}"),
    )?;
    let legacy_row = newest_row(&legacy, LEGACY_ACTOR)?;
    let legacy_picture = attached(&legacy)?;
    ensure(
        legacy_row["id"] != modern_row["id"],
        "Both clients' edits are one history row",
    )?;

    // The desktop shows each edit, and each undo puts back the frame before it.
    let modern_edit = desktop.at("modern-edit")?;
    let legacy_edit = desktop.at("legacy-edit")?;
    let opened = brightness(jpeg_frame)?;
    let brighter = brightness(modern_edit)?;
    let darker = brightness(legacy_edit)?;
    checks.compare(
        modern_edit,
        "the modern client's +1 EV, brighter than the opened JPEG",
        brighter,
        opened,
        Tolerance::Above(MOVED),
    )?;
    checks.compare(
        legacy_edit,
        "the legacy client's -1 EV, darker than the opened JPEG",
        opened,
        darker,
        Tolerance::Above(MOVED),
    )?;
    let closed = desktop.at("closed")?;
    checks.compare(
        closed,
        "the sheet closed over the modern client's edit",
        brightness(closed)?,
        brighter,
        Tolerance::Within(SAME),
    )?;
    let undo_legacy = desktop.at("undo-legacy")?;
    ensure(
        undo_legacy.entry()? == modern_edit.entry()?,
        "The first undo is not the modern client's entry",
    )?;
    checks.compare(
        undo_legacy,
        "the legacy edit undone in the desktop",
        brightness(undo_legacy)?,
        brighter,
        Tolerance::Within(SAME),
    )?;
    let undo_modern = desktop.at("undo-modern")?;
    ensure(
        undo_modern.entry()? == jpeg_frame.entry()?,
        "The second undo is not the opened entry",
    )?;
    checks.compare(
        undo_modern,
        "both edits undone in the desktop",
        brightness(undo_modern)?,
        opened,
        Tolerance::Within(SAME),
    )?;
    ensure(
        modern_edit.entry()? == modern_row["id"] && legacy_edit.entry()? == legacy_row["id"],
        format!(
            "The desktop's entries {} and {} are not the clients' {} and {}",
            modern_edit.entry()?,
            legacy_edit.entry()?,
            modern_row["id"],
            legacy_row["id"]
        ),
    )?;

    // Settings › Agents: both clients listed as session.clients answered, the modern one
    // disconnected while its call was held, and attached again by its next call.
    let listed = desktop.at("listed")?;
    let first = row(listed, MODERN_LABEL)?;
    let clients = &tab(listed)["session_clients"]["clients"];
    ensure(
        clients.as_array().is_some_and(|clients| {
            clients.len() == 3
                && clients
                    .iter()
                    .all(|client| client["transport"] == "loopback")
        }),
        format!("listed: session.clients answered {clients}"),
    )?;
    let sent: Vec<&Value> = desktop
        .events
        .iter()
        .filter(|event| event["event"] == "agents_disconnect")
        .collect();
    let answered: Vec<&Value> = desktop
        .events
        .iter()
        .filter(|event| event["event"] == "agents_disconnected")
        .collect();
    ensure(
        sent.len() == 1
            && sent[0]["detail"]["client_id"] == first["client_id"]
            && answered.len() == 1
            && answered[0]["detail"]["client_id"] == first["client_id"]
            && answered[0]["detail"]["error"].is_null(),
        format!("disconnected: the log records {sent:?} then {answered:?}"),
    )?;
    let disconnected = desktop.at("disconnected")?;
    ensure(
        row(disconnected, MODERN_LABEL).is_err(),
        format!("disconnected: {}", tab(disconnected)),
    )?;
    let held_answer = modern.result("held")?;
    ensure(
        held_answer["isError"] == true
            && held_answer["structuredContent"]["error"]["code"] == "connection-lost",
        format!("The held call answered {held_answer}"),
    )?;
    let replaced = modern.result("replaced")?;
    ensure(
        replaced["isError"] == false && warnings(replaced) == ["session-replaced"],
        format!("The call after the disconnect answered {replaced}"),
    )?;
    let reattached = desktop.at("reattached")?;
    let again = row(reattached, MODERN_LABEL)?;
    ensure(
        again["client_id"] != first["client_id"],
        format!("reattached: the same client {again}"),
    )?;

    checks.note(
        modern_edit,
        "the modern client's edit through luxforge_call, on screen",
        json!({"row": modern_row, "picture": modern_picture}),
    );
    checks.note(
        legacy_edit,
        "the legacy client's edit through edit_set-basic, on screen",
        json!({"row": legacy_row, "picture": legacy_picture}),
    );
    checks.note(
        listed,
        "both clients and the typed client's tool-list watch, listed",
        json!({"rows": tab(listed)["rows"]}),
    );
    checks.note(
        reattached,
        "the modern client attached again after connection-lost, with session-replaced",
        json!({"before": first["client_id"], "after": again["client_id"]}),
    );
    run.record(
        "mcp",
        json!({"raw": raw, "jpeg": jpeg, "schema_checked": {"modern": held[0], "legacy": held[1]},
            "typed_tools": typed.len()}),
    );
    checks.write(
        &out,
        SCENARIO,
        json!({"moved": MOVED, "same": SAME, "large": LARGE, "namespaces": NAMESPACES}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan is well formed and scripts every step the harness acts on.
    #[test]
    fn the_plan_scripts_the_harness_steps() {
        let plan = plan(&[]);
        assert!(plan.validate().is_ok(), "{:?}", plan.validate());
        for step in ["modern-edit", "listed", "reattached", "legacy-edit"] {
            assert!(scripted_number(&plan, step).is_ok(), "{step}");
        }
    }

    /// A message the adapter writes is held to its era's vendored schema: a modern answer without
    /// `resultType` is refused, and a legacy answer to a request never sent is too.
    #[test]
    fn the_transcript_check_refuses_what_an_era_does_not_allow() {
        let mut schemas = Schemas::new(&root().unwrap());
        let sent = json!({"direction": "sent", "message":
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}});
        let listed = json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}});
        assert!(
            schemas
                .transcript(
                    Era::Legacy,
                    &[
                        sent.clone(),
                        json!({"direction": "received", "message": listed})
                    ]
                )
                .is_ok()
        );
        assert!(
            schemas
                .transcript(
                    Era::Modern,
                    &[sent, json!({"direction": "received", "message": listed})]
                )
                .is_err()
        );
        let unasked = json!({"jsonrpc": "2.0", "id": 9, "result": {}});
        assert!(
            schemas
                .transcript(
                    Era::Legacy,
                    &[json!({"direction": "received", "message": unasked})]
                )
                .is_err()
        );
    }
}
