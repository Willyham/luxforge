//! Resolving missing originals through the catalog owner, as clients call it: `source.missing`
//! grouping by source folder with each reason, `source.find` over moved, rewritten, duplicated,
//! deleted and claimed files with its progress, partial report and cancel, `source.relink`
//! committing only what a find verified as one undoable change, and, on macOS, a detached disk
//! image. Every original is a copied fixture in a scratch directory.
use super::super::{OwnerMessage, catalog::CatalogMessage};
use super::{Hold, LibraryMessage};
use crate::{
    AssetId, EditorService, ModuleRegistry,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientId, OwnerHandle},
    library::locate::Phase,
};
use luxforge_testbase::{
    Gate,
    paths::{fixture, temp_dir},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

/// The actor every client request here is made as.
const ACTOR: &str = "resolve-client";

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

/// One owner over a catalog in its own scratch directory.
struct Harness {
    dir: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("resolve-missing-{name}"))
            .canonicalize()
            .unwrap();
        let catalog = dir.join("catalog.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
        let client = owner.register();
        Self {
            dir,
            owner,
            join: Some(join),
            client,
        }
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.send(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, method: &str, params: Value) -> ApiFailure {
        self.send(method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// Read a job until it leaves `queued` and `running`, as a client would.
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// A copy of the fixture `name` (`s0/…`) at `path`, its folder made.
    fn copy(&self, name: &str, path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(fixture(&format!("s0/{name}")), path).unwrap();
        path.canonicalize().unwrap()
    }

    /// Develop the file at `path` and prepare its photograph, as a client opens a file.
    fn import(&self, path: &Path) -> AssetId {
        serde_json::from_value(super::opening::import(&self.owner, self.client, path)).unwrap()
    }

    fn state(&self, asset: &AssetId) -> Value {
        self.ok("asset.state", json!({"asset_id": asset}))
    }

    fn locator(&self, asset: &AssetId) -> PathBuf {
        PathBuf::from(self.state(asset)["asset"]["locator"].as_str().unwrap())
    }

    /// Check `assets` and wait for the job, answering each one's availability in order.
    fn check(&self, assets: &[&AssetId]) -> Vec<String> {
        let started = self.ok(
            "source.check",
            json!({"targets": {"kind": "assets", "asset_ids": assets}}),
        );
        let settled = self.settle(&started["job_id"]);
        assert_eq!(settled["status"], "ready", "{settled}");
        settled["result"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["availability"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Start a find, answering its job once it settles.
    fn find(&self, params: Value) -> Value {
        let started = self.ok("source.find", params);
        assert_eq!(started["deduplicated"], false);
        let settled = self.settle(&started["job_id"]);
        assert_eq!(settled["kind"], "source-find", "{settled}");
        settled
    }

    /// A finished find's rows.
    fn found(&self, params: Value) -> Vec<Value> {
        let job = self.find(params);
        assert_eq!(job["status"], "ready", "{job}");
        job["result"]["rows"].as_array().unwrap().clone()
    }

    fn relink(&self, pairs: &[(&AssetId, &Path)], request_id: &str) -> ApiResponse {
        let pairs: Vec<Value> = pairs
            .iter()
            .map(|(asset, path)| json!({"asset_id": asset, "path": path}))
            .collect();
        self.send(
            "source.relink",
            json!({"pairs": pairs, "mutation": envelope(request_id)}),
        )
    }

    fn sequence(&self) -> u64 {
        self.ok("events.since", json!({"after": 0}))["current_sequence"]
            .as_u64()
            .unwrap()
    }

    fn events_after(&self, after: u64) -> Vec<Value> {
        self.ok("events.since", json!({"after": after}))["events"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn journal(&self) -> Value {
        self.ok("library.journal", json!({"limit": 500}))
    }

    /// The newest library change, with its rows.
    fn last_change(&self) -> Value {
        let journal = self.journal();
        let sequence = journal["changes"].as_array().unwrap().last().unwrap()["sequence"].clone();
        self.ok("library.inspect", json!({"sequence": sequence}))
    }

    fn hold(&self, hold: Option<Hold>) {
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(
                LibraryMessage::Hold(hold),
            )))
            .unwrap();
    }

    /// Hold every library job dispatched from now on once it has read `files` files whole, as it
    /// is about to read the next chunk, until the gate opens.
    fn hold_after(&self, files: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let held = gate.clone();
        let hashed = Arc::new(AtomicUsize::new(0));
        self.hold(Some(Arc::new(move |phase| match phase {
            Phase::Hashed => {
                hashed.fetch_add(1, Ordering::SeqCst);
            }
            Phase::Hashing if hashed.load(Ordering::SeqCst) >= files => held.pass(),
            _ => {}
        })));
        gate
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// What a row of a report says, as `(result, path or paths or by)`.
fn row(rows: &[Value], index: usize) -> (String, Value) {
    let row = &rows[index];
    let result = row["result"].as_str().unwrap().to_owned();
    let detail = match result.as_str() {
        "found" | "different-bytes" => row["path"].clone(),
        "several-identical" => row["paths"].clone(),
        "claimed" => json!([row["path"], row["by"]]),
        _ => Value::Null,
    };
    (result, detail)
}

/// A folder of photographs moved elsewhere, one of them into a subfolder, is listed under the
/// folder it was developed from, found by a search of the new place by name, size and
/// fingerprint without anything changing, and relinked in one library change, announced once,
/// each photograph's source folder becoming its file's folder and its history untouched; a retry
/// answers the first change, undo points every photograph back and redo forward again, and no file
/// is written.
#[test]
fn a_moved_folder_is_found_and_relinked_in_one_change_and_undone() {
    let harness = Harness::new("moved");
    let card = harness.dir.join("card").join("2026-09-12");
    let names = ["DSC_0001.jpg", "DSC_0002.jpg", "DSC_0003.jpg"];
    let originals: Vec<PathBuf> = names
        .iter()
        .zip([
            "orientation-1.jpg",
            "orientation-2.jpg",
            "orientation-3.jpg",
        ])
        .map(|(name, fixture)| harness.copy(fixture, &card.join(name)))
        .collect();
    let bytes: Vec<Vec<u8>> = originals
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let assets: Vec<AssetId> = originals.iter().map(|path| harness.import(path)).collect();
    let refs: Vec<&AssetId> = assets.iter().collect();
    let before: Vec<Value> = assets.iter().map(|asset| harness.state(asset)).collect();

    // The folder moves into the archive, and one photograph into a subfolder of it.
    let archive = harness.dir.join("archive");
    let moved = archive.join("2026").join("Konstanz");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&card, &moved).unwrap();
    fs::create_dir_all(moved.join("picked")).unwrap();
    fs::rename(
        moved.join("DSC_0003.jpg"),
        moved.join("picked").join("DSC_0003.jpg"),
    )
    .unwrap();
    let now: Vec<PathBuf> = [
        moved.join("DSC_0001.jpg"),
        moved.join("DSC_0002.jpg"),
        moved.join("picked").join("DSC_0003.jpg"),
    ]
    .iter()
    .map(|path| path.canonicalize().unwrap())
    .collect();
    assert_eq!(harness.check(&refs), ["missing", "missing", "missing"]);

    let missing = harness.ok("source.missing", json!({}));
    assert_eq!(missing["count"], 3, "{missing}");
    let groups = missing["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{missing}");
    assert_eq!(groups[0]["source_folder"], json!(card));
    assert_eq!(groups[0]["count"], 3);
    assert_eq!(groups[0]["reason"], json!({"kind": "folder-gone"}));
    // An opened file the index does not list is an undated frame of its folder, and its Develop
    // makes a catalog folder named after that folder on disk.
    assert_eq!(groups[0]["catalog_folders"][0]["name"], "2026-09-12");
    assert_eq!(
        harness.ok("source.missing", json!({"grouping": "source-folder"})),
        missing
    );

    // The search changes nothing.
    let journal = harness.journal();
    let sequence = harness.sequence();
    let rows = harness.found(json!({"search_root": archive, "source_folder": card}));
    assert_eq!(rows.len(), 3);
    for (index, name) in names.iter().enumerate() {
        assert_eq!(rows[index]["asset_id"], json!(assets[index]));
        assert_eq!(rows[index]["file_name"], *name);
        assert_eq!(row(&rows, index), ("found".into(), json!(now[index])));
    }
    assert_eq!(harness.journal(), journal);
    assert!(harness.events_after(sequence).is_empty());
    assert_eq!(harness.locator(&assets[0]), originals[0]);

    // Relinked, all three in one change.
    let pairs: Vec<(&AssetId, &Path)> = assets
        .iter()
        .zip(&now)
        .map(|(asset, path)| (asset, path.as_path()))
        .collect();
    let answer = harness.relink(&pairs, "relink-1").result.unwrap();
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(answer["items"], 3);
    assert_eq!(answer["deduplicated"], false);
    let change = answer["change"].as_u64().unwrap();
    for (index, asset) in assets.iter().enumerate() {
        let state = harness.state(asset);
        assert_eq!(state["asset"]["locator"], json!(now[index]));
        assert_eq!(
            state["asset"]["source_root"],
            json!(now[index].parent().unwrap())
        );
        for field in ["revision", "current_entry"] {
            assert_eq!(state[field], before[index][field], "{field}");
        }
        assert_eq!(
            state["asset"]["fingerprint"],
            before[index]["asset"]["fingerprint"]
        );
    }
    let detail = harness.last_change();
    assert_eq!(detail["change"]["sequence"], change);
    assert_eq!(detail["change"]["method"], "source.relink");
    assert_eq!(detail["change"]["actor"], ACTOR);
    assert_eq!(detail["change"]["label"], "Relinked 3 originals");
    let changed = detail["rows"].as_array().unwrap();
    assert_eq!(changed.len(), 3);
    for (index, changed) in changed.iter().enumerate() {
        assert_eq!(
            changed["item"],
            json!({"kind": "asset-source", "asset_id": assets[index]})
        );
        assert_eq!(changed["before"]["source_folder"], json!(card));
        assert_eq!(
            changed["after"]["source_folder"],
            json!(now[index].parent().unwrap())
        );
    }
    let events = harness.events_after(sequence);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["method"], "source.relink");
    assert_eq!(events[0]["library_sequence"], change);
    assert_eq!(harness.ok("source.missing", json!({}))["count"], 0);
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        0
    );

    // A retry answers the first change and announces nothing.
    let sequence = harness.sequence();
    let retry = harness.relink(&pairs, "relink-1").result.unwrap();
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(retry["change"], change);
    assert!(harness.events_after(sequence).is_empty());

    // Undo points every photograph back where it was; redo forward again.
    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo-1")}));
    assert_eq!(undone["outcome"], "applied", "{undone}");
    assert_eq!(undone["items"], 3);
    for (index, asset) in assets.iter().enumerate() {
        assert_eq!(harness.locator(asset), originals[index]);
    }
    let redone = harness.ok("library.redo", json!({"mutation": envelope("redo-1")}));
    assert_eq!(redone["outcome"], "applied", "{redone}");
    for (index, asset) in assets.iter().enumerate() {
        assert_eq!(harness.locator(asset), now[index]);
        assert_eq!(fs::read(&now[index]).unwrap(), bytes[index], "only read");
    }
}

/// Each photograph gets its own result: a moved file is found; a rewritten file of the same name
/// and length, and one of another length, differ and are left as they are; a duplicated file is
/// two identical candidates that wait for a choice; a deleted file is not found; and a file with
/// the photograph's bytes that another photograph already names is claimed. A relink commits only
/// what was verified and free: a pair naming a file of other bytes, or a claimed file, refuses the
/// whole request, naming it, and changes nothing; the chosen identical copy is relinked.
#[test]
fn each_photograph_gets_its_own_result_and_only_what_was_verified_relinks() {
    let harness = Harness::new("results");
    let card = harness.dir.join("card");
    let search = harness.dir.join("search");
    let named = [
        ("moved.jpg", "orientation-1.jpg"),
        ("rewritten.jpg", "orientation-2.jpg"),
        ("longer.jpg", "orientation-3.jpg"),
        ("duplicated.jpg", "orientation-4.jpg"),
        ("deleted.jpg", "orientation-5.jpg"),
        ("claimed.jpg", "orientation-6.jpg"),
    ];
    let originals: Vec<PathBuf> = named
        .iter()
        .map(|(name, fixture)| harness.copy(fixture, &card.join(name)))
        .collect();
    let assets: Vec<AssetId> = originals.iter().map(|path| harness.import(path)).collect();
    let [moved, rewritten, longer, duplicated, deleted, claimed] = &assets[..] else {
        unreachable!()
    };

    let place = |from: &Path, to: &Path| {
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(from, to).unwrap();
        to.canonicalize().unwrap()
    };
    let moved_to = place(&originals[0], &search.join("a").join("moved.jpg"));
    // Another photograph of the same length under the same name.
    fs::remove_file(&originals[1]).unwrap();
    let rewritten_at = harness.copy("orientation-7.jpg", &search.join("b").join("rewritten.jpg"));
    // The original with bytes added.
    let mut added = fs::read(&originals[2]).unwrap();
    added.extend_from_slice(b"rewritten metadata");
    fs::remove_file(&originals[2]).unwrap();
    fs::create_dir_all(search.join("b")).unwrap();
    fs::write(search.join("b").join("longer.jpg"), added).unwrap();
    let longer_at = search.join("b").join("longer.jpg").canonicalize().unwrap();
    let copies = [
        place(&originals[3], &search.join("c").join("duplicated.jpg")),
        harness.copy(
            "orientation-4.jpg",
            &search.join("d").join("duplicated.jpg"),
        ),
    ];
    fs::remove_file(&originals[4]).unwrap();
    // A file with the original's bytes that another photograph names: that photograph's own
    // file, since overwritten with a copy of the original (a Develop never makes two photographs
    // of one file's bytes).
    let taken = harness.copy("greyscale.jpg", &search.join("e").join("claimed.jpg"));
    let other = harness.import(&taken);
    fs::write(&taken, fs::read(&originals[5]).unwrap()).unwrap();
    fs::remove_file(&originals[5]).unwrap();

    let refs: Vec<&AssetId> = assets.iter().collect();
    assert_eq!(harness.check(&refs), ["missing"; 6]);
    let missing = harness.ok("source.missing", json!({}));
    assert_eq!(missing["count"], 6);
    assert_eq!(
        missing["groups"][0]["reason"],
        json!({"kind": "files-gone"}),
        "the folder is there and its files are not: {missing}"
    );

    let rows = harness.found(json!({
        "search_root": search,
        "targets": {"kind": "assets", "asset_ids": refs},
    }));
    assert_eq!(rows.len(), 6);
    assert_eq!(row(&rows, 0), ("found".into(), json!(moved_to)));
    assert_eq!(
        row(&rows, 1),
        ("different-bytes".into(), json!(rewritten_at))
    );
    assert_eq!(row(&rows, 2), ("different-bytes".into(), json!(longer_at)));
    assert_eq!(row(&rows, 3), ("several-identical".into(), json!(copies)));
    assert_eq!(row(&rows, 4), ("not-found".into(), Value::Null));
    assert_eq!(row(&rows, 5), ("claimed".into(), json!([taken, other])));

    let journal = harness.journal();
    let sequence = harness.sequence();
    let unchanged = |harness: &Harness| {
        assert_eq!(harness.journal(), journal);
        assert!(harness.events_after(sequence).is_empty());
        assert_eq!(harness.locator(moved), originals[0]);
    };
    // A file of other bytes was never verified: the whole request is refused, naming it.
    let refusal = harness
        .relink(
            &[(moved, &moved_to), (rewritten, &rewritten_at)],
            "rewritten",
        )
        .error
        .unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    let data = refusal.data.unwrap();
    assert_eq!(data["count"], 1);
    assert_eq!(
        data["pairs"],
        json!([{"asset_id": rewritten, "path": rewritten_at, "reason": "not-verified"}])
    );
    unchanged(&harness);
    // A file another photograph names is never taken, even with the same bytes.
    let refusal = harness
        .relink(&[(claimed, &taken)], "claimed")
        .error
        .unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    assert!(
        refusal.message.contains(other.as_str()),
        "{}",
        refusal.message
    );
    assert_eq!(
        refusal.data.unwrap()["pairs"],
        json!([{"asset_id": claimed, "path": taken, "reason": "claimed", "by": other}])
    );
    let refusal = harness
        .relink(&[(deleted, &longer_at)], "deleted")
        .error
        .unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    unchanged(&harness);
    assert_eq!(harness.locator(claimed), originals[5]);
    assert_eq!(harness.locator(&other), taken);

    // The moved photograph and the chosen identical copy are relinked together.
    let answer = harness
        .relink(&[(moved, &moved_to), (duplicated, &copies[1])], "chosen")
        .result
        .unwrap();
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(answer["items"], 2);
    assert_eq!(harness.locator(moved), moved_to);
    assert_eq!(harness.locator(duplicated), copies[1]);
    assert_eq!(harness.locator(longer), originals[2]);
    assert_eq!(
        harness.last_change()["change"]["label"],
        "Relinked 2 originals"
    );
    assert_eq!(harness.ok("source.missing", json!({}))["count"], 4);
    // The copy not chosen is left as it was.
    assert_eq!(
        fs::read(&copies[0]).unwrap(),
        fs::read(fixture("s0/orientation-4.jpg")).unwrap()
    );
}

/// `source.missing` groups every photograph whose original was recorded missing by the folder on
/// disk it was developed from, in folder order, with the catalog folders its photographs are in
/// now and why: a folder gone from its volume, a folder whose files are gone (one left, or a mix
/// of missing and replaced), and a folder whose every file was replaced. A photograph nothing has
/// checked yet is listed once a refusal records it.
#[test]
fn missing_originals_are_grouped_by_source_folder_with_each_reason() {
    let harness = Harness::new("groups");
    let photos = harness.dir.join("photos");
    let layout = [
        ("gone", "g1.jpg", "orientation-1.jpg"),
        ("gone", "g2.jpg", "orientation-2.jpg"),
        ("thinned", "t1.jpg", "orientation-3.jpg"),
        ("thinned", "t2.jpg", "orientation-4.jpg"),
        ("changed", "c1.jpg", "orientation-5.jpg"),
        ("changed", "c2.jpg", "orientation-6.jpg"),
        ("mixed", "m1.jpg", "orientation-7.jpg"),
        ("mixed", "m2.jpg", "orientation-8.jpg"),
        ("unchecked", "u1.jpg", "portrait.jpg"),
    ];
    let paths: Vec<PathBuf> = layout
        .iter()
        .map(|(folder, name, fixture)| harness.copy(fixture, &photos.join(folder).join(name)))
        .collect();
    let assets: Vec<AssetId> = paths.iter().map(|path| harness.import(path)).collect();
    // One photograph of the gone folder is organized into another catalog folder.
    let konstanz = harness.ok(
        "folder.create",
        json!({"name": "Konstanz", "mutation": envelope("konstanz")}),
    )["folder"]["id"]
        .clone();
    harness.ok(
        "asset.move",
        json!({"targets": {"kind": "assets", "asset_ids": [assets[1]]}, "folder_id": konstanz, "mutation": envelope("move")}),
    );

    fs::remove_dir_all(photos.join("gone")).unwrap();
    fs::remove_file(&paths[2]).unwrap();
    let replace = |path: &Path, fixture_name: &str| {
        fs::remove_file(path).unwrap();
        fs::copy(fixture(&format!("s0/{fixture_name}")), path).unwrap();
    };
    replace(&paths[4], "srgb.jpg");
    replace(&paths[5], "cmyk.jpg");
    fs::remove_file(&paths[6]).unwrap();
    replace(&paths[7], "greyscale.jpg");
    fs::remove_file(&paths[8]).unwrap();

    let checked: Vec<&AssetId> = assets[..8].iter().collect();
    assert_eq!(
        harness.check(&checked),
        [
            "missing",
            "missing",
            "missing",
            "available",
            "changed",
            "changed",
            "missing",
            "changed"
        ]
    );
    let missing = harness.ok("source.missing", json!({}));
    let summary = |missing: &Value| -> Vec<(String, u64, Value, Vec<String>)> {
        missing["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| {
                (
                    Path::new(group["source_folder"].as_str().unwrap())
                        .strip_prefix(&photos)
                        .unwrap()
                        .display()
                        .to_string(),
                    group["count"].as_u64().unwrap(),
                    group["reason"].clone(),
                    group["catalog_folders"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|folder| folder["name"].as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect()
    };
    let expected = vec![
        (
            "changed".to_owned(),
            2,
            json!({"kind": "changed"}),
            vec!["changed".to_owned()],
        ),
        (
            "gone".to_owned(),
            2,
            json!({"kind": "folder-gone"}),
            vec!["gone".to_owned(), "Konstanz".to_owned()],
        ),
        (
            "mixed".to_owned(),
            2,
            json!({"kind": "files-gone"}),
            vec!["mixed".to_owned()],
        ),
        (
            "thinned".to_owned(),
            1,
            json!({"kind": "files-gone"}),
            vec!["thinned".to_owned()],
        ),
    ];
    assert_eq!(summary(&missing), expected, "{missing}");
    assert_eq!(missing["count"], 7);
    assert!(
        missing["groups"][0]["volume_id"]
            .as_str()
            .is_some_and(|volume| volume.starts_with("volume-")),
        "{missing}"
    );
    assert_eq!(
        missing["groups"][1]["catalog_folders"][1]["id"], konstanz,
        "{missing}"
    );

    // Never checked, the last photograph is not listed until a refusal records it.
    let refusal = harness.refused("source.prepare", json!({"asset_id": assets[8]}));
    assert_eq!(refusal.code, "source-unavailable");
    let missing = harness.ok("source.missing", json!({}));
    assert_eq!(missing["count"], 8);
    let mut with_unchecked = expected;
    with_unchecked.push((
        "unchecked".to_owned(),
        1,
        json!({"kind": "files-gone"}),
        vec!["unchecked".to_owned()],
    ));
    assert_eq!(summary(&missing), with_unchecked);
}

/// A search shows its progress and its report so far while it runs — each photograph's result as
/// soon as its files are read, `checking` for the rest — on `job.read` and the activity board; a
/// cancel between files ends it `cancelled`, remembering nothing, so a relink of what it showed is
/// refused and nothing changes. A search run again to its end is relinked.
#[test]
fn a_cancelled_search_shows_its_progress_and_changes_nothing() {
    let harness = Harness::new("cancel");
    let card = harness.dir.join("card");
    let search = harness.dir.join("search");
    let assets: Vec<AssetId> = [
        "orientation-1.jpg",
        "orientation-2.jpg",
        "orientation-3.jpg",
    ]
    .iter()
    .enumerate()
    .map(|(index, fixture)| {
        harness.import(&harness.copy(fixture, &card.join(format!("{index}.jpg"))))
    })
    .collect();
    fs::rename(&card, &search).unwrap();
    let refs: Vec<&AssetId> = assets.iter().collect();
    harness.check(&refs);
    let first = search.join("0.jpg").canonicalize().unwrap();
    let before: Vec<Value> = assets.iter().map(|asset| harness.state(asset)).collect();
    let journal = harness.journal();
    let sequence = harness.sequence();

    // Held as it is about to read the second file, the first photograph settled.
    let gate = harness.hold_after(1);
    let started = harness.ok(
        "source.find",
        json!({"search_root": search, "targets": {"kind": "assets", "asset_ids": refs}}),
    );
    gate.wait_reached(1, "the search's second file");
    let read = harness.ok("job.read", json!({"job_id": started["job_id"]}));
    assert_eq!(read["status"], "running", "{read}");
    assert_eq!(read["kind"], "source-find");
    assert_eq!(read["progress"]["message"], "1 of 3 photographs checked");
    let fraction = read["progress"]["fraction"].as_f64().unwrap();
    assert!((fraction - 1.0 / 3.0).abs() < 1e-6, "{read}");
    let rows = read["result"]["rows"].as_array().unwrap();
    assert_eq!(row(rows, 0), ("found".into(), json!(first)));
    assert_eq!(rows[1]["result"], "checking");
    assert_eq!(rows[2]["result"], "checking");
    let activity = harness.ok("activity.list", json!({}));
    let entry = activity["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["job_id"] == started["job_id"])
        .unwrap_or_else(|| panic!("the search is on the board: {activity}"));
    assert_eq!(entry["kind"], "source.find");
    assert_eq!(entry["label"], "Finding originals");
    assert_eq!(
        entry["detail"],
        format!(
            "3 photographs in {}",
            search.canonicalize().unwrap().display()
        )
    );
    assert_eq!(entry["phase"], "verifying");
    assert_eq!(entry["progress"]["message"], "1 of 3 photographs checked");

    let cancelled = harness.ok("job.cancel", json!({"job_id": started["job_id"]}));
    assert_eq!(cancelled["kind"], "source-find");
    gate.open();
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "cancelled", "{settled}");
    assert_eq!(settled.get("result"), None, "{settled}");
    harness.hold(None);

    // What the stopped search showed was never remembered: a relink of it changes nothing.
    let refusal = harness
        .relink(&[(&assets[0], &first)], "stopped")
        .error
        .unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    assert_eq!(refusal.data.unwrap()["pairs"][0]["reason"], "not-verified");
    for (index, asset) in assets.iter().enumerate() {
        assert_eq!(harness.state(asset), before[index]);
    }
    assert_eq!(harness.journal(), journal);
    assert!(harness.events_after(sequence).is_empty());

    // Searched again to its end, it is relinked.
    let rows = harness
        .found(json!({"search_root": search, "targets": {"kind": "assets", "asset_ids": refs}}));
    assert!(rows.iter().all(|row| row["result"] == "found"), "{rows:?}");
    let answer = harness
        .relink(&[(&assets[0], &first)], "finished")
        .result
        .unwrap();
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(harness.last_change()["change"]["label"], "Relinked 0.jpg");
    assert_eq!(harness.locator(&assets[0]), first);
}

/// A relink commits exactly what a search verified: a file touched since is refused and forgotten,
/// a file gone since is `source-unavailable`, and either leaves every pair of the request as it
/// was; a file searched again is relinked. Malformed requests are refused before anything is
/// looked at.
#[test]
fn a_relink_of_what_changed_since_its_search_changes_nothing() {
    let harness = Harness::new("changed-since");
    let card = harness.dir.join("card");
    let search = harness.dir.join("search");
    let a = harness.import(&harness.copy("orientation-1.jpg", &card.join("a.jpg")));
    let b = harness.import(&harness.copy("orientation-2.jpg", &card.join("b.jpg")));
    fs::rename(&card, &search).unwrap();
    let (a_at, b_at) = (
        search.join("a.jpg").canonicalize().unwrap(),
        search.join("b.jpg").canonicalize().unwrap(),
    );
    harness.check(&[&a, &b]);
    let targets = json!({"kind": "assets", "asset_ids": [a, b]});
    let rows = harness.found(json!({"search_root": search, "targets": targets}));
    assert_eq!(row(&rows, 0), ("found".into(), json!(a_at)));
    assert_eq!(row(&rows, 1), ("found".into(), json!(b_at)));
    let journal = harness.journal();

    // Touched since it was verified: refused, naming it, and nothing changes, b included.
    let file = fs::OpenOptions::new().append(true).open(&a_at).unwrap();
    let len = file.metadata().unwrap().len();
    file.set_len(len + 1).unwrap();
    file.set_len(len).unwrap();
    let refusal = harness
        .relink(&[(&a, &a_at), (&b, &b_at)], "touched")
        .error
        .unwrap();
    assert_eq!(refusal.code, "conflict", "{refusal:?}");
    assert!(
        refusal.message.contains("changed after it was verified"),
        "{}",
        refusal.message
    );
    assert_eq!(
        refusal.data.unwrap(),
        json!({"pairs": [{"asset_id": a, "path": a_at, "reason": "changed"}], "count": 1})
    );
    assert_eq!(harness.journal(), journal);
    assert_eq!(harness.locator(&b), card.join("b.jpg"));
    // It was forgotten: it must be searched again.
    let refusal = harness
        .relink(&[(&a, &a_at)], "touched-again")
        .error
        .unwrap();
    assert_eq!(refusal.data.unwrap()["pairs"][0]["reason"], "not-verified");

    // Gone since: source-unavailable.
    fs::remove_file(&b_at).unwrap();
    let refusal = harness.relink(&[(&b, &b_at)], "gone").error.unwrap();
    assert_eq!(refusal.code, "source-unavailable", "{refusal:?}");
    assert_eq!(refusal.data.unwrap()["pairs"][0]["reason"], "gone");
    assert_eq!(harness.journal(), journal);

    // Searched again: a is found with its new signature and relinked; b is not found.
    let rows = harness.found(json!({"search_root": search, "targets": targets}));
    assert_eq!(row(&rows, 1), ("not-found".into(), Value::Null));
    let answer = harness
        .relink(&[(&a, &a_at)], "searched-again")
        .result
        .unwrap();
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(harness.locator(&a), a_at);
    // A pair whose photograph already names its file needs nothing.
    let again = harness.relink(&[(&a, &a_at)], "already").result.unwrap();
    assert_eq!(again["outcome"], "no-op", "{again}");

    // Malformed requests.
    let relink = |pairs: Value| {
        harness.refused(
            "source.relink",
            json!({"pairs": pairs, "mutation": envelope("malformed")}),
        )
    };
    for (pairs, code) in [
        (json!([]), "validation"),
        (
            json!([{"asset_id": a, "path": "search/a.jpg"}]),
            "validation",
        ),
        (
            json!([{"asset_id": a, "path": a_at}, {"asset_id": a, "path": b_at}]),
            "validation",
        ),
        (
            json!([{"asset_id": AssetId::new(), "path": a_at}]),
            "validation",
        ),
    ] {
        let refusal = relink(pairs.clone());
        assert_eq!(refusal.code, code, "{pairs}: {refusal:?}");
    }
    let many: Vec<Value> = (0..crate::catalog_types::MAX_LIBRARY_BATCH + 1)
        .map(|_| json!({"asset_id": a, "path": a_at}))
        .collect();
    assert_eq!(relink(json!(many)).code, "resource-limit");
    assert_eq!(harness.locator(&a), a_at);
}

/// A search is refused before it starts unless it names exactly one of targets and source_folder,
/// an absolute folder that can be read, and something to look for.
#[test]
fn a_search_names_one_readable_folder_and_what_to_look_for() {
    let harness = Harness::new("refusals");
    let photo = harness.copy("orientation-1.jpg", &harness.dir.join("card").join("a.jpg"));
    let asset = harness.import(&photo);
    let targets = json!({"kind": "assets", "asset_ids": [asset]});
    let card = harness.dir.join("card");
    for (params, code) in [
        (json!({"search_root": harness.dir}), "validation"),
        (
            json!({"search_root": harness.dir, "targets": targets, "source_folder": card}),
            "validation",
        ),
        (
            json!({"search_root": "card", "targets": targets}),
            "validation",
        ),
        (
            json!({"search_root": photo, "targets": targets}),
            "validation",
        ),
        (
            json!({"search_root": harness.dir.join("nowhere"), "targets": targets}),
            "read-error",
        ),
        (
            json!({"search_root": harness.dir, "source_folder": card}),
            "validation",
        ),
        (
            json!({"search_root": harness.dir, "targets": {"kind": "assets", "asset_ids": [AssetId::new()]}}),
            "validation",
        ),
    ] {
        let refusal = harness.refused("source.find", params.clone());
        assert_eq!(refusal.code, code, "{params}: {refusal:?}");
    }
    // An available photograph can be looked for too: its own file is found where it is.
    let rows = harness.found(json!({"search_root": harness.dir, "targets": targets}));
    assert_eq!(row(&rows, 0), ("found".into(), json!(photo)));
    assert_eq!(
        harness.relink(&[(&asset, &photo)], "own").result.unwrap()["outcome"],
        "no-op"
    );
}

/// On the Mac, with a disk image: a detached image's photographs group as `volume-offline` naming
/// it; a search of a folder does not cross into the image mounted inside it; a search on the image
/// fails cleanly when the image is detached while it reads, changing nothing; and re-attached, the
/// original is found on the image and relinked onto that volume.
#[cfg(target_os = "macos")]
#[test]
fn a_detached_disk_image_is_volume_offline_and_a_search_on_it_fails_cleanly() {
    use crate::library::test_disk::DiskImage;

    let harness = Harness::new("disk-image");
    let mut disk = DiskImage::create(&harness.dir, "LuxforgeFind");
    let mount = disk.mount.canonicalize().unwrap();
    let dcim = mount.join("DCIM");
    let images: Vec<AssetId> = [
        ("one.jpg", "orientation-1.jpg"),
        ("two.jpg", "orientation-2.jpg"),
    ]
    .iter()
    .map(|(name, fixture)| harness.import(&harness.copy(fixture, &dcim.join(name))))
    .collect();
    let local = harness.copy(
        "orientation-3.jpg",
        &harness.dir.join("local").join("three.jpg"),
    );
    let three = harness.import(&local);

    // Detached: the image's photographs are one group, offline, naming the volume.
    disk.detach();
    assert_eq!(
        harness.check(&[&images[0], &images[1]]),
        ["offline", "offline"]
    );
    let missing = harness.ok("source.missing", json!({}));
    assert_eq!(missing["count"], 2, "{missing}");
    assert_eq!(missing["groups"][0]["source_folder"], json!(dcim));
    assert_eq!(
        missing["groups"][0]["reason"],
        json!({"kind": "volume-offline", "label": "LuxforgeFind"})
    );
    // A folder on it cannot be searched.
    let refusal = harness.refused(
        "source.find",
        json!({"search_root": dcim, "source_folder": dcim}),
    );
    assert_eq!(refusal.code, "read-error", "{refusal:?}");

    // Re-attached, the local original is moved onto the image. A search of the scratch folder,
    // which holds the image's mount point, does not cross into it.
    disk.attach();
    let on_image = harness.copy(
        "orientation-3.jpg",
        &mount.join("Archive").join("three.jpg"),
    );
    fs::remove_file(&local).unwrap();
    assert_eq!(harness.check(&[&three]), ["missing"]);
    let targets = json!({"kind": "assets", "asset_ids": [three]});
    let rows = harness.found(json!({"search_root": harness.dir, "targets": targets}));
    assert_eq!(row(&rows, 0), ("not-found".into(), Value::Null));

    // Detached while the search reads the file on it: the job fails, and nothing changes.
    let before = harness.state(&three);
    let journal = harness.journal();
    let gate = harness.hold_after(0);
    let started = harness.ok(
        "source.find",
        json!({"search_root": mount, "targets": targets}),
    );
    gate.wait_reached(1, "the search's read");
    disk.detach();
    gate.open();
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "failed", "{settled}");
    assert!(
        matches!(
            settled["error"]["code"].as_str(),
            Some("source-unavailable" | "read-error")
        ),
        "{settled}"
    );
    harness.hold(None);
    let refusal = harness
        .relink(&[(&three, &on_image)], "unplugged")
        .error
        .unwrap();
    assert_eq!(refusal.code, "source-unavailable", "{refusal:?}");
    assert_eq!(harness.state(&three), before);
    assert_eq!(harness.journal(), journal);

    // Re-attached: found on the image and relinked onto that volume.
    disk.attach();
    let rows = harness.found(json!({"search_root": mount, "targets": targets}));
    assert_eq!(row(&rows, 0), ("found".into(), json!(on_image)));
    let answer = harness
        .relink(&[(&three, &on_image)], "onto-image")
        .result
        .unwrap();
    assert_eq!(answer["outcome"], "applied", "{answer}");
    assert_eq!(harness.locator(&three), on_image);
    let detail = harness.last_change();
    assert_ne!(
        detail["rows"][0]["before"]["volume_id"], detail["rows"][0]["after"]["volume_id"],
        "{detail}"
    );
    assert_eq!(
        harness.state(&three)["current_entry"],
        before["current_entry"]
    );
    disk.detach();
}
