//! The `select` smoke scenario's catalog steps: browsing and organizing developed
//! photographs in Select, checked against the core's own answers.
//!
//! Before the launch the run develops real photographs into its generated catalog, as an agent
//! would: it makes the catalog folder [`FOLDER`] (`folder.create`), has the index lane read two
//! folders of the generated JPEGs (`index.refresh`), picks their files (`pick.set`) and develops
//! them into that folder (`pick.plan`, then `pick.develop` with the folder for every event), so
//! the folder's photographs have originals, rendered previews and their metadata. It then asks the
//! core, over a pristine copy, for the answers the frames are checked against ([`expect`]).
//!
//! The frames, after the shell's own and before the switch back to Develop: the folder viewed from
//! the sources panel with its previews; the Metadata browser opened, its counts the core's
//! `browse.facets`; a camera chosen in it, and a search typed, each changing the query and nothing
//! else, the view the size the facet predicted; the view saved as a smart collection
//! (`collection.create-smart` with exactly the query shown) and the smart collection viewed; the
//! folder again; a photograph clicked and moved to another folder (`asset.move` of the selection);
//! five photographs selected and added to Print order from the Info panel (`collection.add` of the
//! selection) and the add undone with `Cmd+Z`; and the folder renamed and nested in Travel
//! (`folder.rename`, `folder.move`) from its menu. Then the batch: five photographs selected, the
//! library preset [`PRESET`] (made by the setup with `preset.create`) applied from the Develop band
//! (`batch.apply-preset` of the selection), its report opened from the status bar, and the five
//! exported into a scratch folder of the run's (`batch.export`), each checked against the job's
//! own `job.read` record. Then sending back: an edited photograph clicked, its Info panel's Send
//! back refused with the core's reason, and an unedited one right-clicked, its menu's Send back
//! sending `asset.send-back` of the selection, checked against `catalog.info` and, after the run,
//! the file picked again and the core's own refusal of each edited photograph. And removing: one
//! photograph removed from its Info panel
//! (`asset.remove` of the selection, after its confirmation) and the removal undone, two removed
//! with ⌫, Removed viewed, one put back (`asset.restore`), and Removed emptied after its
//! confirmation (`catalog.empty-removed`), each checked against `catalog.info`'s counts. After the
//! run the runner reads what the editor left: the smart collection's stored query, the folders,
//! the moved photograph and Print order, each preset photograph's history, the exported files and
//! the counts.
use crate::{
    scenario::{Checks, Frame, Step},
    select_smoke::{ask, grid_drawn, library_sent, over_copy, select},
    *,
};
use luxforge_core::{ClientId, OwnerHandle};
use luxforge_evidence::{self as script, CatalogStep, FacetColumn, LibraryKey, SelectStep};

/// The catalog folder the run develops real photographs into.
pub const FOLDER: &str = "Real photographs";
/// The library preset the setup makes and the run applies: Basic's exposure, which every
/// photograph takes, and a RAW development, which no JPEG takes, so each is listed in the report.
const PRESET: &str = "Warm";
/// The run's scratch folder the batch export writes into, in its output directory.
pub const EXPORT: &str = "batch-export";
/// What the run renames it to, and the generated folder it nests it in.
const RENAMED: &str = "Swiss trip";
const NEST_IN: &str = "Travel";
/// The generated folder a photograph is moved to, and the collection five are added to.
const MOVE_TO: &str = "Bodensee";
const ADD_TO: &str = "Print order";
/// What the search types, and the smart collection's name.
const SEARCH: &str = "Luzern";
const SMART: &str = "iPhone in Luzern";
/// The generated JPEG folders the run develops.
const DEVELOPED: [&str; 2] = ["iPhone export", "Card dumps/2026-09-16"];
/// The actor of the run's own setup changes, which the journal check leaves out.
const SETUP: &str = "setup";
/// The frame the catalog steps end on, which the switch back to Develop keeps.
pub const LAST: &str = "catalog-emptied";

fn mutation(id: &str) -> Value {
    json!({"request_id": format!("select-smoke-setup-{id}"), "actor": SETUP})
}

/// A job's end, read as the desktop reads it.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let job = json!({"job_id": job});
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", job.clone())?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(120),
            format!("the setup's job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Develop the generated JPEGs of [`DEVELOPED`] into [`FOLDER`], through the runner's own client
/// on the run's catalog, before anything reads it.
pub fn prepare(generated: &Path) -> Result {
    let catalog = generated.join(generate_catalog::CATALOG);
    let (owner, join) = OwnerHandle::start(&catalog)
        .map_err(|error| format!("the core cannot open the generated catalog: {error}"))?;
    let client = owner.register();
    let outcome = (|| -> Result {
        let created = ask(
            &owner,
            client,
            "folder.create",
            json!({"name": FOLDER, "mutation": mutation("folder")}),
        )?;
        let folder = created["folder"]["id"].clone();
        let mut paths = Vec::new();
        for relative in DEVELOPED {
            let started = ask(
                &owner,
                client,
                "index.refresh",
                json!({"source": {"kind": "folder", "path": generated.join("images").join(relative)}}),
            )?;
            let read = settle(&owner, client, &started["job_id"])?;
            ensure(
                read["status"] == "ready",
                format!("the setup could not read {relative}: {read}"),
            )?;
            let listed = read["result"]["roots"][0]
                .as_str()
                .ok_or("the listing named no root")?
                .to_owned();
            let mut names: Vec<String> = fs::read_dir(Path::new(&listed))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.to_ascii_lowercase().ends_with(".jpg"))
                .collect();
            names.sort();
            paths.extend(names.into_iter().map(|name| Path::new(&listed).join(name)));
        }
        let targets = json!({"kind": "paths", "paths": paths});
        ask(
            &owner,
            client,
            "pick.set",
            json!({"targets": targets, "picked": true, "mutation": mutation("pick")}),
        )?;
        let plan = ask(&owner, client, "pick.plan", json!({"targets": targets}))?;
        let into: Vec<Value> = plan["events"]
            .as_array()
            .ok_or("pick.plan answered no events")?
            .iter()
            .map(|event| {
                let mut into = json!({"folder": {"kind": "existing", "folder_id": folder}});
                if !event["event_id"].is_null() {
                    into["event_id"] = event["event_id"].clone();
                }
                into
            })
            .collect();
        let started = ask(
            &owner,
            client,
            "pick.develop",
            json!({"into": into, "targets": targets, "mutation": mutation("develop")}),
        )?;
        let developed = settle(&owner, client, &started["job_id"])?;
        ensure(
            developed["status"] == "ready",
            format!("the setup could not develop the photographs: {developed}"),
        )?;
        ask(
            &owner,
            client,
            "preset.create",
            json!({
                "name": PRESET,
                "settings": {
                    "set-basic": {"exposure": 0.3},
                    "set-raw": {"white-balance": "as-shot"},
                },
                "mutation": mutation("preset"),
            }),
        )?;
        Ok(())
    })();
    owner.stop();
    let _ = join.join();
    outcome
}

/// The run's empty scratch folder for the batch export, made afresh in its output directory: the
/// export never writes into a folder of the person's.
pub fn export_folder(out: &Path) -> Result<PathBuf> {
    let folder = out.join(EXPORT);
    if folder.exists() {
        fs::remove_dir_all(&folder)?;
    }
    fs::create_dir_all(&folder)?;
    Ok(folder.canonicalize()?)
}

/// A folder's identity from `folder.list`, by its name.
fn folder_named(folders: &Value, name: &str) -> Result<Value> {
    folders["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|folder| folder["name"] == name)
        .map(|folder| folder["id"].clone())
        .ok_or_else(|| format!("the generated catalog has no folder {name}").into())
}

/// The core's answers the catalog frames are checked against, over a pristine copy of the prepared
/// catalog: the folder and its view, its facets, the camera the run chooses and the view it gives,
/// the search's view, and the folder and collection the photographs go to.
pub fn expect(generated: &Path) -> Result<Value> {
    over_copy(generated, "catalog-expected", |owner, client| {
        let folders = ask(owner, client, "folder.list", json!({}))?;
        let folder = folder_named(&folders, FOLDER)?;
        let source = json!({"kind": "catalog-folder", "folder_id": folder});
        let facets_of = |filter: Value| {
            ask(
                owner,
                client,
                "browse.facets",
                json!({"source": source, "filter": filter, "facets": ["date", "place", "camera", "lens", "kind"]}),
            )
        };
        let facets = facets_of(json!({}))?;
        let camera = facets["counts"]["camera"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|value| {
                value["value"]
                    .as_str()
                    .is_some_and(|key| key.contains("iPhone"))
            })
            .cloned()
            .ok_or("the developed photographs hold no iPhone")?;
        let view = |filter: Value| {
            ask(
                owner,
                client,
                "browse.view",
                json!({"source": source, "filter": filter, "sort": {"key": "capture-time", "descending": true}, "grouping": "none"}),
            )
        };
        let folder_view = view(json!({}))?;
        let cameras = json!([camera["value"]]);
        let camera_view = view(json!({"cameras": cameras}))?;
        let search_view = view(json!({"cameras": cameras, "text": SEARCH}))?;
        let move_to = folder_named(&folders, MOVE_TO)?;
        let moved_view = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "catalog-folder", "folder_id": move_to}}),
        )?;
        let collections = ask(owner, client, "collection.list", json!({}))?;
        let add_to = collections["collections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|collection| collection["name"] == ADD_TO)
            .cloned()
            .ok_or_else(|| format!("the generated catalog has no collection {ADD_TO}"))?;
        let presets = ask(owner, client, "preset.list", json!({}))?;
        let preset = presets["presets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|preset| preset["name"] == PRESET)
            .map(|preset| preset["id"].clone())
            .ok_or_else(|| format!("the generated catalog has no preset {PRESET}"))?;
        let counts = ask(owner, client, "catalog.info", json!({}))?["counts"].clone();
        Ok(json!({
            "preset": {"id": preset},
            "counts": {"photographs": counts["photographs"], "removed": counts["removed"]},
            "folder": {"id": folder, "count": folder_view["count"]},
            "facets": facets,
            "camera": {
                "value": camera["value"],
                "label": camera["label"],
                "count": camera["count"],
                "view": camera_view["count"],
            },
            "search": {"count": search_view["count"]},
            "move_to": {"id": move_to, "count": moved_view["count"]},
            "nest_in": {"id": folder_named(&folders, NEST_IN)?},
            "add_to": {"id": add_to["id"], "count": add_to["count"]},
        }))
    })
}

/// The catalog frames, in order, from the core's answers before the run.
pub fn steps(expected: &Value) -> Result<Vec<Step>> {
    let catalog = |name: &str, step: CatalogStep| Step::new(name, script::Step::Catalog(step));
    let select = |name: &str, step: SelectStep| Step::new(name, script::Step::Select(step));
    let expected = &expected["catalog"];
    let count = expected["folder"]["count"]
        .as_u64()
        .ok_or("The expected answers hold no catalog folder")?;
    let camera = expected["camera"]["label"]
        .as_str()
        .or_else(|| expected["camera"]["value"].as_str())
        .ok_or("The expected answers name no camera")?
        .to_owned();
    let found = expected["search"]["count"].as_u64().unwrap_or(0);
    let export = expected["export"]["path"]
        .as_str()
        .ok_or("The expected answers name no export folder")?
        .to_owned();
    let click = |position, shift| SelectStep::Click {
        position,
        shift,
        command: false,
    };
    Ok(vec![
        catalog("catalog-folder", CatalogStep::Source(FOLDER.into()))
            .status(format!("{FOLDER} \u{b7} {count} in view")),
        catalog("catalog-metadata", CatalogStep::Metadata),
        catalog(
            "catalog-camera",
            CatalogStep::Facet {
                column: FacetColumn::Camera,
                value: camera,
            },
        ),
        catalog("catalog-search", CatalogStep::Search(SEARCH.into())),
        catalog("catalog-smart", CatalogStep::SaveSmart(SMART.into())).status(format!(
            "Created smart collection {SMART} \u{b7} Undo \u{2318}Z"
        )),
        catalog("catalog-smart-viewed", CatalogStep::Source(SMART.into()))
            .status(format!("{SMART} \u{b7} {found} in view")),
        catalog("catalog-back", CatalogStep::Source(FOLDER.into())),
        select("catalog-clicked", click(0, false)),
        catalog("catalog-moved", CatalogStep::MoveTo(MOVE_TO.into())),
        select("catalog-first", click(0, false)),
        select("catalog-range", click(4, true)),
        catalog("catalog-added", CatalogStep::AddTo(ADD_TO.into()))
            .status(format!("Added 5 to {ADD_TO} \u{b7} Undo \u{2318}Z")),
        select("catalog-undone", SelectStep::Library(LibraryKey::Undo)).status(format!(
            "Undid Added 5 to {ADD_TO} \u{b7} Redo \u{21e7}\u{2318}Z"
        )),
        catalog(
            "catalog-renamed",
            CatalogStep::Rename {
                folder: FOLDER.into(),
                name: RENAMED.into(),
            },
        )
        .status(format!(
            "Renamed folder {FOLDER} to {RENAMED} \u{b7} Undo \u{2318}Z"
        )),
        catalog(
            "catalog-nested",
            CatalogStep::Nest {
                folder: RENAMED.into(),
                into: NEST_IN.into(),
            },
        )
        .status(format!(
            "Moved folder {RENAMED} into {NEST_IN} \u{b7} Undo \u{2318}Z"
        )),
        // The batch: five selected, the library preset applied and its report opened, then the
        // five exported into the run's scratch folder.
        select("catalog-batch-first", click(0, false)),
        select("catalog-batch", click(4, true)),
        catalog("catalog-preset", CatalogStep::ApplyPreset(PRESET.into())).status(format!(
            "Applied {PRESET} to 5 photographs \u{b7} 5 without some settings"
        )),
        catalog("catalog-preset-report", CatalogStep::Report),
        catalog("catalog-exported", CatalogStep::ExportInto(export))
            .status_starts("Exported 5 photographs to "),
        // Sending back: an edited photograph's Send back is refused with the core's reason; an
        // unedited one is right-clicked and sent back from its menu.
        select("catalog-edited", click(0, false)),
        catalog("catalog-context", CatalogStep::Context(5)),
        catalog(
            "catalog-sent-back",
            CatalogStep::ContextChoice("Send back".into()),
        )
        .status_starts("Sent back "),
        // Removing: one removed from its Info panel and the removal undone; two removed with ⌫;
        // Removed viewed, one put back, and Removed emptied.
        select("catalog-one", click(5, false)),
        catalog("catalog-remove-asked", CatalogStep::Remove),
        catalog("catalog-removed", CatalogStep::Confirm).status_starts("Removed "),
        select(
            "catalog-remove-undone",
            SelectStep::Library(LibraryKey::Undo),
        )
        .status_starts("Undid Removed "),
        select("catalog-pair-first", click(5, false)),
        select("catalog-pair", click(6, true)),
        catalog("catalog-delete-asked", CatalogStep::DeleteKey),
        catalog("catalog-removed-two", CatalogStep::Confirm)
            .status("Removed 2 photographs \u{b7} Undo \u{2318}Z"),
        select("catalog-removed-view", SelectStep::Source("Removed".into())),
        select("catalog-put-back-one", click(0, false)),
        catalog("catalog-put-back", CatalogStep::PutBack).status_starts("Put back "),
        catalog("catalog-empty-asked", CatalogStep::EmptyRemoved),
        catalog(LAST, CatalogStep::Confirm).status_starts("Emptied Removed: "),
    ])
}

/// The methods the catalog steps add to the desktop's journal, in order. The batches and the
/// emptying are no library changes.
pub fn journal() -> Vec<&'static str> {
    vec![
        "collection.create-smart",
        "asset.move",
        "collection.add",
        "library.undo",
        "folder.rename",
        "folder.move",
        "asset.send-back",
        "asset.remove",
        "library.undo",
        "asset.remove",
        "asset.restore",
    ]
}

fn catalog_block(frame: &Frame) -> &Value {
    &select(frame)["catalog"]
}

/// The batch request a frame's gesture sent: `method` with `params`, apart from the request
/// identity, as an agent writes it, with the desktop's actor, and the job the owner answered.
fn batch_sent(frame: &Frame, name: &str, method: &str, params: Value) -> Result<Value> {
    let sent = &catalog_block(frame)["batch_request"];
    let request_id = &sent["params"]["mutation"]["request_id"];
    ensure(
        request_id.as_str().is_some_and(|id| !id.is_empty()),
        format!("{name}: no batch request recorded: {sent}"),
    )?;
    let mut expected = params;
    expected["mutation"] = json!({"request_id": request_id, "actor": "desktop"});
    ensure(
        sent["method"] == method && sent["params"] == expected,
        format!("{name}: the desktop sent {sent}, an agent writes {method} {expected}"),
    )?;
    ensure(
        sent["error"].is_null() && sent["answer"]["job_id"].is_string(),
        format!("{name}: the owner started no job: {sent}"),
    )?;
    Ok(sent.clone())
}

/// A frame's batch: it ended, the desktop's report is exactly the `result` of the `job.read`
/// record it read for the job it started, and the band and the status bar say its sentence.
fn batch_ended(frame: &Frame, name: &str, kind: &str) -> Result<Value> {
    let block = catalog_block(frame);
    let batch = &block["batch"];
    let record = &block["batch_record"];
    ensure(
        record["status"] == "ready"
            && record["kind"] == kind
            && record["job_id"] == batch["job"]
            && batch["job"] == block["batch_request"]["answer"]["job_id"],
        format!(
            "{name}: the batch's job.read record is {record}, its job {}",
            batch["job"]
        ),
    )?;
    ensure(
        batch["end"]["report"] == record["result"],
        format!(
            "{name}: the desktop shows {}, job.read answered {}",
            batch["end"]["report"], record["result"]
        ),
    )?;
    ensure(
        frame.status()? == batch["sentence"]
            && block["info"]["band"]["line"] == batch["sentence"]
            && block["status_report"] == true,
        format!(
            "{name}: the status bar says {:?}, the band {}, the batch {}",
            frame.status()?,
            block["info"]["band"]["line"],
            batch["sentence"]
        ),
    )?;
    Ok(record["result"].clone())
}

/// The count of the Catalog sources' row `index` (All photographs 0, Removed 3) as a frame drew
/// it, from `catalog.info`.
fn catalog_count(frame: &Frame, index: usize) -> Option<u64> {
    select(frame)["source_rows"]["catalog"][index]["count"]
        .as_str()
        .and_then(|count| count.replace(',', "").parse().ok())
}

/// How many photographs a frame's Removed row counts, and its view holds.
fn removed_and_viewed(frame: &Frame) -> (Option<u64>, Option<u64>) {
    (catalog_count(frame, 3), select(frame)["count"].as_u64())
}

/// A frame's view against the core's answer for it: its source, its query's filter and its size.
fn viewing(frame: &Frame, name: &str, source: &Value, filter: &Value, count: &Value) -> Result {
    let block = select(frame);
    let shown = &catalog_block(frame)["query"];
    ensure(
        &shown["source"] == source && &shown["filter"] == filter && &block["count"] == count,
        format!(
            "{name}: the desktop shows {} of {} filtered {}, the core answered {count} for {source} filtered {filter}",
            block["count"], shown["source"], shown["filter"]
        ),
    )?;
    ensure(
        block["quiet"] == true && block["stale"] == false && catalog_block(frame)["shown"] == true,
        format!("{name}: captured unsettled or not over the catalog: {block}"),
    )
}

/// Check every catalog frame, and what the editor left in its catalog.
pub fn verify(
    checks: &mut Checks,
    launch: &crate::scenario::Checked,
    expected: &Value,
    generated: &Path,
) -> Result {
    let expected = &expected["catalog"];
    let folder = json!({"kind": "catalog-folder", "folder_id": expected["folder"]["id"], "subfolders": true});

    // The folder, viewed from the sources panel with its photographs' rendered previews.
    let frame = launch.at("catalog-folder")?;
    viewing(
        frame,
        "catalog-folder",
        &folder,
        &json!({}),
        &expected["folder"]["count"],
    )?;
    let previews = &select(frame)["previews"];
    ensure(
        previews["photographs"].as_u64() > Some(0)
            && previews["visible"].as_u64() > Some(0)
            && previews["visible_held"] == previews["visible"]
            && previews["refused"] == 0,
        format!("catalog-folder: the cells on screen lack their previews: {previews}"),
    )?;
    checks.note(
        frame,
        "a catalog folder viewed with its photographs' rendered previews",
        json!({"previews": previews, "grid": grid_drawn(frame)?, "title": select(frame)["title"]}),
    );

    // The Metadata browser: the core's facets for the folder, each column's All their sum.
    let frame = launch.at("catalog-metadata")?;
    let block = catalog_block(frame);
    ensure(
        block["facets"] == expected["facets"],
        format!(
            "catalog-metadata: the browser counts {}, browse.facets answered {}",
            block["facets"], expected["facets"]
        ),
    )?;
    for (column, facet) in block["metadata"]
        .as_array()
        .ok_or("catalog-metadata: the browser is not open")?
        .iter()
        .zip(["date", "place", "camera", "lens"])
    {
        let total: u64 = expected["facets"]["counts"][facet]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value["count"].as_u64())
            .sum();
        if facet != "date" {
            ensure(
                column["rows"][0]["label"] == "All"
                    && column["rows"][0]["count"]
                        .as_str()
                        .map(|count| count.replace(',', ""))
                        == Some(total.to_string()),
                format!(
                    "catalog-metadata: {facet}'s All is {}, not {total}",
                    column["rows"][0]
                ),
            )?;
        }
    }
    checks.note(
        frame,
        "the Metadata browser, its counts browse.facets'",
        json!({"metadata": block["metadata"]}),
    );

    // A camera chosen, then a search: each changes its part of the query and nothing else, and
    // the camera's view is the size its facet predicted.
    let cameras = json!([expected["camera"]["value"]]);
    let frame = launch.at("catalog-camera")?;
    viewing(
        frame,
        "catalog-camera",
        &folder,
        &json!({"cameras": cameras}),
        &expected["camera"]["count"],
    )?;
    ensure(
        expected["camera"]["count"] == expected["camera"]["view"],
        "the camera's facet count is not the view it gives",
    )?;
    let before = &catalog_block(launch.at("catalog-metadata")?)["query"];
    let after = &catalog_block(frame)["query"];
    for key in ["source", "sort", "grouping", "thresholds"] {
        ensure(
            before[key] == after[key],
            format!("catalog-camera: choosing a camera changed the query's {key}"),
        )?;
    }
    checks.note(
        frame,
        "a camera from the Metadata browser",
        json!({"query": after}),
    );
    let frame = launch.at("catalog-search")?;
    let searched = json!({"cameras": cameras, "text": SEARCH});
    viewing(
        frame,
        "catalog-search",
        &folder,
        &searched,
        &expected["search"]["count"],
    )?;
    checks.note(
        frame,
        "the search typed",
        json!({"query": catalog_block(frame)["query"], "count": select(frame)["count"]}),
    );
    let query = catalog_block(frame)["query"].clone();

    // Save as smart collection…: exactly the query shown; then the smart collection viewed.
    let frame = launch.at("catalog-smart")?;
    let sent = library_sent(
        frame,
        "catalog-smart",
        "collection.create-smart",
        json!({"name": SMART, "query": query}),
    )?;
    let smart = sent["answer"]["collection"]["id"].clone();
    checks.note(
        frame,
        "the view saved as a smart collection",
        json!({"request": sent}),
    );
    let frame = launch.at("catalog-smart-viewed")?;
    viewing(
        frame,
        "catalog-smart-viewed",
        &json!({"kind": "collection", "collection_id": smart}),
        &json!({}),
        &expected["search"]["count"],
    )?;
    checks.note(
        frame,
        "the smart collection viewed",
        json!({"title": select(frame)["title"]}),
    );

    // A photograph moved to another folder: it leaves the folder's view.
    let frame = launch.at("catalog-back")?;
    viewing(
        frame,
        "catalog-back",
        &folder,
        &json!({}),
        &expected["folder"]["count"],
    )?;
    let frame = launch.at("catalog-moved")?;
    let sent = library_sent(
        frame,
        "catalog-moved",
        "asset.move",
        json!({"targets": {"kind": "selection"}, "folder_id": expected["move_to"]["id"]}),
    )?;
    let left = expected["folder"]["count"].as_u64().unwrap_or(0) - 1;
    viewing(frame, "catalog-moved", &folder, &json!({}), &json!(left))?;
    checks.note(
        frame,
        "a photograph moved to another folder",
        json!({"request": sent}),
    );

    // Five selected and added to Print order from the batch form, then the add undone.
    let frame = launch.at("catalog-range")?;
    let info = &catalog_block(frame)["info"];
    ensure(
        info["title"] == "5 selected" && info["folders"] == json!([[FOLDER, null]]),
        format!("catalog-range: the batch form shows {info}"),
    )?;
    let frame = launch.at("catalog-added")?;
    let sent = library_sent(
        frame,
        "catalog-added",
        "collection.add",
        json!({"collection_id": expected["add_to"]["id"], "targets": {"kind": "selection"}}),
    )?;
    let info = &catalog_block(frame)["info"];
    ensure(
        info["count"] == 5
            && info["collections"]
                .as_array()
                .is_some_and(|chips| chips.contains(&json!([ADD_TO, null]))),
        format!("catalog-added: the batch form shows {info}"),
    )?;
    checks.note(
        frame,
        "five photographs added to a collection",
        json!({"request": sent, "info": info}),
    );
    let frame = launch.at("catalog-undone")?;
    let sent = library_sent(frame, "catalog-undone", "library.undo", json!({}))?;
    let info = &catalog_block(frame)["info"];
    ensure(
        info["collections"]
            .as_array()
            .is_some_and(|chips| !chips.iter().any(|chip| chip[0] == ADD_TO)),
        format!("catalog-undone: the batch form still shows {info}"),
    )?;
    checks.note(
        frame,
        "Cmd+Z undoes the add",
        json!({"request": sent, "info": info}),
    );

    // The folder renamed and nested from its menu.
    let frame = launch.at("catalog-renamed")?;
    let sent = library_sent(
        frame,
        "catalog-renamed",
        "folder.rename",
        json!({"folder_id": expected["folder"]["id"], "name": RENAMED}),
    )?;
    ensure(
        select(frame)["title"]["name"] == RENAMED,
        format!("catalog-renamed: the title says {}", select(frame)["title"]),
    )?;
    checks.note(frame, "the folder renamed", json!({"request": sent}));
    let frame = launch.at("catalog-nested")?;
    let sent = library_sent(
        frame,
        "catalog-nested",
        "folder.move",
        json!({"folder_id": expected["folder"]["id"], "parent_id": expected["nest_in"]["id"]}),
    )?;
    checks.note(
        frame,
        "the folder nested in another",
        json!({"request": sent}),
    );

    // The batch: five selected, the library preset applied to them, its report job.read's.
    let frame = launch.at("catalog-batch")?;
    ensure(
        catalog_block(frame)["info"]["count"] == 5,
        format!(
            "catalog-batch: the batch form shows {}",
            catalog_block(frame)["info"]
        ),
    )?;
    let frame = launch.at("catalog-preset")?;
    let sent = batch_sent(
        frame,
        "catalog-preset",
        "batch.apply-preset",
        json!({"targets": {"kind": "selection"}, "preset_id": expected["preset"]["id"]}),
    )?;
    let preset = batch_ended(frame, "catalog-preset", "batch-preset")?;
    let done = preset["done"].as_array().cloned().unwrap_or_default();
    ensure(
        done.len() == 5
            && preset["skipped"] == json!([])
            && preset["settings_skipped"].as_array().map(Vec::len) == Some(5),
        format!(
            "catalog-preset: five JPEGs take the exposure and not the RAW development: {preset}"
        ),
    )?;
    let info = &catalog_block(frame)["info"];
    ensure(
        info["edited"] == "5 of 5",
        format!("catalog-preset: the view was not read again: {info}"),
    )?;
    checks.note(
        frame,
        "a library preset applied to five photographs, its report job.read's",
        json!({"request": sent, "report": preset, "band": info["band"]}),
    );
    let frame = launch.at("catalog-preset-report")?;
    let sheet = &catalog_block(frame)["sheet"];
    let listed = &sheet["sections"][0];
    ensure(
        sheet["kind"] == "preset"
            && listed["heading"] == "Without some settings \u{b7} 5"
            && listed["rows"].as_array().map(Vec::len) == Some(5),
        format!("catalog-preset-report: the report shows {sheet}"),
    )?;
    checks.note(
        frame,
        "the batch's report, opened from the status bar",
        json!({"sheet": sheet}),
    );

    // The five exported into the run's scratch folder: the files written are the report's.
    let frame = launch.at("catalog-exported")?;
    let export = &expected["export"]["path"];
    let sent = batch_sent(
        frame,
        "catalog-exported",
        "batch.export",
        json!({"targets": {"kind": "selection"}, "destination": export}),
    )?;
    let exported = batch_ended(frame, "catalog-exported", "batch-export")?;
    // Each file written names its renderer, as a single export's result does.
    let mut written: Vec<String> = exported["written"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| {
            ensure(
                matches!(
                    file["renderer"]["record"].as_str(),
                    Some("gpu" | "reference")
                ),
                format!("an exported file names no renderer: {file}"),
            )?;
            let path = Path::new(file["path"].as_str().unwrap_or_default());
            ensure(
                path.parent() == export.as_str().map(Path::new),
                format!("an exported file is outside the folder: {}", path.display()),
            )?;
            Ok(path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default())
        })
        .collect::<Result<_>>()?;
    written.sort();
    let mut on_disk: Vec<String> = fs::read_dir(export.as_str().ok_or("no export folder")?)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    ensure(
        exported["done"].as_array().map(Vec::len) == Some(5)
            && written.len() == 5
            && written == on_disk,
        format!(
            "catalog-exported: the report wrote {written:?}, the folder holds {on_disk:?}: {exported}"
        ),
    )?;
    checks.note(
        frame,
        "the five exported into the run's folder, the files written the report's",
        json!({"request": sent, "report": exported, "folder": on_disk}),
    );

    // Sending back: the edited photograph's Send back refused with the core's reason, which the
    // runner asks the core for after the run; the unedited one right-clicked, its menu's Send back
    // one library change, the photograph leaving the folder and the catalog's count.
    let photographs = expected["counts"]["photographs"].as_u64().unwrap_or(0);
    let frame = launch.at("catalog-edited")?;
    let info = &catalog_block(frame)["info"];
    let edited_reason = info["send_back"]["refused"]
        .as_str()
        .ok_or_else(|| format!("catalog-edited: Send back is not refused: {info}"))?
        .to_owned();
    ensure(
        info["edited"] == "Yes"
            && info["title"]
                .as_str()
                .is_some_and(|name| edited_reason.starts_with(name)),
        format!("catalog-edited: the Info panel shows {info}"),
    )?;
    checks.note(
        frame,
        "an edited photograph's Send back, refused with the core's reason",
        json!({"send_back": info["send_back"]}),
    );
    let moved_count = expected["folder"]["count"].as_u64().unwrap_or(0) - 1;
    let frame = launch.at("catalog-context")?;
    let menu = &catalog_block(frame)["context"];
    let sent_name = catalog_block(frame)["info"]["title"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    ensure(
        menu[0]["label"] == "Send back"
            && menu[0]["enabled"] == true
            && menu[1]["label"] == "Remove from catalog\u{2026}"
            && select(frame)["selection"]["active"] == 5
            && select(frame)["selection"]["count"] == 1,
        format!(
            "catalog-context: the menu shows {menu} over the selection {}",
            select(frame)["selection"]
        ),
    )?;
    checks.note(
        frame,
        "a right-click selects the photograph and opens its menu",
        json!({"menu": menu, "photograph": sent_name}),
    );
    let frame = launch.at("catalog-sent-back")?;
    let sent = library_sent(
        frame,
        "catalog-sent-back",
        "asset.send-back",
        json!({"targets": {"kind": "selection"}}),
    )?;
    ensure(
        frame.status()? == format!("Sent back {sent_name} \u{b7} its file is picked again")
            && select(frame)["count"] == moved_count - 1
            && catalog_count(frame, 0) == Some(photographs - 1)
            && catalog_block(frame)["context"].is_null(),
        format!(
            "catalog-sent-back: the status says {:?}, the view holds {}, All photographs {:?}",
            frame.status()?,
            select(frame)["count"],
            catalog_count(frame, 0)
        ),
    )?;
    checks.note(
        frame,
        "Send back from the menu: one library change, the photograph out of the catalog",
        json!({"request": sent, "status": frame.status()?, "count": select(frame)["count"]}),
    );

    // Removing, each step against catalog.info's counts as the sources panel shows them.
    let removed = expected["counts"]["removed"].as_u64().unwrap_or(0);
    let folder_count = moved_count - 1;
    let frame = launch.at("catalog-one")?;
    ensure(
        removed_and_viewed(frame) == (Some(removed), Some(folder_count)),
        format!(
            "catalog-one: Removed and the view count {:?}, catalog.info said {removed} removed",
            removed_and_viewed(frame)
        ),
    )?;
    let frame = launch.at("catalog-remove-asked")?;
    let sheet = &catalog_block(frame)["sheet"];
    ensure(
        sheet["kind"] == "remove"
            && sheet["confirm"] == "Remove"
            && sheet["note"]
                .as_str()
                .is_some_and(|note| note.contains("stays on disk")),
        format!("catalog-remove-asked: the confirmation shows {sheet}"),
    )?;
    checks.note(
        frame,
        "Remove from catalog… asks first",
        json!({"sheet": sheet}),
    );
    let steps = [
        (
            "catalog-removed",
            "asset.remove",
            json!({"targets": {"kind": "selection"}}),
            removed + 1,
            folder_count - 1,
        ),
        (
            "catalog-remove-undone",
            "library.undo",
            json!({}),
            removed,
            folder_count,
        ),
        (
            "catalog-removed-two",
            "asset.remove",
            json!({"targets": {"kind": "selection"}}),
            removed + 2,
            folder_count - 2,
        ),
    ];
    for (name, method, params, in_removed, in_view) in steps {
        let frame = launch.at(name)?;
        let sent = library_sent(frame, name, method, params)?;
        ensure(
            removed_and_viewed(frame) == (Some(in_removed), Some(in_view)),
            format!(
                "{name}: Removed and the view count {:?}, not {in_removed} and {in_view}",
                removed_and_viewed(frame)
            ),
        )?;
        checks.note(
            frame,
            match name {
                "catalog-removed" => "one photograph removed, one library change",
                "catalog-remove-undone" => "Cmd+Z puts it back",
                _ => "two removed with Delete",
            },
            json!({"request": sent, "removed": in_removed, "count": in_view}),
        );
    }
    let frame = launch.at("catalog-delete-asked")?;
    ensure(
        catalog_block(frame)["sheet"]["confirm"] == "Remove 2",
        format!(
            "catalog-delete-asked: Delete asks {}",
            catalog_block(frame)["sheet"]
        ),
    )?;
    let frame = launch.at("catalog-removed-view")?;
    ensure(
        select(frame)["source"]["kind"] == "removed"
            && removed_and_viewed(frame) == (Some(removed + 2), Some(removed + 2)),
        format!(
            "catalog-removed-view: Removed shows {:?} of {}",
            removed_and_viewed(frame),
            select(frame)["source"]
        ),
    )?;
    checks.note(
        frame,
        "Removed viewed, holding what catalog.info counts",
        json!({"count": select(frame)["count"]}),
    );
    let frame = launch.at("catalog-put-back")?;
    let sent = library_sent(
        frame,
        "catalog-put-back",
        "asset.restore",
        json!({"targets": {"kind": "selection"}}),
    )?;
    ensure(
        removed_and_viewed(frame) == (Some(removed + 1), Some(removed + 1)),
        format!(
            "catalog-put-back: Removed and its view count {:?}",
            removed_and_viewed(frame)
        ),
    )?;
    checks.note(
        frame,
        "one put back, one library change",
        json!({"request": sent}),
    );
    let frame = launch.at("catalog-empty-asked")?;
    let sheet = &catalog_block(frame)["sheet"];
    let deleting = if removed + 1 == 1 {
        "Delete 1 photograph".to_owned()
    } else {
        format!("Delete {} photographs", removed + 1)
    };
    ensure(
        sheet["kind"] == "empty"
            && sheet["confirm"] == deleting.as_str()
            && sheet["note"]
                .as_str()
                .is_some_and(|note| note.contains("cannot be undone")),
        format!("catalog-empty-asked: the confirmation shows {sheet}"),
    )?;
    let frame = launch.at(LAST)?;
    let calls = &catalog_block(frame)["empty_calls"];
    ensure(
        calls.as_array().map(Vec::len) == Some(1)
            && calls[0]["params"]["mutation"]["actor"] == "desktop"
            && calls[0]["answer"]["deleted"] == removed + 1
            && calls[0]["answer"]["remaining"] == 0
            && removed_and_viewed(frame) == (Some(0), Some(0))
            && catalog_count(frame, 0) == Some(photographs - 2),
        format!(
            "{LAST}: emptying answered {calls}; Removed and its view count {:?}, All photographs {:?}",
            removed_and_viewed(frame),
            catalog_count(frame, 0)
        ),
    )?;
    checks.note(
        frame,
        "Removed emptied, catalog.info counting none left",
        json!({"calls": calls, "photographs": catalog_count(frame, 0)}),
    );

    // What the editor left: the smart collection storing exactly the query shown, the folder
    // renamed and nested, the photograph in its new folder, and Print order as it was.
    let after = over_copy(generated, "catalog-after", |owner, client| {
        let collections = ask(owner, client, "collection.list", json!({}))?;
        let folders = ask(owner, client, "folder.list", json!({}))?;
        let moved = ask(
            owner,
            client,
            "browse.view",
            json!({"source": {"kind": "catalog-folder", "folder_id": expected["move_to"]["id"]}}),
        )?;
        // Each photograph the preset reached, with its history's labels.
        let mut histories = Vec::new();
        for asset in &done {
            let page = ask(
                owner,
                client,
                "history.list",
                json!({"asset_id": asset, "limit": 20}),
            )?;
            let labels: Vec<Value> = page["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|entry| entry["label"].clone())
                .collect();
            histories.push(json!({"asset_id": asset, "labels": labels}));
        }
        let counts = ask(owner, client, "catalog.info", json!({}))?["counts"].clone();
        // The core's own refusal to send back each photograph the preset edited, asked of this
        // copy, which a refusal leaves as it was.
        let mut refusals = Vec::new();
        for asset in &done {
            let refused = ask(
                owner,
                client,
                "asset.send-back",
                json!({"targets": {"kind": "assets", "asset_ids": [asset]}, "mutation": mutation("send-back")}),
            );
            refusals.push(match refused {
                Ok(answer) => json!({"answered": answer}),
                Err(error) => json!(error.to_string()),
            });
        }
        let picks = ask(owner, client, "pick.list", json!({}))?;
        Ok(json!({
            "collections": collections,
            "folders": folders,
            "moved": moved["count"],
            "histories": histories,
            "counts": counts,
            "refusals": refusals,
            "picks": picks,
        }))
    })?;
    for history in after["histories"].as_array().into_iter().flatten() {
        let presets = history["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|label| *label == &json!(format!("Preset: {PRESET}")))
            .count();
        ensure(
            presets == 1,
            format!("A photograph the preset reached has {presets} preset entries: {history}"),
        )?;
    }
    ensure(
        after["counts"]["removed"] == 0 && after["counts"]["photographs"] == photographs - 2,
        format!(
            "The catalog left counts {}, {photographs} photographs before",
            after["counts"]
        ),
    )?;
    // The edited photograph's Send back said the core's own reason, and the sent-back photograph's
    // file is picked again by the desktop.
    let refusal = format!("asset.send-back: conflict: {edited_reason}");
    ensure(
        after["refusals"]
            .as_array()
            .is_some_and(|refusals| refusals.contains(&json!(refusal))),
        format!(
            "The core refuses the edited photographs with {}, the desktop said {edited_reason:?}",
            after["refusals"]
        ),
    )?;
    let repicked = after["picks"]["picks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pick| {
            pick["path"]
                .as_str()
                .is_some_and(|path| Path::new(path).file_name() == Some(sent_name.as_ref()))
        })
        .cloned()
        .ok_or_else(|| format!("{sent_name} is not picked again: {}", after["picks"]))?;
    ensure(
        repicked["actor"] == "desktop",
        format!("{sent_name} is picked again as {}", repicked["actor"]),
    )?;
    let stored = after["collections"]["collections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|collection| collection["id"] == smart)
        .cloned()
        .ok_or("The smart collection is not in the catalog")?;
    ensure(
        stored["query"] == query,
        format!(
            "The smart collection stores {}, the view showed {query}",
            stored["query"]
        ),
    )?;
    let renamed = after["folders"]["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|folder| folder["id"] == expected["folder"]["id"])
        .cloned()
        .ok_or("The folder is gone")?;
    ensure(
        renamed["name"] == RENAMED && renamed["parent_id"] == expected["nest_in"]["id"],
        format!("The folder is left as {renamed}"),
    )?;
    ensure(
        after["moved"].as_u64() == expected["move_to"]["count"].as_u64().map(|count| count + 1),
        format!(
            "{MOVE_TO} holds {}, it held {} before one was moved to it",
            after["moved"], expected["move_to"]["count"]
        ),
    )?;
    let print = after["collections"]["collections"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|collection| collection["id"] == expected["add_to"]["id"])
        .cloned()
        .unwrap_or_default();
    ensure(
        print["count"] == expected["add_to"]["count"],
        format!(
            "{ADD_TO} holds {}, not {} as before",
            print["count"], expected["add_to"]["count"]
        ),
    )?;
    checks.note(
        launch.at(LAST)?,
        "the catalog the editor left: the smart collection's query as shown, the folder renamed and nested, the photograph moved, the add undone, one preset entry on each photograph the preset reached, the edited ones refused a send-back with the reason the desktop said, the sent-back file picked again, Removed empty",
        json!({"smart": stored, "folder": renamed, "moved": after["moved"], "print": print, "histories": after["histories"], "counts": after["counts"], "refusals": after["refusals"], "repicked": repicked}),
    );
    Ok(())
}
