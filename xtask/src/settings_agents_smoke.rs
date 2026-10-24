//! Settings › Agents in two launches. The `agents` launch, built with `luxforge-mcp` beside it,
//! opens the tab from the palette with no agent connected, chooses Codex and copies its
//! configuration, chooses Claude Code, and then, while its script waits for the list to name it,
//! this process starts the real `luxforge-mcp` built beside the editor against the launch's live
//! session and calls a tool through it as Claude Code would: the tab lists the client as
//! "Claude Code via MCP", and the tab's Disconnect ends its session and empties the list again.
//! The `missing` launch runs the same editor alone in a folder of its own, as a build with no
//! `luxforge-mcp` beside it, and the tab says what it looked for and how to build it.
//!
//! Every frame's tab is checked against what the core says: the configuration shown and copied
//! is the core's own snippet for the server the tab found, and the connected row is
//! `session.clients`' answer as the desktop read it, correlated with the log's disconnect, and
//! with what the adapter itself answered.
use crate::{
    launch as background,
    scenario::{
        Checked, Frame, Launch, Plan, Run, Step,
        launch::{self as launching, Guard, Poll, Watcher},
    },
    smoke::Scenario,
    usage_smoke::{scripted_number, step_begun},
    *,
};
use luxforge_core::agents::{self, Client, Platform};
use luxforge_evidence::{self as script, AgentsStep, PaletteStep};
use std::{
    io::Write,
    process::{ChildStdin, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub const SCENARIO: &str = "settings-agents";
/// The launch with `luxforge-mcp` beside it.
pub const AGENTS: &str = "agents";
/// The launch without it.
pub const MISSING: &str = "missing";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// What the adapter's stdout, stderr and exit are kept in.
const MCP: &str = "mcp.json";
/// The label `luxforge-mcp` identifies itself with for a client that calls itself Claude Code.
const LABEL: &str = "Claude Code via MCP";
const UNNAMED_NONE: &str = "No agent is connected.";

pub const NOTE: &str = "Two launches. `agents` runs the editor with `luxforge-mcp` beside it (on macOS, in the background bundle's `Contents/Helpers`). Once its script's `connected` step has begun, this process starts that `luxforge-mcp --catalog <agents>/catalog.sqlite`, sends `initialize` naming the client Claude Code, `notifications/initialized` and one `luxforge_call` of `session.state`, and keeps it running until the launch exits; `mcp.json` holds its answers, stderr and exit code. `missing` runs a hard link (or copy) of the editor alone in a temporary folder beside it, so the tab finds no `luxforge-mcp`.";

/// `agents`: the empty list, the configuration for two clients and Copy, a real connection and
/// its Disconnect.
pub fn agents_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("empty", PaletteStep::Run("settings agents".into())).commits(0),
        Step::new("codex", AgentsStep::Client(Client::Codex.id().into())).commits(0),
        Step::new("copied", AgentsStep::Copy)
            .commits(0)
            .status("Copied the Codex configuration"),
        Step::new(
            "claude-code",
            AgentsStep::Client(Client::ClaudeCode.id().into()),
        )
        .commits(0),
        // The harness connects `luxforge-mcp` once this step has begun.
        Step::new("connected", AgentsStep::Listed(vec![LABEL.into()])).commits(0),
        Step::new("disconnected", AgentsStep::Disconnect(LABEL.into())).commits(0),
    ])
}

/// `missing`: the tab of a build with no `luxforge-mcp` beside it.
pub fn missing_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("agents", script::Step::settings_tab("agents")).commits(0),
    ])
}

fn launch_of(scenario: &Scenario, name: &str, plan: &Plan, sources: &[PathBuf]) -> Launch {
    let mut launch = Launch::named(name)
        .open_all(sources)
        .script(&format!("{name}-script.json"), plan.script());
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    if background::software_adapter_requested() {
        // Cold LLVM shader compilation on a software adapter, bounded by the editor's own
        // five-minute script deadline.
        launch = launch.deadline(Duration::from_secs(300));
    }
    launch
}

/// The row's own run: the two launches and the harness's `luxforge-mcp`.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    run.note(NOTE);
    let outcome = (|| -> Result {
        run.hash(&sources)?;
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

        let plan = agents_plan(&sources);
        let mut launch = launch_of(scenario, AGENTS, &plan, &sources);
        if let Some(mcp) = mcp {
            let evidence = std::path::absolute(run.out().join(AGENTS))?;
            launch = launch
                .watch(watcher(
                    mcp,
                    evidence.clone(),
                    evidence.join("catalog.sqlite"),
                    std::path::absolute(run.out())?,
                    scripted_number(&plan, "connected")?,
                ))
                .keep(MCP);
        }
        let dir = run.launch(launch)?.dir;
        let connected = plan.check(&dir)?;

        let plan = missing_plan(&sources);
        let dir = run
            .launch(launch_of(scenario, MISSING, &plan, &sources).alone())?
            .dir;
        let missing = plan.check(&dir)?;

        let launches = [connected, missing];
        (scenario.verify)(&mut run, &launches)?;
        run.sources_unchanged()?;
        let backend = launches[0]
            .frames
            .last()
            .map_or(Value::Null, |frame| frame.state()["backend"].clone());
        run.record("backend", backend);
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))
}

// -- The harness's agent. --

/// `luxforge-mcp` as this process runs it: killed if the run ends before it is waited for.
struct Adapter {
    child: std::process::Child,
    stdin: Option<ChildStdin>,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Adapter {
    fn start(mcp: &Path, catalog: &Path, out: &Path) -> Result<Self> {
        let mut child = launching::mcp_command(mcp, catalog)
            .stdin(Stdio::piped())
            .stdout(fs::File::create(out.join("mcp-stdout.jsonl"))?)
            .stderr(fs::File::create(out.join("mcp-stderr.log"))?)
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or("luxforge-mcp has no stdin")?;
        // As a client starts a stdio server: initialize, initialized, then its first tool call,
        // which attaches the adapter to the live session.
        for message in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "1.0"},
            }}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "luxforge_call", "arguments": {"method": "session.state"},
            }}),
        ] {
            writeln!(stdin, "{message}")?;
        }
        stdin.flush()?;
        Ok(Self {
            child,
            stdin: Some(stdin),
        })
    }

    /// Close its stdin, as a client that quits does, and wait for it to exit.
    fn finish(mut self, out: &Path) -> Result<Value> {
        drop(self.stdin.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break Some(status);
            }
            if started.elapsed() > Duration::from_secs(10) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let answers = fs::read_to_string(out.join("mcp-stdout.jsonl"))?
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap_or_else(|_| json!(line)))
            .collect::<Vec<_>>();
        Ok(json!({
            "exit_code": status.and_then(|status| status.code()),
            "stopped": status.is_none(),
            "answers": answers,
            "stderr": fs::read_to_string(out.join("mcp-stderr.log"))?,
        }))
    }
}

/// Wait for the `agents` launch, starting `luxforge-mcp` against its live session once the
/// script's `connected` step has begun, and closing it once the launch has exited.
fn watcher(
    mcp: PathBuf,
    evidence: PathBuf,
    catalog: PathBuf,
    out: PathBuf,
    connected_step: u64,
) -> Watcher {
    Box::new(
        move |child: &mut Guard,
              _: Instant,
              deadline: Duration|
              -> Result<(Option<ExitStatus>, Value)> {
            let mut adapter = None;
            let poll = Poll {
                every: Duration::from_millis(50),
                from: Instant::now(),
                deadline,
                late: "The agents launch timed out; killed and reaped",
            };
            let status = launching::until_exit(child, poll, |_| {
                if adapter.is_none() && step_begun(&evidence, connected_step) {
                    adapter = Some(Adapter::start(&mcp, &catalog, &out)?);
                }
                Ok(())
            })?;
            let kept = match adapter {
                Some(adapter) => adapter.finish(&out)?,
                None => json!({"started": false}),
            };
            Ok((Some(status), kept))
        },
    )
}

// -- The checks. --

fn tab(frame: &Frame) -> &Value {
    &frame.state()["settings"]["agents"]
}

/// The core's configuration for `client` and the server the tab found.
fn core_snippet(found: &Path, client: Client) -> Result<String> {
    Ok(agents::snippet(
        client,
        agents::Launch {
            binary: found,
            catalog: None,
            headless: false,
        },
        Platform::current(),
    )
    .map_err(|error| format!("The core has no {} configuration: {error:?}", client.id()))?
    .text)
}

fn check_snippet(frame: &Frame, step: &str, found: &Path, client: Client) -> Result {
    let tab = tab(frame);
    ensure(
        tab["chosen"] == client.id(),
        format!("{step}: chosen {} not {}", tab["chosen"], client.id()),
    )?;
    let core = core_snippet(found, client)?;
    ensure(
        tab["snippet"]["text"] == core.as_str(),
        format!(
            "{step}: the tab shows {} not the core's {core:?}",
            tab["snippet"]["text"]
        ),
    )
}

fn events<'a>(launch: &'a Checked, name: &str) -> Vec<&'a Value> {
    launch
        .events
        .iter()
        .filter(|event| event["event"] == name)
        .collect()
}

/// The scenario's checks over both launches and the adapter's record, from the files the run
/// wrote, so a replay checks the recorded run the same way.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [agents_launch, missing] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };

    // The empty list, with the server found beside the editor.
    let empty = agents_launch.at("empty")?;
    let settings = &empty.state()["settings"];
    ensure(
        settings["open"] == "agents",
        format!("empty: the sheet is at {}", settings["open"]),
    )?;
    let found = PathBuf::from(
        tab(empty)["server"]["found"]
            .as_str()
            .ok_or_else(|| format!("empty: no server found: {}", tab(empty)["server"]))?,
    );
    ensure(
        found.file_name() == Some(std::ffi::OsStr::new("luxforge-mcp")),
        format!("empty: found {}", found.display()),
    )?;
    ensure(
        tab(empty)["rows"] == json!([]) && tab(empty)["empty"] == UNNAMED_NONE,
        format!("empty: {}", tab(empty)),
    )?;
    ensure(
        tab(empty)["session_clients"] == json!({"clients": []}),
        format!(
            "empty: session.clients answered {}",
            tab(empty)["session_clients"]
        ),
    )?;
    check_snippet(empty, "empty", &found, Client::ClaudeCode)?;

    // Codex chosen and copied: the core's text, on the clipboard and in the log.
    let codex = agents_launch.at("codex")?;
    check_snippet(codex, "codex", &found, Client::Codex)?;
    let copied = agents_launch.at("copied")?;
    let core = core_snippet(&found, Client::Codex)?;
    ensure(
        tab(copied)["copied"] == json!({"client": "codex", "text": core}),
        format!("copied: {}", tab(copied)["copied"]),
    )?;
    let logged = events(agents_launch, "agents_configuration_copied");
    ensure(
        logged.len() == 1 && logged[0]["detail"]["text"] == core.as_str(),
        format!("copied: the log records {logged:?}"),
    )?;
    check_snippet(
        agents_launch.at("claude-code")?,
        "claude-code",
        &found,
        Client::ClaudeCode,
    )?;

    // The real adapter, listed as session.clients answered.
    let connected = agents_launch.at("connected")?;
    let rows = tab(connected)["rows"]
        .as_array()
        .ok_or("connected: no rows")?;
    let [row] = rows.as_slice() else {
        return Err(format!("connected: rows {rows:?}").into());
    };
    ensure(
        row["label"] == LABEL
            && row["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("Connected ")),
        format!("connected: row {row}"),
    )?;
    let listed = &tab(connected)["session_clients"]["clients"];
    let [client] = listed.as_array().map(Vec::as_slice).unwrap_or_default() else {
        return Err(format!("connected: session.clients answered {listed}").into());
    };
    ensure(
        client["client_id"] == row["client_id"]
            && client["label"] == LABEL
            && client["transport"] == "loopback"
            && client["requests"].as_u64().is_some_and(|n| n >= 1),
        format!("connected: the row {row} is not session.clients' {client}"),
    )?;

    // Disconnect: the same client, ended, and the list empty again.
    let disconnected = agents_launch.at("disconnected")?;
    ensure(
        tab(disconnected)["rows"] == json!([]) && tab(disconnected)["empty"] == UNNAMED_NONE,
        format!("disconnected: {}", tab(disconnected)),
    )?;
    ensure(
        tab(disconnected)["session_clients"] == json!({"clients": []}),
        format!(
            "disconnected: session.clients answered {}",
            tab(disconnected)["session_clients"]
        ),
    )?;
    let sent = events(agents_launch, "agents_disconnect");
    let answered = events(agents_launch, "agents_disconnected");
    ensure(
        sent.len() == 1
            && sent[0]["detail"]["client_id"] == row["client_id"]
            && answered.len() == 1
            && answered[0]["detail"]["client_id"] == row["client_id"]
            && answered[0]["detail"]["error"].is_null(),
        format!("disconnected: the log records {sent:?} then {answered:?}"),
    )?;

    // What the adapter itself answered: initialize, then the tool call through the session.
    let adapter = read_json(&run.out().join(MCP))?;
    let answers = adapter["answers"].as_array().ok_or("mcp: no answers")?;
    let answer = |id: u64| answers.iter().find(|answer| answer["id"] == id);
    ensure(
        answer(1).is_some_and(|answer| answer["result"]["protocolVersion"].is_string()),
        format!("mcp: initialize answered {:?}", answer(1)),
    )?;
    ensure(
        answer(2).is_some_and(|answer| {
            answer["result"].is_object() && answer["result"]["isError"] != true
        }),
        format!("mcp: the tool call answered {:?}", answer(2)),
    )?;
    ensure(
        adapter["exit_code"] == 0,
        format!(
            "mcp: exited {} ({})",
            adapter["exit_code"], adapter["stderr"]
        ),
    )?;

    // The build with nothing beside it.
    let alone = missing.at("agents")?;
    let looked = PathBuf::from(
        tab(alone)["server"]["missing"]
            .as_str()
            .ok_or_else(|| format!("missing: {}", tab(alone)["server"]))?,
    );
    ensure(
        looked
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|folder| {
                folder.to_string_lossy().starts_with(".luxforge-alone-")
                    || cfg!(target_os = "macos")
            })
            && !looked.exists(),
        format!("missing: looked for {}", looked.display()),
    )?;
    ensure(
        tab(alone)["snippet"].is_null() && tab(alone)["rows"] == json!([]),
        format!("missing: {}", tab(alone)),
    )?;
    run.record(
        "agents",
        json!({"server": found, "client": row, "missing": looked}),
    );
    Ok(())
}
