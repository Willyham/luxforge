//! The `external-module` smoke scenario: the Split toning example, a separately authored module,
//! built by `module-package` and installed, used, turned off, uninstalled and reinstalled through
//! the desktop's own gestures and the common APIs, on the real editor in the background, with no
//! change to host source for the module (`docs/design/module-manager.md#evidence-and-measurement`).
//!
//! Four launches share one data root and configuration (`shared/host/data`, `shared/host/config`,
//! empty before the first) and the first launch's catalog:
//!
//! - **install**: `schema.list` without the module, Install module… with the package picked
//!   (`module.package.inspect` behind its review sheet), Install, and Relaunch, which an evidence
//!   run records and does not perform.
//! - **installed**: the module in `module.list` and `schema.list` and its band in the panel, an
//!   edit through its slider and one through `edit.set-split-toning`, its GPU program probed and
//!   enabled on the window's adapter, a GPU export held to a reference export by the colour class's
//!   limits, a preset and a copy-settings paste carrying it to a second photograph, an MCP client's
//!   typed tool call on that photograph, the module turned off with the export unchanged, and
//!   Uninstall… confirmed and Relaunch.
//! - **uninstalled**: the first photograph keeps its layer, its band reads Not installed and its
//!   export refuses; Install module… again, and Relaunch.
//! - **reinstalled**: the band is the module's again, its program enabled, and the GPU and
//!   reference exports match the installed launch's byte for byte.
//!
//! Every frame of a `modules` step records the owner's `module.layout.read`, `module.list` and
//! `module.package.list` beside it, and the checks hold what the desktop draws to them.
use crate::{
    mcp_smoke::{Agent, Era},
    module_package,
    scenario::{
        Checked, Checks, Frame, Launch, Plan, Run, Step,
        launch::{self as launching, Guard, Poll, Watcher},
    },
    smoke::Scenario,
    usage_smoke::{open_asset, scripted_number, step_begun},
    *,
};
use luxforge_core::module_package::{COLOUR_LIMITS, ProbeDifference};
use luxforge_evidence::{
    self as script, CopySettingsStep as Copy, ModuleCustomize, ModuleSwitch, ModulesStep,
    PresetCreateStep, SliderStep, UsageAwait, UsageStep,
};
use std::{
    process::ExitStatus,
    time::{Duration, Instant},
};

pub const SCENARIO: &str = "external-module";
/// The photograph the module edits.
pub const PHOTO: &str = "fixtures/s0/orientation-1.jpg";
/// The photograph the copied settings, the preset and the MCP client's edit reach.
pub const OTHER: &str = "fixtures/s0/orientation-6.jpg";
/// The example module's crate, outside the workspace.
const EXAMPLE: &str = "examples/modules/split-toning";
/// Where the run keeps the package it builds.
const PACKAGE: &str = "package/split-toning.lfmodule";
/// The data root and configuration every launch shares, empty before the first.
const SHARED_DATA: &str = "shared/host/data";
const SHARED_CONFIG: &str = "shared/host/config";
const INSTALL: &str = "install";
const INSTALLED: &str = "installed";
const UNINSTALLED: &str = "uninstalled";
const REINSTALLED: &str = "reinstalled";
const MODULE: &str = "org.example.splittoning";
const EFFECT: &str = "org.example.splittoning.adjust";
const SET: &str = "edit.set-split-toning";
const RESET: &str = "edit.reset-split-toning";
/// The module's set action, as its controls name it.
const ACTION: &str = "set-split-toning";
const BASIC: &str = "luxforge.basic";
const TITLE: &str = "Split toning";
/// What the MCP client's typed tool for the module's edit is called.
const TOOL: &str = "edit_set-split-toning";
const PRESET: &str = "Split tone look";
/// The module's copy and preset groups.
const GROUPS: [&str; 3] = [
    "Split toning · Highlights",
    "Split toning · Balance",
    "Split toning · Shadows",
];
const EXPORT_GPU: &str = "split-toning.jpg";
const EXPORT_REFERENCE: &str = "split-toning-reference.jpg";
const EXPORT_OFF: &str = "split-toning-off.jpg";
const EXPORT_REFUSED: &str = "split-toning-refused.jpg";
/// The adapter's record: the MCP client's exit, its tools and its edit's answer.
const MCP: &str = "mcp.json";
/// The MCP client's edit of the second photograph.
const MCP_SHADOW_HUE: f64 = 160.0;
/// The installed launch's process deadline: the editor's own five-minute script deadline.
const DEADLINE: Duration = Duration::from_secs(300);

pub const NOTE: &str = "Four launches, `install`, `installed`, `uninstalled` and `reinstalled`, each started with `--evidence-data shared/host/data` and `--evidence-config shared/host/config` (both empty before `install`), and the last three with `--evidence-catalog install/catalog.sqlite`, so they are one person's data, configuration and catalog across relaunches. Before the first launch this process builds the example `examples/modules/split-toning` with `cargo xtask module-package` into `package/split-toning.lfmodule`, which `install` and `uninstalled` install through Install module…. Each launch's Relaunch is recorded, not performed; the next launch is this process's. During `installed`, once its `mcp-edit` step has begun, this process runs `luxforge-mcp --catalog install/catalog.sqlite --tools asset,edit,history` against the live session as a client of the 2025-11-25 revision, lists its tools and calls `edit_set-split-toning` on the open photograph; `mcp-legacy.jsonl` holds every line, `mcp.json` its exit and answers.";

fn modules(name: &str, step: ModulesStep) -> Step {
    Step::new(name, script::Step::Modules(step)).commits(0)
}

/// The edit an agent makes elsewhere, awaited: captured once the open photograph's revision moves.
fn awaited() -> UsageStep {
    UsageStep::Await(UsageAwait {
        edited: true,
        timeout_ms: Some(script::MAX_USAGE_AWAIT_MS),
        ..UsageAwait::default()
    })
}

/// `install`: the module unknown to `schema.list`, Install module… and its review sheet, Install,
/// and Relaunch.
pub fn install_plan(package: &str) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(EFFECT),
        Step::new("schema-before", script::Step::api("schema.list")).commits(0),
        modules("inspected", ModulesStep::Install(package.into())),
        modules("installed", ModulesStep::ReviewInstall),
        modules("relaunch", ModulesStep::Relaunch),
    ])
}

/// `installed`: the module through the desktop, the API, the GPU, exports, a preset, copy settings
/// and MCP, then off, and Uninstall….
pub fn installed_plan() -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(EFFECT),
        Step::new("module-list", script::Step::api("module.list")).commits(0),
        Step::new("schema-list", script::Step::api("schema.list")).commits(0),
        // Basic collapsed, so the module's band, expanded by default, has its sliders on screen.
        Step::new("basic-collapsed", script::Step::section(BASIC, false))
            .commits(0)
            .collapsed(BASIC)
            .expanded(MODULE),
        // Through the desktop: the highlights' saturation slider, released.
        Step::new(
            "desktop-edit",
            SliderStep::new(ACTION, "highlight-saturation", [40.0]).release(),
        )
        .no_draft()
        .commits(1),
        // Through the API: the module's own method, as any client calls it.
        Step::new(
            "api-edit",
            script::Step::call(
                SET,
                json!({"shadow-saturation": 30, "balance": 20, "shadow-hue": 200}),
            ),
        )
        .commits(1),
        // The view's first use of the program asked for its probe: captured once it has run.
        modules("probed", ModulesStep::Probed(MODULE.into())),
        // Settings › Modules at the module: its row and detail say where the program draws.
        modules(
            "program-detail",
            ModulesStep::Customize(ModuleCustomize {
                toolset: "adjustments".into(),
                module: Some(MODULE.into()),
            }),
        ),
        Step::new("detail-closed", script::Step::settings(false)).commits(0),
        Step::new("gpu-export", script::Step::export(EXPORT_GPU, false)).commits(0),
        Step::new(
            "reference-export",
            script::Step::export_reference(EXPORT_REFERENCE),
        )
        .commits(0),
        // A preset of the module's groups, captured from this entry.
        Step::new(
            "preset",
            PresetCreateStep {
                analysis: Vec::new(),
                name: PRESET.into(),
                group: None,
                groups: GROUPS.iter().map(|group| (*group).to_owned()).collect(),
                submit: true,
            },
        )
        .commits(0),
        // Copy settings… with the module's groups alone.
        Step::new("copy-chooser", Copy::Chooser).commits(0),
        Step::new("copy-none", Copy::None).commits(0),
        Step::new(
            "copy-highlights",
            Copy::Check {
                group: GROUPS[0].into(),
                checked: true,
            },
        )
        .commits(0),
        Step::new(
            "copy-balance",
            Copy::Check {
                group: GROUPS[1].into(),
                checked: true,
            },
        )
        .commits(0),
        Step::new(
            "copy-shadows",
            Copy::Check {
                group: GROUPS[2].into(),
                checked: true,
            },
        )
        .commits(0),
        Step::new("copied", Copy::Chosen).commits(0),
        // The second photograph, pasted onto.
        Step::new("other", script::Step::Open { path: OTHER.into() }).no_layer(EFFECT),
        Step::new("pasted", Copy::Paste).commits(1),
        Step::new("unpasted", script::Step::api("history.undo"))
            .commits(1)
            .label("Original")
            .no_layer(EFFECT),
        Step::new("preset-applied", script::Step::preset(PRESET))
            .commits(1)
            .label(format!("Preset: {PRESET}")),
        // The MCP client's edit, made once this step has begun.
        Step::new("mcp-edit", awaited()).commits(1),
        // The first photograph again: the module turned off, its export unchanged, and on.
        Step::new("photo", script::Step::Open { path: PHOTO.into() }),
        modules(
            "off",
            ModulesStep::Enabled(ModuleSwitch {
                module: MODULE.into(),
                on: false,
            }),
        ),
        Step::new("off-export", script::Step::export(EXPORT_OFF, false)).commits(0),
        modules(
            "on",
            ModulesStep::Enabled(ModuleSwitch {
                module: MODULE.into(),
                on: true,
            }),
        ),
        Step::new("modules-tab", script::Step::settings_tab("modules")).commits(0),
        modules("uninstall", ModulesStep::Uninstall(MODULE.into())),
        modules("uninstalled", ModulesStep::ConfirmUninstall),
        modules("relaunch", ModulesStep::Relaunch),
    ])
}

/// `uninstalled`: the layer kept, the band Not installed, the export refused; Install module…
/// again and Relaunch.
pub fn uninstalled_plan(package: &str) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        // Basic collapsed, so the Not installed band after the registered sections is on screen.
        Step::new("basic-collapsed", script::Step::section(BASIC, false))
            .commits(0)
            .collapsed(BASIC),
        Step::new(
            "refused-export",
            script::Step::export(EXPORT_REFUSED, false),
        )
        .commits(0)
        .refused(format!("plan: incompatible: unavailable effect {EFFECT}")),
        modules("inspected", ModulesStep::Install(package.into())),
        modules("installed", ModulesStep::ReviewInstall),
        modules("relaunch", ModulesStep::Relaunch),
    ])
}

/// `reinstalled`: the module's band again, its program enabled, and both exports again.
pub fn reinstalled_plan() -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("basic-collapsed", script::Step::section(BASIC, false))
            .commits(0)
            .collapsed(BASIC),
        modules("probed", ModulesStep::Probed(MODULE.into())),
        Step::new("gpu-export", script::Step::export(EXPORT_GPU, false)).commits(0),
        Step::new(
            "reference-export",
            script::Step::export_reference(EXPORT_REFERENCE),
        )
        .commits(0),
    ])
}

/// One launch: the shared data and configuration, the first launch's catalog after it, the first
/// photograph opened.
fn launch_of(
    run: &Run,
    scenario: &Scenario,
    name: &str,
    plan: &Plan,
    sources: &[PathBuf],
) -> Result<Launch> {
    let out = std::path::absolute(run.out())?;
    let mut launch = Launch::named(name)
        .evidence_data(&out.join(SHARED_DATA))
        .evidence_config(&out.join(SHARED_CONFIG))
        .open_all(&sources[..1])
        .script(&format!("{name}-script.json"), plan.script());
    if name != INSTALL {
        launch = launch.evidence_catalog(&out.join(INSTALL).join("catalog.sqlite"), "quit");
    }
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    Ok(launch)
}

/// The row's own run: the package built, the four launches, and the MCP client beside the second.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    run.note(NOTE);
    let outcome = (|| -> Result {
        run.hash(&sources)?;
        let package = std::path::absolute(run.out().join(PACKAGE))?;
        if !run.replaying() {
            // No host source changes for the module: the package is the example crate's, built
            // and checked by the authoring command a module author runs.
            let built = module_package::run(run.root(), &run.root().join(EXAMPLE), &package)?;
            run.record("package", json!({"path": PACKAGE, "hash": built.hash}));
            for dir in [SHARED_DATA, SHARED_CONFIG] {
                fs::create_dir_all(run.out().join(dir))?;
            }
        }
        // A replay's scripts name the package where the recorded run built it.
        let package = match run.recorded(&package) {
            Some(recorded) => std::path::absolute(recorded)?,
            None => package,
        };
        let package = package.to_string_lossy().into_owned();
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

        let plan = install_plan(&package);
        let launch = launch_of(&run, scenario, INSTALL, &plan, &sources)?;
        let dir = run.launch(launch)?.dir;
        let install = plan.check(&dir)?;

        let plan = installed_plan();
        let mut launch = launch_of(&run, scenario, INSTALLED, &plan, &sources)?.deadline(DEADLINE);
        if let Some(mcp) = mcp {
            let out = std::path::absolute(run.out())?;
            launch = launch
                .watch(watcher(
                    mcp,
                    out.join(INSTALLED),
                    out.join(INSTALL).join("catalog.sqlite"),
                    out.clone(),
                    scripted_number(&plan, "mcp-edit")?,
                ))
                .keep(MCP);
        }
        let dir = run.launch(launch)?.dir;
        let installed = plan.check(&dir)?;
        // The originals after the edits and exports, as after every launch.
        run.sources_unchanged()?;

        let plan = uninstalled_plan(&package);
        let launch = launch_of(&run, scenario, UNINSTALLED, &plan, &sources)?;
        let dir = run.launch(launch)?.dir;
        let uninstalled = plan.check(&dir)?;

        let plan = reinstalled_plan();
        let launch = launch_of(&run, scenario, REINSTALLED, &plan, &sources)?;
        let dir = run.launch(launch)?.dir;
        let reinstalled = plan.check(&dir)?;

        let launches = [install, installed, uninstalled, reinstalled];
        (scenario.verify)(&mut run, &launches)?;
        run.sources_unchanged()?;
        let backend = launches[1]
            .frames
            .last()
            .map_or(Value::Null, |frame| frame.state()["backend"].clone());
        run.record("backend", backend);
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))
}

// -- The harness's MCP client. --

/// The client's handshake, its tools and its edit of `asset` through the module's typed tool.
fn mcp_edit(agent: &mut Agent, asset: &str) -> Result<Value> {
    agent.ask(
        "initialize",
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "codex", "title": "Codex", "version": "1.0.0"}}),
    )?;
    agent.notify("notifications/initialized")?;
    let tools = agent.ask("tools", "tools/list", json!({}))?;
    let names: Vec<Value> = tools["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| tool["name"].clone())
        .collect();
    let edit = agent.method(
        "edit",
        SET,
        json!({"asset_id": asset, "shadow-hue": MCP_SHADOW_HUE}),
    )?;
    let history = agent.method(
        "history",
        "history.list",
        json!({"asset_id": asset, "limit": 3}),
    )?;
    Ok(json!({"asset_id": asset, "tools": names, "edit": edit, "history": history}))
}

/// Wait for the installed launch, running the MCP client once its `mcp-edit` step has begun and
/// finishing it once the launch has exited. Its record is written whatever happens.
fn watcher(mcp: PathBuf, evidence: PathBuf, catalog: PathBuf, out: PathBuf, step: u64) -> Watcher {
    Box::new(
        move |child: &mut Guard,
              _: Instant,
              deadline: Duration|
              -> Result<(Option<ExitStatus>, Value)> {
            let mut agent: Option<Agent> = None;
            let mut answered = Value::Null;
            let poll = Poll {
                every: Duration::from_millis(50),
                from: Instant::now(),
                deadline,
                late: "The installed launch timed out; killed and reaped",
            };
            let outcome = launching::until_exit(child, poll, |_| {
                if agent.is_none() && step_begun(&evidence, step) {
                    let asset = open_asset(&evidence)?;
                    let started = agent.insert(Agent::start(
                        &mcp,
                        &catalog,
                        Era::Legacy,
                        &["--tools", "asset,edit,history"],
                        &out,
                    )?);
                    answered = mcp_edit(started, &asset)?;
                }
                Ok(())
            });
            let mut record = json!({"answered": answered});
            if let Some(agent) = agent {
                record["client"] = agent.finish(&out)?;
            }
            if let Err(error) = &outcome {
                record["error"] = json!(error.to_string());
            }
            write_json(&out.join(MCP), &record)?;
            Ok((Some(outcome?), record))
        },
    )
}

// -- The checks. --

/// What the Settings sheet's Modules tab records of external modules.
fn external(frame: &Frame) -> &Value {
    &frame.state()["settings"]["modules"]["external"]
}

/// The owner's reads a `modules` step recorded beside its frame.
fn owner<'a>(frame: &'a Frame, name: &str) -> Result<&'a Value> {
    let owner = &frame["step"]["owner"];
    ensure(
        owner.is_object(),
        format!("{name}: the step records no owner reads"),
    )?;
    Ok(owner)
}

/// The `module.package.list` row of the module, as the owner answered beside the frame.
fn package_row<'a>(frame: &'a Frame, name: &str) -> Result<Option<&'a Value>> {
    Ok(owner(frame, name)?["module_package_list"]["modules"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["module"] == MODULE))
}

/// The tools panel's band of `module`, as the frame draws it.
fn band<'a>(frame: &'a Frame, module: &str) -> Option<&'a Value> {
    frame.state()["module_layout"]["bands"]
        .as_array()?
        .iter()
        .find(|band| band["module"] == module)
}

/// Whether `schema.list`'s answer names `method`.
fn schema_names(answer: &Value, method: &str) -> bool {
    match &answer["methods"] {
        Value::Object(methods) => methods.contains_key(method),
        Value::Array(methods) => methods
            .iter()
            .any(|entry| entry == method || entry["name"] == method),
        _ => false,
    }
}

/// The open photograph's asset.
fn asset(frame: &Frame) -> Option<&str> {
    frame.state()["develop"]["document"]["asset_id"].as_str()
}

/// An 8-bit sRGB export decoded to linear-sRGB pixels, row by row.
fn linear(path: &Path) -> Result<(Vec<[f32; 3]>, usize, usize)> {
    let image = image::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .to_rgb8();
    let decode = |code: u8| luxforge_reference::srgb::decode(code) as f32;
    let pixels = image.pixels().map(|pixel| pixel.0.map(decode)).collect();
    Ok((pixels, image.width() as usize, image.height() as usize))
}

/// The run's file at `name` inside `dir`, or the recorded run's in a replay.
fn file(run: &Run, path: &Path) -> PathBuf {
    if path.exists() {
        path.into()
    } else {
        run.recorded(path).unwrap_or_else(|| path.into())
    }
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [install, installed, uninstalled, reinstalled] = launches else {
        return Err(format!("Expected four launches, found {}", launches.len()).into());
    };
    let mut checks = Checks::new();

    // -- install: an empty store, the review sheet, the package stored, Relaunch recorded. --
    let before = install.at("schema-before")?;
    ensure(
        !schema_names(&before["step"]["result"], SET) && band(before, MODULE).is_none(),
        "Before installing, schema.list or the panel already has the module",
    )?;
    let inspected = install.at("inspected")?;
    let review = &external(inspected)["review"];
    ensure(
        review["state"] == "inspected"
            && review["module"] == MODULE
            && review["title"] == "Install Split toning 0.1.0?"
            && review["confirm"]["label"] == "Install"
            && review["confirm"]["enabled"] == true,
        format!("inspected: the review sheet shows {review}"),
    )?;
    ensure(
        package_row(inspected, "inspected")?.is_none(),
        "inspected: the store holds the module before Install",
    )?;
    checks.note(inspected, "the review sheet of the package", review.clone());
    let stored = install.at("installed")?;
    let row =
        package_row(stored, "installed")?.ok_or("installed: module.package.list has no row")?;
    ensure(
        row["state"] == "pending-relaunch"
            && row["installed"]["version"] == "0.1.0"
            && row["running"].is_null(),
        format!("installed: the row is {row}"),
    )?;
    ensure(
        external(stored)["review"].is_null()
            && external(stored)["pending"]
                .as_str()
                .is_some_and(|line| line.contains("relaunch")),
        format!(
            "installed: the sheet is still open or no relaunch is pending: {}",
            external(stored)
        ),
    )?;
    checks.note(stored, "installed, pending a relaunch", row.clone());
    let relaunch = |launch: &Checked, name: &str| -> Result<Value> {
        let frame = launch.at("relaunch")?;
        let recorded = &external(frame)["relaunch"];
        let args: Vec<&str> = recorded["command"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        ensure(
            recorded["evidence"] == true
                && recorded["command"]["program"].is_string()
                // Relaunch drops each opened photograph, so the remembered catalog reopens it.
                && !args.iter().any(|arg| arg.ends_with(".jpg"))
                && args.contains(&"--evidence-data"),
            format!("{name}: Relaunch recorded {recorded}"),
        )?;
        Ok(recorded.clone())
    };
    let first_relaunch = relaunch(install, INSTALL)?;
    checks.note(
        install.at("relaunch")?,
        "Relaunch recorded, without --open",
        first_relaunch,
    );
    // The store holds exactly the bytes built.
    let package = file(run, &run.out().join(PACKAGE));
    let package_hash = format!("sha256:{}", hash(&package)?);
    ensure(
        row["installed"]["hash"] == package_hash.as_str(),
        format!(
            "installed: the store holds {} for the package {package_hash}",
            row["installed"]["hash"]
        ),
    )?;

    // -- installed: the common APIs, the panel, the GPU and exports, preset, copy, MCP, off. --
    let opened = installed.at("opened")?;
    let section = band(opened, MODULE).ok_or("installed: the panel draws no Split toning band")?;
    ensure(
        section["title"] == TITLE
            && section["badge"] == "Community"
            && section["unavailable"] == false,
        format!("installed: the band is {section}"),
    )?;
    let listed = installed.at("module-list")?["step"]["result"]["modules"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|module| module["id"] == MODULE)
        .cloned()
        .ok_or("installed: module.list does not list the module")?;
    ensure(
        listed["origin"] == "community" && listed["toolset"] == "adjustments",
        format!("installed: module.list lists {listed}"),
    )?;
    let schema = &installed.at("schema-list")?["step"]["result"];
    ensure(
        schema_names(schema, SET) && schema_names(schema, RESET),
        "installed: schema.list does not name the module's actions",
    )?;
    checks.note(
        installed.at("schema-list")?,
        "module.list and schema.list with the module",
        json!({"module": listed, "methods": [SET, RESET]}),
    );
    let desktop = installed
        .at("desktop-edit")?
        .payload(EFFECT)
        .ok_or("desktop-edit: no layer")?;
    ensure(
        desktop["highlight-saturation"] == 40.0,
        format!("desktop-edit: the layer holds {desktop}"),
    )?;
    let edited = installed
        .at("api-edit")?
        .payload(EFFECT)
        .ok_or("api-edit: no layer")?
        .clone();
    ensure(
        edited["highlight-saturation"] == 40.0
            && edited["shadow-saturation"] == 30.0
            && edited["balance"] == 20.0,
        format!("api-edit: the layer holds {edited}"),
    )?;
    checks.note(
        installed.at("api-edit")?,
        "edited through the slider and edit.set-split-toning",
        json!({"desktop": desktop, "api": edited}),
    );

    // The program probed and enabled on the window's adapter; the frame drawn on the GPU.
    let probed = |launch: &Checked, name: &str| -> Result<Value> {
        let frame = launch.at("probed")?;
        let row = package_row(frame, name)?.ok_or("probed: no row")?;
        let program = &row["program"];
        ensure(
            program["state"] == "enabled" && program["adapter"].is_string(),
            format!("{name}: the program is {program}"),
        )?;
        Ok(json!({"program": program}))
    };
    let program = probed(installed, INSTALLED)?;
    let adapter = program["program"]["adapter"].clone();
    checks.note(installed.at("probed")?, "the program enabled", program);
    // The Modules tab draws the same state: the row the owner listed, and its detail.
    let tab = installed.at("program-detail")?;
    let drawn = external(tab)["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["module"] == MODULE)
        .cloned()
        .ok_or("program-detail: the Modules tab lists no external row")?;
    let detail = &tab.state()["settings"]["modules"]["detail"];
    ensure(
        drawn["program"]["state"] == "enabled"
            && drawn["program"]["adapter"] == adapter
            && detail["title"] == TITLE
            && detail["external"]["program"].is_string(),
        format!("program-detail: the tab draws {drawn} with the detail {detail}"),
    )?;
    checks.note(
        tab,
        "Settings › Modules: the program drawing on the adapter",
        json!({"row": drawn, "detail": detail["external"]}),
    );
    let probe_events: Vec<&Value> = installed
        .events
        .iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|name| name.starts_with("gpu_program_probe"))
        })
        .collect();
    ensure(
        probe_events.iter().any(|event| {
            event["event"] == "gpu_program_probed" && event["detail"]["state"]["state"] == "enabled"
        }),
        format!("installed: the log has no enabled probe: {probe_events:?}"),
    )?;

    // The GPU export within the colour class of the reference export.
    let gpu = file(run, &installed.evidence.join(EXPORT_GPU));
    let reference = file(run, &installed.evidence.join(EXPORT_REFERENCE));
    let (ours, width, height) = linear(&gpu)?;
    let (theirs, ref_width, ref_height) = linear(&reference)?;
    ensure(
        (width, height) == (ref_width, ref_height),
        format!("The exports are {width}×{height} and {ref_width}×{ref_height}"),
    )?;
    let renderer = |launch: &Checked, step: &str| -> Result<Value> {
        Ok(launch.at(step)?["step"]["export"]["record"]["result"]["renderer"]["record"].clone())
    };
    for (launch, name) in [(installed, INSTALLED), (reinstalled, REINSTALLED)] {
        ensure(
            renderer(launch, "gpu-export")? == "gpu"
                && renderer(launch, "reference-export")? == "reference",
            format!(
                "{name}: the exports were drawn by {} and {}",
                renderer(launch, "gpu-export")?,
                renderer(launch, "reference-export")?
            ),
        )?;
    }
    ensure(
        renderer(installed, "off-export")? == "gpu",
        "The export with the module off was not drawn on the GPU",
    )?;
    let difference = ProbeDifference::of(&ours, &theirs, width, height)?;
    ensure(
        difference.within(&COLOUR_LIMITS),
        format!("The GPU export is outside the colour class against the reference: {difference}"),
    )?;
    let gpu_plan = &installed.at("gpu-export")?["step"]["export"];
    checks.note(
        installed.at("gpu-export")?,
        "the GPU export against the reference export",
        json!({"difference": difference.to_string(), "adapter": adapter,
               "gpu_export": gpu_plan, "reference_export": installed.at("reference-export")?["step"]["export"]}),
    );

    // The preset and the copied settings carry the layer to the second photograph.
    let pasted = installed
        .at("pasted")?
        .payload(EFFECT)
        .ok_or("pasted: no layer")?;
    ensure(
        pasted == &edited,
        format!("pasted: the layer holds {pasted}, not {edited}"),
    )?;
    let preset = installed
        .at("preset-applied")?
        .payload(EFFECT)
        .ok_or("preset-applied: no layer")?;
    ensure(
        preset == &edited,
        format!("preset-applied: the layer holds {preset}, not {edited}"),
    )?;
    let other = asset(installed.at("other")?).ok_or("other: no photograph")?;
    let photo = asset(installed.at("opened")?).ok_or("opened: no photograph")?;
    ensure(other != photo, "The second photograph is the first")?;
    checks.note(
        installed.at("preset-applied")?,
        "the copied settings and the preset on the second photograph",
        json!({"pasted": pasted, "preset": preset}),
    );

    // The MCP client's typed tool.
    let mcp = read_json(&file(run, &run.out().join(MCP)))?;
    let answered = &mcp["answered"];
    ensure(
        answered["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool == TOOL)),
        format!(
            "The MCP client's tools do not list {TOOL}: {}",
            answered["tools"]
        ),
    )?;
    ensure(
        answered["asset_id"] == other,
        format!("The MCP client edited {} not {other}", answered["asset_id"]),
    )?;
    let agent = installed
        .at("mcp-edit")?
        .payload(EFFECT)
        .ok_or("mcp-edit: no layer")?;
    ensure(
        agent["shadow-hue"] == MCP_SHADOW_HUE && agent["shadow-saturation"] == 30.0,
        format!("mcp-edit: the layer holds {agent}"),
    )?;
    checks.note(
        installed.at("mcp-edit")?,
        "the MCP client's edit_set-split-toning on screen",
        json!({"layer": agent, "edit": answered["edit"], "history": answered["history"]}),
    );

    // Off: the edit kept and the export unchanged.
    let off = installed.at("off")?;
    let kept = band(off, MODULE).ok_or("off: no band")?;
    ensure(
        kept["kept"].is_array()
            && owner(off, "off")?["module_list"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|module| module["id"] == MODULE && module["enabled"] == false),
        format!("off: the band is {kept}"),
    )?;
    let off_export = file(run, &installed.evidence.join(EXPORT_OFF));
    let (gpu_hash, off_hash) = (hash(&gpu)?, hash(&off_export)?);
    ensure(
        gpu_hash == off_hash,
        format!("The export with the module off differs: {off_hash}, not {gpu_hash}"),
    )?;
    checks.note(
        installed.at("off-export")?,
        "the module off: Off · edit kept, the export unchanged",
        json!({"band": kept, "export": off_hash}),
    );
    // The originals, hashed before the first launch, unchanged after the exports.
    let originals = run.recorded_value("fixture_hashes").clone();
    for source in run.sources() {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("A source without a file name")?;
        ensure(
            json!(hash(source)?) == originals[name],
            format!("{} changed", source.display()),
        )?;
    }
    checks.note(
        installed.at("off-export")?,
        "the originals' bytes unchanged",
        originals,
    );

    // Uninstall… confirmed, pending a relaunch.
    let asked = &external(installed.at("uninstall")?)["uninstall"];
    ensure(
        asked["module"] == MODULE && asked["title"] == "Uninstall Split toning?",
        format!("uninstall: the confirmation is {asked}"),
    )?;
    let gone = installed.at("uninstalled")?;
    let row = package_row(gone, "uninstalled")?.ok_or("uninstalled: no row")?;
    ensure(
        row["installed"].is_null() && row["state"] == "pending-relaunch",
        format!("uninstalled: the row is {row}"),
    )?;
    relaunch(installed, INSTALLED)?;

    // -- uninstalled: the layer kept, Not installed, the export refused. --
    let opened = uninstalled.at("opened")?;
    ensure(
        asset(opened) == Some(photo),
        format!(
            "uninstalled: the photograph is {:?}, not {photo}",
            asset(opened)
        ),
    )?;
    ensure(
        opened.payload(EFFECT) == Some(&edited),
        format!(
            "uninstalled: the layer is {:?}, not {edited}",
            opened.payload(EFFECT)
        ),
    )?;
    let missing = band(opened, MODULE).ok_or("uninstalled: no Not installed band")?;
    ensure(
        missing["unavailable"] == true
            && missing["unavailable_reason"] == "Not installed"
            && missing["title"] == MODULE,
        format!("uninstalled: the band is {missing}"),
    )?;
    let refused = uninstalled.at("refused-export")?;
    ensure(
        !uninstalled.evidence.join(EXPORT_REFUSED).exists(),
        "uninstalled: the refused export wrote a file",
    )?;
    checks.note(
        refused,
        "uninstalled: the layer kept, Not installed, the export refused",
        json!({"band": missing, "step": refused["step"], "layer": opened.payload(EFFECT)}),
    );
    relaunch(uninstalled, UNINSTALLED)?;

    // -- reinstalled: the same exports byte for byte. --
    let opened = reinstalled.at("opened")?;
    let section = band(opened, MODULE).ok_or("reinstalled: no band")?;
    ensure(
        section["badge"] == "Community" && section["unavailable"] == false,
        format!("reinstalled: the band is {section}"),
    )?;
    let program = probed(reinstalled, REINSTALLED)?;
    ensure(
        program["program"]["adapter"] == adapter,
        format!(
            "reinstalled: the program is enabled on {}",
            program["program"]
        ),
    )?;
    for name in [EXPORT_GPU, EXPORT_REFERENCE] {
        let (first, again) = (
            hash(&file(run, &installed.evidence.join(name)))?,
            hash(&file(run, &reinstalled.evidence.join(name)))?,
        );
        ensure(
            first == again,
            format!("{name} differs after reinstalling: {again}, not {first}"),
        )?;
    }
    checks.note(
        reinstalled.at("gpu-export")?,
        "reinstalled: the exports match the first byte for byte",
        json!({"gpu": gpu_hash, "program": program}),
    );

    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "package": package_hash,
            "scope": "Four launches over one data root, configuration and catalog. The GPU export \
                      is held to the reference export by CIEDE2000 on 8-bit sRGB within the colour \
                      class's limits; the off, reinstalled GPU and reinstalled reference exports to \
                      the first byte for byte; every modules step's frame to the owner's reads \
                      beside it",
        }),
    )
}
