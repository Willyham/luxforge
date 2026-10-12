//! The Settings sheet in a developer launch: opened from the palette at General, which shows Auto
//! collapse history on by default, then at Experiments, each flag kind changed through
//! its own control, a number the field refuses, a Reset, developer mode turned off for the next
//! launch while `--developer` holds this one, the sheet closed with Escape and opened again over
//! what the host stored. Then, with the Performance section collapsed so its live readings stay
//! out of the comparison, the General rows' preferences, each through its row's own control:
//! the canvas background set to grey and back to dark, checked in the canvas beside the
//! photograph with the panels unchanged; the interface size set to 125%, checked in the title
//! bar's drawn height against 100%, with the sheet drawn at that size, and back to 100%; the mask
//! overlay colour set to white; the lens switch turned off; and a catalog folder chosen inside the
//! evidence directory, whose relaunch note the Catalog row shows until Use Default. Nothing is
//! committed, and the photograph is untouched throughout.
//!
//! The Models tab follows the Privacy tab, against the developer models proof and a loopback proof
//! endpoint this process starts, which serves its two files with HTTP ranges and cuts the first
//! download of the encoder part-way: the tab listing the model with its trade-off, use
//! restriction, requirement and this machine's memory; the interrupted download kept and named
//! with what it holds; the next download resuming from that byte and installing both files; the
//! model loaded through `module.runtime.load`, then released with Unload now and removed with
//! Remove, each through the tab's own button. Every Models frame is checked against the
//! `module.status` answer it was drawn from, and the endpoint's record shows the ranged request.
//!
//! The launch has explicit usage statistics test eligibility with a loopback receiver, so it shows
//! the startup notice and the Privacy tab. Once the opens are captured the notice is claimed and
//! drawn with its two buttons, answered Share statistics, and later the Privacy tab is opened,
//! its What is sent list expanded and its switch turned off and on again, each checked against
//! the consent the host stored. The frames record the stored choice and the effective state,
//! never an identifier, and the destination is the loopback receiver, never the production
//! origin. Nothing listens there: no delivery is made by this scenario.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so the scenario imports none after
//! the lens switch: that an import with the switch off commits no lens entry is proved by the
//! core's owner tests (`first_open_skips_the_lens_module_while_the_switch_is_off_and_asks_it_when_on`
//! and `an_owner_import_follows_the_lens_preference_at_start_and_after_a_change`). Here the switch
//! is checked to reach the row and the stored preferences.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    smoke::{self, APP, Scenario},
    *,
};
use luxforge_core::{
    MODELS_PROOF_MODULE, PROOF_DECODER_PATH, PROOF_ENCODER_PATH, PROOF_MODEL_RESOURCE,
    PROOF_MODEL_RESTRICTION, PROOF_MODEL_TRADEOFF,
};
use luxforge_evidence::{self as script, CapabilityAction, CapabilityStep};
use luxforge_testbase::ProofEndpoint;
use luxforge_testkit::proof_protocol;

/// The usage statistics test receiver the launch is eligible with: loopback, on the discard port,
/// where nothing listens.
pub const USAGE_RECEIVER: &str = "http://127.0.0.1:9/v1/usage/batches";
/// The production origin, which no frame may name as its destination.
const PRODUCTION_ORIGIN: &str = "api.luxforge.app";

/// The refusal a number off its step leaves in the status bar.
const NUMBER_REFUSED: &str = "Proof number takes a number from 0 to 100 in steps of 5";

/// The folder chosen for the catalog, inside the launch's evidence directory.
const CHOSEN: &str = "chosen-catalog";
/// The catalog file a chosen folder holds, and an evidence run's own catalog.
const CATALOG_FILE: &str = "catalog.sqlite";
/// The evidence directory the table's plan names, for `smoke --list`, the plan checks and the
/// script dumps: a run plans over its own launch's evidence directory instead.
const LISTED_EVIDENCE: &str = "/out/app";
/// The proof endpoint the table's plan names in its download grant; a run names the one it
/// started.
const LISTED_ENDPOINT: &str = "http://127.0.0.1:1";
/// The key the proof endpoint is started with; the models proof sends none.
const ENDPOINT_KEY: &str = "settings-models";
/// Where the first download of the encoder is cut, in bytes, and where the next one resumes.
const CUT: u64 = 1000;
/// The proof model's two files together.
const MODEL_BYTES: u64 = 9000;

/// The canvas backgrounds the scenario sets, as the capture must show them.
const DARK: [u8; 3] = [0x19, 0x19, 0x1b];
const GREY: [u8; 3] = [0x77, 0x77, 0x77];
/// What the canvas colour may differ by in any channel: nothing, since it is a flat fill.
const CANVAS_TOLERANCE: f64 = 0.0;

/// The table's plan.
pub fn plan(_: &[PathBuf]) -> Plan {
    planned(Path::new(LISTED_EVIDENCE), LISTED_ENDPOINT)
}

/// The row's own run: the table's launch, its catalog folder chosen inside its own evidence
/// directory, and a loopback proof endpoint in this process that serves the models proof's files,
/// cutting the encoder's first download part-way. A replay checks the recorded run's endpoint and
/// what it saw, which are in its `result.json`.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let endpoint = ProofEndpoint::start(ENDPOINT_KEY, proof_protocol())?;
    endpoint.cut_next_model(CUT as usize);
    let recorded = if run.replaying() {
        Some(read_json(&run.out().join("result.json"))?)
    } else {
        None
    };
    let base = match &recorded {
        Some(result) => result["models_endpoint"]["base"]
            .as_str()
            .ok_or("The recorded run names no models endpoint")?
            .to_owned(),
        None => endpoint.base_url(),
    };
    run.record("models_endpoint", json!({"base": base}));
    let launched = base.clone();
    smoke::launch_planned_with(
        run,
        scenario,
        sources,
        |run, _| Ok(vec![planned(&evidence(run)?, &base)]),
        move |launch| launch.proof_endpoint(&launched),
        |run, _| {
            let seen = match &recorded {
                Some(result) => result["models_endpoint"]["requests"].clone(),
                None => json!(
                    endpoint
                        .requests()
                        .iter()
                        .map(
                            |request| json!({"path": request.path, "range": request.header("range"),
                                              "status": request.status})
                        )
                        .collect::<Vec<_>>()
                ),
            };
            run.record(
                "models_endpoint",
                json!({"base": endpoint_base(run)?, "requests": seen}),
            );
            Ok(())
        },
    )
}

/// The proof endpoint this run recorded.
fn endpoint_base(run: &Run) -> Result<String> {
    Ok(run.recorded_value("models_endpoint")["base"]
        .as_str()
        .ok_or("The run recorded no models endpoint")?
        .to_owned())
}

/// The launch's evidence directory as an absolute path, which the catalog step must name: in a
/// replay, the recorded run's, whose script named it.
fn evidence(run: &Run) -> Result<PathBuf> {
    let out = run
        .recorded(run.out())
        .unwrap_or_else(|| run.out().to_path_buf());
    Ok(std::path::absolute(out)?.join(APP.name))
}

/// The catalog file the catalog step chooses, inside `evidence`.
fn chosen(evidence: &Path) -> PathBuf {
    evidence.join(CHOSEN).join(CATALOG_FILE)
}

/// One General row changed through its own control.
fn preference(name: &str, field: &str, value: Value) -> Step {
    Step::new(name, script::Step::preference([(field, value)])).commits(0)
}

/// One capability step on the models proof, whose frame waits for the jobs it starts.
fn models_step(name: &str, action: CapabilityAction) -> Step {
    Step::new(name, CapabilityStep::new(MODELS_PROOF_MODULE, action)).commits(0)
}

/// One request through the API on the models proof's model, as any client sends it.
fn models_api(name: &str, method: &str) -> Step {
    Step::new(
        name,
        script::Step::call(
            method,
            json!({"module_id": MODELS_PROOF_MODULE, "resource_id": PROOF_MODEL_RESOURCE}),
        ),
    )
    .commits(0)
}

fn planned(evidence: &Path, endpoint: &str) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        // The startup notice, claimed once the opens are captured, and answered.
        Step::new("usage-notice", script::UsageStep::Notice).commits(0),
        Step::new(
            "usage-share",
            script::UsageStep::Answer(script::NoticeAnswer::Share),
        )
        .commits(0),
        Step::new(
            "general",
            script::PaletteStep::Run("settings general".into()),
        )
        .commits(0),
        Step::new("sheet", script::PaletteStep::Run("experiments".into())).commits(0),
        Step::new(
            "choice",
            script::Step::flag("proof.choice", Some(json!("third"))),
        )
        .commits(0),
        Step::new(
            "number",
            script::Step::flag("proof.number", Some(json!(75))),
        )
        .commits(0),
        Step::new(
            "number-refused",
            script::Step::flag("proof.number", Some(json!(33))),
        )
        .commits(0)
        .status(NUMBER_REFUSED),
        Step::new("reset", script::Step::flag("proof.choice", None)).commits(0),
        Step::new(
            "developer-off",
            script::Step::flag("developer", Some(json!(false))),
        )
        .commits(0),
        Step::new("closed", script::Step::key(script::KEY_ESCAPE)).commits(0),
        Step::new("reopened", script::Step::settings(true)).commits(0),
        // The Privacy tab: its list expanded, and the switch off and on again.
        Step::new("privacy", script::Step::settings_tab("privacy")).commits(0),
        Step::new("privacy-details", script::UsageStep::Details(true)).commits(0),
        Step::new("privacy-off", script::UsageStep::Share(false)).commits(0),
        Step::new("privacy-on", script::UsageStep::Share(true)).commits(0),
        // The Models tab: the model listed, its download granted, interrupted and resumed, the
        // model loaded, then Unload now and Remove through the tab's own buttons.
        Step::new("models", script::Step::settings_tab("models")).commits(0),
        Step::new(
            "models-allowed",
            script::Step::call(
                "module.permission.grant",
                json!({"module_id": MODELS_PROOF_MODULE, "capability": "model",
                       "scope": {"resource": PROOF_MODEL_RESOURCE, "version": "1", "origin": endpoint}}),
            ),
        )
        .commits(0),
        models_api("models-interrupted", "module.resource.install"),
        models_step("models-partial", CapabilityAction::Settle),
        models_api("models-resuming", "module.resource.install"),
        models_step("models-installed", CapabilityAction::Settle),
        models_api("models-loading", "module.runtime.load"),
        models_step("models-loaded", CapabilityAction::Settle),
        models_step(
            "models-unloaded",
            CapabilityAction::Unload(PROOF_MODEL_RESOURCE.into()),
        ),
        models_step(
            "models-removed",
            CapabilityAction::Remove(PROOF_MODEL_RESOURCE.into()),
        ),
        // The display preferences with the sheet closed, so the canvas and the title bar are
        // drawn undimmed.
        Step::new("closed-again", script::Step::key(script::KEY_ESCAPE)).commits(0),
        // The Performance section collapsed, so no reading it refreshes on its own schedule lands
        // between the frames the canvas background is compared across.
        Step::new("performance-collapsed", script::Step::performance(false)).commits(0),
        preference("grey", "canvas_background", json!("grey")),
        preference("dark", "canvas_background", json!("dark")),
        preference("size-125", "interface_size", json!(125)),
        // The sheet at 125%, which must fit the window.
        Step::new(
            "general-125",
            script::PaletteStep::Run("settings general".into()),
        )
        .commits(0),
        preference("size-100", "interface_size", json!(100)),
        preference("mask-white", "mask_overlay_colour", json!("white")),
        preference("lens-off", "auto_lens_profile", json!(false)),
        preference(
            "catalog-chosen",
            "catalog",
            json!(chosen(evidence).to_string_lossy()),
        ),
        preference("catalog-default", "catalog", Value::Null),
    ])
}

fn settings(frame: &Frame) -> &Value {
    &frame.state()["settings"]
}

/// The row the frame drew for `id`.
fn row<'a>(frame: &'a Frame, id: &str) -> Result<&'a Value> {
    settings(frame)["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .ok_or_else(|| format!("The frame shows no {id} row: {}", settings(frame)).into())
}

/// The flag as the host last listed it to the sheet.
fn listed<'a>(frame: &'a Frame, id: &str) -> Result<&'a Value> {
    settings(frame)["flags"]["flags"]
        .as_array()
        .and_then(|flags| flags.iter().find(|flag| flag["id"] == id))
        .ok_or_else(|| format!("The host listed no {id} flag").into())
}

/// The capture's colour at a fraction of its width and height.
fn colour_at(frame: &Frame, at: [f64; 2]) -> Result<[u8; 3]> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let x = (f64::from(width) * at[0]) as u32;
    let y = (f64::from(height) * at[1]) as u32;
    Ok(image.get_pixel(x.min(width - 1), y.min(height - 1)).0)
}

/// Inside the sheet, under its one tab: the empty foot of the tab rail, which the sheet draws on
/// the Bar surface over the dimmed workspace. The sheet is 720 by 480 points, centred in the
/// 1440 by 900 window, and its rail is 160 points wide.
const RAIL_FOOT: [f64; 2] = [(360.0 + 80.0) / 1440.0, (690.0 - 40.0) / 900.0];
const BAR: [u8; 3] = [0x23, 0x23, 0x26];

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 3)
}

/// The session workspace the frame records, less the mask overlay colour a General row sets.
fn session_workspace(frame: &Frame) -> Value {
    let mut workspace = frame.state()["workspace"].clone();
    if let Some(fields) = workspace.as_object_mut() {
        fields.remove("mask_overlay_colour");
    }
    workspace
}

fn preferences(frame: &Frame) -> &Value {
    &frame.state()["preferences"]
}

/// The General row the frame drew for `field`.
fn general_row<'a>(frame: &'a Frame, field: &str) -> Result<&'a Value> {
    settings(frame)["general"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == field))
        .ok_or_else(|| {
            format!(
                "The frame shows no General {field} row: {}",
                settings(frame)["general"]
            )
            .into()
        })
}

/// The frame's canvas region, `[left, top, right, bottom]` physical pixels.
fn canvas_rect(frame: &Frame) -> Result<[u32; 4]> {
    serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "The frame records no canvas rectangle".into())
}

/// The canvas beside the photograph: midway across the gap between the canvas's left edge and the
/// photograph's, at the photograph's vertical middle. The largest channel difference from
/// `expected` over a 5 × 5 patch there, and the patch's centre.
fn beside_photo(frame: &Frame, expected: [u8; 3]) -> Result<(f64, [u32; 2])> {
    let [canvas_left, ..] = canvas_rect(frame)?;
    let [left, top, _, bottom] = frame.photo()?;
    ensure(
        left >= canvas_left + 12,
        format!(
            "No canvas beside the photograph: it starts at {left}, the canvas at {canvas_left}"
        ),
    )?;
    let centre = [(canvas_left + left) / 2, (top + bottom) / 2];
    let image = frame.image()?;
    let mut worst = 0u8;
    for y in centre[1] - 2..=centre[1] + 2 {
        for x in centre[0] - 2..=centre[0] + 2 {
            let pixel = image.get_pixel(x, y).0;
            for (a, b) in pixel.iter().zip(expected) {
                worst = worst.max(a.abs_diff(b));
            }
        }
    }
    Ok((f64::from(worst), centre))
}

/// The pixels outside the canvas region, the panels and the bars, that differ between two
/// captures of the same size.
fn panels_changed(a: &Frame, b: &Frame) -> Result<u64> {
    let [left, top, right, bottom] = canvas_rect(a)?;
    ensure(
        canvas_rect(b)? == [left, top, right, bottom],
        "The canvas region moved",
    )?;
    let (a, b) = (a.image()?, b.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The captures differ in size",
    )?;
    Ok(a.enumerate_pixels()
        .filter(|(x, y, pixel)| {
            let inside = (left..right).contains(x) && (top..bottom).contains(y);
            !inside && **pixel != *b.get_pixel(*x, *y)
        })
        .count() as u64)
}

/// The pixels inside `photo`, less its outermost row and column on each side, that differ between
/// two captures.
fn photo_changed(a: &Frame, b: &Frame, photo: [u32; 4]) -> Result<u64> {
    let [left, top, right, bottom] = photo;
    let (a, b) = (a.image()?, b.image()?);
    Ok((top + 1..bottom - 1)
        .flat_map(|y| (left + 1..right - 1).map(move |x| (x, y)))
        .filter(|(x, y)| a.get_pixel(*x, *y) != b.get_pixel(*x, *y))
        .count() as u64)
}

/// The drawn height of the title bar with the rule under it, in the capture's pixels: up the
/// column through the middle of the photograph from just above its top edge, over the canvas
/// surface between them, to the first row that is not the canvas surface, which is the rule's
/// lowest.
fn title_bar_height(frame: &Frame) -> Result<u32> {
    let [left, top, right, _] = frame.photo()?;
    let image = frame.image()?;
    let x = (left + right) / 2;
    let mut y = top
        .checked_sub(2)
        .ok_or("The photograph touches the window's top")?;
    ensure(
        image.get_pixel(x, y).0 == DARK,
        format!("The canvas above the photograph is not the dark surface at {x},{y}"),
    )?;
    while y > 0 && image.get_pixel(x, y - 1).0 == DARK {
        y -= 1;
    }
    ensure(y > 0, "No title bar above the canvas")?;
    Ok(y)
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let opened = launch.at("opened")?;
    let mut checks = Checks::new();
    checks.note(
        opened,
        "the unedited photograph before the sheet",
        pixels::identity_photo(opened)?,
    );
    ensure(
        settings(opened)["open"].is_null(),
        "The sheet was open at launch",
    )?;
    let behind = colour_at(opened, RAIL_FOOT)?;
    ensure(
        !near(behind, BAR),
        "The workspace behind the sheet is already the sheet's colour, so the check proves nothing",
    )?;

    for step in launch.names().iter().skip(1) {
        let frame = launch.at(step)?;
        // The sheet is the desktop's own view state, and a flag or a preference touches no
        // photograph. The mask overlay colour is the session's, checked on its own below.
        ensure(
            session_workspace(frame) == session_workspace(opened)
                && frame.state()["stack"] == opened.state()["stack"],
            format!("Step {step:?} changed the session workspace or the stack"),
        )?;
        // Collapsing the Performance section stores it as the next launch's, and that step does
        // not wait for the write: its own frame may hold that one write, which the next frame,
        // a preference step that waits, must show finished.
        let allowed = u64::from(step == "performance-collapsed");
        ensure(
            settings(frame)["writes_outstanding"]
                .as_u64()
                .is_some_and(|outstanding| outstanding <= allowed)
                && settings(frame)["error"].is_null(),
            format!(
                "Step {step:?} left a write outstanding or failed: {}",
                settings(frame)
            ),
        )?;
    }

    let general = launch.at("general")?;
    let collapse = settings(general)["general"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "auto_collapse_history"));
    ensure(
        settings(general)["open"] == "general"
            && settings(general)["general"]["error"].is_null()
            && collapse
                == Some(
                    &json!({"id": "auto_collapse_history", "control": {"toggle": true},
                                "saving": false}),
                ),
        format!(
            "The palette did not open General with Auto collapse history on: {}",
            settings(general)
        ),
    )?;
    let drawn = colour_at(general, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The General tab is not drawn: the rail's foot is {drawn:?}, not the Bar surface"),
    )?;
    checks.note(
        general,
        "the General tab with Auto collapse history on by default",
        settings(general)["general"].clone(),
    );

    let sheet = launch.at("sheet")?;
    let ids: Vec<_> = settings(sheet)["rows"]
        .as_array()
        .ok_or("The sheet drew no rows")?
        .iter()
        .map(|row| row["id"].clone())
        .collect();
    ensure(
        settings(sheet)["open"] == "experiments"
            && ids
                == [
                    json!("developer"),
                    json!("ai.generative"),
                    json!("proof.choice"),
                    json!("proof.number"),
                ],
        format!("The palette opened another sheet: {}", settings(sheet)),
    )?;
    ensure(
        row(sheet, "developer")?["notes"] == json!(["On for this launch: --developer"]),
        "The developer row does not say --developer holds this launch",
    )?;
    let drawn = colour_at(sheet, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The sheet is not drawn: the rail's foot is {drawn:?}, not the Bar surface"),
    )?;
    checks.note(
        sheet,
        "the Experiments tab over the dimmed workspace",
        json!({"rows": ids, "rail_foot": drawn}),
    );

    let choice = launch.at("choice")?;
    ensure(
        row(choice, "proof.choice")?["control"] == json!({"choice": "third"})
            && listed(choice, "proof.choice")?["value"] == "third"
            && listed(choice, "proof.choice")?["stored"] == true,
        "The choice did not reach the host",
    )?;
    let number = launch.at("number")?;
    ensure(
        row(number, "proof.number")?["control"] == json!({"number": "75", "invalid": false})
            && listed(number, "proof.number")?["value"].as_f64() == Some(75.0),
        "The number did not reach the host",
    )?;
    let refused = launch.at("number-refused")?;
    ensure(
        row(refused, "proof.number")?["control"] == json!({"number": "33", "invalid": true})
            && listed(refused, "proof.number")?["value"].as_f64() == Some(75.0),
        "A number off its step was sent, or the field forgot what was typed",
    )?;
    let reset = launch.at("reset")?;
    ensure(
        row(reset, "proof.choice")?["control"] == json!({"choice": "first"})
            && row(reset, "proof.choice")?["can_reset"] == false
            && listed(reset, "proof.choice")?["stored"] == false,
        "Reset left a stored value",
    )?;
    let developer = launch.at("developer-off")?;
    let flag = listed(developer, "developer")?;
    ensure(
        flag["value"] == false
            && flag["active"] == true
            // The launch's own switch forced it; the sheet's note, checked above, names it.
            && flag["override"].is_string()
            && developer.state()["developer"] == true,
        format!("Developer mode's next launch or this one is wrong: {flag}"),
    )?;
    checks.note(
        developer,
        "developer mode off for the next launch, held on for this one",
        flag.clone(),
    );

    let closed = launch.at("closed")?;
    ensure(
        settings(closed)["open"].is_null(),
        "Escape left the sheet open",
    )?;
    let after = colour_at(closed, RAIL_FOOT)?;
    ensure(
        near(after, behind),
        format!("The sheet is still drawn after Escape: {after:?}, not {behind:?}"),
    )?;
    checks.note(
        closed,
        "the unedited photograph after the sheet",
        pixels::identity_photo(closed)?,
    );

    let reopened = launch.at("reopened")?;
    ensure(
        listed(reopened, "proof.number")?["value"].as_f64() == Some(75.0)
            && listed(reopened, "proof.choice")?["stored"] == false
            && listed(reopened, "developer")?["value"] == false,
        "Reopening read back other values than the host stored",
    )?;

    usage_statistics(launch, &mut checks)?;
    models(run, launch, &mut checks)?;
    general_preferences(run, launch, &mut checks)?;
    checks.write(
        &launch.evidence,
        "settings",
        json!({"canvas": {"dark": DARK, "grey": GREY, "tolerance_per_channel": CANVAS_TOLERANCE,
               "scope": "a 5 × 5 patch of the canvas beside the photograph"}}),
    )
}

fn usage(frame: &Frame) -> &Value {
    &frame.state()["usage"]
}

/// The Models tab as a frame recorded it.
fn models_tab(frame: &Frame) -> &Value {
    &settings(frame)["models"]
}

/// The proof model's one row and the `module.status` resource row it was drawn from. A developer
/// run also lists `luxforge.ai`'s models, which the scenario leaves alone.
fn model_rows(frame: &Frame) -> Result<(&Value, &Value)> {
    let tab = models_tab(frame);
    let row = tab["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["module_id"] == MODELS_PROOF_MODULE)
                .collect::<Vec<_>>()
        })
        .filter(|rows| rows.len() == 1)
        .map(|rows| rows[0])
        .ok_or_else(|| format!("The Models tab does not list the proof's one model: {tab}"))?;
    let status = tab["status"][MODELS_PROOF_MODULE]["resources"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == PROOF_MODEL_RESOURCE))
        .ok_or_else(|| format!("The held status lists no {PROOF_MODEL_RESOURCE}: {tab}"))?;
    Ok((row, status))
}

/// Every Models frame against the `module.status` answer it was drawn from: the row's state from
/// the resource's state and the resident set, a kept partial download named exactly when one is
/// staged and nothing is installing it, Remove offered exactly when there is something to remove, the fit and requirement from
/// the status's own, the loaded models from the resident set and the machine line from the
/// machine's memory.
fn correlated(step: &str, frame: &Frame) -> Result<Value> {
    let tab = models_tab(frame);
    let (row, status) = model_rows(frame)?;
    let runtime = &tab["status"][MODELS_PROOF_MODULE]["runtime"];
    let loaded: Vec<&Value> = runtime["models"]
        .as_array()
        .map(|models| models.iter().collect())
        .unwrap_or_default();
    let state = row["state"].as_str().unwrap_or_default();
    let staged = status["staged_bytes"].as_u64().filter(|bytes| *bytes > 0);
    let expected_state = match (status["state"].as_str(), loaded.first()) {
        (Some("installed"), Some(model)) if model["state"] == "loaded" => state == "Loaded",
        (Some("installed"), Some(_)) => state == "Loading",
        (Some("installed"), None) => state == "Installed",
        (Some("installing"), _) => state.starts_with("Installing"),
        (Some("failed"), _) => state.starts_with("Failed: "),
        (Some("not-installed"), _) => state.starts_with("Not installed"),
        _ => false,
    };
    let memory = tab["status"][MODELS_PROOF_MODULE]["machine"]["memory_bytes"].as_u64();
    let machine = tab["machine"].as_str().unwrap_or_default();
    let resident: Vec<&Value> = tab["resident"]
        .as_array()
        .map(|rows| rows.iter().collect())
        .unwrap_or_default();
    ensure(
        expected_state
            // A row names a kept partial download while nothing is installing it.
            && state.contains("downloaded, resumes")
                == (staged.is_some() && status["state"] != "installing")
            && row["removable"] == json!(status["state"] == "installed" || staged.is_some())
            && row["fits"] == status["fits"]
            && row["tradeoff"] == PROOF_MODEL_TRADEOFF
            && row["use_restriction"] == PROOF_MODEL_RESTRICTION
            && row["requirement"] == "Needs 2 GB of memory · 64.0 MB while loaded · fake · fp32"
            && status["requirements"]["min_memory_bytes"] == 2u64 << 30
            && resident.len() == loaded.len()
            && resident
                .iter()
                .zip(&loaded)
                .all(|(drawn, held)| drawn["resource_id"] == held["resource_id"])
            // The machine's own memory, which the desktop read from the operating system.
            && memory.is_some_and(|memory| memory >= 1 << 30)
            && machine.starts_with("This machine has ")
            // luxforge.ai's models are named first, then the proof's.
            && machine.contains("FLUX.2 klein 4B with the object removal LoRA needs 16 GB")
            && machine.ends_with("; Proof segmenter needs 2 GB.")
            && tab["tiers"] == json!([])
            && tab["errors"] == json!([]),
        format!("Step {step:?}'s Models tab does not match the module.status it holds: {tab}"),
    )?;
    Ok(
        json!({"state": state, "status": status["state"], "staged_bytes": staged,
              "resident": loaded.len(), "machine": machine}),
    )
}

/// The Models tab: listed with this machine's memory, the interrupted download kept and named, the
/// resumed one installing both files from the cut byte, the model loaded and listed resident, then
/// Unload now and Remove, each correlated to `module.status` and the endpoint's record.
fn models(run: &Run, launch: &Checked, checks: &mut Checks) -> Result {
    let steps = [
        "models",
        "models-allowed",
        "models-interrupted",
        "models-partial",
        "models-resuming",
        "models-installed",
        "models-loading",
        "models-loaded",
        "models-unloaded",
        "models-removed",
    ];
    let mut seen = serde_json::Map::new();
    for step in steps {
        let frame = launch.at(step)?;
        ensure(
            settings(frame)["open"] == "models",
            format!(
                "Step {step:?} is not on the Models tab: {}",
                settings(frame)
            ),
        )?;
        seen.insert(step.into(), correlated(step, frame)?);
    }
    let listed = launch.at("models")?;
    let (row, status) = model_rows(listed)?;
    ensure(
        row["state"] == "Not installed"
            && row["detail"] == "v1 · 8.8 KB · 2 files"
            && status["bytes"] == MODEL_BYTES
            && status["fits"] == true,
        format!("The Models tab did not list the model uninstalled: {row}"),
    )?;
    let drawn = colour_at(listed, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The Models tab is not drawn: the rail's foot is {drawn:?}, not the Bar surface"),
    )?;
    let partial = launch.at("models-partial")?;
    let (row, status) = model_rows(partial)?;
    ensure(
        status["staged_bytes"] == CUT
            && row["state"].as_str().is_some_and(|state| {
                state.ends_with("1000 B of 8.8 KB downloaded, resumes on the next download")
            })
            && row["removable"] == true,
        format!("The interrupted download was not kept and named: {row}"),
    )?;
    let installed = launch.at("models-installed")?;
    let (row, status) = model_rows(installed)?;
    let path = status["path"].as_str().map(PathBuf::from);
    ensure(
        row["state"] == "Installed"
            && status["state"] == "installed"
            && status.get("staged_bytes").is_none()
            && status["files"]
                == json!([{"name": PROOF_MODEL_RESOURCE, "bytes": 6000}, {"name": "decoder", "bytes": 3000}])
            && row["remove_request"]["method"] == "module.resource.remove"
            && row["remove_request"]["params"]["resource_id"] == PROOF_MODEL_RESOURCE,
        format!("The resumed download did not install both files: {row} {status}"),
    )?;
    let loaded = launch.at("models-loaded")?;
    let tab = models_tab(loaded);
    ensure(
        model_rows(loaded)?.0["state"] == "Loaded"
            && tab["resident"][0]["resource_id"] == PROOF_MODEL_RESOURCE
            && tab["resident"][0]["unload_request"]
                == json!({"method": "module.runtime.unload",
                          "params": {"module_id": MODELS_PROOF_MODULE, "resource_id": PROOF_MODEL_RESOURCE}}),
        format!("The loaded model is not listed resident with its Unload now request: {tab}"),
    )?;
    let unloaded = launch.at("models-unloaded")?;
    ensure(
        model_rows(unloaded)?.0["state"] == "Installed"
            && models_tab(unloaded)["resident"] == json!([]),
        format!("Unload now left the model loaded: {}", models_tab(unloaded)),
    )?;
    let removed = launch.at("models-removed")?;
    let (row, status) = model_rows(removed)?;
    ensure(
        row["state"] == "Not installed"
            && row["removable"] == false
            && status["state"] == "not-installed"
            && path.as_ref().is_some_and(|path| !path.exists()),
        format!("Remove left the model installed: {row} {status} {path:?}"),
    )?;
    // The endpoint saw the encoder whole and cut, then from the cut byte, and the decoder once.
    let requests = &run.recorded_value("models_endpoint")["requests"];
    let encoder: Vec<&Value> = requests
        .as_array()
        .ok_or("The run recorded nothing the models endpoint saw")?
        .iter()
        .filter(|request| request["path"] == PROOF_ENCODER_PATH)
        .collect();
    let decoder = requests
        .as_array()
        .into_iter()
        .flatten()
        .filter(|request| request["path"] == PROOF_DECODER_PATH)
        .count();
    ensure(
        encoder.len() == 2
            && encoder[0]["range"].is_null()
            && encoder[1]["range"] == format!("bytes={CUT}-")
            && encoder[1]["status"] == 206
            && decoder == 1,
        format!(
            "The endpoint did not see one cut download and one resumed from byte {CUT}: {requests}"
        ),
    )?;
    checks.note(
        installed,
        "the Models tab over a download cut at 1000 bytes and resumed with a Range request",
        json!({"steps": seen, "endpoint": requests}),
    );
    Ok(())
}

/// The usage statistics steps: the notice claimed, drawn and answered, and the Privacy tab's
/// switch, each against the consent the host stored; no frame records an identifier or names the
/// production origin as its destination.
fn usage_statistics(launch: &Checked, checks: &mut Checks) -> Result {
    for step in launch.names() {
        let usage = usage(launch.at(step)?);
        let keys: Vec<&String> = usage
            .as_object()
            .ok_or_else(|| format!("Step {step:?} records no usage statistics"))?
            .keys()
            .collect();
        ensure(
            usage["eligible"] == true && !keys.iter().any(|key| key.ends_with("_id")),
            format!("Step {step:?} is not eligible or records an identifier: {usage}"),
        )?;
        ensure(
            usage["destination"].as_str().is_none_or(|destination| {
                destination == USAGE_RECEIVER && !destination.contains(PRODUCTION_ORIGIN)
            }),
            format!("Step {step:?} names a destination other than the test receiver: {usage}"),
        )?;
    }
    let opened = launch.at("opened")?;
    ensure(
        usage(opened)["prompt"] == "waiting" && usage(opened)["notice"].is_null(),
        format!(
            "The notice was claimed before the opens were captured: {}",
            usage(opened)
        ),
    )?;

    let notice = launch.at("usage-notice")?;
    let drawn = usage(notice);
    ensure(
        drawn["prompt"] == "shown"
            && drawn["notice"]
                == json!({
                    "title": "Help improve Luxforge?",
                    "body": "Luxforge can send usage events to api.luxforge.app for debugging \
                             and analytics. They never include your name, contact details, IP \
                             address, photos, filenames or photo metadata. You can change this \
                             in Settings \u{203a} Privacy.",
                    "buttons": ["Share statistics", "No thanks"],
                })
            && drawn["stored"]["prompt_shown"] == true
            && drawn["stored"]["enabled"] == false
            && drawn["effective"] == false
            && drawn["destination"] == USAGE_RECEIVER,
        format!("The notice was not claimed and drawn as designed: {drawn}"),
    )?;
    let share = launch.at("usage-share")?;
    // The card is drawn above the status bar, centred: the band there changes when it goes.
    let changed = band_changed(notice, share, [0.3, 0.7], [0.70, 0.95])?;
    ensure(
        changed > 2_000,
        format!("The notice is not drawn: only {changed} pixels of its band changed when it went"),
    )?;
    let shared = usage(share);
    ensure(
        shared["prompt"] == "shared"
            && shared["notice"].is_null()
            && shared["stored"]["enabled"] == true
            && shared["effective"] == true
            && shared["writes_outstanding"] == 0,
        format!("Share statistics did not store and apply the opt-in: {shared}"),
    )?;
    checks.note(
        notice,
        "the startup notice, claimed once the opens were captured",
        json!({"notice": drawn["notice"], "stored": drawn["stored"], "band_pixels_changed": changed}),
    );

    let privacy = launch.at("privacy")?;
    ensure(
        settings(privacy)["open"] == "privacy"
            && settings(privacy)["tabs"]
                == json!(["general", "appearance", "models", "experiments", "privacy"])
            && usage(privacy)["privacy"]["switch"] == true
            && usage(privacy)["privacy"]["error"].is_null()
            && usage(privacy)["privacy"]["details_open"] == false,
        format!(
            "The Privacy tab does not show the stored opt-in: {}",
            usage(privacy)
        ),
    )?;
    let drawn = colour_at(privacy, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The Privacy tab is not drawn: the rail's foot is {drawn:?}"),
    )?;
    let details = launch.at("privacy-details")?;
    ensure(
        usage(details)["privacy"]["details_open"] == true,
        "What is sent did not open",
    )?;
    let off = launch.at("privacy-off")?;
    ensure(
        usage(off)["privacy"]["switch"] == false
            && usage(off)["effective"] == false
            && usage(off)["stored"]["enabled"] == false
            && usage(off)["stored"]["prompt_shown"] == true
            && usage(off)["privacy"]["error"].is_null(),
        format!(
            "Turning the switch off did not store the opt-out: {}",
            usage(off)
        ),
    )?;
    let on = launch.at("privacy-on")?;
    ensure(
        usage(on)["privacy"]["switch"] == true
            && usage(on)["effective"] == true
            && usage(on)["stored"]["enabled"] == true
            && usage(on)["prompt"] == "shared",
        format!(
            "Turning the switch on did not store the opt-in: {}",
            usage(on)
        ),
    )?;
    checks.note(
        on,
        "the Privacy switch off and on again, each stored by usage.set",
        json!({"off": usage(off)["stored"], "on": usage(on)["stored"]}),
    );
    Ok(())
}

/// How many pixels differ between two same-sized captures inside a band given as fractions of the
/// width and the height.
fn band_changed(a: &Frame, b: &Frame, across: [f64; 2], down: [f64; 2]) -> Result<u64> {
    let (a, b) = (a.image()?, b.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The captures differ in size",
    )?;
    let (width, height) = a.dimensions();
    let span = |range: [f64; 2], size: u32| {
        (f64::from(size) * range[0]) as u32..(f64::from(size) * range[1]) as u32
    };
    let (xs, ys) = (span(across, width), span(down, height));
    Ok(ys
        .flat_map(|y| xs.clone().map(move |x| (x, y)))
        .filter(|(x, y)| a.get_pixel(*x, *y) != b.get_pixel(*x, *y))
        .count() as u64)
}

/// The General rows' steps: what each frame draws and records of the preference it set.
fn general_preferences(run: &Run, launch: &Checked, checks: &mut Checks) -> Result {
    let closed = launch.at("performance-collapsed")?;
    ensure(
        settings(closed)["open"].is_null(),
        "Escape left the sheet open",
    )?;
    for step in [
        "closed-again",
        "performance-collapsed",
        "grey",
        "dark",
        "size-125",
    ] {
        ensure(
            settings(launch.at(step)?)["open"].is_null(),
            format!("The sheet is open at step {step:?}, dimming the canvas"),
        )?;
    }
    for step in launch.names().iter().skip_while(|step| *step != "grey") {
        let frame = launch.at(step)?;
        ensure(
            preferences(frame)["error"].is_null()
                && preferences(frame)["writing"].is_null()
                && preferences(frame)["waiting"].is_null(),
            format!(
                "Step {step:?} left a preference write outstanding or failed: {}",
                preferences(frame)
            ),
        )?;
    }

    // The canvas background: grey beside the photograph, the panels and bars as they were, and
    // dark again.
    let (before, _) = beside_photo(closed, DARK)?;
    checks.compare(
        closed,
        "the dark canvas beside the photograph before the change",
        before,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    let grey = launch.at("grey")?;
    ensure(
        preferences(grey)["display"]["canvas_background"] == "grey"
            && preferences(grey)["applied"]["canvas_background"] == "grey"
            && preferences(grey)["stored"]["canvas_background"] == "grey",
        format!(
            "The grey canvas was not applied and stored: {}",
            preferences(grey)
        ),
    )?;
    let (difference, at) = beside_photo(grey, GREY)?;
    checks.compare(
        grey,
        "the grey canvas beside the photograph, largest channel difference from #777777",
        difference,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    let changed = panels_changed(closed, grey)?;
    checks.compare(
        grey,
        "pixels outside the canvas region changed by the grey canvas",
        changed as f64,
        0.0,
        Tolerance::Within(0.0),
    )?;
    let photo = grey.photo()?;
    ensure(
        photo == closed.photo()?,
        "The grey canvas moved the photograph",
    )?;
    let changed = photo_changed(closed, grey, photo)?;
    checks.compare(
        grey,
        "pixels of the photograph changed by the grey canvas",
        changed as f64,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.note(
        grey,
        "the grey canvas around the unedited photograph",
        json!({"beside_photo": at, "photo": photo}),
    );
    let dark = launch.at("dark")?;
    let (difference, _) = beside_photo(dark, DARK)?;
    checks.compare(
        dark,
        "the dark canvas beside the photograph, largest channel difference from #19191b",
        difference,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    ensure(
        preferences(dark)["display"]["canvas_background"] == "dark",
        "The dark canvas was not applied",
    )?;

    // The interface size: the combined scale factor, and the title bar drawn 1.25 times as tall.
    let size = launch.at("size-125")?;
    let display = &preferences(size)["display"];
    let system = display["system_scale_factor"]
        .as_f64()
        .ok_or("The frame records no system scale factor")?;
    let scale = display["scale_factor"]
        .as_f64()
        .ok_or("The frame records no scale factor")?;
    ensure(
        display["interface_size"] == 125 && preferences(size)["applied"]["interface_size"] == 125,
        format!("The frame does not record the interface size at 125%: {display}"),
    )?;
    checks.compare(
        size,
        "the scale factor against the system's × 1.25",
        scale,
        system * 1.25,
        Tolerance::Within(1e-6),
    )?;
    let (at_100, at_125) = (title_bar_height(dark)?, title_bar_height(size)?);
    checks.compare(
        size,
        "the title bar's drawn height with its rule at 125%, against 1.25 × its height at 100%",
        f64::from(at_125),
        f64::from(at_100) * 1.25,
        Tolerance::Within(1.0),
    )?;
    for (frame, height) in [(dark, at_100), (size, at_125)] {
        ensure(
            canvas_rect(frame)?[1] == height,
            format!(
                "The canvas the frame records starts at row {}, not where the title bar is drawn to, {height}",
                canvas_rect(frame)?[1]
            ),
        )?;
    }
    checks.note(
        size,
        "the workspace at 125%",
        json!({"display": display, "title_bar_rows": {"100": at_100, "125": at_125}}),
    );
    let sheet = launch.at("general-125")?;
    ensure(
        settings(sheet)["open"] == "general"
            && preferences(sheet)["display"]["interface_size"] == 125,
        "The General tab did not open at 125%",
    )?;
    checks.note(
        sheet,
        "the General tab at 125%, for visual review",
        settings(sheet)["general"].clone(),
    );
    let back = launch.at("size-100")?;
    ensure(
        preferences(back)["display"]["interface_size"] == 100
            && preferences(back)["display"]["scale_factor"].as_f64() == Some(system)
            && preferences(back)["stored"]["interface_size"] == 100,
        format!(
            "The interface size did not return to 100%: {}",
            preferences(back)
        ),
    )?;

    // The mask overlay colour and the lens switch, as the rows and the preferences show them.
    let white = launch.at("mask-white")?;
    ensure(
        general_row(white, "mask_overlay_colour")?["control"] == json!({"choice": "white"})
            && preferences(white)["applied"]["mask_overlay_colour"] == "white"
            && preferences(white)["stored"]["mask_overlay_colour"] == "white"
            && white.state()["workspace"]["mask_overlay_colour"] == "white",
        format!(
            "The mask overlay colour is not white in the row, the preferences and the session: {} {}",
            general_row(white, "mask_overlay_colour")?,
            preferences(white)
        ),
    )?;
    let lens = launch.at("lens-off")?;
    ensure(
        general_row(lens, "auto_lens_profile")?["control"] == json!({"toggle": false})
            && preferences(lens)["applied"]["auto_lens_profile"] == false
            && preferences(lens)["stored"]["auto_lens_profile"] == false,
        format!(
            "The lens switch is not off in the row and the preferences: {}",
            preferences(lens)
        ),
    )?;
    checks.note(
        lens,
        "the lens switch off; that a RAW import then commits no lens entry is the core's owner tests'",
        general_row(lens, "auto_lens_profile")?.clone(),
    );

    // The catalog: chosen inside the evidence directory for the next launch, then Use Default.
    let evidence = evidence(run)?;
    let file = chosen(&evidence);
    let this_launch = "This launch uses the evidence run's catalog";
    let relaunch = format!("Relaunch to use {}", file.display());
    let catalog = launch.at("catalog-chosen")?;
    let row = &general_row(catalog, "catalog")?["control"];
    ensure(
        row["catalog"] == json!(evidence.join(CATALOG_FILE).to_string_lossy())
            && row["stored"] == json!(file.to_string_lossy())
            && row["notes"] == json!([this_launch, relaunch])
            && preferences(catalog)["stored"]["catalog"] == json!(file.to_string_lossy()),
        format!("The Catalog row does not show the chosen folder for the next launch: {row}"),
    )?;
    checks.note(
        catalog,
        "a catalog folder chosen for the next launch",
        row.clone(),
    );
    let default = launch.at("catalog-default")?;
    let row = &general_row(default, "catalog")?["control"];
    ensure(
        row["stored"].is_null()
            && row["notes"] == json!([this_launch])
            && preferences(default)["stored"]["catalog"].is_null(),
        format!("Use Default left a stored location or its relaunch note: {row}"),
    )?;
    ensure(
        !file.exists(),
        "Choosing a catalog folder created a catalog before the next launch",
    )?;
    checks.note(default, "Use Default", row.clone());
    Ok(())
}
