//! The `select` smoke scenario: the Select workspace's shell over a generated catalog, in the real
//! editor at 1440 × 900.
//!
//! The run generates its catalog and index first (`generate-catalog --files 2000 --assets 3000
//! --images 120`, seed 1) into `generated/`: an index of files with no image data behind them,
//! and a folder of real JPEGs. The desktop draws no previews yet, so every cell is the placeholder
//! at its photograph's shape. The run asks the core for its own answers over a pristine copy
//! before the editor touches it: the event list, the event "Konstanz · 12–13 Sep" viewed as the
//! desktop first shows it and grouped by Day, the rows of both, and the folder of real images read
//! and viewed. The editor then opens that catalog with nothing
//! open. Its frames, in [`plan`] order: Develop with nothing open; `G` showing Select, its events
//! listed; the event opened from the sources panel (the grouped grid, the Info panel, the status
//! line); the first cell made active with `→`, then the next, then the selection extended with
//! Shift+`→`; the Group chip set to Day; an agent's `pick.set` through a second client, which the
//! desktop reads through its own event sync and answers by evaluating its view again; a folder of
//! real generated JPEGs (`--images 120`, with EXIF and thumbnails and their ground truth in
//! `images/manifest.json`) browsed on disk, which the index lane reads before it is viewed; and
//! back to Develop through the switch. Between the agent's pick and that folder, long-running work:
//! a first look at a folder of 16,000 one-byte `.jpg` files the run writes beside `generated/`
//! (`first-look/`), captured while the index lane still reads it with the view's progress sheet;
//! Continue in background, captured with the sheet gone and the job in the status bar and the
//! Performance section; and Cancel on its Performance row, captured once the job has ended
//! cancelled on the activity board and the section has read it again.
//!
//! Each frame's `select` block is checked against the core's own answers: the view's size, picks
//! and group layout as `browse.view` answered the runner's client, and the selection as the owner
//! answered the desktop's own `session.state` when the step settled. After the run the runner reads
//! the catalog the editor left, through its own client again, for the agent's pick and the view it
//! made. Everything compared is written to `app/select-checks.json`; the core's answers from before
//! the run are kept in `select-expected.json`, so a replay checks the same frames against them.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{self as script, ArrowKey, SelectMenu, SelectStep, SelectWorkspace};

pub const SCENARIO: &str = "select";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates its catalog and index into `generated/` with \
    `cargo xtask generate-catalog --files 2000 --assets 3000 --images 120 --seed 1`, asks the core for the \
    answers the frames are checked against over a pristine copy (`select-expected.json`), writes \
    16,000 one-byte `.jpg` files into `first-look/` for the first look it cancels, and launches \
    the editor over that catalog with `--catalog`.";
/// Where the run writes its catalog and index.
pub const GENERATED: &str = "generated";
/// The core's answers from before the run.
pub const EXPECTED: &str = "select-expected.json";
const SEED: u64 = 1;
const FILES: u32 = 2000;
const ASSETS: u32 = 3000;
/// Real generated JPEGs, with EXIF and embedded thumbnails, and their ground truth in
/// `images/manifest.json`: the folder browsed on disk.
const IMAGES: u32 = 120;
/// The event the run opens, as `event.list` names it, and as the sources panel and the title bar
/// label it.
const EVENT: &str = "Konstanz \u{b7} 12\u{2013}13 Sep";
const EVENT_LABEL: &str = "Konstanz";
/// The actor the evidence driver's second client picks as.
const AGENT: &str = "evidence-agent";
/// The folder whose first look is long-running work, written into the run's output directory.
const FIRST_LOOK: &str = "first-look";
/// Its files, in folders of [`FIRST_LOOK_FOLDER`]: enough that the index lane takes several seconds
/// to list and read them on the owner's M4, so the sheet, Continue in background and Cancel are
/// each captured while the job runs.
const FIRST_LOOK_FILES: u32 = 16_000;
const FIRST_LOOK_FOLDER: u32 = 250;

/// Every frame, in order. The agent picks `pick`, a file of the Day-grouped view the core said
/// was not picked, on the first screen.
pub fn plan(pick: u32, count: u64, folder: &str, images: u64, first_look: &str) -> Plan {
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let arrow = |direction, extend| SelectStep::Arrow { direction, extend };
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        select("event", SelectStep::Source(EVENT.into()))
            .status(format!("{EVENT_LABEL} \u{b7} {count} in view")),
        select("right", arrow(ArrowKey::Right, false)),
        select("right-again", arrow(ArrowKey::Right, false)),
        select("extended", arrow(ArrowKey::Right, true)),
        select(
            "grouped",
            SelectStep::Choose {
                menu: SelectMenu::Group,
                item: "Day".into(),
            },
        ),
        select(
            "picked",
            SelectStep::AgentPick {
                positions: vec![pick],
                picked: true,
            },
        )
        .status(format!(
            "{EVENT_LABEL} changed elsewhere and was read again \u{b7} {count} in view"
        )),
        select("first-look", SelectStep::FirstLook(first_look.into())),
        select("background", SelectStep::ContinueInBackground),
        select("cancelled", SelectStep::CancelWork).status_starts("Cancelled reading "),
        select("folder", SelectStep::Folder(folder.into()))
            .status(format!("images \u{b7} {images} in view")),
        select("develop", SelectStep::Switch(SelectWorkspace::Develop)),
    ])
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("select-smoke-{method}"),
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

/// The first look's folder: [`FIRST_LOOK_FILES`] one-byte `.jpg` files, which the index lane lists
/// and reads a header from — unreadable, which is quick and needs no image data on disk.
fn write_first_look(dir: &Path) -> Result {
    let _ = fs::remove_dir_all(dir);
    for index in 0..FIRST_LOOK_FILES {
        let folder = dir.join(format!("{:03}", index / FIRST_LOOK_FOLDER));
        if index % FIRST_LOOK_FOLDER == 0 {
            fs::create_dir_all(&folder)?;
        }
        fs::write(folder.join(format!("IMG_{index:05}.jpg")), b"x")?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// `body` over a scratch copy of the catalog in `generated`, through an owner of its own and a
/// client of the runner's, so the run's own catalog is only ever the editor's.
fn over_copy<T>(
    generated: &Path,
    name: &str,
    body: impl FnOnce(&OwnerHandle, ClientId) -> Result<T>,
) -> Result<T> {
    let scratch =
        std::env::temp_dir().join(format!("luxforge-select-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    copy_dir(generated, &scratch)?;
    let (owner, join) = OwnerHandle::start(&scratch.join(generate_catalog::CATALOG))
        .map_err(|error| format!("the core cannot open the generated catalog: {error}"))?;
    let client = owner.register();
    let outcome = body(&owner, client);
    owner.stop();
    let _ = join.join();
    let _ = fs::remove_dir_all(&scratch);
    outcome
}

/// A summary's size, picks and group layout, as the frames record them.
fn view_record(summary: &Value) -> Value {
    let groups = &summary["groups"];
    let len = |key: &str| groups[key].as_array().map_or(0, Vec::len);
    json!({
        "count": summary["count"],
        "picked": summary["picked"],
        "groups": {"days": len("days"), "cameras": len("cameras"), "moments": len("moments")},
    })
}

/// The core's answers before the run: the events, the event as the desktop first views it and
/// grouped by Day, where the items the arrows select move to under Day, the file the agent picks,
/// and the folder of real images as the index lane reads it and the view it gives, beside the
/// moments its manifest records.
fn expect(generated: &Path) -> Result<Value> {
    let images = generated.join("images");
    let manifest = read_json(&images.join("manifest.json"))?;
    let files = manifest["files"]
        .as_array()
        .ok_or("The image manifest lists no files")?;
    let moments: std::collections::BTreeSet<&str> = files
        .iter()
        .filter(|file| file["kind"] != "single")
        .filter_map(|file| file["moment"].as_str())
        .collect();
    over_copy(generated, "expected", |owner, client| {
        let list = ask(owner, client, "event.list", json!({}))?;
        let events = list["events"]
            .as_array()
            .ok_or("event.list lists no events")?;
        let event = events
            .iter()
            .find(|event| event["name"] == EVENT)
            .ok_or_else(|| format!("the generated catalog has no event {EVENT}"))?;
        let source = json!({"kind": "event", "event_id": event["id"]});
        let first = ask(owner, client, "browse.view", json!({"source": source}))?;
        let first_rows = ask(
            owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 200}),
        )?;
        let day = ask(
            owner,
            client,
            "browse.view",
            json!({"source": source, "grouping": "day"}),
        )?;
        let day_rows = ask(
            owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 200}),
        )?;
        let rows = |rows: &Value| rows["rows"].as_array().cloned().unwrap_or_default();
        let (first_rows, day_rows) = (rows(&first_rows), rows(&day_rows));
        // The arrows select positions 1 and 2 of the first view; the owner carries the selection
        // over by item, so under Day they are wherever those files sort.
        let moved: Vec<u64> = [1, 2]
            .iter()
            .map(|at: &usize| {
                let file = &first_rows[*at]["file_id"];
                day_rows
                    .iter()
                    .find(|row| &row["file_id"] == file)
                    .and_then(|row| row["position"].as_u64())
                    .ok_or("a selected file is missing from the Day view")
            })
            .collect::<std::result::Result<_, _>>()?;
        let pick = day_rows
            .iter()
            .find(|row| {
                row["picked"] == false
                    && row["item"] == "file"
                    && row["position"]
                        .as_u64()
                        .is_some_and(|at| (3..30).contains(&at))
            })
            .ok_or("the Day view's first screen has no unpicked file")?;
        let undated = events.iter().any(|event| event["undated"] == true);
        // The folder of real images, read as the desktop reads it and viewed with its subfolders.
        let started = ask(
            owner,
            client,
            "index.refresh",
            json!({"source": {"kind": "folder", "path": images}}),
        )?;
        let job = json!({"job_id": started["job_id"]});
        let read = loop {
            let read = ask(owner, client, "job.read", job.clone())?;
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                break read;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        ensure(
            read["status"] == "ready",
            format!("the core could not read the images: {read}"),
        )?;
        let listed = read["result"]["roots"][0].clone();
        let folder = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "folder", "path": listed, "subfolders": true}}),
        )?;
        Ok(json!({
            "folder": {
                "path": images,
                "listed": listed,
                "view": view_record(&folder),
                "manifest": {"images": files.len(), "moments": moments.len()},
            },
            "event": {"id": event["id"], "name": event["name"], "count": event["count"]},
            "events": events.len(),
            "months": list["months"].as_array().map_or(0, Vec::len) + usize::from(undated),
            "first": view_record(&first),
            "day": view_record(&day),
            "day_selection": moved,
            "pick": {
                "position": pick["position"],
                "file_id": pick["file_id"],
                "path": pick["path"],
            },
        }))
    })
}

/// Generate the catalog and the core's answers (a replay reads the recorded ones), plan the
/// launch from them, launch the editor over the catalog and check it.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let expected_file = run.out().join(EXPECTED);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        if !run.replaying() {
            generate_catalog::run(
                &generated,
                &generate_catalog::Options {
                    seed: SEED,
                    files: Some(FILES),
                    assets: Some(ASSETS),
                    images: Some(IMAGES),
                },
            )?;
            let mut expected = expect(&generated)?;
            let first_look = run.out().join(FIRST_LOOK);
            write_first_look(&first_look)?;
            expected["first_look"] = json!({"path": first_look, "files": FIRST_LOOK_FILES});
            write_json(&expected_file, &expected)?;
        }
        let expected = read_json(&expected_file)?;
        let pick = expected["pick"]["position"]
            .as_u64()
            .ok_or("The expected answers name no file to pick")? as u32;
        let count = expected["first"]["count"]
            .as_u64()
            .ok_or("The expected answers hold no view")?;
        let folder = expected["folder"]["path"]
            .as_str()
            .ok_or("The expected answers name no folder")?;
        let images = expected["folder"]["view"]["count"]
            .as_u64()
            .ok_or("The expected answers hold no folder view")?;
        let first_look = expected["first_look"]["path"]
            .as_str()
            .ok_or("The expected answers name no first-look folder")?;
        let plan = plan(pick, count, folder, images, first_look);
        let mut launch = Launch::app()
            .catalog(&generated.join(generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()?;
        run.record("backend", checked.at("event")?.state()["backend"].clone());
        Ok(())
    })
}

fn select(frame: &Frame) -> &Value {
    &frame.state()["select"]
}

/// The frame's selection is the one the owner answered the desktop's own `session.state` with when
/// the step settled.
fn selection_is_the_owners(frame: &Frame, name: &str) -> Result<Value> {
    let shown = &select(frame)["selection"];
    let owner = &frame["step"]["owner_browse"];
    ensure(
        owner.is_object(),
        format!(
            "{name}: the step recorded no owner session: {}",
            frame["step"]
        ),
    )?;
    ensure(
        &owner["selection"] == shown,
        format!(
            "{name}: the grid draws {shown}, the owner holds {}",
            owner["selection"]
        ),
    )?;
    ensure(
        owner["revision"] == select(frame)["revision"],
        format!(
            "{name}: the session's view is revision {}, the grid's {}",
            owner["revision"],
            select(frame)["revision"]
        ),
    )?;
    Ok(json!({"selection": shown, "owner": owner}))
}

/// A frame's view against the core's own answer for it.
fn view_is(frame: &Frame, name: &str, expected: &Value) -> Result<Value> {
    let shown = select(frame);
    let recorded = json!({
        "count": shown["count"],
        "picked": shown["picked"],
        "groups": shown["groups"],
    });
    ensure(
        &recorded == expected,
        format!("{name}: the desktop shows {recorded}, the core answered {expected}"),
    )?;
    // The grid laid out exactly the owner's layout: a heading per day and camera group, a frame
    // per moment, every item one cell.
    let blocks = &shown["blocks"];
    for key in ["days", "cameras", "moments"] {
        ensure(
            blocks[key] == expected["groups"][key],
            format!(
                "{name}: the grid drew {} {key}, the layout has {}",
                blocks[key], expected["groups"][key]
            ),
        )?;
    }
    ensure(
        shown["items"] == expected["count"] && shown["cells"] == expected["count"],
        format!("{name}: the grid does not hold every item once: {shown}"),
    )?;
    ensure(
        shown["quiet"] == true && shown["stale"] == false && shown["loading"] == false,
        format!("{name}: the view was captured unsettled: {shown}"),
    )?;
    ensure(
        shown["rows"].as_u64().is_some_and(|rows| rows > 0),
        format!("{name}: no row was read for the visible window"),
    )?;
    Ok(recorded)
}

/// The Select centre shows its grid: the region between the panels holds the placeholders' and the
/// headings' colours, not one flat canvas.
fn grid_drawn(frame: &Frame) -> Result<Value> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let scale = frame["scale"].as_f64().unwrap_or(1.0);
    let at = |points: f64| (points * scale).round() as u32;
    let (left, right) = (at(241.0) + 8, width.saturating_sub(at(301.0) + 8));
    let (top, bottom) = (at(84.0) + 8, height.saturating_sub(at(90.0)));
    ensure(
        left < right && top < bottom,
        "The capture is too small for the Select layout",
    )?;
    let mut colours = std::collections::BTreeMap::<[u8; 3], u32>::new();
    let mut samples = 0u32;
    for y in (top..bottom).step_by(6) {
        for x in (left..right).step_by(6) {
            *colours.entry(image.get_pixel(x, y).0).or_default() += 1;
            samples += 1;
        }
    }
    let most = colours.values().copied().max().unwrap_or(0);
    ensure(
        colours.len() >= 4 && most < samples * 9 / 10,
        format!(
            "The Select grid in {} is blank: {} colours, {most} of {samples} samples one colour",
            frame.path()?.display(),
            colours.len()
        ),
    )?;
    Ok(
        json!({"sampled_bounds": [left, top, right, bottom], "distinct_colours": colours.len(),
        "most_common_samples": most, "samples": samples}),
    )
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let mut checks = Checks::new();

    let opened = launch.at("opened")?;
    ensure(
        select(opened)["shown"] == "develop",
        "The editor did not open in Develop",
    )?;

    // `G`: Select, its events listed by month.
    let shown = launch.at("select")?;
    let block = select(shown);
    ensure(block["shown"] == "select", "G did not show Select")?;
    ensure(
        block["events"] == expected["events"],
        format!(
            "Select lists {} events, the core {}",
            block["events"], expected["events"]
        ),
    )?;
    let months = block["listed"].as_array().ok_or("No sources recorded")?;
    ensure(
        months.len() as u64 == expected["months"].as_u64().unwrap_or(0),
        format!("The sources panel shows {} month headings", months.len()),
    )?;
    ensure(
        months.iter().any(|month| {
            month["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|event| event == EVENT_LABEL))
        }),
        format!("The sources panel lists no {EVENT_LABEL}"),
    )?;
    checks.note(
        shown,
        "Select with the events listed",
        json!({"sources": months}),
    );

    // The event opened: the grouped grid, the title bar, the status line and the Info panel.
    let event = launch.at("event")?;
    let block = select(event);
    ensure(
        block["source"]["kind"] == "event"
            && block["source"]["event_id"] == expected["event"]["id"],
        format!("The view is of {}, not the event", block["source"]),
    )?;
    let first = view_is(event, "event", &expected["first"])?;
    ensure(
        block["grouping"] == "day-camera-moment" && block["title"]["name"] == EVENT_LABEL,
        format!("The event's title or grouping: {block}"),
    )?;
    let picked = expected["first"]["picked"].as_u64().unwrap_or(0);
    ensure(
        block["title"]["picks"] == picked,
        "Develop N does not count the view's picks",
    )?;
    let line = format!(
        "Camera previews \u{b7} auto-organized \u{b7} {} in view \u{b7} {picked} picked",
        expected["first"]["count"]
    );
    ensure(
        block["status_line"] == line.as_str(),
        format!("The status line reads {}", block["status_line"]),
    )?;
    ensure(
        block["info"]["kind"] == "nothing",
        "The Info panel describes an item before one is selected",
    )?;
    checks.note(
        event,
        "the event's grouped grid, as the core laid it out",
        json!({"view": first, "grid": grid_drawn(event)?, "owner": event["step"]["owner_browse"]}),
    );

    // The arrows: the first cell active, the next, then the selection extended with Shift.
    for (name, selection) in [
        (
            "right",
            json!({"count": 1, "ranges": [{"start": 0, "len": 1}], "active": 0}),
        ),
        (
            "right-again",
            json!({"count": 1, "ranges": [{"start": 1, "len": 1}], "active": 1}),
        ),
        (
            "extended",
            json!({"count": 2, "ranges": [{"start": 1, "len": 2}], "active": 2}),
        ),
    ] {
        let frame = launch.at(name)?;
        let recorded = selection_is_the_owners(frame, name)?;
        ensure(
            select(frame)["selection"] == selection,
            format!(
                "{name}: the selection is {}, expected {selection}",
                select(frame)["selection"]
            ),
        )?;
        ensure(
            select(frame)["revision"] == block["revision"],
            format!("{name}: selecting evaluated the view again"),
        )?;
        checks.note(frame, "the session's selection, drawn", recorded);
    }
    let extended = launch.at("extended")?;
    ensure(
        select(extended)["info"]["kind"] == "several"
            && select(extended)["info"]["count"] == "2 selected",
        format!(
            "The Info panel for two selected: {}",
            select(extended)["info"]
        ),
    )?;

    // The Group chip: the query changed its grouping and nothing else, and the owner carried the
    // selection over by item.
    let grouped = launch.at("grouped")?;
    let block = select(grouped);
    let before = select(extended);
    for key in ["source", "filter", "sort"] {
        ensure(
            block[key] == before[key],
            format!(
                "The Group chip changed the query's {key}: {} to {}",
                before[key], block[key]
            ),
        )?;
    }
    ensure(
        block["grouping"] == "day",
        format!("The Group chip left the grouping {}", block["grouping"]),
    )?;
    let day = view_is(grouped, "grouped", &expected["day"])?;
    ensure(
        block["revision"].as_u64() > before["revision"].as_u64(),
        "The Group chip did not evaluate the view again",
    )?;
    let mut positions: Vec<u64> = expected["day_selection"]
        .as_array()
        .ok_or("No expected Day selection")?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    positions.sort_unstable();
    let selected: Vec<u64> = block["selection"]["ranges"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|range| {
            let start = range["start"].as_u64().unwrap_or(0);
            start..start + range["len"].as_u64().unwrap_or(0)
        })
        .collect();
    ensure(
        selected == positions,
        format!(
            "Under Day the selection is {selected:?}, the core's order puts it at {positions:?}"
        ),
    )?;
    let recorded = selection_is_the_owners(grouped, "grouped")?;
    checks.note(
        grouped,
        "the Day grouping, evaluated again with the selection carried over",
        json!({"view": day, "selection": recorded}),
    );

    // An agent's pick: the desktop learned of it through its event sync alone and evaluated the
    // view again, with one more pick, the selection kept.
    let picked_frame = launch.at("picked")?;
    let block = select(picked_frame);
    let step = &picked_frame["step"];
    ensure(
        step["file_ids"] == json!([expected["pick"]["file_id"]]) && step["actor"] == AGENT,
        format!("The agent picked {} as {}", step["file_ids"], step["actor"]),
    )?;
    let day_picked = expected["day"]["picked"].as_u64().unwrap_or(0);
    ensure(
        block["picked"] == day_picked + 1,
        format!(
            "After the agent's pick the view holds {} picks",
            block["picked"]
        ),
    )?;
    ensure(
        block["revision"].as_u64() > select(grouped)["revision"].as_u64()
            && block["stale"] == false,
        "The agent's pick was not read again",
    )?;
    ensure(
        block["selection"] == select(grouped)["selection"],
        "The agent's pick changed the selection",
    )?;
    let recorded = selection_is_the_owners(picked_frame, "picked")?;
    // What the editor left in its catalog, read through the runner's own client.
    let generated = run.out().join(GENERATED);
    let after = over_copy(&generated, "after", |owner, client| {
        let picks = ask(owner, client, "pick.list", json!({}))?;
        let path = &expected["pick"]["path"];
        let pick = picks["picks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|pick| &pick["path"] == path)
            .cloned()
            .ok_or("The agent's pick is not in the catalog")?;
        ensure(
            pick["actor"] == AGENT,
            format!("The pick's actor is {}", pick["actor"]),
        )?;
        let view = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "event", "event_id": expected["event"]["id"]}, "grouping": "day"}),
        )?;
        Ok(json!({"pick": pick, "view": view_record(&view)}))
    })?;
    ensure(
        after["view"]["picked"] == block["picked"] && after["view"]["count"] == block["count"],
        format!(
            "The desktop shows {block}, the catalog it left answers {}",
            after["view"]
        ),
    )?;
    checks.note(
        picked_frame,
        "an agent's pick read again through the event sync",
        json!({"selection": recorded, "catalog": after, "status": picked_frame.state()["status"]}),
    );

    long_work(&mut checks, launch, picked_frame)?;

    // A folder of real images browsed on disk: read by the index lane, then viewed with its
    // subfolders, its days, cameras and moments the core's own and its moments the manifest's.
    let folder = launch.at("folder")?;
    let block = select(folder);
    ensure(
        block["source"]["kind"] == "folder" && block["source"]["subfolders"] == true,
        format!("The folder is viewed as {}", block["source"]),
    )?;
    let view = view_is(folder, "folder", &expected["folder"]["view"])?;
    let manifest = &expected["folder"]["manifest"];
    ensure(
        block["count"] == manifest["images"] && block["groups"]["moments"] == manifest["moments"],
        format!(
            "The folder shows {} photographs and {} moments, its manifest {} and {}",
            block["count"], block["groups"]["moments"], manifest["images"], manifest["moments"]
        ),
    )?;
    ensure(
        block["title"]["name"] == "images" && block["reading_folder"] == false,
        format!("The folder's title or reading state: {}", block["title"]),
    )?;
    checks.note(
        folder,
        "a folder of real images read and viewed",
        json!({"view": view, "manifest": manifest, "grid": grid_drawn(folder)?}),
    );

    // Back to Develop: Select keeps its view for when it is shown again.
    let develop = launch.at("develop")?;
    let block = select(develop);
    ensure(
        block["shown"] == "develop",
        "The switch did not return to Develop",
    )?;
    for key in ["revision", "count", "picked", "selection", "grouping"] {
        ensure(
            block[key] == select(folder)[key],
            format!("Switching to Develop changed Select's {key}"),
        )?;
    }
    checks.note(
        develop,
        "Develop again, Select's view kept",
        json!({"select": block}),
    );
    checks.write(&launch.evidence, "select", json!({"expected": expected}))
}

fn long_work_of(frame: &Frame) -> &Value {
    &frame.state()["long_work"]
}

/// The board entry of `job_id` among the catalog work a frame's long work lists running.
fn running_job<'a>(frame: &'a Frame, job_id: &Value) -> Option<&'a Value> {
    long_work_of(frame)["running"]
        .as_array()?
        .iter()
        .find(|entry| &entry["job_id"] == job_id)
}

/// How many distinct colours a band of the Select centre holds, sampled every 4 px: `from` and `to`
/// are fractions of the grid region's height, between the filter bar and the status bar.
fn centre_colours(frame: &Frame, from: f64, to: f64) -> Result<usize> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let scale = frame["scale"].as_f64().unwrap_or(1.0);
    let at = |points: f64| (points * scale).round() as u32;
    let (left, right) = (at(241.0) + 8, width.saturating_sub(at(301.0) + 8));
    let (top, bottom) = (at(84.0) + 8, height.saturating_sub(at(90.0)));
    let span = f64::from(bottom - top);
    let (first, last) = (top + (span * from) as u32, top + (span * to) as u32);
    let mut colours = std::collections::BTreeSet::new();
    for y in (first..last).step_by(4) {
        for x in (left..right).step_by(4) {
            colours.insert(image.get_pixel(x, y).0);
        }
    }
    Ok(colours.len())
}

/// Long-running work: the first look's progress sheet over the empty view, drawing exactly what
/// the job on the board reports; Continue in background, the job still running in the status bar
/// and the Performance section with Cancel; and the job cancelled from its row, ended cancelled on
/// the board and in the section, the view told.
fn long_work(checks: &mut Checks, launch: &Checked, before: &Frame) -> Result {
    let look = launch.at("first-look")?;
    let work = long_work_of(look);
    let sheet = &work["sheet"];
    ensure(
        sheet.is_object(),
        format!("The first look shows no progress sheet: {work}"),
    )?;
    let job_id = sheet["job_id"].clone();
    ensure(
        work["waiting"]["job_id"] == job_id
            && sheet["title"] == format!("Reading {FIRST_LOOK}").as_str()
            && select(look)["reading_folder"] == true,
        format!("The sheet is not the waiting first look's: {work}"),
    )?;
    let entry = running_job(look, &job_id)
        .ok_or_else(|| format!("The sheet's job is not running on the board: {work}"))?;
    ensure(
        entry["kind"] == "index.refresh",
        format!("The sheet follows {entry}"),
    )?;
    // Exactly what the work reports: its own count, and a fraction only when it reports one.
    let fraction = entry["progress"]["fraction"].as_f64();
    ensure(
        sheet["count"] == entry["progress"]["message"]
            && match (fraction, sheet["fraction"].as_f64()) {
                (None, None) => true,
                (Some(reported), Some(drawn)) => (reported - drawn).abs() < 1e-6,
                _ => false,
            },
        format!("The sheet shows {sheet}, the board reports {entry}"),
    )?;
    ensure(
        sheet["estimate"].is_null()
            || sheet["estimate"].as_str().is_some_and(|estimate| {
                estimate.starts_with("about ") && estimate.ends_with(" left")
            }),
        format!("The sheet's estimate reads {}", sheet["estimate"]),
    )?;
    ensure(
        work["busiest"]["jobs"]
            .as_u64()
            .is_some_and(|jobs| jobs >= 1),
        format!("The status bar shows no job during the first look: {work}"),
    )?;
    // The sheet sits over the waiting view's empty canvas, not over the grid it replaced.
    let flat = centre_colours(look, 0.05, 0.2)?;
    let grid = centre_colours(before, 0.05, 0.2)?;
    let sheet_colours = centre_colours(look, 0.45, 0.55)?;
    ensure(
        flat <= 2 && grid > flat && sheet_colours >= 4,
        format!(
            "The first look's centre: {flat} colours above the sheet (the grid before it drew {grid}), {sheet_colours} across it"
        ),
    )?;
    checks.note(
        look,
        "a first look's progress sheet, drawing the job's own count and fraction",
        json!({"sheet": sheet, "board": entry, "status_bar": work["busiest"],
            "centre_colours": {"above_sheet": flat, "grid_before": grid, "across_sheet": sheet_colours}}),
    );

    let background = launch.at("background")?;
    let work = long_work_of(background);
    ensure(
        work["sheet"].is_null() && work["background"] == job_id,
        format!("Continue in background left {work}"),
    )?;
    let entry = running_job(background, &job_id)
        .ok_or_else(|| format!("The job stopped when it went to the background: {work}"))?;
    let rows = background.state()["performance"]["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let row = rows
        .iter()
        .find(|row| row["work"]["job_id"] == job_id)
        .ok_or_else(|| format!("The Performance section lists no row to cancel: {rows:?}"))?;
    ensure(
        row["running"] == true
            && row["work"]["count"].is_string()
            && row["label"]
                .as_str()
                .is_some_and(|label| label.starts_with("Indexing ") && label.ends_with(FIRST_LOOK)),
        format!("The job's Performance row: {row}"),
    )?;
    ensure(
        work["busiest"]["jobs"]
            .as_u64()
            .is_some_and(|jobs| jobs >= 1)
            && work["timers"]["refresh"] == true,
        format!("The job left the status bar: {work}"),
    )?;
    // The view it was replacing is back, drawn in the band the sheet's empty canvas left flat.
    let returned = centre_colours(background, 0.05, 0.2)?;
    ensure(
        returned > 2,
        format!("The centre is still empty after Continue in background: {returned} colours"),
    )?;
    checks.note(
        background,
        "Continue in background: the sheet gone, the job in the status bar and the section",
        json!({"status_bar": work["busiest"], "row": row, "board": entry, "centre_colours": returned}),
    );

    let cancelled = launch.at("cancelled")?;
    let work = long_work_of(cancelled);
    ensure(
        cancelled["step"]["cancel"]["job_id"] == job_id,
        format!(
            "Cancel was pressed on {}, not the first look's row",
            cancelled["step"]["cancel"]
        ),
    )?;
    ensure(
        running_job(cancelled, &job_id).is_none()
            && work["cancels"]
                .as_array()
                .is_some_and(|cancels| cancels.contains(&job_id)),
        format!("The cancelled job still runs: {work}"),
    )?;
    let ended = work["recent"]
        .as_array()
        .and_then(|recent| recent.iter().find(|entry| entry["job_id"] == job_id))
        .ok_or_else(|| format!("The board does not list the job as ended: {work}"))?;
    ensure(
        ended["outcome"] == "cancelled",
        format!("The job ended {}", ended["outcome"]),
    )?;
    ensure(
        select(cancelled)["reading_folder"] == false && work["waiting"].is_null(),
        "The view still waits on the cancelled job",
    )?;
    ensure(
        cancelled.state()["status"]
            .as_str()
            .is_some_and(|status| status.ends_with(&format!("/{FIRST_LOOK}"))),
        format!(
            "The status bar does not say the first look was cancelled: {}",
            cancelled.state()["status"]
        ),
    )?;
    let performance = &cancelled.state()["performance"];
    let sampled = performance["activity"]["recent"]
        .as_array()
        .and_then(|recent| recent.iter().find(|entry| entry["job_id"] == job_id));
    ensure(
        sampled.is_some_and(|entry| entry["outcome"] == "cancelled"),
        format!(
            "The Performance section has not read the cancelled job: {}",
            performance["activity"]
        ),
    )?;
    let rows = performance["jobs"].as_array().cloned().unwrap_or_default();
    ensure(
        rows.iter().all(|row| row["work"]["job_id"] != job_id),
        "The cancelled job still has a Cancel",
    )?;
    let finished = rows.iter().find(|row| {
        row["running"] == false
            && row["label"]
                .as_str()
                .is_some_and(|label| label.ends_with(FIRST_LOOK))
    });
    if let Some(row) = finished {
        ensure(
            row["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("Cancelled ")),
            format!("The finished row reads {row}"),
        )?;
    }
    checks.note(
        cancelled,
        "the job cancelled from its row: ended cancelled on the board and in the section",
        json!({"board": ended, "section_row": finished, "status": cancelled.state()["status"],
            "status_bar": work["busiest"]}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_select_plan_is_well_formed() {
        let plan = plan(7, 205, "/generated/images", 120, "/run/first-look");
        plan.validate().unwrap();
        assert_eq!(plan.len(), 13);
        assert!(plan.scripted());
    }
}
