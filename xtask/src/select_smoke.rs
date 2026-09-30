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
//! desktop reads through its own event sync and answers by evaluating its view again; the grouping
//! set back to Day › Camera › Moment, its day headings and moment headers counting their picks; a
//! single file clicked and picked with `P`; a bracket picked with its header's Pick all; `Cmd+Z`
//! undoing Pick all, then the desktop's pick, then finding nothing of the desktop's left to undo
//! while the agent's pick stays; `Shift+Cmd+Z` redoing the desktop's pick; a folder of real
//! generated JPEGs (`--images 120`, with EXIF and thumbnails and their ground truth in
//! `images/manifest.json`) browsed on disk, which the index lane reads before it is viewed; and
//! back to Develop through the switch. Between the agent's pick and that folder, long-running work:
//! a first look at a folder of 16,000 one-byte `.jpg` files the run writes beside `generated/`
//! (`first-look/`), captured while the index lane still reads it with the view's progress sheet;
//! Continue in background, captured with the sheet gone and the job in the status bar and the
//! Performance section; and Cancel on its Performance row, captured once the job has ended
//! cancelled on the activity board and the section has read it again.
//!
//! Then the catalog ([`select_catalog_smoke`]): real photographs developed into it before the run,
//! viewed from their catalog folder with their previews, counted and narrowed by the Metadata
//! browser and the search, saved as a smart collection, moved, collected and organized.
//!
//! Each frame's `select` block is checked against the core's own answers: the view's size, picks
//! and group layout as `browse.view` answered the runner's client, the headings' and headers' pick
//! counts as the layout counts them, each library gesture's request as an agent writes it, and the
//! selection as the owner answered the desktop's own `session.state` when the step settled. The
//! sources panel's cards, volumes and catalog counts are checked against `card.list`,
//! `volume.list` and `catalog.info`. After the run the runner reads the catalog the editor left,
//! through its own client again, for the picks, the journal of the desktop's and the agent's
//! changes, and the view they made. Everything compared is written to `app/select-checks.json`; the
//! core's answers from before the run are kept in `select-expected.json`, so a replay checks the
//! same frames against them.
use crate::{
    generate_catalog,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, plan::only},
    select_catalog_smoke,
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{
    self as script, ArrowKey, LibraryKey, SelectMenu, SelectStep, SelectWorkspace,
};

pub const SCENARIO: &str = "select";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates its catalog and index into `generated/` with \
    `cargo xtask generate-catalog --files 2000 --assets 3000 --images 120 --seed 1`, develops 28 of \
    its JPEGs (`iPhone export` and `Card dumps/2026-09-16`) into a new catalog folder `Real \
    photographs` through the runner's own client (`folder.create`, `index.refresh`, `pick.set`, \
    `pick.plan`, `pick.develop`, as the actor `setup`), asks the core for the \
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

/// The picks the run makes, from the core's answers before it: the agent's, a file of the
/// Day-grouped view the core said was not picked, on the first screen; the desktop's own `P`, an
/// unpicked single file of the event as first shown, on the first screen, with its name; and the
/// bracket Pick all picks, by its first frame's position, and the frames it picks.
#[derive(Clone, Debug, PartialEq)]
pub struct Picks {
    pub agent: u32,
    pub own: u32,
    pub own_name: String,
    pub bracket: u32,
    /// The journal's label for Pick all: "Picked 3 files", or the one file it picks by name.
    pub bracket_label: String,
}

/// Every frame, in order; `first_look` is the large folder whose first look is sent to the
/// background and cancelled.
pub fn plan(
    picks: &Picks,
    count: u64,
    folder: &str,
    images: u64,
    first_look: &str,
    catalog: Vec<Step>,
) -> Plan {
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let arrow = |direction, extend| SelectStep::Arrow { direction, extend };
    let own = format!("Picked {}", picks.own_name);
    let all = &picks.bracket_label;
    Plan::new(
        [
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
                    positions: vec![picks.agent],
                    picked: true,
                },
            )
            .status(format!(
                "{EVENT_LABEL} changed elsewhere and was read again \u{b7} {count} in view"
            )),
            select(
                "regrouped",
                SelectStep::Choose {
                    menu: SelectMenu::Group,
                    item: "Day \u{203a} Camera \u{203a} Moment".into(),
                },
            ),
            select(
                "clicked",
                SelectStep::Click {
                    position: picks.own,
                    shift: false,
                    command: false,
                },
            ),
            Step::new("own-pick", script::Step::key("p"))
                .status(format!("{own} \u{b7} Undo \u{2318}Z")),
            select(
                "pick-all",
                SelectStep::PickAll {
                    position: picks.bracket,
                },
            )
            .status(format!("{all} \u{b7} Undo \u{2318}Z")),
            select("undo", SelectStep::Library(LibraryKey::Undo))
                .status(format!("Undid {all} \u{b7} Redo \u{21e7}\u{2318}Z")),
            select("undo-again", SelectStep::Library(LibraryKey::Undo))
                .status(format!("Undid {own} \u{b7} Redo \u{21e7}\u{2318}Z")),
            select("nothing-to-undo", SelectStep::Library(LibraryKey::Undo))
                .status("Nothing to undo"),
            select("redo", SelectStep::Library(LibraryKey::Redo))
                .status(format!("Redid {own} \u{b7} Undo \u{2318}Z")),
            select("first-look", SelectStep::FirstLook(first_look.into())),
            select("background", SelectStep::ContinueInBackground),
            select("cancelled", SelectStep::CancelWork).status_starts("Cancelled reading "),
            select("folder", SelectStep::Folder(folder.into()))
                .status(format!("images \u{b7} {images} in view")),
        ]
        .into_iter()
        // The catalog: browsed and organized (`select_catalog_smoke`).
        .chain(catalog)
        .chain([select(
            "develop",
            SelectStep::Switch(SelectWorkspace::Develop),
        )])
        .collect(),
    )
}

/// One request through the runner's own client.
pub(crate) fn ask(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<Value> {
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
pub(crate) fn over_copy<T>(
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

/// A count as the sources panel writes it: `1,042`.
fn thousands(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The sources panel the core's answers say: the cards by their volume's name, the volumes On
/// disk but the cards with their dots, and the Catalog rows' counts from `catalog.info`.
fn sources_record(cards: &Value, volumes: &Value, info: &Value) -> Value {
    let counts = &info["counts"];
    let count = |key: &str| json!(thousands(counts[key].as_u64().unwrap_or(0)));
    let missing = counts["unavailable"].as_u64().unwrap_or(0);
    json!({
        "cards": cards["cards"].as_array().into_iter().flatten()
            .map(|card| card["volume"]["label"].clone()).collect::<Vec<_>>(),
        "volumes": volumes["volumes"].as_array().into_iter().flatten()
            .filter(|volume| volume["card"] == false)
            .map(|volume| json!({
                "name": volume["volume"]["label"],
                "dot": if volume["offline"] == true { "offline" } else { "mounted" },
            }))
            .collect::<Vec<_>>(),
        "catalog": [
            count("photographs"),
            count("recently_developed"),
            if missing > 0 { json!({"unavailable": thousands(missing)}) } else { Value::Null },
            count("removed"),
        ],
    })
}

/// The sources panel as a frame drew it, in the shape of [`sources_record`].
fn sources_drawn(block: &Value) -> Value {
    let sources = &block["source_rows"];
    let rows = |key: &str| sources[key].as_array().cloned().unwrap_or_default();
    json!({
        "cards": rows("cards").iter().map(|row| row["name"].clone()).collect::<Vec<_>>(),
        "volumes": rows("on_disk").iter()
            .filter(|row| row["indent"] == 0 && !row["dot"].is_null())
            .map(|row| json!({"name": row["name"], "dot": row["dot"]}))
            .collect::<Vec<_>>(),
        "catalog": rows("catalog").iter().map(|row| row["count"].clone()).collect::<Vec<_>>(),
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
        // The desktop's own pick: an unpicked single file of the event as first shown, on the first
        // screen, not the agent's; and the first bracket not all picked whose frames the first
        // block of rows holds and neither pick names.
        let event_moments = first["groups"]["moments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let span = |moment: &Value| {
            let start = moment["start"].as_u64().unwrap_or(0);
            start..start + moment["len"].as_u64().unwrap_or(0)
        };
        let in_moment = |at: u64| {
            event_moments
                .iter()
                .any(|moment| span(moment).contains(&at))
        };
        let own = first_rows
            .iter()
            .find(|row| {
                let at = row["position"].as_u64().unwrap_or(0);
                row["picked"] == false
                    && row["item"] == "file"
                    && row["file_id"] != pick["file_id"]
                    && (3..30).contains(&at)
                    && !in_moment(at)
            })
            .ok_or("the event's first screen has no unpicked single file")?;
        let file_at = |at: u64| first_rows.iter().find(|row| row["position"] == at);
        let bracket = event_moments
            .iter()
            .find(|moment| {
                let frames = span(moment);
                moment["kind"] == "bracket"
                    && moment["picked"].as_u64() < moment["len"].as_u64()
                    && frames.end <= 200
                    && frames.clone().all(|at| {
                        file_at(at).is_some_and(|row| {
                            row["file_id"] != pick["file_id"] && row["file_id"] != own["file_id"]
                        })
                    })
            })
            .ok_or("the event's first rows hold no bracket to pick")?;
        let bracket_files: Vec<Value> = span(bracket)
            .filter_map(file_at)
            .map(|row| json!({"file_id": row["file_id"], "path": row["path"], "picked": row["picked"]}))
            .collect();
        let cards = ask(owner, client, "card.list", json!({}))?;
        let volumes = ask(owner, client, "volume.list", json!({}))?;
        let info = ask(owner, client, "catalog.info", json!({}))?;
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
            "own": {
                "position": own["position"],
                "file_id": own["file_id"],
                "path": own["path"],
                "name": own["file_name"],
            },
            "bracket": {
                "index": event_moments.iter().position(|moment| moment == bracket),
                "start": bracket["start"],
                "len": bracket["len"],
                "picked": bracket["picked"],
                "files": bracket_files,
            },
            "source_rows": sources_record(&cards, &volumes, &info),
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
            // Real photographs developed into the catalog, before anything reads it.
            select_catalog_smoke::prepare(&generated)?;
            let mut expected = expect(&generated)?;
            expected["catalog"] = select_catalog_smoke::expect(&generated)?;
            let first_look = run.out().join(FIRST_LOOK);
            write_first_look(&first_look)?;
            expected["first_look"] = json!({"path": first_look, "files": FIRST_LOOK_FILES});
            write_json(&expected_file, &expected)?;
        }
        let expected = read_json(&expected_file)?;
        let picks = picks_of(&expected)?;
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
        let catalog = select_catalog_smoke::steps(&expected)?;
        let plan = plan(&picks, count, folder, images, first_look, catalog);
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

/// The run's picks, from the core's answers before it.
fn picks_of(expected: &Value) -> Result<Picks> {
    let position = |value: &Value, what: &str| {
        value
            .as_u64()
            .map(|at| at as u32)
            .ok_or_else(|| format!("The expected answers name no {what}"))
    };
    let bracket = &expected["bracket"];
    let unpicked: Vec<&str> = bracket["files"]
        .as_array()
        .ok_or("The expected bracket has no frames")?
        .iter()
        .filter(|file| file["picked"] == false)
        .filter_map(|file| file["path"].as_str())
        .collect();
    let bracket_label = match unpicked.as_slice() {
        [one] => format!(
            "Picked {}",
            Path::new(one).file_name().map_or_else(
                || (*one).to_owned(),
                |name| name.to_string_lossy().into_owned()
            )
        ),
        many => format!("Picked {} files", many.len()),
    };
    Ok(Picks {
        agent: position(&expected["pick"]["position"], "file to pick")?,
        own: position(&expected["own"]["position"], "file for P")?,
        own_name: expected["own"]["name"]
            .as_str()
            .ok_or("The expected answers name no file for P")?
            .to_owned(),
        bracket: position(&bracket["start"], "bracket")?,
        bracket_label,
    })
}

pub(crate) fn select(frame: &Frame) -> &Value {
    &frame.state()["select"]
}

/// Each day heading and moment header says the picks the owner's layout counted for it: a day
/// "· N picked" exactly when N > 0, and a moment "N picked" exactly for the moments with picks.
fn headers_count_picks(frame: &Frame, name: &str) -> Result<Value> {
    let block = select(frame);
    let days = block["groups_picked"]["days"]
        .as_array()
        .ok_or_else(|| format!("{name}: no day picks recorded"))?;
    let headings = block["headers"]["days"]
        .as_array()
        .ok_or_else(|| format!("{name}: no day headings recorded"))?;
    ensure(
        days.len() == headings.len(),
        format!("{name}: {} days, {} headings", days.len(), headings.len()),
    )?;
    for (picked, heading) in days.iter().zip(headings) {
        let picked = picked.as_u64().unwrap_or(0);
        let heading = heading.as_str().unwrap_or_default();
        let says = heading.ends_with(&format!("\u{b7} {} picked", thousands(picked)));
        ensure(
            if picked > 0 {
                says
            } else {
                !heading.contains("picked")
            },
            format!("{name}: a day with {picked} picks is headed {heading:?}"),
        )?;
    }
    let moments: Vec<Value> = block["groups_picked"]["moments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|pair| {
            json!([
                pair[0],
                format!("{} picked", thousands(pair[1].as_u64().unwrap_or(0)))
            ])
        })
        .collect();
    ensure(
        block["headers"]["moments"].as_array() == Some(&moments),
        format!(
            "{name}: the moment headers say {}, the layout counts {moments:?}",
            block["headers"]["moments"]
        ),
    )?;
    Ok(json!({"days": days, "headings": headings, "moments": moments}))
}

/// The library request a frame's gesture sent: `method` with `params`, apart from the request
/// identity, as an agent writes it, with the desktop's actor, and what the owner answered.
pub(crate) fn library_sent(
    frame: &Frame,
    name: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let library = &select(frame)["library"];
    let request_id = &library["params"]["mutation"]["request_id"];
    ensure(
        request_id.as_str().is_some_and(|id| !id.is_empty()),
        format!("{name}: no library request recorded: {library}"),
    )?;
    let mut expected = params;
    expected["mutation"] = json!({"request_id": request_id, "actor": "desktop"});
    ensure(
        library["method"] == method && library["params"] == expected,
        format!("{name}: the desktop sent {library}, an agent writes {method} {expected}"),
    )?;
    ensure(
        library["error"].is_null(),
        format!("{name}: the owner refused it: {}", library["error"]),
    )?;
    Ok(library.clone())
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
pub(crate) fn grid_drawn(frame: &Frame) -> Result<Value> {
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
    let drawn = sources_drawn(block);
    ensure(
        drawn == expected["source_rows"],
        format!(
            "The sources panel shows {drawn}, card.list, volume.list and catalog.info answer {}",
            expected["source_rows"]
        ),
    )?;
    checks.note(
        shown,
        "Select with the events, cards, volumes and catalog counts listed",
        json!({"events": months, "source_rows": drawn}),
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
    checks.note(
        picked_frame,
        "an agent's pick read again through the event sync",
        json!({"selection": recorded, "status": picked_frame.state()["status"]}),
    );

    // Day › Camera › Moment again: the day headings and moment headers count their picks, the
    // agent's among them.
    let regrouped = launch.at("regrouped")?;
    let block = select(regrouped);
    ensure(
        block["grouping"] == "day-camera-moment"
            && block["picked"] == select(picked_frame)["picked"],
        format!(
            "Regrouped: {} with {} picks",
            block["grouping"], block["picked"]
        ),
    )?;
    let headers = headers_count_picks(regrouped, "regrouped")?;
    checks.note(
        regrouped,
        "the day headings and moment headers count their picks",
        headers,
    );
    let base = block["picked"].as_u64().unwrap_or(0);

    // A click on a single file, then `P`: `pick.set` of the selection as an agent writes it, the
    // view read again with the pick, the selection kept, the status bar saying the change.
    let clicked = launch.at("clicked")?;
    let own = expected["own"]["position"].clone();
    ensure(
        select(clicked)["selection"]
            == json!({"count": 1, "ranges": [{"start": own, "len": 1}], "active": own}),
        format!("The click selected {}", select(clicked)["selection"]),
    )?;
    let own_pick = launch.at("own-pick")?;
    let block = select(own_pick);
    let sent = library_sent(
        own_pick,
        "own-pick",
        "pick.set",
        json!({"targets": {"kind": "selection"}, "picked": true}),
    )?;
    ensure(
        block["picked"] == base + 1
            && block["revision"].as_u64() > select(clicked)["revision"].as_u64()
            && block["selection"] == select(clicked)["selection"]
            && block["info"]["pick"]["picked"] == true,
        format!("After P the view shows {block}"),
    )?;
    let headers = headers_count_picks(own_pick, "own-pick")?;
    let recorded = selection_is_the_owners(own_pick, "own-pick")?;
    checks.note(
        own_pick,
        "P picks the selection, a journaled change of the desktop's",
        json!({"request": sent, "headers": headers, "selection": recorded, "status": own_pick.state()["status"]}),
    );
    let own_picked = base + 1;

    // The bracket's Pick all: `pick.set` of its frames' files; its header counts them all.
    let pick_all = launch.at("pick-all")?;
    let block = select(pick_all);
    let bracket = &expected["bracket"];
    let files: Vec<Value> = bracket["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| file["file_id"].clone())
        .collect();
    let sent = library_sent(
        pick_all,
        "pick-all",
        "pick.set",
        json!({"targets": {"kind": "files", "file_ids": files}, "picked": true}),
    )?;
    let len = bracket["len"].as_u64().unwrap_or(0);
    let newly = len - bracket["picked"].as_u64().unwrap_or(0);
    ensure(
        block["picked"] == own_picked + newly,
        format!("After Pick all the view holds {} picks", block["picked"]),
    )?;
    let index = bracket["index"].clone();
    let headers = headers_count_picks(pick_all, "pick-all")?;
    ensure(
        block["headers"]["moments"]
            .as_array()
            .is_some_and(|moments| moments.contains(&json!([index, format!("{len} picked")])))
            && block["headers"]["pick_all"]
                .as_array()
                .is_some_and(|actions| actions.iter().all(|action| action[0] != index)),
        format!("The bracket's header after Pick all: {}", block["headers"]),
    )?;
    checks.note(
        pick_all,
        "Pick all picks the bracket's files",
        json!({"request": sent, "headers": headers, "status": pick_all.state()["status"]}),
    );

    // `Cmd+Z`: Pick all undone; again: the desktop's pick undone, the agent's kept; again: nothing
    // of the desktop's is left, so nothing changes and the view is not read again.
    let undo_params = json!({});
    let steps = [
        ("undo", own_picked, true),
        ("undo-again", base, true),
        ("nothing-to-undo", base, false),
    ];
    let mut before = pick_all;
    for (name, picked, reads) in steps {
        let frame = launch.at(name)?;
        let block = select(frame);
        let sent = library_sent(frame, name, "library.undo", undo_params.clone())?;
        let revision = block["revision"].as_u64();
        ensure(
            block["picked"] == picked
                && if reads {
                    revision > select(before)["revision"].as_u64()
                } else {
                    revision == select(before)["revision"].as_u64()
                        && sent["answer"]["outcome"] == "no-op"
                },
            format!(
                "{name}: the view shows {} picks at revision {revision:?}: {sent}",
                block["picked"]
            ),
        )?;
        let headers = headers_count_picks(frame, name)?;
        checks.note(
            frame,
            "Cmd+Z undoes the desktop's newest change, never the agent's",
            json!({"request": sent, "headers": headers, "status": frame.state()["status"]}),
        );
        before = frame;
    }

    // `Shift+Cmd+Z` redoes the desktop's pick.
    let redo = launch.at("redo")?;
    let block = select(redo);
    let sent = library_sent(redo, "redo", "library.redo", json!({}))?;
    ensure(
        block["picked"] == own_picked
            && block["revision"].as_u64() > select(before)["revision"].as_u64(),
        format!("After the redo the view holds {} picks", block["picked"]),
    )?;
    let headers = headers_count_picks(redo, "redo")?;

    // What the editor left in its catalog, read through the runner's own client: the agent's pick,
    // the desktop's redone pick, the bracket's frames as they were, and the journal of both.
    let generated = run.out().join(GENERATED);
    let after = over_copy(&generated, "after", |owner, client| {
        let picks = ask(owner, client, "pick.list", json!({}))?;
        let picks = picks["picks"].as_array().cloned().unwrap_or_default();
        let actor_of = |path: &Value| {
            picks
                .iter()
                .find(|pick| &pick["path"] == path)
                .map(|pick| pick["actor"].clone())
        };
        ensure(
            actor_of(&expected["pick"]["path"]) == Some(json!(AGENT)),
            "The agent's pick is not in the catalog as the agent's",
        )?;
        ensure(
            actor_of(&expected["own"]["path"]) == Some(json!("desktop")),
            "The desktop's redone pick is not in the catalog as the desktop's",
        )?;
        for file in bracket["files"].as_array().into_iter().flatten() {
            ensure(
                actor_of(&file["path"]).is_some() == (file["picked"] == true),
                format!("The bracket's frame {} is not as it was", file["path"]),
            )?;
        }
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        let changes: Vec<Value> = journal["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|change| change["actor"] == "desktop" || change["actor"] == AGENT)
            .map(|change| json!([change["actor"], change["method"], change["label"]]))
            .collect();
        let view = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "event", "event_id": expected["event"]["id"]}, "grouping": "day"}),
        )?;
        Ok(json!({"changes": changes, "view": view_record(&view)}))
    })?;
    let own_label = format!(
        "Picked {}",
        expected["own"]["name"].as_str().unwrap_or_default()
    );
    let all_label = picks_of(&expected)?.bracket_label;
    let agent_label = format!(
        "Picked {}",
        Path::new(expected["pick"]["path"].as_str().unwrap_or_default())
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let journal = json!([
        [AGENT, "pick.set", agent_label],
        ["desktop", "pick.set", own_label],
        ["desktop", "pick.set", all_label],
        ["desktop", "library.undo", format!("Undo {all_label}")],
        ["desktop", "library.undo", format!("Undo {own_label}")],
        ["desktop", "library.redo", format!("Redo {own_label}")],
    ]);
    // Then the catalog's changes, as its steps made them.
    let catalog: Vec<Value> = after["changes"]
        .as_array()
        .map(|changes| changes.iter().skip(6).cloned().collect())
        .unwrap_or_default();
    ensure(
        after["changes"]
            .as_array()
            .map(|changes| &changes[..changes.len().min(6)])
            == journal.as_array().map(Vec::as_slice)
            && catalog
                .iter()
                .map(|change| (change[0].as_str(), change[1].as_str()))
                .eq(select_catalog_smoke::journal()
                    .into_iter()
                    .map(|method| (Some("desktop"), Some(method)))),
        format!("The journal holds {}, expected {journal}", after["changes"]),
    )?;
    ensure(
        after["view"]["picked"] == block["picked"] && after["view"]["count"] == block["count"],
        format!(
            "The desktop shows {block}, the catalog it left answers {}",
            after["view"]
        ),
    )?;
    checks.note(
        redo,
        "Shift+Cmd+Z redoes the desktop's pick; the catalog it left holds both clients' changes",
        json!({"request": sent, "headers": headers, "catalog": after, "status": redo.state()["status"]}),
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

    select_catalog_smoke::verify(&mut checks, launch, &expected, &generated)?;

    // Back to Develop: Select keeps its view for when it is shown again.
    let develop = launch.at("develop")?;
    let last = launch.at(select_catalog_smoke::LAST)?;
    let block = select(develop);
    ensure(
        block["shown"] == "develop",
        "The switch did not return to Develop",
    )?;
    for key in ["revision", "count", "picked", "selection", "grouping"] {
        ensure(
            block[key] == select(last)[key],
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
        let picks = Picks {
            agent: 7,
            own: 5,
            own_name: "DSC_0012.NEF".into(),
            bracket: 20,
            bracket_label: "Picked 3 files".into(),
        };
        let expected = json!({"catalog": {
            "folder": {"count": 28},
            "camera": {"label": "iPhone 15 Pro"},
            "search": {"count": 9},
        }});
        let catalog = crate::select_catalog_smoke::steps(&expected).unwrap();
        let plan = plan(
            &picks,
            205,
            "/generated/images",
            120,
            "/run/first-look",
            catalog,
        );
        plan.validate().unwrap();
        assert_eq!(plan.len(), 36);
        assert!(plan.scripted());
    }
}
