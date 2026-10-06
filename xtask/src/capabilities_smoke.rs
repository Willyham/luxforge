//! Rendered evidence for module capabilities: the developer proof module set up through generic
//! `api` steps, as any client sets it up, and driven through the desktop's own task control,
//! consent notice and Apply, against a loopback [`ProofEndpoint`] this process starts. The editor
//! runs hidden in the background harness with `--developer --proof-endpoint`, so its settings,
//! grants, resources and in-memory secret store live inside the evidence directory.
//!
//! The script's secret steps carry a sentinel key, so the script itself is written outside the
//! output directory and removed after the run; the copy kept beside the evidence is redacted, and
//! the editor records those steps redacted too. Every file under the output directory is scanned
//! for the sentinel afterwards.
use crate::{
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance, pixels, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{
    AssetId, EditorService, EntryId, ModuleRegistry, PROOF_GENERATE_PATH, RegistryOptions,
};
use luxforge_evidence::{self as script, CapabilityAction, CapabilityStep};
use luxforge_testbase::ProofEndpoint;
use luxforge_testkit::proof_protocol;
use std::{sync::Arc, time::Duration};

const MODULE: &str = "luxforge.capabilities";
const TASK: &str = "generate-proof-tint";
const TINT_EFFECT: &str = "luxforge.capabilities.tint";
/// The label the plan gives its profile, which later steps name it by.
const PROFILE: &str = "Local proof";
/// How long the endpoint holds the palette download and each generation, so a frame can be
/// captured while the install or the task is still running. Well inside every transfer and adapter
/// deadline.
const DELAY: Duration = Duration::from_millis(4000);
/// A captured tinted frame and the core's own render of the same stack may differ by this much per
/// channel, in 8-bit codes, over the window's mean: linear filtering of the magnified photograph.
const MEAN_TOLERANCE: f64 = 2.0;
/// One capability step of the proof module, whose frame waits for the jobs it starts.
fn step(action: CapabilityAction) -> CapabilityStep {
    CapabilityStep::new(MODULE, action)
}

/// Every frame, in order, against the endpoint at `base`, with the sentinel key in the secret
/// steps' script and redacted in what is kept and recorded. The settings, the profile, its key,
/// the download grant and the install are `api` steps; the consent notice, the task control and
/// Apply are the desktop's own, since they are what must be seen. Only Apply commits anything:
/// every other step is the capability's own state, not the photograph's history.
pub fn plan(base: &str, key: &str, wrong: &str) -> Plan {
    let layout = |name: &str, script: script::Step| Step::new(name, script).commits(0);
    let capability = |name: &str, step: CapabilityStep| Step::new(name, step).commits(0);
    let gesture = |name: &str, action: CapabilityAction| capability(name, step(action));
    let api = |name: &str, method: &str, params: Value| {
        Step::new(name, script::Step::call(method, params)).commits(0)
    };
    // A secret step is sent with its value, and kept and recorded with it redacted.
    let secret = |name: &str, value: &str| {
        api(
            name,
            "module.settings.set-secret",
            json!({"module_id": MODULE, "profile_id": {"name": PROFILE}, "setting": "api-key", "value": value}),
        )
    };
    let task = || CapabilityAction::Task(TASK.into());
    let steps = vec![
        Step::opened("opened"),
        // Layout: the sections that start expanded are collapsed, the proof section is expanded
        // and the panel is scrolled to its end, so the whole capability block is on screen.
        layout(
            "basic-collapsed",
            script::Step::section("luxforge.basic", false),
        )
        .collapsed("luxforge.basic"),
        layout(
            "crop-collapsed",
            script::Step::section("luxforge.crop", false),
        )
        .collapsed("luxforge.crop"),
        layout("expanded", script::Step::section(MODULE, true)).expanded(MODULE),
        layout("scrolled", script::Step::tools_scroll(1.0)).expanded(MODULE),
        // The block's first read, which the settings writes are made against.
        gesture("read", CapabilityAction::Settle),
        // Its settings through the API: strength, a profile, the profile's endpoint and its key.
        api(
            "strength",
            "module.settings.set",
            json!({"module_id": MODULE, "values": {"strength": 0.8}}),
        ),
        api(
            "profile",
            "module.profile.create",
            json!({"module_id": MODULE, "adapter": "proof-echo", "label": PROFILE}),
        ),
        api(
            "endpoint",
            "module.settings.set",
            json!({"module_id": MODULE, "profile_id": {"name": PROFILE}, "values": {"endpoint": format!("{base}{PROOF_GENERATE_PATH}")}}),
        ),
        secret("key", key),
        // The palette's download granted and installed through the API: installing, then
        // installed.
        api(
            "palette-allowed",
            "module.permission.grant",
            json!({"module_id": MODULE, "capability": "palette", "scope": {"resource": "proof-palette", "version": "1", "origin": base}}),
        ),
        api(
            "installing",
            "module.resource.install",
            json!({"module_id": MODULE, "resource_id": "proof-palette"}),
        ),
        gesture("installed", CapabilityAction::Settle),
        // The photo-data consent, declined, then asked again and allowed; the running task, its
        // result, and Apply.
        gesture("task-asked", task()).notice("Allow Capabilities proof to send photo data?"),
        gesture("denied", CapabilityAction::Consent(false)),
        gesture("task-asked-again", task()),
        capability("running", step(CapabilityAction::Consent(true)).no_wait()),
        gesture("succeeded", CapabilityAction::Settle),
        Step::new("applied", step(CapabilityAction::Apply))
            .commits(1)
            .label("Apply proof tint"),
        // A second real task is cancelled through its displayed Performance row.
        capability("cancel-running", step(task()).no_wait()),
        layout("cancel-listed", script::Step::wait(1600)),
        layout(
            "cancel-requested",
            script::Step::PerformanceCancel { row: 0 },
        ),
        gesture("cancelled", CapabilityAction::Settle),
        // A wrong key makes the endpoint refuse, and the failure is shown.
        secret("wrong-key", wrong),
        gesture("refused-task", task()),
    ];
    let mut monitored = Vec::new();
    for entry in steps {
        let name = entry.name().to_owned();
        if cfg!(target_os = "macos") && name == "refused-task" {
            monitored.push(native_step("failure-hidden", "hide_window"));
        }
        monitored.push(entry);
        if cfg!(target_os = "macos") {
            match name.as_str() {
                "opened" => monitored.push(native_step("native-visible", "show_window")),
                "installing" => {
                    monitored.push(native_step("download-hidden-1", "hide_window"));
                    monitored.push(held_observation(1));
                }
                "installed" => {
                    monitored.push(native_step("download-restored-1", "show_window"));
                    for repeat in 2..=3 {
                        monitored.push(api(
                            &format!("download-remove-{repeat}"),
                            "module.resource.remove",
                            json!({"module_id":MODULE,"resource_id":"proof-palette"}),
                        ));
                        monitored.push(gesture(
                            &format!("download-removed-{repeat}"),
                            CapabilityAction::Settle,
                        ));
                        monitored.push(api(
                            &format!("download-start-{repeat}"),
                            "module.resource.install",
                            json!({"module_id":MODULE,"resource_id":"proof-palette"}),
                        ));
                        monitored.push(native_step(
                            &format!("download-hidden-{repeat}"),
                            "hide_window",
                        ));
                        monitored.push(held_observation(repeat));
                        monitored.push(gesture(
                            &format!("download-finished-{repeat}"),
                            CapabilityAction::Settle,
                        ));
                        monitored.push(native_step(
                            &format!("download-restored-{repeat}"),
                            "show_window",
                        ));
                    }
                }
                "running" => {
                    monitored.push(native_step("success-hidden", "hide_window"));
                    monitored.push(layout("held-first", script::Step::wait(1000)));
                    monitored.push(layout("held-quiet", script::Step::wait(1000)));
                }
                "succeeded" => monitored.push(native_step("success-restored", "show_window")),
                "cancel-listed" => monitored.push(native_step("cancel-hidden", "hide_window")),
                "cancelled" => monitored.push(native_step("cancel-restored", "show_window")),
                "refused-task" => monitored.push(native_step("failure-restored", "show_window")),
                _ => {}
            }
        }
    }
    Plan::new(monitored)
}

fn held_observation(repeat: usize) -> Step {
    Step::new(
        format!("cpu_download_{repeat}"),
        script::Step::Observe(script::IdleStep {
            settle_ms: 1000,
            ms: 20_000,
        }),
    )
    .commits(0)
}

fn native_step(name: &str, action: &str) -> Step {
    Step::new(
        name,
        script::Step::WindowVisibility {
            action: action.into(),
        },
    )
    .commits(0)
}

/// A fresh sentinel: nothing in the repository or the run can contain it by accident.
fn sentinel(prefix: &str) -> String {
    let seed = format!(
        "{prefix}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
    );
    let digest = format!("{:x}", Sha256::digest(seed.as_bytes()));
    format!("{prefix}-{}", &digest[..32])
}

/// Every file under `dir` that holds any of `needles`, by path. The needles themselves are never
/// written anywhere.
fn holding(dir: &Path, needles: &[&str]) -> Result<(usize, Vec<String>)> {
    let mut scanned = 0;
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            scanned += 1;
            let bytes = fs::read(&path)?;
            if needles.iter().any(|needle| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            }) {
                found.push(path.display().to_string());
            }
        }
    }
    Ok((scanned, found))
}

pub const NOTE: &str = "The runner starts a loopback proof endpoint in its own process, writes the script (whose secret steps carry a sentinel key) outside the output directory and keeps a redacted copy as `script.json`, so the script path in the argument array below was a temporary file. A replay reruns every check over the recorded frames and catalog but carries the endpoint's own record and the secret scan from the recorded checks: they are what the endpoint in the runner's process saw and what the run's own sentinel found, and neither is in the evidence.";

/// The scenario's own run: a proof endpoint in this process for the launch to talk to, the
/// launch's plan made with that endpoint and a fresh sentinel key, the row's checks over the
/// launch, and what the endpoint saw and a scan of every file the run wrote for the sentinel.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let key = sentinel("sentinel");
    let wrong = sentinel("wrong");
    let endpoint = ProofEndpoint::start(&key, proof_protocol())?;
    endpoint.set_delay(DELAY);
    endpoint.set_palette_delay(if cfg!(target_os = "macos") {
        Duration::from_secs(27)
    } else {
        DELAY
    });
    // A replay checks the recorded run's steps, whose endpoint steps name the endpoint the
    // recorded run's own process started.
    let base = if run.replaying() {
        read_json(&run.out().join("result.json"))?["endpoint"]
            .as_str()
            .ok_or("The recorded run names no endpoint")?
            .to_owned()
    } else {
        endpoint.base_url()
    };
    let plan = plan(&base, &key, &wrong);
    run.record("endpoint", json!(endpoint.base_url()));
    if let Some(note) = scenario.note {
        run.note(note);
    }
    let mut launch = Launch::app()
        .developer()
        .proof_endpoint(&base)
        .open_all(&sources)
        .secret_script(plan.script(), plan.kept())
        .deadline(Duration::from_secs(160));
    if cfg!(target_os = "macos") {
        launch = launch
            .watch(Box::new(crate::visibility_smoke::watch))
            .keep(crate::visibility_smoke::READINGS);
    }
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    let outcome = (|| -> Result {
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        let checked = plan.check(&evidence)?;
        // Everything the run wrote so far, before the checks add their own records.
        let (scanned, found) = holding(run.out(), &[&key, &wrong])?;
        (scenario.verify)(&mut run, std::slice::from_ref(&checked))?;
        // A replay has no endpoint and no sentinel of the recorded run's: those two checks are
        // what the recorded run found.
        let file = evidence.join("capabilities-checks.json");
        let recorded = run
            .recorded(&file)
            .map(|path| {
                read_json(&path).map_err(|error| {
                    format!("The recorded run's checks, which a replay carries the endpoint's record and the secret scan from, cannot be read: {error}")
                })
            })
            .transpose()?;
        let mut checks = read_json(&file)?;
        checks["endpoint"] = match &recorded {
            Some(recorded) => recorded["endpoint"].clone(),
            None => endpoint_checks(&endpoint)?,
        };
        checks["secret_scan"] = match &recorded {
            Some(recorded) => recorded["secret_scan"].clone(),
            None => json!({
                "files_scanned": scanned,
                "files_holding_a_sentinel": found,
                "sentinels": 2,
                "scope": "Every file under the output directory, as bytes",
            }),
        };
        write_json(&file, &checks)?;
        ensure(found.is_empty(), format!("A secret reached {found:?}"))?;
        run.sources_unchanged()?;
        run.record("backend", checked.at("opened")?.state()["backend"].clone());
        Ok(())
    })();
    // The runner's own files are scanned too: nothing it wrote may hold the key either.
    run.finish(outcome, |out| {
        let (_, found) = holding(out, &[&key, &wrong])?;
        ensure(found.is_empty(), format!("A secret reached {found:?}"))
    })
}

fn capability(frame: &Value) -> &Value {
    &frame["state"]["capabilities"][MODULE]
}

fn consent<'a>(frame: &'a Value, kind: &str) -> Result<&'a Value> {
    let consent = &capability(frame)["consent"];
    ensure(
        consent["kind"] == kind,
        format!("Expected a {kind} consent notice, found {consent}"),
    )?;
    Ok(consent)
}

fn tint_layers(frame: &Value) -> Vec<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter(|layer| layer["effect"] == TINT_EFFECT)
                .collect()
        })
        .unwrap_or_default()
}

/// Every frame's capability summary against what its step must have left behind, and the tinted
/// frame against the core's own render. Writes `capabilities-checks.json`, to which the run adds
/// what the endpoint saw and the secret scan.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let base = run
        .recorded_value("endpoint")
        .as_str()
        .ok_or("The run recorded no endpoint to render the tint against")?
        .to_owned();
    ensure(
        launch
            .events
            .iter()
            .any(|event| event["event"] == "capability_answer"),
        "No capability round trip was logged",
    )?;
    let mut checks = Checks::new();
    for frame in &launch.frames {
        // A secret is only ever whether it is present, in every frame.
        for profile in capability(frame)["settings"]["profiles"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let secret = &profile["fields"]["api-key"];
            ensure(
                secret["secret_present"].is_boolean() && secret.get("value").is_none(),
                format!("The api-key field reads {secret}"),
            )?;
        }
        checks.note(
            frame,
            "the capability block's status line and the canvas's notices",
            json!({"status_line": capability(frame)["status_line"], "notices": frame.notices()}),
        );
    }
    let at = |step: &str| launch.at(step);
    let settings = |step: &str| -> Result<Value> { Ok(capability(at(step)?)["settings"].clone()) };
    let profile = |step: &str| -> Result<Value> { Ok(settings(step)?["profiles"][0].clone()) };
    // The section expanded and scrolled to its end, and its block read.
    ensure(
        capability(at("read")?)["loaded"] == true && settings("read")?["revision"] == 0,
        "The block was not read before its settings were written",
    )?;
    // Each settings write, read back into the block.
    let strength = settings("strength")?;
    ensure(
        strength["fields"]["strength"]["value"] == 0.8 && strength["revision"] == 1,
        format!("Strength was not set: {strength}"),
    )?;
    let created = profile("profile")?;
    ensure(
        created["adapter"] == "proof-echo"
            && created["label"] == PROFILE
            && created["status"] == "incomplete",
        format!("The profile was not created: {created}"),
    )?;
    let pointed = profile("endpoint")?;
    ensure(
        pointed["fields"]["endpoint"]["value"]
            .as_str()
            .is_some_and(|endpoint| endpoint.ends_with(PROOF_GENERATE_PATH))
            && pointed["status"] == "missing-credentials",
        format!("The endpoint was not set: {pointed}"),
    )?;
    let keyed = profile("key")?;
    ensure(
        pointed["fields"]["api-key"]["secret_present"] == false
            && keyed["fields"]["api-key"]["secret_present"] == true,
        "The key did not go from not set to set",
    )?;
    ensure(
        keyed["status"] == "ready",
        "The profile is not ready with its key",
    )?;
    // The download granted, installing with progress, then installed.
    ensure(
        capability(at("palette-allowed")?)["permissions"]["live"] == 1,
        "The download grant is not counted",
    )?;
    let installing = &capability(at("installing")?)["resources"][0];
    ensure(
        installing["state"] == "installing"
            && installing["progress"]
                .as_f64()
                .is_some_and(|fraction| (0.0..=1.0).contains(&fraction)),
        format!("The install is not running with progress: {installing}"),
    )?;
    ensure(
        capability(at("installing")?)["jobs"]
            .as_array()
            .is_some_and(|jobs| {
                jobs.iter()
                    .any(|job| job["kind"] == "install" && job["status"] == "running")
            }),
        "No running install job was tracked",
    )?;
    ensure(
        capability(at("installed")?)["resources"][0]["state"] == "installed",
        "The palette was not installed",
    )?;
    // The photo-data consent, whose notice the plan holds, declined, then asked again.
    let asked = consent(at("task-asked")?, "remote-image-request")?;
    ensure(
        asked["scope"]["data"] == "sample-grid-8"
            && asked["scope"]["adapter"] == "proof-echo"
            && asked["denied"] == false,
        format!("Wrong photo-data consent {asked}"),
    )?;
    let denied = capability(at("denied")?);
    ensure(
        denied["consent"].is_null()
            && denied["permissions"]["denials"] == 1
            && denied["tasks"][TASK]["status"] == "failed"
            && denied["tasks"][TASK]["error"]["code"] == "consent-required",
        format!("The denial was not recorded, or the task went on: {denied}"),
    )?;
    ensure(
        consent(at("task-asked-again")?, "remote-image-request")?["denied"] == true,
        "A second ask does not say it was declined before",
    )?;
    // The running task and its result.
    let running = &capability(at("running")?)["tasks"][TASK];
    ensure(
        running["status"] == "running"
            && running["progress"]["fraction"]
                .as_f64()
                .is_some_and(|fraction| (0.0..=1.0).contains(&fraction)),
        format!("The task is not running with progress: {running}"),
    )?;
    let succeeded = at("succeeded")?;
    let done = &capability(succeeded)["tasks"][TASK];
    let artifact = done["artifact"]
        .as_str()
        .ok_or_else(|| format!("The task did not succeed: {done}"))?
        .to_owned();
    ensure(
        done["status"] == "ready" && done["apply_available"] == true,
        format!("The task has no result to apply: {done}"),
    )?;
    ensure(
        tint_layers(succeeded).is_empty(),
        "A tint was committed before Apply",
    )?;
    if cfg!(target_os = "macos") {
        let first = at("held-first")?;
        let quiet = at("held-quiet")?;
        ensure(
            first["state"]["job_monitoring"]["held"]
                .as_u64()
                .is_some_and(|held| held > 0),
            "No capability reader waited on the held job",
        )?;
        ensure(
            first["state"]["job_monitoring"] == quiet["state"]["job_monitoring"],
            "An unchanged held capability job created a periodic monitoring read",
        )?;
        ensure(
            capability(first)["tasks"][TASK]["status"] == "running"
                && capability(quiet)["tasks"][TASK]["status"] == "running",
            "The held monitoring window did not contain a running job",
        )?;
        for frame in [
            first,
            quiet,
            succeeded,
            at("cancelled")?,
            at("refused-task")?,
        ] {
            ensure(
                frame["state"]["visibility"]["window_hidden"] == true
                    && frame["state"]["visibility"]["sampling_allowed"] == false
                    && frame["state"]["visibility"]["evidence_invisible_window_override"] == false,
                "A hidden capability outcome was not adopted under the native gate",
            )?;
            ensure(
                frame["state"]["long_work"]["timers"] == json!({"throttle":false,"refresh":false}),
                "A hidden capability outcome retained display timers",
            )?;
        }
        ensure(
            first["state"]["performance"]["reads_requested"]
                == quiet["state"]["performance"]["reads_requested"]
                && first["state"]["performance"]["reads_requested"]
                    == succeeded["state"]["performance"]["reads_requested"],
            "Resource sampling continued during hidden capability work",
        )?;
        checks.note(quiet, "unchanged held job sleeps; native-hidden success/failure/cancellation adopted", json!({"first":first["state"]["job_monitoring"],"quiet":quiet["state"]["job_monitoring"],"succeeded":succeeded["state"]["job_monitoring"]}));
    }
    if cfg!(target_os = "macos") {
        let external = read_json(
            &launch
                .evidence
                .parent()
                .ok_or("Evidence has no parent")?
                .join(crate::visibility_smoke::READINGS),
        )?;
        let samples = external["samples"]
            .as_array()
            .ok_or("No held-job process readings")?;
        let observations: Vec<&Value> = launch
            .events
            .iter()
            .filter(|event| event["event"] == "presentation_observation_ended")
            .collect();
        ensure(
            observations.len() == 3,
            "Expected three held-download CPU windows",
        )?;
        for (index, observation) in observations.iter().enumerate() {
            let detail = &observation["detail"];
            let before = &detail["before"];
            let after = &detail["after"];
            ensure(
                detail["window_ms"] == 20_000
                    && before["job_monitoring"]["held"]
                        .as_u64()
                        .is_some_and(|held| held > 0)
                    && before["job_monitoring"] == after["job_monitoring"],
                "An unchanged held download produced reader calls or replies",
            )?;
            ensure(
                before["performance"]["reads_requested"] == after["performance"]["reads_requested"]
                    && before["long_work"]["wakes"] == after["long_work"]["wakes"],
                "Hidden held work retained presentation sampling or board wakes",
            )?;
            for state in [before, after] {
                ensure(
                    state["visibility"]["sampling_allowed"] == false
                        && state["visibility"]["window_hidden"] == true
                        && state["long_work"]["timers"]
                            == json!({"throttle":false,"refresh":false}),
                    "Held download measurement was not native hidden",
                )?;
            }
            let report = crate::visibility_smoke::external_window(
                before["wall_ms"].as_u64().ok_or("Missing CPU start")?,
                detail["wall_ms"].as_u64().ok_or("Missing CPU end")?,
                samples,
            )?;
            checks.note(at(&format!("cpu_download_{}",index+1))?, "unchanged hidden download has no periodic monitoring", json!({"external":report,"internal":detail,"scope":"20 second held transfer on the orientation fixture; evidence timers suspended, external ps every500ms"}));
        }
    }
    // Apply: exactly one tint layer, referencing the task's artifact.
    let applied = at("applied")?;
    let layers = tint_layers(applied);
    ensure(
        layers.len() == 1,
        "Apply did not commit exactly one tint layer",
    )?;
    ensure(
        capability(applied)["tasks"][TASK]["applied"] == true
            && capability(succeeded)["tasks"][TASK]["applied"] == false,
        "The task control does not say its result is applied",
    )?;
    ensure(
        layers[0]["artifacts"] == json!([artifact])
            && layers[0]["payload"] == json!({"artifact": artifact}),
        format!(
            "The tint layer does not name the task's artifact: {}",
            layers[0]
        ),
    )?;
    // A wrong key makes the endpoint refuse, and the failure is shown.
    let listed = &at("cancel-listed")?["state"]["performance"]["jobs"];
    ensure(
        listed.as_array().is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job["running"] == true && job["job_id"].is_string())
        }),
        "No cancellable task was displayed in Performance",
    )?;
    let cancelled = at("cancelled")?;
    let cancelled_id = &at("cancel-requested")?["step"]["job_id"];
    ensure(
        capability(cancelled)["tasks"][TASK]["error"]["code"] == "cancelled"
            && capability(cancelled)["jobs"]
                .as_array()
                .is_some_and(|jobs| {
                    jobs.iter()
                        .any(|job| job["job_id"] == *cancelled_id && job["status"] == "cancelled")
                }),
        "The Performance button did not cancel the capability task",
    )?;
    ensure(
        tint_layers(cancelled) == tint_layers(applied),
        "Cancelling changed the accepted edit",
    )?;
    ensure(
        profile("wrong-key")?["fields"]["api-key"]["secret_present"] == true,
        "The replaced key does not read set",
    )?;
    let refused = at("refused-task")?;
    let failed = &capability(refused)["tasks"][TASK];
    ensure(
        failed["status"] == "failed"
            && failed["error"]["code"] == "read-error"
            && failed["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("proof-echo answered 401")),
        format!("The failure is not the endpoint's refusal: {failed}"),
    )?;
    ensure(
        tint_layers(refused).len() == 1,
        "A failed task changed the recipe",
    )?;
    let render = render_checks(&mut checks, launch, &base)?;
    checks.write(
        &launch.evidence,
        "capabilities",
        json!({
            "artifact": artifact,
            "gains": done["result"]["gains"],
            "failure": failed["error"],
            "install_progress": installing["progress"],
            "task_progress": running["progress"],
            "render": render,
        }),
    )
}

/// What the endpoint itself saw: one held palette download, one authorized generation answered
/// with a tint and one refused for its key.
fn endpoint_checks(endpoint: &ProofEndpoint) -> Result<Value> {
    let requests = endpoint.requests();
    let palette: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/proof-palette.bin")
        .collect();
    let generate: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/generate")
        .collect();
    ensure(
        palette.len() == if cfg!(target_os = "macos") { 3 } else { 1 }
            && palette.iter().all(|request| request.status == 200),
        "The palette downloads did not match the held-install qualification",
    )?;
    ensure(
        generate.len() == 3
            && generate[0].authorized
            && generate[0].status == 200
            && generate[0].samples.is_some()
            && generate[1].authorized
            && !generate[2].authorized
            && generate[2].status == 401,
        "The endpoint did not see the accepted, cancelled and refused generations",
    )?;
    Ok(json!({
        "palette_downloads": palette.len(),
        "generations": generate.iter().map(|request| json!({
            "authorized": request.authorized,
            "status": request.status,
            "body_bytes": request.body_bytes,
            "rgb": request.rgb,
        })).collect::<Vec<_>>(),
    }))
}

/// Mean 8-bit RGB of an image region given as fractions of a rectangle.
fn window_mean(
    rgb: impl Fn(u32, u32) -> [u8; 3],
    bounds: [f64; 4],
    window: [f64; 2],
) -> Result<[f64; 3]> {
    let [left, top, right, bottom] = bounds;
    let (width, height) = (right - left, bottom - top);
    let x0 = (left + window[0] * width).round() as u32;
    let x1 = (left + window[1] * width).round() as u32;
    let y0 = (top + window[0] * height).round() as u32;
    let y1 = (top + window[1] * height).round() as u32;
    ensure(x1 > x0 && y1 > y0, "An empty window")?;
    let mut total = [0.0; 3];
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = rgb(x, y);
            for channel in 0..3 {
                total[channel] += f64::from(pixel[channel]);
            }
        }
    }
    let count = f64::from((x1 - x0) * (y1 - y0));
    Ok(total.map(|sum| sum / count))
}

/// The tinted frame against the frame before Apply and against the core's own render of the
/// committed stack: the centred window's mean moves the way the published gains say, and matches
/// the independent render within [`MEAN_TOLERANCE`].
fn render_checks(checks: &mut Checks, launch: &Checked, base: &str) -> Result<Value> {
    const WINDOW_FRACTIONS: [f64; 2] = [0.25, 0.75];
    let before = launch.at("succeeded")?;
    let after = launch.at("applied")?;
    // The untinted frame still shows the fixture's exact colours where the editor drew the
    // photograph; a tint changes no geometry, so the tinted frame's photograph is in the same
    // place.
    let placed = pixels::identity_photo(before)?;
    let bounds: [f64; 4] = serde_json::from_value(placed["bounds"].clone())?;
    ensure(
        after.columns()? == before.columns()? && after.photo()? == before.photo()?,
        "The photograph moved between the frames",
    )?;
    let capture_mean = |frame: &Frame| -> Result<[f64; 3]> {
        let image = frame.image()?;
        window_mean(|x, y| image.get_pixel(x, y).0, bounds, WINDOW_FRACTIONS)
    };
    let untinted = capture_mean(before)?;
    let tinted = capture_mean(after)?;
    let gains: Vec<f64> =
        serde_json::from_value(capability(before)["tasks"][TASK]["result"]["gains"].clone())?;
    ensure(gains.len() == 3, "The task published no gains")?;
    for channel in 0..3 {
        let expected = gains[channel] - 1.0;
        let moved = tinted[channel] - untinted[channel];
        if expected.abs() > 0.01 {
            ensure(
                moved.signum() == expected.signum() && moved.abs() >= 0.5,
                format!(
                    "Channel {channel} moved {moved:.2} for a gain of {:.4}",
                    gains[channel]
                ),
            )?;
        }
    }
    // The core's own render of the stack the tinted frame shows, from the catalog the run left.
    let asset: AssetId =
        serde_json::from_value(capability(before)["tasks"][TASK]["asset_id"].clone())?;
    let entry = EntryId::parse(
        after["state"]["stack"]["entry"]
            .as_str()
            .ok_or("The tinted frame names no entry")?,
    )?;
    // The registry the developer launch served, through the same assembly.
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        developer: true,
        proof_endpoint: Some(base),
        ..RegistryOptions::default()
    })?;
    let mut service =
        EditorService::open_with(&launch.evidence.join("catalog.sqlite"), Arc::new(registry))?;
    service.prepare(&service.entry_needs(&asset, Some(&entry))?)?;
    let raster = service.render_entry(&asset, &entry)?;
    drop(service);
    let rendered = window_mean(
        |x, y| {
            let at = ((y * raster.width + x) * 4) as usize;
            [raster.rgba[at], raster.rgba[at + 1], raster.rgba[at + 2]]
        },
        [0.0, 0.0, f64::from(raster.width), f64::from(raster.height)],
        WINDOW_FRACTIONS,
    )?;
    for channel in 0..3 {
        checks.compare(
            after,
            &format!("channel {channel} of the tinted frame against the core's render"),
            tinted[channel],
            rendered[channel],
            Tolerance::Within(MEAN_TOLERANCE),
        )?;
    }
    Ok(json!({
        "photo_bounds": bounds,
        "window": {"fractions": WINDOW_FRACTIONS, "of": "the photograph's own bounds in the capture and the raster"},
        "untinted_mean": untinted,
        "tinted_mean": tinted,
        "core_render_mean": rendered,
        "gains": gains,
        "tolerance_codes": MEAN_TOLERANCE,
        "raster": [raster.width, raster.height],
        "scope": "Mean 8-bit sRGB of the centred half of the displayed photograph, read back from the renderer, against EditorService::render_entry of the committed stack; not a colorimetric claim",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kept_script_is_redacted_and_the_scan_finds_a_planted_sentinel() {
        let plan = plan("http://127.0.0.1:1", "planted-key", "planted-wrong");
        let sent = plan.script().to_string();
        assert!(sent.contains("planted-key") && sent.contains("planted-wrong"));
        let kept = plan.kept().to_string();
        assert!(!kept.contains("planted-key") && !kept.contains("planted-wrong"));
        assert_eq!(
            kept.matches(luxforge_core::capabilities::redact::REDACTED)
                .count(),
            2
        );
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("clean.json"), kept).unwrap();
        fs::write(dir.path().join("nested/leak.log"), b"...planted-key...").unwrap();
        let (scanned, found) = holding(dir.path(), &["planted-key", "planted-wrong"]).unwrap();
        assert_eq!(scanned, 2);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("leak.log"));
        assert!(sentinel("sentinel").starts_with("sentinel-"));
        assert_ne!(sentinel("a"), sentinel("b"));
    }

    #[test]
    fn a_window_mean_reads_the_fractional_window_of_its_bounds() {
        // A 4 × 4 image whose centre 2 × 2 is white and everything else black.
        let pixel = |x: u32, y: u32| {
            if (1..3).contains(&x) && (1..3).contains(&y) {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            }
        };
        assert_eq!(
            window_mean(pixel, [0.0, 0.0, 4.0, 4.0], [0.25, 0.75]).unwrap(),
            [255.0; 3]
        );
        assert_eq!(
            window_mean(pixel, [0.0, 0.0, 4.0, 4.0], [0.0, 1.0]).unwrap(),
            [63.75; 3]
        );
    }
}
