//! Rendered evidence for module capabilities: the developer proof module driven through the
//! desktop's own capability section, task control and consent notice, against a loopback
//! [`ProofEndpoint`] this process starts. The editor runs hidden in the background harness with
//! `--developer --proof-endpoint`, so its settings, grants, resources and in-memory secret store
//! live inside the evidence directory.
//!
//! The script's secret steps carry a sentinel key, so the script itself is written outside the
//! output directory and removed after the run; the copy kept beside the evidence is redacted, and
//! the editor records those steps redacted too. Every file under the output directory is scanned
//! for the sentinel afterwards.
use crate::{
    scenario::{Checked, Frame, Launch, Plan, Run, Step, pixels, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{
    AssetId, CapabilitiesProofModule, EditorService, EntryId, ModuleRegistry, ProofEndpoint,
};
use std::{sync::Arc, time::Duration};

const MODULE: &str = "luxforge.capabilities";
const TASK: &str = "generate-proof-tint";
const TINT_EFFECT: &str = "luxforge.capabilities.tint";
/// How long the endpoint holds the palette download and each generation, so a frame can be
/// captured while the install or the task is still running. Well inside every transfer and adapter
/// deadline.
const DELAY: Duration = Duration::from_millis(1200);
/// A captured tinted frame and the core's own render of the same stack may differ by this much per
/// channel, in 8-bit codes, over the window's mean: linear filtering of the magnified photograph.
const MEAN_TOLERANCE: f64 = 2.0;
/// What a secret step's value is kept and recorded as.
const REDACTED: &str = "<redacted>";

/// One capability step of the proof module.
fn step(gesture: Value) -> Value {
    let mut object = gesture.as_object().expect("a gesture object").clone();
    object.insert("module".into(), json!(MODULE));
    json!({ "capability": object })
}

/// Every frame, in order, with the sentinel key in the secret steps' script and redacted in what is
/// kept and recorded. Only Apply commits anything: every settings write, consent, install, task
/// and grant is the capability's own state, not the photograph's history.
pub fn plan(generate: &str, key: &str, wrong: &str) -> Plan {
    let layout = |name: &str, script: Value| Step::new(name, script).commits(0);
    let capability = |name: &str, gesture: Value| Step::new(name, step(gesture)).commits(0);
    let secret = |name: &str, value: &str| {
        let gesture =
            |value: &str| json!({"secret": {"field": "api-key", "value": value, "profile": 0}});
        Step::secret(name, step(gesture(value)), step(gesture(REDACTED))).commits(0)
    };
    Plan::new(vec![
        Step::opened("opened"),
        // Layout: the sections that start expanded are collapsed, the proof section is expanded
        // and the panel is scrolled to its end, so the whole capability block is on screen.
        layout(
            "basic-collapsed",
            json!({"section":{"module":"luxforge.basic","expanded":false}}),
        )
        .collapsed("luxforge.basic"),
        layout(
            "transform-collapsed",
            json!({"section":{"module":"luxforge.transform","expanded":false}}),
        )
        .collapsed("luxforge.transform"),
        layout(
            "crop-collapsed",
            json!({"section":{"module":"luxforge.crop","expanded":false}}),
        )
        .collapsed("luxforge.crop"),
        layout(
            "expanded",
            json!({"section":{"module":MODULE,"expanded":true}}),
        )
        .expanded(MODULE),
        layout("scrolled", json!({"tools_scroll":1.0})).expanded(MODULE),
        // Its settings: strength, a profile, the profile's endpoint and its key.
        capability("settings", json!({"section": "settings"})),
        capability(
            "strength",
            json!({"set": {"field": "strength", "value": 0.8}}),
        ),
        capability(
            "profile",
            json!({"profile": {"create": {"adapter": "proof-echo", "label": "Local proof"}}}),
        ),
        capability(
            "endpoint",
            json!({"set": {"field": "endpoint", "value": generate, "profile": 0}}),
        ),
        secret("key", key),
        capability("status", json!({"section": "status"})),
        // The download's consent, declined, then asked again and allowed: installing, then
        // installed, then active.
        capability(
            "install-asked",
            json!({"install": {"resource": "proof-palette"}}),
        ),
        capability("denied", json!({"consent": "deny"})),
        capability(
            "install-asked-again",
            json!({"install": {"resource": "proof-palette"}}),
        ),
        capability("installing", json!({"consent": "allow", "wait": false})),
        capability("installed", json!({"settle": true})),
        capability("activated", json!({"activate": true})),
        // The photo-data consent, the running task and its result, and Apply.
        capability("task-asked", json!({"task": {"task": TASK}})),
        capability("running", json!({"consent": "allow", "wait": false})),
        capability("succeeded", json!({"settle": true})),
        Step::new("applied", step(json!({"apply": true})))
            .commits(1)
            .label("Apply proof tint"),
        // A wrong key makes the endpoint refuse, and the failure is shown; then the photo-data
        // grant is revoked.
        secret("wrong-key", wrong),
        capability("refused-task", json!({"task": {"task": TASK}})),
        capability("revoked", json!({"revoke": 1})),
    ])
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
    let endpoint = ProofEndpoint::start(&key)?;
    endpoint.set_delay(DELAY);
    endpoint.set_palette_delay(DELAY);
    let base = endpoint.base_url();
    // A replay checks the recorded run's steps, whose endpoint step names the endpoint the
    // recorded run's own process started.
    let generate = if run.replaying() {
        let recorded = read_json(&run.out().join("result.json"))?;
        let path = endpoint
            .generate_url()
            .strip_prefix(&base)
            .ok_or("The endpoint's generate URL is not under its base")?
            .to_owned();
        format!(
            "{}{path}",
            recorded["endpoint"]
                .as_str()
                .ok_or("The recorded run names no endpoint")?
        )
    } else {
        endpoint.generate_url()
    };
    let plan = plan(&generate, &key, &wrong);
    run.record("endpoint", json!(base));
    if let Some(note) = scenario.note {
        run.note(note);
    }
    let mut launch = Launch::app()
        .developer()
        .proof_endpoint(&base)
        .open_all(&sources)
        .secret_script(plan.script(), plan.kept());
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    let outcome = (|| -> Result {
        run.hash(&sources)?;
        let evidence = run.launch(launch)?;
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
    let mut per_frame = Vec::new();
    for frame in &launch.frames {
        // A secret is only ever set or not set, in every frame.
        for profile in capability(frame)["settings"]["profiles"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let secret = &profile["fields"]["api-key"];
            ensure(
                secret == "set" || secret == "not set",
                format!("The api-key field reads {secret}"),
            )?;
        }
        per_frame.push(json!({
            "frame": frame["file"],
            "view": capability(frame)["view"],
            "notices": frame.notices(),
        }));
    }
    let at = |step: &str| launch.at(step);
    let settings = |step: &str| -> Result<Value> { Ok(capability(at(step)?)["settings"].clone()) };
    let profile = |step: &str| -> Result<Value> { Ok(settings(step)?["profiles"][0].clone()) };
    // The section expanded and scrolled to its end, then its settings.
    ensure(
        capability(at("settings")?)["view"] == "settings"
            && capability(at("settings")?)["loaded"] == true,
        "The settings sub-view did not open on the read settings",
    )?;
    // Each settings write.
    let strength = settings("strength")?;
    ensure(
        strength["fields"]["strength"] == "0.80" && strength["revision"] == 1,
        format!("Strength was not set: {strength}"),
    )?;
    let created = profile("profile")?;
    ensure(
        created["adapter"] == "proof-echo"
            && created["label"] == "Local proof"
            && created["status"] == "incomplete",
        format!("The profile was not created: {created}"),
    )?;
    let pointed = profile("endpoint")?;
    ensure(
        pointed["fields"]["endpoint"]
            .as_str()
            .is_some_and(|endpoint| endpoint.ends_with("/generate"))
            && pointed["status"] == "missing-credentials",
        format!("The endpoint was not set: {pointed}"),
    )?;
    let keyed = profile("key")?;
    ensure(
        pointed["fields"]["api-key"] == "not set" && keyed["fields"]["api-key"] == "set",
        "The key did not go from not set to set",
    )?;
    ensure(
        keyed["status"] == "ready",
        "The profile is not ready with its key",
    )?;
    ensure(
        capability(at("status")?)["view"] == "status",
        "The status sub-view did not open",
    )?;
    // The download's consent, declined, then asked again.
    let asked_frame = at("install-asked")?;
    let asked = consent(asked_frame, "download-artifact")?;
    ensure(
        asked["denied"] == false && asked["scope"]["resource"] == "proof-palette",
        format!("Wrong download consent {asked}"),
    )?;
    ensure(
        asked_frame
            .notices()
            .contains(&"Allow Capabilities proof to download a resource?".into()),
        "The download consent notice is not shown",
    )?;
    let denied = capability(at("denied")?);
    ensure(
        denied["consent"].is_null()
            && denied["permissions"]["denials"] == 1
            && denied["resources"][0]["state"] == "not-installed",
        "The denial was not recorded, or something was installed",
    )?;
    ensure(
        consent(at("install-asked-again")?, "download-artifact")?["denied"] == true,
        "A second ask does not say it was declined before",
    )?;
    // Installing, with progress, then installed, then active.
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
    ensure(
        capability(at("activated")?)["activation"]["state"] == "active",
        format!(
            "The module is not active: {}",
            capability(at("activated")?)["activation"]
        ),
    )?;
    // The photo-data consent, the running task and its result.
    let task_frame = at("task-asked")?;
    let asked = consent(task_frame, "remote-image-request")?;
    ensure(
        asked["scope"]["data"] == "sample-grid-8"
            && asked["scope"]["adapter"] == "proof-echo"
            && asked["denied"] == false,
        format!("Wrong photo-data consent {asked}"),
    )?;
    ensure(
        task_frame
            .notices()
            .contains(&"Allow Capabilities proof to send photo data?".into()),
        "The photo-data consent notice is not shown",
    )?;
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
    ensure(
        profile("wrong-key")?["fields"]["api-key"] == "set",
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
    // The photo-data grant revoked, in the open permissions list.
    let permissions = &capability(at("revoked")?)["permissions"];
    let revoked: Vec<&Value> = permissions["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|grant| !grant["revoked"].is_null())
        .collect();
    ensure(
        permissions["open"] == true
            && revoked.len() == 1
            && revoked[0]["kind"] == "remote-image-request",
        format!("The remote grant is not listed revoked: {permissions}"),
    )?;
    let render = render_checks(launch, &base)?;
    write_json(
        &launch.evidence.join("capabilities-checks.json"),
        &json!({
            "frames": per_frame,
            "artifact": artifact,
            "gains": done["result"]["gains"],
            "failure": failed["error"],
            "install_progress": installing["progress"],
            "task_progress": running["progress"],
            "render": render,
        }),
    )?;
    Ok(())
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
        palette.len() == 1 && palette[0].status == 200,
        "The palette was not downloaded exactly once",
    )?;
    ensure(
        generate.len() == 2
            && generate[0].authorized
            && generate[0].status == 200
            && generate[0].samples.is_some()
            && !generate[1].authorized
            && generate[1].status == 401,
        "The endpoint did not see one authorized and one refused generation",
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
fn render_checks(launch: &Checked, base: &str) -> Result<Value> {
    const WINDOW_FRACTIONS: [f64; 2] = [0.25, 0.75];
    let before = launch.at("succeeded")?;
    let after = launch.at("applied")?;
    // The untinted frame still shows the fixture's exact colours, which locate the photograph; a
    // tint changes no geometry, so the tinted frame's photograph is in the same place.
    let placed = pixels::identity_photo(before)?;
    let bounds: [f64; 4] = serde_json::from_value(placed["bounds"].clone())?;
    ensure(
        after.columns()? == before.columns()?,
        "The photo surface moved between the frames",
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
    let mut registry = ModuleRegistry::builtin();
    registry.register(Arc::new(CapabilitiesProofModule::new(base)))?;
    let service =
        EditorService::open_with(&launch.evidence.join("catalog.sqlite"), Arc::new(registry))?;
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
        ensure(
            (tinted[channel] - rendered[channel]).abs() <= MEAN_TOLERANCE,
            format!(
                "Channel {channel} shows {:.2} where the core renders {:.2}",
                tinted[channel], rendered[channel]
            ),
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
        let plan = plan(
            "http://127.0.0.1:1/generate",
            "planted-key",
            "planted-wrong",
        );
        let sent = plan.script().to_string();
        assert!(sent.contains("planted-key") && sent.contains("planted-wrong"));
        let kept = plan.kept().to_string();
        assert!(!kept.contains("planted-key") && !kept.contains("planted-wrong"));
        assert_eq!(kept.matches(REDACTED).count(), 2);
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
