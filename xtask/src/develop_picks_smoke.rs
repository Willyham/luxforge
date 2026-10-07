//! The `develop-picks` smoke scenario: from picks on a camera card to Develop, in the real editor at
//! 1440 × 900.
//!
//! The run first makes its photographs: real generated JPEGs (`generate-catalog --images 120`),
//! the first Nikon Z 8 folder of the generated card copied onto a disk image labelled "NIKON Z 8"
//! (on macOS made with `hdiutil` and attached at a mount point inside the run as a volume a person
//! browses, with `-noautoopen`, so that Luxforge knows it as a removable volume, as a card is —
//! `-nobrowse` would leave it part of the volume it is mounted in; elsewhere a scratch folder
//! stands in for it, which is not removable). Into a new catalog
//! it adds a catalog folder, "Portfolio picks", and an indexed folder, `Pictures`, holding copies
//! of two of the card's first three files (`Card dumps/2026-09-12`), as a photographer who copied
//! part of the card would have. Over a pristine copy of that catalog it asks the core, through its
//! own client, for the answers the frames are checked against (`develop-picks-expected.json`): the
//! card's folder as the desktop first views it, and `pick.plan` of its first three files picked.
//!
//! The editor then opens that catalog with nothing open. Its frames, in [`plan`] order: `G` showing
//! Select; the card's `DCIM` folder browsed; its first file clicked and picked with `P`; its second
//! the same; its third clicked and picked with `P`, then Develop N's confirmation opened;
//! Or add to an existing folder choosing "Portfolio picks"; a name typed over it, back to a new
//! folder; Escape, which changes nothing; Develop N again; the name typed again; Develop, captured
//! once the Develop has ended and Develop shows the first photograph it developed; the
//! neighbours' large previews decoded; `→`, captured in the frame after the key with the next
//! photograph's cached preview drawn; and that photograph's exact render.
//!
//! Every frame's confirmation is checked against the core's own `pick.plan` answer, the Develop's
//! request against what an agent writes for the same choice, and its `job.read` record against
//! where the catalog the editor left points each photograph (`develop-picks-after.json`): the two
//! picks with copies at their copies, the third at the card. Each photograph drawn is its own:
//! the picture's middle against its file's, decoded independently.
use crate::{
    generate_catalog,
    resolve_missing_smoke::DiskImage,
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step, Tolerance, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{self as script, DevelopStep, SelectStep, SetStep};

pub const SCENARIO: &str = "develop-picks";
/// What `reproduce.md` says the run does before it launches.
pub const NOTE: &str = "The run first generates real JPEGs with `cargo xtask generate-catalog \
    --images 120 --seed 1`, copies the generated card's first Nikon Z 8 folder onto a disk image \
    labelled NIKON Z 8, made with `hdiutil` and attached inside the run with `-noautoopen` as a \
    removable volume, as a card is (a scratch folder elsewhere than macOS), and makes `generated/catalog.sqlite` with a catalog folder, \
    Portfolio picks, and an indexed folder holding copies of two of the card's first three files; \
    it asks the core for the answers the frames are checked against over a pristine copy \
    (`develop-picks-expected.json`) and launches the editor over that catalog with `--catalog`; \
    after the run it reads the catalog the editor left (`develop-picks-after.json`) and detaches \
    the image.";
/// Where the run writes its photographs, image and catalog.
pub const GENERATED: &str = "generated";
pub const EXPECTED: &str = "develop-picks-expected.json";
pub const AFTER: &str = "develop-picks-after.json";
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// The card's label, and the folder of the generated card copied onto it.
const CARD: &str = "NIKON Z 8";
const CARD_FOLDER: &str = "100NZ8_1";
/// The catalog folder the run makes, which Or add to an existing folder offers.
const EXISTING: &str = "Portfolio picks";
/// The name typed over the proposed one.
const NAMED: &str = "Konstanz trip";
/// The card's first files picked, and how many of them have copies.
const PICKS: usize = 3;
const COPIED: usize = 2;
/// How far a patch of the drawn picture may be from the same patch of its file, in 8-bit codes
/// of luminance: the decoders differ in rounding and the picture is resampled to the screen.
const DRAWN_TOLERANCE: f64 = 10.0;

/// Every frame, in order, from the core's answers.
pub fn plan(expected: &Value) -> Result<Plan> {
    let card = expected["card"]
        .as_str()
        .ok_or("The expected answers name no card folder")?;
    let count = expected["count"]
        .as_u64()
        .ok_or("The expected answers hold no card view")?;
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let develop = |name: &str, step: DevelopStep| Step::new(name, script::Step::Develop(step));
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
    let key = |name: &str, key: &str| Step::new(name, script::Step::key(key));
    Ok(Plan::new(vec![
        Step::opened("opened"),
        key("select", "g"),
        select("card", SelectStep::Folder(card.into()))
            .status(format!("DCIM \u{b7} {count} in view")),
        click("first", 0),
        key("pick-1", "p").status_starts("Picked "),
        click("second", 1),
        key("pick-2", "p").status_starts("Picked "),
        click("third", 2),
        key("pick-3", "p").status_starts("Picked "),
        develop("active", DevelopStep::Picks),
        develop(
            "existing",
            DevelopStep::Existing {
                event: 0,
                folder: EXISTING.into(),
            },
        ),
        develop(
            "named",
            DevelopStep::Name {
                event: 0,
                text: NAMED.into(),
            },
        ),
        develop("cancel", DevelopStep::Cancel).status("Nothing developed"),
        develop("again", DevelopStep::Picks),
        develop(
            "renamed",
            DevelopStep::Name {
                event: 0,
                text: NAMED.into(),
            },
        ),
        develop("developed", DevelopStep::Confirm),
        develop("ready", DevelopStep::Ready),
        develop("next", DevelopStep::Step(SetStep::Next)),
        develop("settled", DevelopStep::Settle),
    ]))
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("develop-picks-smoke-{method}"),
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

fn mutation(id: &str) -> Value {
    json!({"request_id": format!("setup-{id}"), "actor": "setup"})
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

/// The card's folder read by the index lane and viewed as the desktop first views it: its listed
/// path, its count and its rows, `{position, file_name, path}`.
fn card_view(owner: &OwnerHandle, client: ClientId, folder: &Path) -> Result<Value> {
    let started = ask(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": folder}}),
    )?;
    let read = settle(owner, client, &started["job_id"])?;
    ensure(
        read["status"] == "ready",
        format!("the core could not read the card: {read}"),
    )?;
    let listed = read["result"]["roots"][0].clone();
    let view = ask(
        owner,
        client,
        "browse.view",
        json!({"source": {"kind": "folder", "path": listed, "subfolders": true}}),
    )?;
    let count = view["count"].as_u64().ok_or("The card view has no count")?;
    let rows = ask(
        owner,
        client,
        "browse.rows",
        json!({"from": 0, "count": count}),
    )?;
    let rows: Vec<Value> = rows["rows"]
        .as_array()
        .ok_or("The card view answered no rows")?
        .iter()
        .map(|row| json!({"position": row["position"], "file_name": row["file_name"], "path": row["path"]}))
        .collect();
    Ok(json!({"listed": listed, "count": count, "rows": rows, "view": view["query"]}))
}

/// Make the card, the catalog and its indexed copies, and ask the core for its answers over a
/// pristine copy. The image stays attached until the catalog the editor left has been read.
fn make(generated: &Path) -> Result<(DiskImage, Value)> {
    generate_catalog::run(
        &generated.join("source"),
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )?;
    let source = generated
        .join("source")
        .join("images")
        .join("card")
        .join("DCIM")
        .join(CARD_FOLDER);
    let card = DiskImage::card(generated, CARD)?;
    let dcim = card.root()?.join("DCIM");
    copy_dir(&source, &dcim.join(CARD_FOLDER))?;
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let pictures = generated.join("Pictures");
    let copies = pictures.join("Card dumps").join("2026-09-12");
    let (existing, view) = with_owner(&catalog, |owner, client| {
        let created = ask(
            owner,
            client,
            "folder.create",
            json!({"name": EXISTING, "mutation": mutation("folder")}),
        )?;
        let view = card_view(owner, client, &dcim)?;
        Ok((created["folder"].clone(), view))
    })?;
    let rows = view["rows"].as_array().ok_or("no rows")?.clone();
    ensure(
        rows.len() > PICKS,
        format!("The card holds {} files", rows.len()),
    )?;
    let picks: Vec<Value> = rows[..PICKS]
        .iter()
        .map(|row| row["path"].clone())
        .collect();
    let copied: Vec<String> = rows[..COPIED]
        .iter()
        .filter_map(|row| row["file_name"].as_str().map(str::to_owned))
        .collect();
    fs::create_dir_all(&copies)?;
    for (row, name) in rows.iter().zip(&copied) {
        let from = row["path"].as_str().ok_or("a path")?;
        fs::copy(from, copies.join(name))?;
    }
    let pictures = pictures.canonicalize()?;
    with_owner(&catalog, |owner, client| {
        let added = ask(
            owner,
            client,
            "index.add-folder",
            json!({"path": pictures, "mutation": mutation("pictures")}),
        )?;
        if !added["job_id"].is_null() {
            let read = settle(owner, client, &added["job_id"])?;
            ensure(
                read["status"] == "ready",
                format!("the core could not list the copies: {read}"),
            )?;
        }
        Ok(())
    })?;
    // The plan of the three picks, over a pristine copy the editor never opens.
    let scratch = std::env::temp_dir().join(format!(
        "luxforge-develop-picks-expected-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let index = generated.join(format!("{}.index", generate_catalog::CATALOG));
    fs::copy(&catalog, scratch.join(generate_catalog::CATALOG))?;
    if index.is_dir() {
        copy_dir(
            &index,
            &scratch.join(format!("{}.index", generate_catalog::CATALOG)),
        )?;
    }
    let answers = with_owner(&scratch.join(generate_catalog::CATALOG), |owner, client| {
        let view = card_view(owner, client, &dcim)?;
        ask(
            owner,
            client,
            "pick.set",
            json!({"targets": {"kind": "paths", "paths": picks}, "picked": true,
                "mutation": mutation("picks")}),
        )?;
        ask(owner, client, "browse.view", view["view"].clone())?;
        let plan = ask(owner, client, "pick.plan", json!({}))?;
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        Ok(json!({"plan": plan, "journal": journal["changes"].as_array().map_or(0, Vec::len)}))
    });
    let _ = fs::remove_dir_all(&scratch);
    let answers = answers?;
    let copies = copies.canonicalize()?;
    Ok((
        card,
        json!({
            "card": dcim.canonicalize()?,
            "listed": view["listed"],
            "count": view["count"],
            "rows": rows,
            "picks": picks,
            "copied": copied,
            "copies": copies,
            "existing": existing,
            "plan": answers["plan"],
            // The changes the catalog holds before the editor opens it: the folder and the indexed
            // folder, the expected run's picks being its own.
            "journal": answers["journal"].as_u64().unwrap_or(0).saturating_sub(1),
        }),
    ))
}

/// What the catalog the editor left says: its folders, where each photograph points, the picks
/// left, and the changes recorded after the run's own setup.
fn after(generated: &Path, expected: &Value) -> Result<Value> {
    let catalog = generated.join(generate_catalog::CATALOG);
    with_owner(&catalog, |owner, client| {
        let folders = ask(owner, client, "folder.list", json!({}))?;
        let view = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "all-photographs"}}),
        )?;
        let count = view["count"].as_u64().unwrap_or(0);
        let mut photographs = Vec::new();
        if count > 0 {
            let rows = ask(
                owner,
                client,
                "browse.rows",
                json!({"from": 0, "count": count}),
            )?;
            for row in rows["rows"].as_array().into_iter().flatten() {
                let state = ask(
                    owner,
                    client,
                    "asset.state",
                    json!({"asset_id": row["asset_id"]}),
                )?;
                photographs.push(json!({
                    "asset_id": row["asset_id"],
                    "file_name": row["file_name"],
                    "folder_id": row["folder_id"],
                    "locator": state["asset"]["locator"],
                }));
            }
        }
        let picks = ask(owner, client, "pick.list", json!({}))?;
        let journal = ask(owner, client, "library.journal", json!({"limit": 500}))?;
        let changes: Vec<Value> = journal["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .skip(expected["journal"].as_u64().unwrap_or(0) as usize)
            .map(|change| {
                json!({"method": change["method"], "label": change["label"], "actor": change["actor"]})
            })
            .collect();
        Ok(json!({
            "folders": folders["folders"],
            "photographs": photographs,
            "picks": picks["picks"],
            "changes": changes,
        }))
    })
}

/// Make the card and catalog and the core's answers (a replay reads the recorded ones), launch the
/// editor over it, read the catalog it left and check the frames.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let expected_file = run.out().join(EXPECTED);
    let after_file = run.out().join(AFTER);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let card = if run.replaying() {
            None
        } else {
            fs::create_dir_all(&generated)?;
            let (card, expected) = make(&generated)?;
            write_json(&expected_file, &expected)?;
            Some(card)
        };
        let expected = read_json(&expected_file)?;
        let plan = plan(&expected)?;
        let mut launch = Launch::app()
            .catalog(&generated.join(generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        if let Some(card) = card {
            write_json(&after_file, &after(&generated, &expected)?)?;
            drop(card);
        }
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, std::slice::from_ref(&checked))?;
        run.sources_unchanged()?;
        Ok(())
    })
}

fn develop(frame: &Frame) -> &Value {
    &frame.state()["develop"]
}

/// The confirmation's request as the frame records it, without its request identity.
fn params(frame: &Frame) -> Value {
    let mut params = develop(frame)["confirm"]["params"].clone();
    if let Some(object) = params.as_object_mut() {
        object.remove("mutation");
    }
    params
}

/// Mean luminance of the middle of the image at `path`, a quarter of its short side either way,
/// decoded independently of the editor.
fn file_luminance(path: &str) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    crate::scenario::pixels::mean_luminance(
        &image,
        (f64::from(width) / 2.0, f64::from(height) / 2.0),
        i64::from(width.min(height) / 4),
    )
}

/// Mean luminance of the middle of the photograph the frame draws, a quarter of its short side
/// either way.
fn drawn_luminance(frame: &Frame) -> Result<f64> {
    let [left, top, right, bottom] = frame.photo()?;
    let (width, height) = (f64::from(right - left), f64::from(bottom - top));
    crate::scenario::pixels::mean_luminance(
        frame.image()?,
        (f64::from(left) + width / 2.0, f64::from(top) + height / 2.0),
        (width.min(height) / 4.0) as i64,
    )
}

/// The confirmation a frame shows is the core's plan, its one event's folder as `field` or
/// `existing` says, the card's picks with their copies said, and the request it would send.
fn confirmation(
    frame: &Frame,
    name: &str,
    expected: &Value,
    folder: Value,
    model_folder: (&str, Option<&str>),
) -> Result {
    let confirm = &develop(frame)["confirm"];
    ensure(
        confirm["plan"] == expected["plan"],
        format!(
            "{name}: the confirmation shows {}, the core planned {}",
            confirm["plan"], expected["plan"]
        ),
    )?;
    let model = &confirm["model"];
    ensure(
        model["title"] == format!("Develop {PICKS} picks"),
        format!("{name}: titled {}", model["title"]),
    )?;
    let event = &model["events"][0];
    let (tag, value) = model_folder;
    ensure(
        event["tag"] == tag
            && match tag {
                "new folder" => event["field"] == json!(value),
                _ => event["existing"] == json!(value),
            },
        format!("{name}: the event's folder is {event}"),
    )?;
    let notes = model["notes"].as_array().ok_or("no notes")?;
    ensure(
        notes.len() == 1
            && notes[0].as_str().is_some_and(|note| {
                note.starts_with(&format!("{PICKS} picks are on {CARD}."))
                    && note.contains(&format!("{COPIED} have copies in indexed folders"))
            })
            && model["copies"] == true,
        format!("{name}: the card's picks are said as {notes:?}"),
    )?;
    let event_id = &expected["plan"]["events"][0]["event_id"];
    let mut into = json!({"folder": folder});
    if !event_id.is_null() {
        into["event_id"] = event_id.clone();
    }
    let wanted = json!({"into": [into], "use_copies": true, "confirm_removable": true});
    ensure(
        params(frame) == wanted,
        format!(
            "{name}: the request would be {}, expected {wanted}",
            params(frame)
        ),
    )?;
    Ok(())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let after = read_json(
        &run.recorded(&run.out().join(AFTER))
            .unwrap_or_else(|| run.out().join(AFTER)),
    )?;
    let mut checks = Checks::new();
    let plan = &expected["plan"];
    let event = &plan["events"][0];
    ensure(
        plan["count"] == PICKS && plan["events"].as_array().map(Vec::len) == Some(1),
        format!("The core planned {plan}"),
    )?;
    let removable = &event["removable"][0];
    ensure(
        removable["label"] == CARD
            && removable["count"] == PICKS
            && removable["with_copy"] == COPIED,
        format!(
            "The core found the picks on the card as {}",
            event["removable"]
        ),
    )?;
    let proposed = event["folder"]["name"]
        .as_str()
        .ok_or_else(|| format!("The core proposes no new folder: {event}"))?;

    // The card's folder is the core's view; the picks are Select's own `pick.set`s.
    let card = launch.at("card")?;
    ensure(
        card.state()["select"]["count"] == expected["count"],
        format!("The card shows {}", card.state()["select"]["count"]),
    )?;
    for (name, position) in [("pick-1", 0usize), ("pick-2", 1), ("pick-3", 2)] {
        let library = &launch.at(name)?.state()["select"]["library"];
        ensure(
            library["method"] == "pick.set"
                && library["params"]["picked"] == true
                && library["error"].is_null(),
            format!("{name}: the pick sent {library}"),
        )?;
        checks.note(
            launch.at(name)?,
            &format!(
                "{name}: P picked {}",
                expected["rows"][position]["file_name"]
            ),
            json!({"library": library}),
        );
    }

    // Develop N opens the confirmation on the core's plan after the three picks.
    let active = launch.at("active")?;
    confirmation(
        active,
        "active",
        &expected,
        json!({"kind": "new", "name": proposed}),
        ("new folder", Some(proposed)),
    )?;
    let existing_id = expected["existing"]["id"].clone();
    confirmation(
        launch.at("existing")?,
        "existing",
        &expected,
        json!({"kind": "existing", "folder_id": existing_id}),
        ("existing folder", Some(EXISTING)),
    )?;
    let named = json!({"kind": "new", "name": NAMED});
    confirmation(
        launch.at("named")?,
        "named",
        &expected,
        named.clone(),
        ("new folder", Some(NAMED)),
    )?;
    // Escape changes nothing: no confirmation, nothing sent.
    let cancel = launch.at("cancel")?;
    ensure(
        develop(cancel)["confirm"].is_null() && develop(cancel)["last"]["request"].is_null(),
        format!("cancel: {}", develop(cancel)),
    )?;
    confirmation(
        launch.at("again")?,
        "again",
        &expected,
        json!({"kind": "new", "name": proposed}),
        ("new folder", Some(proposed)),
    )?;
    confirmation(
        launch.at("renamed")?,
        "renamed",
        &expected,
        named.clone(),
        ("new folder", Some(NAMED)),
    )?;

    // Develop: the request the confirmation showed, its job ready, and Develop on what it answered.
    let developed = launch.at("developed")?;
    let last = &develop(developed)["last"];
    let mut sent = last["request"].clone();
    ensure(
        sent["mutation"]["actor"] == "desktop",
        format!("developed: sent as {}", sent["mutation"]),
    )?;
    if let Some(object) = sent.as_object_mut() {
        object.remove("mutation");
    }
    ensure(
        sent == params(launch.at("renamed")?),
        format!(
            "developed: sent {sent}, the confirmation showed {}",
            params(launch.at("renamed")?)
        ),
    )?;
    let record = &last["record"];
    ensure(
        record["status"] == "ready" && last["answer"]["job_id"] == record["job_id"],
        format!(
            "developed: the job ended {record}, answered {}",
            last["answer"]
        ),
    )?;
    let report = &record["result"];
    let developed_list = report["developed"].as_array().ok_or("no developed list")?;
    ensure(
        developed_list.len() == PICKS && report["failed"] == json!([]),
        format!("developed: the report is {report}"),
    )?;
    let copies = expected["copies"].as_str().unwrap_or_default();
    let copied: Vec<&str> = expected["copied"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for pick in developed_list {
        let name = Path::new(pick["path"].as_str().unwrap_or_default())
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let used = pick["used"].as_str();
        let photograph = after["photographs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|photo| photo["asset_id"] == pick["asset_id"])
            .ok_or_else(|| format!("The catalog left holds no {}", pick["asset_id"]))?;
        if copied.contains(&name.as_str()) {
            ensure(
                used.is_some_and(|used| Path::new(used) == Path::new(copies).join(&name))
                    && photograph["locator"] == json!(used),
                format!("{name}: its copy was not used: {pick}, {photograph}"),
            )?;
        } else {
            ensure(
                used.is_none() && photograph["locator"] == pick["path"],
                format!("{name}: not developed from the card: {pick}, {photograph}"),
            )?;
        }
    }
    let set = &develop(developed)["set"];
    let assets: Vec<&Value> = developed_list
        .iter()
        .map(|pick| &pick["asset_id"])
        .collect();
    ensure(
        set["assets"]
            .as_array()
            .map(|held| held.iter().collect::<Vec<_>>())
            == Some(assets.clone())
            && set["active"] == 0,
        format!("developed: the set is {set}, the Develop answered {assets:?}"),
    )?;
    ensure(
        developed.state()["select"]["shown"] == "develop"
            && develop(developed)["filmstrip_shown"] == true
            && develop(developed)["strip"]["total"] == PICKS
            && develop(developed)["document"]["asset_id"] == *assets[0],
        format!("developed: Develop shows {}", develop(developed)),
    )?;
    let folders = after["folders"].as_array().ok_or("no folders")?;
    let folder = folders
        .iter()
        .find(|folder| folder["name"] == NAMED)
        .ok_or("The catalog left has no folder named as typed")?;
    ensure(
        folder["count"] == PICKS
            && after["photographs"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|photo| photo["folder_id"] == folder["id"]),
        format!("The folder typed holds {folder}"),
    )?;
    ensure(
        after["picks"] == json!([]),
        format!("Picks were left: {}", after["picks"]),
    )?;
    let changes = after["changes"].as_array().ok_or("no changes")?;
    let picks_recorded = changes
        .iter()
        .filter(|change| change["method"] == "pick.set" && change["actor"] == "desktop")
        .count();
    ensure(
        picks_recorded == PICKS
            && changes
                .iter()
                .any(|change| change["method"] == "pick.develop" && change["actor"] == "desktop")
            && changes.iter().all(|change| {
                change["actor"] == "desktop"
                    && matches!(change["method"].as_str(), Some("pick.set" | "pick.develop"))
            }),
        format!("The journal recorded {changes:?}"),
    )?;
    let path_of = |asset: &Value| -> Result<String> {
        after["photographs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|photo| photo["asset_id"] == *asset)
            .and_then(|photo| photo["locator"].as_str().map(str::to_owned))
            .ok_or_else(|| format!("No locator for {asset}").into())
    };
    checks.compare(
        developed,
        "developed: the first photograph's middle against its file's",
        drawn_luminance(developed)?,
        file_luminance(&path_of(assets[0])?)?,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;

    // →: the next photograph's cached preview, drawn in the frame after the key — the surface's own
    // record of the frame it first drew it in — and still on screen, or already replaced by the
    // photograph's render when its small original opened before the capture.
    let next = launch.at("next")?;
    let state = develop(next);
    let (preview, timing) = (&state["preview"], &state["timing"]);
    ensure(
        timing["asset_id"] == *assets[1]
            && !timing["preview_version"].is_null()
            && timing["presented_after"] == 1
            && state["set"]["active"] == 1,
        format!("next: the move's preview was not drawn in the frame after the key: {timing}"),
    )?;
    if preview.is_null() {
        ensure(
            state["document"]["asset_id"] == *assets[1],
            format!("next: no preview, and {} open", state["document"]),
        )?;
    } else {
        ensure(
            preview["asset_id"] == *assets[1]
                && preview["drawn"] == true
                // Before the open answers, nothing is open; after, the photograph at the preview's
                // own entry, until its render replaces the preview.
                && (state["document"].is_null()
                    || (state["document"]["asset_id"] == *assets[1]
                        && state["document"]["entry_id"] == preview["entry_id"])),
            format!("next: the preview drawn is {preview}"),
        )?;
        ensure(
            next.state()["status_bar"]["render"]
                .as_str()
                .is_some_and(|render| render.contains("preview")),
            format!(
                "next: the render slot reads {}",
                next.state()["status_bar"]["render"]
            ),
        )?;
    }
    checks.compare(
        next,
        "next: the picture's middle against its photograph's file",
        drawn_luminance(next)?,
        file_luminance(&path_of(assets[1])?)?,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    let settled = launch.at("settled")?;
    ensure(
        develop(settled)["document"]["asset_id"] == *assets[1]
            && develop(settled)["preview"].is_null(),
        format!("settled: {}", develop(settled)),
    )?;
    checks.compare(
        settled,
        "settled: the render's middle against its photograph's file",
        drawn_luminance(settled)?,
        file_luminance(&path_of(assets[1])?)?,
        Tolerance::Within(DRAWN_TOLERANCE),
    )?;
    checks.write(
        &launch.evidence,
        "develop-picks",
        json!({"expected": {"plan": plan, "copied": copied}, "tolerance": DRAWN_TOLERANCE}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_develop_picks_plan_is_well_formed() {
        let expected = json!({"card": "/Volumes/NIKON Z 8/DCIM", "count": 20});
        let plan = plan(&expected).unwrap();
        plan.validate().unwrap();
        assert_eq!(plan.len(), 19);
        assert!(plan.scripted());
    }
}
