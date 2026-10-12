//! The `hdr-merge` smoke scenario: an exposure bracket merged into one HDR photograph in Select,
//! developed, removed and put back, then reopened with its bracket offline, in the real editor at
//! 1440 × 900 ([HDR merge design](../../docs/design/hdr-merge.md), "Evidence and acceptance").
//!
//! The run first writes two synthetic RAW brackets with `luxforge_testkit::hdr_bracket`, each
//! three 1536 × 1024 DNGs at 0, −2 and +2 EV: a still one (`Brackets/Still`, seed 1) and a
//! handheld one with a moving ball (`Brackets/Handheld`, seed 2). It makes a new catalog, adds
//! `Brackets` to the indexed folders and asks the core for its `merge.plan` of the still bracket
//! (`hdr-merge-expected.json`), hashing every frame. Writing the brackets admits their synthetic
//! body, a RAW test camera, in this process; each editor launch is started with
//! `LUXFORGE_TEST_CAMERAS=1`, without which the editor refuses it.
//!
//! The first launch, in [`first_plan`] order: `G`; the still bracket's folder, whose header offers
//! Import merged and Pick merged; Pick merged, which opens the merge preview sheet, captured once it
//! draws the preview; Deghosting turned Off, captured once the new revision is drawn; Show deghost
//! overlay on; the sheet's Pick merged, captured while its pending row shows the job's progress;
//! the merge's end, one collapsed HDR tile, selected and in the Development set; `S`
//! expanding the stack; an original clicked and picked with `P`; Develop, showing the result from
//! its saved source; one Exposure edit; `merge.inspect` of the open result; back to Select, the
//! result clicked and Remove merge (`⌫`); `Cmd+Z`, `Shift+Cmd+Z` and `Cmd+Z` again; then the
//! handheld bracket's folder; its preview opened by Import merged, Deghosting put back to Auto and
//! the overlay turned on, showing the moving ball's regions, and Cancel; then Import merged
//! ⌥-clicked, merging at once with the last settings (Deghosting Off, remembered from the Pick),
//! and its end. Each Select frame records the owner's own development set and every HDR merge job
//! on its activity board beside the session.
//!
//! The catalog the first launch left is read (`hdr-merge-after-first.json`); the brackets' folder
//! is renamed, so no original can be read; and the second launch, in [`second_plan`] order, opens
//! the result from its catalog folder in Develop and inspects it. The folder is named back, the
//! catalog read again (`hdr-merge-after.json`) and every frame's hash checked.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, BASIC_EFFECT, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{
    self as script, CatalogStep, DevelopStep, LibraryKey, MergeMode, MergePreviewStep, MergeToggle,
    SelectStep, SelectWorkspace,
};
use luxforge_testkit::hdr_bracket::{BracketSpec, write_bracket};
use std::collections::BTreeSet;

pub const SCENARIO: &str = "hdr-merge";
/// What `reproduce.md` says the run does before, between and after its launches.
pub const NOTE: &str = "The run first writes two synthetic RAW brackets with \
    `luxforge_testkit::hdr_bracket::write_bracket` (three 1536 × 1024 DNGs each at 0, −2 and +2 EV: \
    `generated/Brackets/Still`, seed 1, and `generated/Brackets/Handheld`, seed 2, handheld with a \
    moving ball), makes `generated/catalog.sqlite` with `Brackets` indexed, and asks the core for \
    the still bracket's `merge.plan` (`hdr-merge-expected.json`). Both launches set \
    `LUXFORGE_TEST_CAMERAS=1`, so the editor admits the brackets' synthetic test body. The first launch merges, develops, \
    removes and puts back over that catalog with `--catalog`; the run then reads the catalog it \
    left (`hdr-merge-after-first.json`), renames `Brackets` to `Brackets offline`, and the second \
    launch reopens the same catalog and opens the result from its catalog folder; the folder is \
    named back and the catalog read again (`hdr-merge-after.json`).";
/// Where the run writes its brackets and catalog.
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "hdr-merge-expected.json";
pub const AFTER_FIRST: &str = "hdr-merge-after-first.json";
pub const AFTER: &str = "hdr-merge-after.json";
const BRACKETS: &str = "Brackets";
const OFFLINE: &str = "Brackets offline";
const STILL: &str = "Still";
const HANDHELD: &str = "Handheld";
const FRAMES: usize = 3;
/// The Exposure the run's one edit sets, in stops.
const EXPOSURE: f64 = 1.0;
/// How far the reopened result's middle may be from the edited one's, in 8-bit codes of
/// luminance: the same saved source and edit, drawn at the same size by the same renderer.
const REOPEN_TOLERANCE: f64 = 3.0;
/// How much brighter one stop of Exposure must make the middle, in 8-bit codes.
const EDIT_LIFT: f64 = 10.0;

/// The first launch's frames: the merge, its stack, Develop, removal and its undo, and Import.
pub fn first_plan(expected: &Value) -> Result<Plan> {
    let folder = |name: &str| -> Result<String> {
        expected["brackets"][name]["folder"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("The expected answers name no {name} folder").into())
    };
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let key = |name: &str, key: &str| Step::new(name, script::Step::key(key));
    let merge = |name: &str, mode, last_settings| {
        select(
            name,
            SelectStep::Merge {
                position: 0,
                mode,
                last_settings,
            },
        )
    };
    let preview = |name: &str, step| select(name, SelectStep::MergePreview(step));
    let click = |name: &str, position| {
        select(
            name,
            SelectStep::Click {
                position,
                shift: false,
                command: false,
            },
        )
    };
    Ok(Plan::new(vec![
        Step::opened("opened"),
        key("select", "g"),
        select("still", SelectStep::Folder(folder(STILL)?)),
        merge("previewing", MergeMode::Pick, false),
        preview("deghosted", MergePreviewStep::Deghost(MergeToggle::Off)),
        preview("overlaid", MergePreviewStep::Overlay(true)),
        preview("merging", MergePreviewStep::Merge(MergeMode::Pick)),
        select("merged", SelectStep::Merged),
        key("expanded", "s"),
        click("original", 1),
        key("picked", "p").status_starts("Picked "),
        select("develop", SelectStep::Switch(SelectWorkspace::Develop)),
        Step::new(
            "edited",
            script::Step::call("edit.set-basic", json!({ "exposure": EXPOSURE })),
        )
        .commits(1)
        .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE })),
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
        key("back", "g"),
        click("result", 0),
        Step::new("removed", script::Step::Catalog(CatalogStep::DeleteKey)),
        select("restored", SelectStep::Library(LibraryKey::Undo)),
        select("redone", SelectStep::Library(LibraryKey::Redo)),
        select("undone", SelectStep::Library(LibraryKey::Undo)),
        select("handheld", SelectStep::Folder(folder(HANDHELD)?)),
        merge("handheld_preview", MergeMode::Import, false),
        preview(
            "handheld_deghost",
            MergePreviewStep::Deghost(MergeToggle::Auto),
        ),
        preview("handheld_overlay", MergePreviewStep::Overlay(true)),
        preview("handheld_cancelled", MergePreviewStep::Cancel),
        merge("importing", MergeMode::Import, true),
        select("imported", SelectStep::Merged),
    ]))
}

/// The second launch's frames, over the catalog the first left with the brackets offline: the
/// result's catalog folder, the result opened in Develop, and its merge inspected.
pub fn second_plan(after: &Value) -> Result<Plan> {
    let folder = after["reopen"]["folder"]
        .as_str()
        .ok_or("The first launch's catalog names no folder for the result")?;
    let position = after["reopen"]["position"]
        .as_u64()
        .ok_or("The first launch's catalog places no result")?;
    Ok(Plan::new(vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "folder",
            script::Step::Catalog(CatalogStep::Source(folder.to_owned())),
        ),
        Step::new(
            "result",
            script::Step::Select(SelectStep::Click {
                position: position as u32,
                shift: false,
                command: false,
            }),
        ),
        Step::new("reopened", script::Step::Develop(DevelopStep::Active))
            .payload(BASIC_EFFECT, json!({ "exposure": EXPOSURE })),
        Step::new("inspected", script::Step::call("merge.inspect", json!({}))),
    ]))
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("hdr-merge-smoke-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// `body` through an owner of the runner's own over `catalog`.
fn with_owner<T>(
    catalog: &Path,
    body: impl FnOnce(&OwnerHandle, ClientId) -> Result<T>,
) -> Result<T> {
    let (owner, join) = OwnerHandle::start(catalog)
        .map_err(|error| format!("the core cannot open the catalog: {error}"))?;
    let client = owner.register();
    let outcome = body(&owner, client);
    owner.stop();
    let _ = join.join();
    outcome
}

/// Read a job until it ends.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", json!({ "job_id": job }))?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(120),
            format!("A job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Write both brackets, make the catalog with their folder indexed, and ask the core for its plan
/// of the still bracket. Answers the expected record and every frame, in order.
fn make(generated: &Path) -> Result<(Value, Vec<PathBuf>)> {
    let brackets = generated.join(BRACKETS);
    let still = write_bracket(
        &brackets.join(STILL),
        &BracketSpec {
            frames: FRAMES,
            seed: 1,
            ..BracketSpec::default()
        },
    )?;
    let handheld = write_bracket(
        &brackets.join(HANDHELD),
        &BracketSpec {
            frames: FRAMES,
            seed: 2,
            handheld: true,
            moving_subject: Some(2),
            first_number: 11,
            ..BracketSpec::default()
        },
    )?;
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let brackets = brackets.canonicalize()?;
    let still: Vec<PathBuf> = still
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let handheld: Vec<PathBuf> = handheld
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let plan = with_owner(&catalog, |owner, client| {
        let added = ask(
            owner,
            client,
            "index.add-folder",
            json!({"path": brackets,
                "mutation": {"request_id": "setup-brackets", "actor": "setup"}}),
        )?;
        if !added["job_id"].is_null() {
            let read = settle(owner, client, &added["job_id"])?;
            ensure(
                read["status"] == "ready",
                format!("the core could not list the brackets: {read}"),
            )?;
        }
        ask(
            owner,
            client,
            "merge.plan",
            json!({"kind": "hdr", "targets": {"kind": "paths", "paths": still}}),
        )
    })?;
    let folder = |name: &str| brackets.join(name);
    let expected = json!({
        "brackets": {
            STILL: {"folder": folder(STILL), "frames": still},
            HANDHELD: {"folder": folder(HANDHELD), "frames": handheld},
        },
        "plan": plan,
    });
    Ok((expected, still.into_iter().chain(handheld).collect()))
}

/// The result a merge step's frame recorded: the job's `ready` record's result.
fn merge_answer(frame: &Frame) -> &Value {
    &frame["step"]["merge_record"]["result"]
}

/// What the catalog says of the two results and the run's changes, over an owner of the runner's
/// own: each merge inspected, the first result's history, its catalog folder and its place in that
/// folder's view (which the second launch opens it from), and the library journal.
fn after(catalog: &Path, results: &[Value]) -> Result<Value> {
    with_owner(catalog, |owner, client| {
        let mut merges = Vec::new();
        for result in results {
            merges.push(ask(
                owner,
                client,
                "merge.inspect",
                json!({ "asset_id": result }),
            )?);
        }
        let history = ask(
            owner,
            client,
            "history.list",
            json!({ "asset_id": results[0] }),
        )?;
        // The result's row in a view of `source`, read whole.
        let row_in = |source: Value| -> Result<Value> {
            let view = ask(owner, client, "browse.view", json!({ "source": source }))?;
            let count = view["count"].as_u64().unwrap_or(0);
            if count == 0 {
                return Ok(Value::Null);
            }
            let rows = ask(
                owner,
                client,
                "browse.rows",
                json!({"from": 0, "count": count}),
            )?;
            Ok(rows["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["asset_id"] == results[0])
                .cloned()
                .unwrap_or(Value::Null))
        };
        let photograph = row_in(json!({"kind": "all-photographs"}))?;
        let folders = ask(owner, client, "folder.list", json!({}))?;
        let folder = folders["folders"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|folder| !photograph.is_null() && folder["id"] == photograph["folder_id"])
            .cloned()
            .unwrap_or(Value::Null);
        let position = if folder.is_null() {
            Value::Null
        } else {
            row_in(json!({"kind": "catalog-folder", "folder_id": folder["id"]}))?["position"]
                .clone()
        };
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        let changes: Vec<Value> = journal["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|change| {
                json!({"method": change["method"], "label": change["label"], "actor": change["actor"]})
            })
            .collect();
        Ok(json!({
            "merges": merges,
            "history": history["entries"],
            "photograph": photograph,
            "reopen": {"folder": folder["name"], "folder_id": folder["id"], "position": position},
            "changes": changes,
        }))
    })
}

/// Make the brackets and catalog (a replay reads the recorded answers), launch the editor over it,
/// read the catalog it left, take the brackets offline, launch it again, and check every frame.
pub fn run(mut run: Run, scenario: &'static Scenario, _: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let catalog = generated.join(generate_catalog::CATALOG);
    let expected_file = run.out().join(EXPECTED);
    let first_file = run.out().join(AFTER_FIRST);
    let after_file = run.out().join(AFTER);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let frames = if run.replaying() {
            Vec::new()
        } else {
            fs::create_dir_all(&generated)?;
            let (expected, frames) = make(&generated)?;
            write_json(&expected_file, &expected)?;
            frames
        };
        let expected = read_json(&recorded(run, &expected_file))?;
        if !run.replaying() {
            run.hash(&frames)?;
        }
        // The brackets are of a RAW test camera, which the editor admits only when asked.
        let window = |launch: Launch| match scenario.window {
            Some(window) => launch.window(window).test_cameras(),
            None => launch.test_cameras(),
        };
        let first = first_plan(&expected)?;
        let launched = run.launch(window(
            Launch::named("first")
                .catalog(&catalog)
                .script("first-script.json", first.script()),
        ))?;
        let first_checked = first.check(&launched.dir)?;
        let results = [
            merge_answer(first_checked.at("merged")?)["result_asset_id"].clone(),
            merge_answer(first_checked.at("imported")?)["result_asset_id"].clone(),
        ];
        if !run.replaying() {
            write_json(&first_file, &after(&catalog, &results)?)?;
        }
        let after_first = read_json(&recorded(run, &first_file))?;
        let second = second_plan(&after_first)?;
        let brackets = generated.join(BRACKETS);
        let offline = generated.join(OFFLINE);
        if !run.replaying() {
            fs::rename(&brackets, &offline)?;
        }
        let launched = run.launch(window(
            Launch::named("second")
                .catalog(&catalog)
                .script("second-script.json", second.script()),
        ));
        if !run.replaying() {
            fs::rename(&offline, &brackets)?;
        }
        let second_checked = second.check(&launched?.dir)?;
        if !run.replaying() {
            write_json(&after_file, &after(&catalog, &results)?)?;
        }
        (scenario.verify)(run, &[first_checked, second_checked])?;
        if !run.replaying() {
            run.sources_unchanged()?;
        }
        Ok(())
    })
}

/// What the recorded run holds at `file` in a replay, else `file`.
fn recorded(run: &Run, file: &Path) -> PathBuf {
    run.recorded(file).unwrap_or_else(|| file.to_owned())
}

fn select(frame: &Frame) -> &Value {
    &frame.state()["select"]
}

fn develop(frame: &Frame) -> &Value {
    &frame.state()["develop"]
}

/// The asset identities the owner's development set held when the frame was settled.
fn owner_set(frame: &Frame) -> Result<Vec<Value>> {
    let set = &frame["step"]["owner_set"];
    ensure(
        set.is_object() && set["error"].is_null(),
        format!("The owner's development set was not read: {set}"),
    )?;
    Ok(set["members"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|member| member["asset_id"].clone())
        .collect())
}

/// The HDR merge jobs the owner's activity board listed when the frame was settled.
fn merge_jobs(frame: &Frame) -> BTreeSet<String> {
    frame["step"]["owner_merge_jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|job| job["job_id"].as_str().map(str::to_owned))
        .collect()
}

/// The frame's stacked rows, as their cells draw them.
fn stacks(frame: &Frame) -> Vec<Value> {
    select(frame)["stacks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The one row leading a collapsed stack of the bracket, as the merge's result: its badge, its
/// originals beneath it, and whether it is in the Development set.
fn collapsed_leader(frame: &Frame, name: &str, result: &Value, in_set: bool) -> Result {
    let rows = stacks(frame);
    ensure(
        rows.len() == 1 && select(frame)["count"] == 1,
        format!(
            "{name}: the view holds {} items and these stacked rows: {rows:?}",
            select(frame)["count"]
        ),
    )?;
    let row = &rows[0];
    ensure(
        row["item"]["asset_id"] == *result
            && row["role"] == "leader"
            && row["kind"] == "hdr"
            && row["size"] == FRAMES
            && row["collapsed"] == FRAMES
            && row["badge"] == json!(["HDR \u{b7} 3 exposures", "HDR \u{b7} 3", false])
            && row["in_set"] == in_set,
        format!("{name}: the result's cell is {row}"),
    )?;
    Ok(())
}

/// The library request a frame's Select step sent, without its envelope's identity.
fn library(frame: &Frame) -> &Value {
    &select(frame)["library"]
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("hdr-merge has two launches, not {}", launches.len()).into());
    };
    let expected = read_json(&recorded(run, &run.out().join(EXPECTED)))?;
    let after_first = read_json(&recorded(run, &run.out().join(AFTER_FIRST)))?;
    let after = read_json(&recorded(run, &run.out().join(AFTER)))?;
    let mut checks = Checks::new();
    let plan = &expected["plan"];
    ensure(
        plan["suitable"] == true && plan["inputs"].as_array().map(Vec::len) == Some(FRAMES),
        format!("The core plans the still bracket as {plan}"),
    )?;

    // The bracket: its header offers both merges beside Pick all.
    let still = first.at("still")?;
    let actions = &select(still)["headers"]["actions"];
    ensure(
        actions
            == &json!([[
                0,
                [format!("Pick all {FRAMES}"), "Import merged", "Pick merged"]
            ]])
            && select(still)["count"] == FRAMES
            && stacks(still).is_empty(),
        format!("still: the header offers {actions}"),
    )?;

    // Pick merged opens the preview sheet with Pick merged primary: the plan, then the preview,
    // its picture decoded and its status line the preview's own.
    let previewing = first.at("previewing")?;
    let sheet = &select(previewing)["merge"]["sheet"];
    let methods = |sheet: &Value| -> Vec<String> {
        sheet["sent"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|sent| sent["method"].as_str().map(str::to_owned))
            .collect()
    };
    ensure(
        sheet["title"] == "Merge HDR \u{b7} 3 exposures"
            && sheet["primary"] == "Pick merged"
            && sheet["preview_id"].is_string()
            && sheet["picture"].is_string()
            && sheet["picture_shown"] == sheet["picture"]
            && sheet["current"] == true
            && sheet["refusal"].is_null()
            && sheet["enabled"] == true
            && sheet["status"]
                .as_str()
                .is_some_and(|status| status.starts_with("Preview from "))
            && methods(sheet) == ["merge.plan", "merge.preview"]
            && select(previewing)["merge"]["pending"].is_null(),
        format!("previewing: the sheet is {sheet}"),
    )?;
    let preview_id = sheet["preview_id"].clone();
    // Deghosting Off: the whole options sent to the same preview, drawn as a newer revision.
    let deghosted = first.at("deghosted")?;
    let changed = &select(deghosted)["merge"]["sheet"];
    let update = changed["sent"]
        .as_array()
        .and_then(|sent| sent.last())
        .cloned()
        .unwrap_or_default();
    ensure(
        changed["deghost"] == "Off"
            && changed["preview_id"] == preview_id
            && changed["current"] == true
            && changed["picture_shown"] == changed["picture"]
            && changed["revision"].as_u64() > sheet["revision"].as_u64()
            && update
                == json!({"method": "merge.preview-update",
                    "params": {"preview_id": preview_id, "hdr": {"deghost": "off"}}}),
        format!("deghosted: the sheet is {changed}"),
    )?;
    let overlaid = first.at("overlaid")?;
    let shown = &select(overlaid)["merge"]["sheet"];
    ensure(
        shown["overlay"] == true && methods(shown).len() == methods(changed).len(),
        format!("overlaid: the sheet is {shown}"),
    )?;
    checks.note(
        overlaid,
        "previewing: the sheet draws the preview and redraws it for a changed setting",
        json!({"opened": sheet, "deghosted": changed, "overlaid": shown}),
    );

    // The sheet's Pick merged: one pending row with the job's progress, nothing hidden yet.
    let merging = first.at("merging")?;
    let pending = &select(merging)["merge"]["pending"];
    let job = pending["job_id"]
        .as_str()
        .ok_or_else(|| format!("merging: no pending merge: {}", select(merging)["merge"]))?
        .to_owned();
    ensure(
        pending["mode"] == "Pick merged"
            && pending["frames"] == FRAMES
            && pending["message"].is_string()
            && pending["failed"].is_null()
            && select(merging)["headers"]["pending"]
                == json!([{"title": "Merging HDR \u{b7} 3 exposures", "detail": pending["message"],
                    "action": "Cancel", "failed": false}])
            && select(merging)["count"] == FRAMES,
        format!(
            "merging: the pending row is {}",
            select(merging)["headers"]["pending"]
        ),
    )?;
    let sent = &merging["step"]["merge_sent"];
    let create = sent
        .as_array()
        .and_then(|sent| {
            sent.iter()
                .find(|request| request["method"] == "merge.create")
        })
        .ok_or_else(|| format!("merging: no merge.create was sent: {sent}"))?;
    ensure(
        create["params"]["kind"] == "hdr"
            && create["params"]["add_to_development_set"] == true
            && create["params"]["mutation"]["actor"] == "desktop"
            && create["params"]["preview_id"] == preview_id
            && create["params"].get("plan_id").is_none()
            && create["params"]["hdr"] == json!({"deghost": "off"})
            && select(merging)["merge"]["sheet"].is_null(),
        format!("merging: Pick merged sent {create}"),
    )?;
    checks.note(
        merging,
        "merging: Pick merged's pending row shows the job's progress",
        json!({"pending": pending, "record": merging["step"]["merge_record"]}),
    );

    // The result: one collapsed HDR tile, selected, active and in the Development set.
    let merged = first.at("merged")?;
    let answer = merge_answer(merged);
    let result = answer["result_asset_id"].clone();
    ensure(
        merged["step"]["merge_record"]["job_id"] == job.as_str()
            && answer["merged"] == true
            && answer["reused"] == false
            && answer["inputs"] == FRAMES
            && result.is_string(),
        format!(
            "merged: the job answered {}",
            merged["step"]["merge_record"]
        ),
    )?;
    collapsed_leader(merged, "merged", &result, true)?;
    let info = &select(merged)["info"];
    ensure(
        info["kind"] == "one"
            && info["stack"]["title"] == "HDR \u{b7} 3 exposures"
            && info["stack"]["in_set"] == true
            && select(merged)["selection"]["active"] == 0,
        format!("merged: the Info panel shows {info}"),
    )?;
    ensure(
        owner_set(merged)? == vec![result.clone()],
        format!("merged: the owner's set is {}", merged["step"]["owner_set"]),
    )?;
    // The board keeps finished work as recent only past its threshold and only the newest
    // sixteen, so a quick merge may be gone already; no other merge may be there.
    ensure(
        merge_jobs(merged).is_subset(&BTreeSet::from([job.clone()])),
        format!("merged: the board lists merges {:?}", merge_jobs(merged)),
    )?;

    // Expanded: the result, then its originals in capture order with their EV offsets.
    let expanded = first.at("expanded")?;
    let rows = stacks(expanded);
    // Each original's footer words, and its offset within a hundredth of a stop of the bracket's.
    let words: Vec<Value> = rows
        .iter()
        .skip(1)
        .map(|row| row["exposure"].clone())
        .collect();
    let offsets = rows
        .iter()
        .skip(1)
        .zip([0.0, -2.0, 2.0])
        .all(|(row, offset)| {
            row["offset_ev"]
                .as_f64()
                .is_some_and(|ev| (ev - offset).abs() < 0.01)
        });
    ensure(
        rows.len() == FRAMES + 1
            && rows[0]["role"] == "leader"
            && rows[0]["item"]["asset_id"] == result
            && rows[0]["expansion"] == "expanded"
            && rows[0]["badge"] == json!(["HDR \u{b7} 3 exposures", "HDR \u{b7} 3", true])
            && rows.iter().skip(1).all(|row| row["role"] == "original")
            && words == vec![json!("0 EV"), json!("\u{2212}2 EV"), json!("+2 EV")]
            && offsets,
        format!("expanded: the stack shows {rows:?}"),
    )?;
    ensure(
        select(expanded)["merge"]["stack"]["expansion"] == "expanded",
        format!("expanded: S sent {}", select(expanded)["merge"]["stack"]),
    )?;

    // An original picked: an ordinary file pick, the result's membership unchanged.
    let picked = first.at("picked")?;
    ensure(
        library(picked)["method"] == "pick.set"
            && library(picked)["params"]["picked"] == true
            && library(picked)["error"].is_null(),
        format!("picked: P sent {}", library(picked)),
    )?;
    let original = stacks(picked)
        .into_iter()
        .find(|row| row["position"] == 1)
        .ok_or("picked: no stacked row at position 1")?;
    ensure(
        original["role"] == "original" && original["picked"] == true,
        format!("picked: the original is {original}"),
    )?;
    ensure(
        owner_set(picked)? == vec![result.clone()],
        format!("picked: the owner's set is {}", picked["step"]["owner_set"]),
    )?;

    // Develop: the result from its saved source, then one Exposure edit.
    let opened = first.at("develop")?;
    ensure(
        develop(opened)["document"]["asset_id"] == result
            && develop(opened)["document"]["source"] == "derived"
            && develop(opened)["document"]["title"] == answer["file_name"]
            && select(opened)["shown"] == "develop",
        format!("develop: Develop shows {}", develop(opened)["document"]),
    )?;
    let edited = first.at("edited")?;
    let before = crate::filmstrip_smoke::drawn_luminance(opened)?;
    let lifted = crate::filmstrip_smoke::drawn_luminance(edited)?;
    checks.compare(
        edited,
        "edited: one stop of Exposure lifts the result's middle",
        lifted,
        before,
        Tolerance::Above(EDIT_LIFT),
    )?;
    let inspected = &first.at("inspected")?["step"]["result"];
    ensure(
        inspected["result_asset_id"] == result
            && inspected["state"] == "active"
            && inspected["source"]["status"] == "available"
            && inspected["inputs"].as_array().is_some_and(|inputs| {
                inputs.len() == FRAMES && inputs.iter().all(|input| input["file"] == "available")
            }),
        format!("inspected: merge.inspect answered {inspected}"),
    )?;

    // Remove merge, then its undo, redo and undo again: the originals and the result in turn, the
    // set following, and no merge run again.
    let clicked = first.at("result")?;
    ensure(
        select(clicked)["selection"]["active"] == 0
            && select(clicked)["info"]["stack"]["leader"] == true,
        format!("result: the active item is {}", select(clicked)["info"]),
    )?;
    let removed = first.at("removed")?;
    ensure(
        library(removed)["method"] == "merge.remove"
            && library(removed)["error"].is_null()
            && stacks(removed).is_empty()
            && select(removed)["count"] == FRAMES,
        format!(
            "removed: ⌫ sent {}, the view holds {} items",
            library(removed),
            select(removed)["count"]
        ),
    )?;
    ensure(
        owner_set(removed)?.is_empty(),
        format!(
            "removed: the owner's set is {}",
            removed["step"]["owner_set"]
        ),
    )?;
    for (name, shown) in [("restored", true), ("redone", false), ("undone", true)] {
        let frame = first.at(name)?;
        let sent = &library(frame)["method"];
        ensure(
            sent == if shown {
                "library.undo"
            } else {
                "library.redo"
            } && library(frame)["error"].is_null(),
            format!("{name}: sent {}", library(frame)),
        )?;
        if shown {
            let rows = stacks(frame);
            let leader = rows
                .iter()
                .find(|row| row["role"] == "leader")
                .ok_or_else(|| format!("{name}: no result is shown: {rows:?}"))?;
            ensure(
                leader["item"]["asset_id"] == result && leader["in_set"] == true,
                format!("{name}: the result's cell is {leader}"),
            )?;
            ensure(
                owner_set(frame)? == vec![result.clone()],
                format!("{name}: the owner's set is {}", frame["step"]["owner_set"]),
            )?;
        } else {
            ensure(
                stacks(frame).is_empty() && owner_set(frame)?.is_empty(),
                format!("{name}: the result is still shown or in the set"),
            )?;
        }
        ensure(
            merge_jobs(frame).is_subset(&BTreeSet::from([job.clone()])),
            format!("{name}: the board lists merges {:?}", merge_jobs(frame)),
        )?;
    }

    // The handheld bracket's preview: Deghosting back to Auto, the moving ball's regions drawn
    // from the revision's overlay, then Cancel, which closes the preview and merges nothing.
    let handheld = first.at("handheld_overlay")?;
    let sheet = &select(handheld)["merge"]["sheet"];
    let regions = sheet["regions"].as_array().cloned().unwrap_or_default();
    ensure(
        sheet["primary"] == "Import merged"
            && sheet["deghost"] == "Auto"
            && sheet["overlay"] == true
            && sheet["current"] == true
            && !regions.is_empty()
            && regions.iter().all(|label| {
                label
                    .as_str()
                    .is_some_and(|label| label.starts_with("From "))
            }),
        format!("handheld_overlay: the sheet is {sheet}"),
    )?;
    let cancelled = first.at("handheld_cancelled")?;
    ensure(
        select(cancelled)["merge"]["sheet"].is_null()
            && select(cancelled)["merge"]["closed"] == json!({"preview_id": sheet["preview_id"]})
            && select(cancelled)["merge"]["pending"].is_null(),
        format!(
            "handheld_cancelled: the merge state is {}",
            select(cancelled)["merge"]
        ),
    )?;
    checks.note(
        handheld,
        "handheld_overlay: the deghost overlay labels the moving regions",
        json!({"regions": regions}),
    );

    // Import merged, ⌥-clicked: a second result at once with the last settings, in the catalog
    // and not in the set.
    let importing = first.at("importing")?;
    let import_job = select(importing)["merge"]["pending"]["job_id"]
        .as_str()
        .ok_or("importing: no pending merge")?
        .to_owned();
    let import_create = importing["step"]["merge_sent"]
        .as_array()
        .and_then(|sent| {
            sent.iter()
                .find(|request| request["method"] == "merge.create")
        })
        .ok_or("importing: no merge.create was sent")?;
    ensure(
        import_create["params"]
            .get("add_to_development_set")
            .is_none()
            && import_create["params"]["plan_id"].is_string()
            && import_create["params"].get("preview_id").is_none()
            && import_create["params"]["hdr"] == json!({"deghost": "off"})
            && select(importing)["merge"]["sheet"].is_null(),
        format!("importing: Import merged sent {import_create}"),
    )?;
    let imported = first.at("imported")?;
    let import_answer = merge_answer(imported);
    let second_result = import_answer["result_asset_id"].clone();
    ensure(
        import_answer["merged"] == true
            && import_answer["reused"] == false
            && second_result != result,
        format!("imported: the job answered {import_answer}"),
    )?;
    collapsed_leader(imported, "imported", &second_result, false)?;
    ensure(
        owner_set(imported)? == vec![result.clone()],
        format!(
            "imported: the owner's set is {}",
            imported["step"]["owner_set"]
        ),
    )?;
    let jobs: BTreeSet<String> = first
        .names()
        .iter()
        .map(|name| first.at(name).map(merge_jobs))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    ensure(
        jobs == BTreeSet::from([job.clone(), import_job.clone()]),
        format!("The board listed merges {jobs:?}, the desktop started {job} and {import_job}"),
    )?;

    // The catalog the first launch left: both merges active, the result's edit in its history.
    let merges = after_first["merges"].as_array().ok_or("no merges read")?;
    ensure(
        merges.len() == 2
            && merges.iter().all(|merge| {
                merge["state"] == "active" && merge["source"]["status"] == "available"
            }),
        format!("The first launch left the merges {merges:?}"),
    )?;
    let history = after_first["history"].as_array().ok_or("no history read")?;
    ensure(
        history.len() >= 2,
        format!("The result's history holds {history:?}"),
    )?;
    let creates = after_first["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|change| change["method"] == "merge.create")
        .count();
    ensure(
        creates == 2,
        format!(
            "The journal records {creates} merges: {}",
            after_first["changes"]
        ),
    )?;

    // Reopened with the brackets offline: the result renders from its saved source with its edit.
    let reopened = second.at("reopened")?;
    ensure(
        develop(reopened)["document"]["asset_id"] == result
            && develop(reopened)["document"]["title"] == answer["file_name"]
            && !reopened.state()["status_bar"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("derived:"),
        format!("reopened: Develop shows {}", develop(reopened)["document"]),
    )?;
    checks.compare(
        reopened,
        "reopened: the result's middle, inputs offline, against the edited one's",
        crate::filmstrip_smoke::drawn_luminance(reopened)?,
        lifted,
        Tolerance::Within(REOPEN_TOLERANCE),
    )?;
    let offline = &second.at("inspected")?["step"]["result"];
    ensure(
        offline["result_asset_id"] == result
            && offline["state"] == "active"
            && offline["source"]["status"] == "available"
            && offline["inputs"]
                .as_array()
                .is_some_and(|inputs| inputs.iter().all(|input| input["file"] == "missing")),
        format!("inspected: with the brackets offline merge.inspect answered {offline}"),
    )?;
    // Named back: the same two merges, nothing merged again.
    let merges = after["merges"].as_array().ok_or("no merges read")?;
    ensure(
        merges.len() == 2 && merges.iter().all(|merge| merge["state"] == "active"),
        format!("After the second launch the merges are {merges:?}"),
    )?;
    ensure(
        after["changes"].as_array().map(Vec::len)
            == after_first["changes"].as_array().map(Vec::len),
        format!(
            "The second launch changed the library: {}",
            after["changes"]
        ),
    )?;
    checks.write(
        &first.evidence,
        "hdr-merge",
        json!({"result": result, "imported": second_result, "jobs": jobs,
            "tolerance": {"reopen": REOPEN_TOLERANCE, "lift": EDIT_LIFT}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hdr_merge_plans_are_well_formed() {
        let expected = json!({"brackets": {
            STILL: {"folder": "/tmp/Brackets/Still"},
            HANDHELD: {"folder": "/tmp/Brackets/Handheld"},
        }});
        let first = first_plan(&expected).unwrap();
        first.validate().unwrap();
        assert!(first.scripted());
        assert_eq!(first.len(), 20);
        let second =
            second_plan(&json!({"reopen": {"folder": "Jun 2026", "position": 0}})).unwrap();
        second.validate().unwrap();
        assert_eq!(second.len(), 6);
        // Registered, opening nothing a checkout lacks, so `verify --tier rendered` runs it.
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert!(row.rendered() && !row.takes_source());
    }
}
