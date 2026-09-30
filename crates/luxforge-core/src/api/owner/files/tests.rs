//! The index lane through the catalog owner, as clients call it, over generated folders in scratch
//! directories: listing with exclusions and header metadata, reconciliation, the revision and its
//! events, cancellation, the file limit, a rebuilt index, volumes, cards, folders on disk, indexed
//! folders as library changes with undo and redo, and offline folders.
use super::{super::catalog::CatalogMessage, FilesMessage};
use crate::{
    ModuleRegistry,
    api::{ApiRequest, ApiResponse, ClientId, OwnerHandle, owner::OwnerMessage},
    export::metadata::header::tests::camera_jpeg,
    index::{database, volumes::MountSource, walk::WalkLimits},
};
use luxforge_testbase::{Gate, wait_for};
use luxforge_watch::Mount;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered")
}

fn ok(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = call(owner, client, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

fn refused(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> (String, String) {
    let error = call(owner, client, method, params)
        .error
        .unwrap_or_else(|| panic!("{method} was refused"));
    (error.code, error.message)
}

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "agent"})
}

/// Hand the files lane a test's message through the owner's own channel.
fn tell(owner: &OwnerHandle, message: FilesMessage) {
    owner
        .sender
        .send(OwnerMessage::Catalog(CatalogMessage::Files(message)))
        .unwrap();
}

/// The job's record once it has ended.
fn finished(owner: &OwnerHandle, client: ClientId, job_id: &Value) -> Value {
    wait_for("the index job to end", || {
        let job = ok(owner, client, "job.read", json!({"job_id": job_id}));
        (!matches!(job["status"].as_str(), Some("queued" | "running"))).then_some(job)
    })
}

/// Refresh `source` and answer the job's report, which must be ready.
fn refresh(owner: &OwnerHandle, client: ClientId, source: Value) -> Value {
    let started = ok(owner, client, "index.refresh", json!({"source": source}));
    let job = finished(owner, client, &started["job_id"]);
    assert_eq!(job["status"], "ready", "{job}");
    job["result"].clone()
}

/// Write `bytes` at `path`, making its folders.
fn put(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Every file under `dir`, links included as links, with its hash, length and modification time.
fn manifest(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u64, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.path().symlink_metadata().unwrap();
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                let bytes = std::fs::read(entry.path()).unwrap();
                out.insert(
                    entry.path(),
                    (
                        Sha256::digest(&bytes).to_vec(),
                        metadata.len(),
                        metadata.modified().unwrap(),
                    ),
                );
            }
        }
    }
    out
}

/// A scratch directory with a catalog, and the owner serving it.
struct Fixture {
    dir: PathBuf,
    catalog: PathBuf,
    owner: Option<(OwnerHandle, JoinHandle<()>)>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("files-{name}"))
            .canonicalize()
            .unwrap();
        let catalog = dir.join("catalog.sqlite");
        let mut fixture = Self {
            dir,
            catalog,
            owner: None,
        };
        fixture.start();
        fixture
    }

    fn start(&mut self) {
        self.owner = Some(
            OwnerHandle::start_with(&self.catalog, Arc::new(ModuleRegistry::builtin())).unwrap(),
        );
    }

    fn stop(&mut self) {
        if let Some((owner, join)) = self.owner.take() {
            owner.stop();
            join.join().unwrap();
        }
    }

    fn owner(&self) -> &OwnerHandle {
        &self.owner.as_ref().unwrap().0
    }

    fn index_dir(&self) -> PathBuf {
        crate::index::index_dir(&self.catalog)
    }

    /// Every row of the index: its path, id and header state, in path order.
    fn rows(&self) -> Vec<(PathBuf, i64, String)> {
        let connection = database::connect_at(&self.index_dir()).unwrap();
        let mut statement = connection
            .prepare("SELECT path, id, header_state FROM files ORDER BY path")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get(1)?,
                    row.get(2)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn revision(&self) -> u64 {
        database::revision(&database::connect_at(&self.index_dir()).unwrap()).unwrap()
    }

    fn row_id(&self, path: &Path) -> Option<i64> {
        self.rows()
            .into_iter()
            .find(|(row, ..)| row == path)
            .map(|(_, id, _)| id)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A tree of every kind the listing meets: camera JPEGs with full headers, a fixture JPEG, RAW names
/// holding no RAW, and every excluded kind, with links out of the root and a loop.
fn mixed_tree(root: &Path, outside: &Path) -> Vec<PathBuf> {
    let camera = camera_jpeg();
    let listed = [
        ("2026-09-27 Lake/DSC_0001.jpg", camera.as_slice()),
        ("2026-09-27 Lake/DSC_0002.JPG", camera.as_slice()),
        ("2026-09-27 Lake/deeper/DSC_0003.jpeg", camera.as_slice()),
        ("card dump/DSC_0004.NEF", b"not a raw file".as_slice()),
        ("card dump/L100.dng", b"not a dng".as_slice()),
    ];
    let fixture = std::fs::read(luxforge_testbase::paths::jpeg()).unwrap();
    put(&root.join("plain.jpg"), &fixture);
    for (name, bytes) in listed {
        put(&root.join(name), bytes);
    }
    for skipped in [
        ".hidden.jpg",
        ".thumbnails/a.jpg",
        "notes.txt",
        "sidecar.xmp",
        "Photos Library.photoslibrary/originals/b.jpg",
        "Lightroom/Catalog Previews.lrdata/c.jpg",
        "Lightroom/Catalog Smart Previews.lrdata/d.dng",
        "session/CaptureOne/Cache/e.jpg",
        "$RECYCLE.BIN/f.jpg",
        "Viewer.app/Contents/g.jpg",
    ] {
        put(&root.join(skipped), &camera);
    }
    put(&outside.join("outside.jpg"), &camera);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(outside, root.join("link to outside")).unwrap();
        symlink(outside.join("outside.jpg"), root.join("linked.jpg")).unwrap();
        symlink(root, root.join("2026-09-27 Lake/loop")).unwrap();
    }
    let mut expected: Vec<PathBuf> = listed
        .iter()
        .map(|(name, _)| root.join(name))
        .chain([root.join("plain.jpg")])
        .collect();
    expected.sort();
    expected
}

/// Every supported file is listed with its header metadata; links, other volumes, hidden files,
/// packages, caches, system folders and Luxforge's own directories (the catalog lives in the root)
/// are skipped; no file changes; and the job is on the activity board.
#[test]
fn index_refresh_lists_supported_files_with_their_headers_and_changes_nothing() {
    let mut fixture = Fixture::new("list");
    fixture.stop();
    let root = fixture.dir.join("photos");
    let outside = fixture.dir.join("outside");
    let expected = mixed_tree(&root, &outside);
    // The catalog inside the root: its index and artifact directories are Luxforge's own.
    fixture.catalog = root.join("catalog.sqlite");
    fixture.start();
    let owner = fixture.owner();
    let client = owner.register();
    // An artifact directory with a supported file, as a published artifact could be.
    put(
        &root.join("catalog.artifacts/objects/h.jpg"),
        &camera_jpeg(),
    );
    let before = manifest(&fixture.dir);

    let report = refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(report["roots"], json!([root]));
    assert_eq!(report["files"], 6, "{report}");
    assert_eq!(
        (
            &report["added"],
            &report["headers_read"],
            &report["unreadable"]
        ),
        (&json!(6), &json!(6), &json!(2)),
        "{report}"
    );
    let rows = fixture.rows();
    let paths: Vec<PathBuf> = rows.iter().map(|(path, ..)| path.clone()).collect();
    assert_eq!(paths, expected);
    for (path, _, state) in &rows {
        let raw = path
            .extension()
            .is_some_and(|ext| ext != "jpg" && ext != "JPG" && ext != "jpeg");
        assert_eq!(
            state,
            if raw { "unreadable" } else { "ok" },
            "{}",
            path.display()
        );
    }
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    let (_, id, _) = rows
        .iter()
        .find(|(path, ..)| path.ends_with("DSC_0001.jpg"))
        .unwrap();
    let record = database::file(&connection, crate::catalog_types::FileId(*id))
        .unwrap()
        .unwrap();
    let header = record.header.header().expect("a header");
    assert_eq!(header.camera.as_ref().unwrap().model, "NIKON Z 6");
    assert!(header.capture.is_some() && header.position.is_some());
    assert_eq!(header.lens.as_deref(), Some("NIKKOR Z 24-70mm f/4 S"));
    assert!(header.thumbnail.is_some(), "the thumbnail's location");
    let root_row = database::root(&connection, &root).unwrap().unwrap();
    assert_eq!(root_row.kind, crate::catalog_types::RootKind::Browsed);
    assert_eq!(root_row.file_count, Some(6));
    assert!(root_row.listed_ms.is_some() && !root_row.offline);

    // No file changed its bytes, length, time or place: every file there before is there after,
    // the catalog's own files aside.
    let catalog_files = |path: &Path| {
        path.starts_with(fixture.index_dir())
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("catalog.sqlite"))
    };
    for (path, entry) in before.iter().filter(|(path, _)| !catalog_files(path)) {
        assert_eq!(manifest_entry(path), *entry, "{} changed", path.display());
    }
    let after = manifest(&fixture.dir);
    assert!(
        after
            .keys()
            .filter(|path| !catalog_files(path))
            .eq(before.keys().filter(|path| !catalog_files(path)))
    );
}

fn manifest_entry(path: &Path) -> (Vec<u8>, u64, std::time::SystemTime) {
    let metadata = path.symlink_metadata().unwrap();
    (
        Sha256::digest(std::fs::read(path).unwrap()).to_vec(),
        metadata.len(),
        metadata.modified().unwrap(),
    )
}

/// A second listing keeps unchanged rows unread, reads changed and new files, drops vanished ones
/// and carries a moved file's row; returning to an unchanged folder reads nothing.
#[test]
fn reconciling_reads_only_what_changed_and_carries_moves() {
    let fixture = Fixture::new("reconcile");
    let owner = fixture.owner();
    let client = owner.register();
    let root = fixture.dir.join("photos");
    let camera = camera_jpeg();
    for name in ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "sub/e.jpg"] {
        put(&root.join(name), &camera);
    }
    let first = refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(
        (&first["added"], &first["headers_read"]),
        (&json!(5), &json!(5))
    );
    let moved_id = fixture.row_id(&root.join("d.jpg")).unwrap();
    let kept_id = fixture.row_id(&root.join("a.jpg")).unwrap();

    std::fs::write(root.join("b.jpg"), [camera.as_slice(), b"edited"].concat()).unwrap();
    std::fs::remove_file(root.join("c.jpg")).unwrap();
    std::fs::rename(root.join("d.jpg"), root.join("sub/renamed.jpg")).unwrap();
    put(&root.join("new/f.jpg"), &camera);
    let second = refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(
        [
            &second["files"],
            &second["added"],
            &second["changed"],
            &second["moved"],
            &second["removed"],
            &second["headers_read"]
        ],
        [
            &json!(5),
            &json!(1),
            &json!(1),
            &json!(1),
            &json!(1),
            &json!(2)
        ],
        "{second}"
    );
    assert_eq!(
        fixture.row_id(&root.join("sub/renamed.jpg")),
        Some(moved_id)
    );
    assert_eq!(fixture.row_id(&root.join("a.jpg")), Some(kept_id));
    assert_eq!(fixture.row_id(&root.join("c.jpg")), None);
    assert_eq!(fixture.rows().len(), 5);

    let third = refresh(owner, client, json!({"kind": "folder", "path": root}));
    for count in ["added", "changed", "moved", "removed", "headers_read"] {
        assert_eq!(third[count], 0, "{count}: {third}");
    }
}

/// Every batch advances the index's revision once, in its own transaction, and records one event
/// naming it; a listing too large for one batch commits several.
#[test]
fn every_batch_advances_the_revision_and_records_one_event() {
    let fixture = Fixture::new("revision");
    let owner = fixture.owner();
    let client = owner.register();
    let root = fixture.dir.join("photos");
    for index in 0..(crate::index::lane::INDEX_BATCH + 10) {
        put(
            &root.join(format!("{:03}/DSC_{index:04}.NEF", index / 100)),
            b"x",
        );
    }
    let start = ok(owner, client, "events.since", json!({"after": 0}))["current_sequence"]
        .as_u64()
        .unwrap();
    let report = refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(report["added"], crate::index::lane::INDEX_BATCH + 10);
    let events = ok(owner, client, "events.since", json!({"after": start}))["events"]
        .as_array()
        .unwrap()
        .clone();
    let revisions: Vec<u64> = events
        .iter()
        .filter_map(|event| event["index_revision"].as_u64())
        .collect();
    assert!(revisions.len() >= 2, "more than one batch: {events:?}");
    assert_eq!(
        revisions.len(),
        events.len(),
        "only index events: {events:?}"
    );
    // One event per batch, each naming the revision it left, then one as the job ended, naming
    // the revision the job left.
    let (ended, batches) = revisions.split_last().unwrap();
    assert_eq!(
        batches,
        (1..=batches.len() as u64).collect::<Vec<_>>(),
        "one revision per batch, one event per revision"
    );
    assert_eq!(
        ended,
        batches.last().unwrap(),
        "the job's end names the last"
    );
    assert_eq!(fixture.revision(), *ended);
    assert!(
        events
            .iter()
            .all(|event| event["method"] == "index.refresh")
    );
}

/// `job.cancel` stops a listing held mid-walk, with its progress on the activity board meanwhile;
/// the root is not marked listed and nothing vanished is dropped.
#[test]
fn job_cancel_stops_a_listing_mid_walk() {
    let fixture = Fixture::new("cancel");
    let owner = fixture.owner();
    let client = owner.register();
    let root = fixture.dir.join("photos");
    for folder in ["a", "b", "c"] {
        put(&root.join(folder).join("x.jpg"), &camera_jpeg());
    }
    let gate = Arc::new(Gate::new());
    tell(owner, FilesMessage::Hold(gate.clone()));
    gate.shut();
    let started = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": root}}),
    );
    gate.wait_reached(1, "the listing's first folder");
    let job = ok(
        owner,
        client,
        "job.read",
        json!({"job_id": started["job_id"]}),
    );
    assert_eq!(job["status"], "running");
    assert_eq!(job["kind"], "index-refresh");
    let board = ok(owner, client, "activity.list", json!({}));
    let entry = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "index.refresh")
        .expect("the listing is on the board");
    assert_eq!(entry["job_id"], started["job_id"]);
    assert_eq!(entry["label"], "Indexing");
    assert_eq!(entry["phase"], "listing");
    assert_eq!(entry["progress"]["message"], "0 files found", "{entry}");
    assert_eq!(entry["progress"].get("fraction"), None, "no extent yet");
    // The same source while it runs answers the same job.
    let again = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": root}}),
    );
    assert_eq!(
        (&again["job_id"], &again["deduplicated"]),
        (&started["job_id"], &json!(true))
    );

    ok(
        owner,
        client,
        "job.cancel",
        json!({"job_id": started["job_id"]}),
    );
    let job = finished(owner, client, &started["job_id"]);
    gate.open();
    assert_eq!(job["status"], "cancelled", "{job}");
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    assert!(
        database::root(&connection, &root).unwrap().is_none(),
        "not listed"
    );
    // The lane runs the next job.
    let report = refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(report["files"], 3);

    // Listing it again, the count is read against the extent the last listing found.
    gate.shut();
    let reached = gate.reached();
    let started = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": root}}),
    );
    gate.wait_reached(reached + 1, "the second listing's first folder");
    let board = ok(owner, client, "activity.list", json!({}));
    let entry = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["job_id"] == started["job_id"])
        .expect("the listing is on the board");
    assert_eq!(
        entry["progress"]["message"], "0 of about 3 files",
        "{entry}"
    );
    ok(
        owner,
        client,
        "job.cancel",
        json!({"job_id": started["job_id"]}),
    );
    assert_eq!(
        finished(owner, client, &started["job_id"])["status"],
        "cancelled"
    );
    gate.open();
}

/// Past its file limit a listing is refused with `resource-limit`.
#[test]
fn a_listing_past_its_file_limit_is_refused() {
    let fixture = Fixture::new("limit");
    let owner = fixture.owner();
    let client = owner.register();
    tell(
        owner,
        FilesMessage::Limits(WalkLimits {
            files: 3,
            ..WalkLimits::default()
        }),
    );
    let root = fixture.dir.join("photos");
    for index in 0..5 {
        put(&root.join(format!("{index}/a.jpg")), b"x");
    }
    let started = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "folder", "path": root}}),
    );
    let job = finished(owner, client, &started["job_id"]);
    assert_eq!(job["status"], "failed");
    assert_eq!(job["error"]["code"], "resource-limit", "{job}");
}

/// Deleting the index directory while Luxforge is closed loses nothing but time: listing again
/// rebuilds the same rows.
#[test]
fn a_deleted_index_rebuilds_the_same_content() {
    let mut fixture = Fixture::new("rebuild");
    let root = fixture.dir.join("photos");
    let camera = camera_jpeg();
    for name in ["a.jpg", "b/c.jpg", "b/d.NEF"] {
        put(&root.join(name), &camera);
    }
    let content = |fixture: &Fixture| -> Vec<_> {
        let connection = database::connect_at(&fixture.index_dir()).unwrap();
        fixture
            .rows()
            .into_iter()
            .map(|(_, id, _)| {
                let record = database::file(&connection, crate::catalog_types::FileId(id))
                    .unwrap()
                    .unwrap();
                format!(
                    "{:?}",
                    crate::catalog_types::FileRecord {
                        last_seen_ms: 0,
                        ..record
                    }
                )
            })
            .collect()
    };
    {
        let owner = fixture.owner();
        let client = owner.register();
        refresh(owner, client, json!({"kind": "folder", "path": root}));
    }
    let before = content(&fixture);
    assert_eq!(before.len(), 3);
    fixture.stop();
    std::fs::remove_dir_all(fixture.index_dir()).unwrap();
    fixture.start();
    let owner = fixture.owner();
    let client = owner.register();
    refresh(owner, client, json!({"kind": "folder", "path": root}));
    assert_eq!(content(&fixture), before);
}

/// A fixed mount table: a card (with a `DCIM` folder) and a drive, as scratch directories.
fn mounts(fixture: &Fixture) -> (Arc<Mutex<Vec<Mount>>>, PathBuf, PathBuf) {
    let card = fixture.dir.join("NIKON Z 6");
    let drive = fixture.dir.join("Photos SSD");
    put(&card.join("DCIM/100NZ6_1/DSC_0001.jpg"), &camera_jpeg());
    put(&card.join("PRIVATE/clip.jpg"), &camera_jpeg());
    put(&drive.join("2026/trip/a.jpg"), &camera_jpeg());
    let table = Arc::new(Mutex::new(vec![
        crate::index::volumes::tests::mount_at(&card, "NIKON Z 6", 1, true),
        crate::index::volumes::tests::mount_at(&drive, "Photos SSD", 2, false),
    ]));
    tell(
        fixture.owner(),
        FilesMessage::Mounts(MountSource::Fixed(table.clone())),
    );
    (table, card, drive)
}

/// `volume.list` names the mounted volumes and the known ones offline, `card.list` the cards, and
/// listing a card fills in its files and cameras; `disk.folders` lists subfolders without the
/// excluded ones.
#[test]
fn volumes_cards_and_folders_on_disk() {
    let fixture = Fixture::new("volumes");
    let owner = fixture.owner();
    let client = owner.register();
    let (table, card, drive) = mounts(&fixture);
    let volumes = ok(owner, client, "volume.list", json!({}))["volumes"].clone();
    assert_eq!(volumes.as_array().unwrap().len(), 2, "{volumes}");
    assert_eq!(volumes[0]["volume"]["label"], "NIKON Z 6");
    assert_eq!(
        (
            &volumes[0]["card"],
            &volumes[0]["volume"]["removable"],
            &volumes[0]["offline"]
        ),
        (&json!(true), &json!(true), &json!(false))
    );
    assert_eq!(
        (&volumes[1]["card"], &volumes[1]["volume"]["removable"]),
        (&json!(false), &json!(false))
    );
    let card_id = volumes[0]["volume"]["id"].clone();

    let cards = ok(owner, client, "card.list", json!({}))["cards"].clone();
    assert_eq!(cards.as_array().unwrap().len(), 1);
    assert_eq!(cards[0]["dcim"], json!(card.join("DCIM")));
    assert_eq!(cards[0].get("files"), None, "not listed yet");
    let report = refresh(owner, client, json!({"kind": "card", "volume_id": card_id}));
    assert_eq!(report["files"], 1, "only the DCIM folder: {report}");
    let cards = ok(owner, client, "card.list", json!({}))["cards"].clone();
    assert_eq!(cards[0]["files"], 1);
    assert_eq!(cards[0]["cameras"][0], "NIKON Z 6");

    // A folder added on the drive makes it a known volume, offline once it is taken out.
    ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": drive.join("2026"), "mutation": envelope("add")}),
    );
    table
        .lock()
        .unwrap()
        .retain(|mount| mount.mount_point != drive);
    let volumes = ok(owner, client, "volume.list", json!({}))["volumes"].clone();
    let offline: Vec<_> = volumes
        .as_array()
        .unwrap()
        .iter()
        .filter(|volume| volume["offline"] == true)
        .map(|volume| volume["volume"]["label"].clone())
        .collect();
    assert_eq!(offline, [json!("Photos SSD")]);
    let (code, _) = refused(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "card", "volume_id": "volume-00000000000000000000000000000009"}}),
    );
    assert_eq!(code, "source-unavailable");

    for name in [
        "2026",
        "2025",
        ".hidden",
        "Previews.lrdata",
        "Library.photoslibrary",
    ] {
        std::fs::create_dir_all(drive.join(name)).unwrap();
    }
    let folders = ok(owner, client, "disk.folders", json!({"path": drive}));
    let names: Vec<_> = folders["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|folder| folder["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["2025", "2026"]);
    assert_eq!(folders["truncated"], false);
    let (code, _) = refused(
        owner,
        client,
        "disk.folders",
        json!({"path": drive.join("2026/trip/a.jpg")}),
    );
    assert_eq!(code, "validation");
    let (code, _) = refused(owner, client, "disk.folders", json!({"path": "relative"}));
    assert_eq!(code, "validation");
}

/// Adding and removing an indexed folder are library changes: journaled, retried from the journal,
/// refused by the contract's rules, listed on add, forgotten on remove, and undone and redone with
/// the index following.
#[test]
fn indexed_folders_are_library_changes_with_undo_and_redo() {
    let fixture = Fixture::new("folders");
    let owner = fixture.owner();
    let client = owner.register();
    let photos = fixture.dir.join("photos");
    for name in ["a.jpg", "trip/b.jpg"] {
        put(&photos.join(name), &camera_jpeg());
    }
    let added = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": photos, "mutation": envelope("add-1")}),
    );
    assert_eq!(added["change"]["outcome"], "applied", "{added}");
    assert_eq!(added["folder"]["path"], json!(photos));
    assert_eq!(added["folder"]["actor"], "agent");
    let job = finished(owner, client, &added["job_id"]);
    assert_eq!(job["result"]["files"], 2, "{job}");
    let folders = ok(owner, client, "index.folders", json!({}))["folders"].clone();
    assert_eq!(folders[0]["path"], json!(photos));
    assert_eq!(
        (&folders[0]["files"], &folders[0]["offline"]),
        (&json!(2), &json!(false))
    );
    let journal = ok(owner, client, "library.journal", json!({}))["changes"].clone();
    assert_eq!(journal[0]["label"], "Added photos to indexed folders");
    assert_eq!(journal[0]["method"], "index.add-folder");

    // A retry answers the first change; the same folder again changes nothing.
    let retry = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": photos, "mutation": envelope("add-1")}),
    );
    assert_eq!(retry["change"]["change"], added["change"]["change"]);
    assert_eq!(
        (&added["deduplicated"], &retry["deduplicated"]),
        (&json!(false), &json!(true))
    );
    // Both answers parse as the typed answer the method declares, the owner's top-level
    // `deduplicated` included.
    for answer in [&added, &retry] {
        let typed: crate::catalog_types::IndexFolderAnswer =
            serde_json::from_value(answer.clone()).expect("a typed index.add-folder answer");
        assert_eq!(typed.deduplicated, answer["deduplicated"] == json!(true));
    }
    let again = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": photos, "mutation": envelope("add-2")}),
    );
    assert_eq!(again["change"]["outcome"], "no-op");

    // Refusals: a file, a package, Luxforge's own directory, a folder inside or around.
    std::fs::create_dir_all(fixture.dir.join("Photos Library.photoslibrary")).unwrap();
    for (path, expected) in [
        (photos.join("a.jpg"), "validation"),
        (
            fixture.dir.join("Photos Library.photoslibrary"),
            "validation",
        ),
        (fixture.index_dir(), "validation"),
        (photos.join("trip"), "conflict"),
        (fixture.dir.clone(), "conflict"),
        (PathBuf::from("relative/path"), "validation"),
        (fixture.dir.join("missing"), "read-error"),
    ] {
        let (code, message) = refused(
            owner,
            client,
            "index.add-folder",
            json!({"path": path, "mutation": envelope(&format!("refused-{}", path.display()))}),
        );
        assert_eq!(code, expected, "{}: {message}", path.display());
    }

    // Removing forgets its rows; undo adds it back and lists it again; redo removes it again.
    let removed = ok(
        owner,
        client,
        "index.remove-folder",
        json!({"path": photos, "mutation": envelope("remove-1")}),
    );
    assert_eq!(removed["outcome"], "applied");
    wait_for("the removed folder's rows to go", || {
        fixture.rows().is_empty().then_some(())
    });
    assert!(
        ok(owner, client, "index.folders", json!({}))["folders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let undone = ok(
        owner,
        client,
        "library.undo",
        json!({"mutation": envelope("undo-1")}),
    );
    assert_eq!(undone["outcome"], "applied");
    assert_eq!(
        ok(owner, client, "index.folders", json!({}))["folders"][0]["path"],
        json!(photos)
    );
    wait_for("the folder to be listed again", || {
        (fixture.rows().len() == 2).then_some(())
    });
    ok(
        owner,
        client,
        "library.redo",
        json!({"mutation": envelope("redo-1")}),
    );
    wait_for("the folder's rows to go again", || {
        fixture.rows().is_empty().then_some(())
    });
    let removed_again = ok(
        owner,
        client,
        "index.remove-folder",
        json!({"path": photos, "mutation": envelope("remove-2")}),
    );
    assert_eq!(removed_again["outcome"], "no-op");
}

/// After a restart, a retried `index.add-folder` or `index.remove-folder` is answered from the
/// journal with the change its first attempt recorded, and records nothing.
#[test]
fn a_retry_after_a_restart_is_answered_from_the_journal() {
    let mut fixture = Fixture::new("retry");
    let photos = fixture.dir.join("photos");
    put(&photos.join("a.jpg"), &camera_jpeg());
    let add = json!({"path": photos, "mutation": envelope("add-1")});
    let remove = json!({"path": photos, "mutation": envelope("remove-1")});
    let (added, removed) = {
        let owner = fixture.owner();
        let client = owner.register();
        let added = ok(owner, client, "index.add-folder", add.clone());
        finished(owner, client, &added["job_id"]);
        let removed = ok(owner, client, "index.remove-folder", remove.clone());
        (added, removed)
    };
    fixture.stop();
    fixture.start();
    let owner = fixture.owner();
    let client = owner.register();
    let journal = |owner: &OwnerHandle| {
        ok(owner, client, "library.journal", json!({}))["changes"]
            .as_array()
            .unwrap()
            .len()
    };
    let recorded = journal(owner);
    let retried = ok(owner, client, "index.add-folder", add);
    assert_eq!(retried["change"]["change"], added["change"]["change"]);
    assert_eq!(retried["change"]["deduplicated"], true, "{retried}");
    let typed: crate::catalog_types::IndexFolderAnswer =
        serde_json::from_value(retried.clone()).expect("a typed retried answer");
    assert!(typed.change.deduplicated);
    assert_eq!(retried["folder"]["path"], json!(photos));
    assert_eq!(retried.get("job_id"), None, "nothing is listed again");
    let retried = ok(owner, client, "index.remove-folder", remove);
    assert_eq!(retried["change"], removed["change"]);
    assert_eq!(journal(owner), recorded, "nothing recorded");
    assert!(
        ok(owner, client, "index.folders", json!({}))["folders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// A folder on a volume that is not mounted is offline: its rows stay, `index.folders` says so, a
/// refresh of it is `source-unavailable` and a refresh of every indexed folder reports it.
#[test]
fn an_offline_folder_keeps_its_rows() {
    let fixture = Fixture::new("offline");
    let owner = fixture.owner();
    let client = owner.register();
    let (table, _, drive) = mounts(&fixture);
    let trip = drive.join("2026/trip");
    let added = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": trip, "mutation": envelope("add")}),
    );
    finished(owner, client, &added["job_id"]);
    assert_eq!(fixture.rows().len(), 1);

    // Unplug the drive: it leaves the table and its mount point is gone.
    table
        .lock()
        .unwrap()
        .retain(|mount| mount.mount_point != drive);
    let unplugged = fixture.dir.join("unplugged");
    std::fs::rename(&drive, &unplugged).unwrap();
    let folders = ok(owner, client, "index.folders", json!({}))["folders"].clone();
    assert_eq!(folders[0]["offline"], true, "{folders}");
    assert_eq!(folders[0]["files"], 1);
    let started = ok(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "indexed-folder", "path": trip}}),
    );
    let job = finished(owner, client, &started["job_id"]);
    assert_eq!(job["error"]["code"], "source-unavailable", "{job}");
    let all = refresh(owner, client, json!({"kind": "all-indexed"}));
    assert_eq!(all["offline"], json!([trip]));
    assert_eq!(fixture.rows().len(), 1, "its rows stay");
    let connection = database::connect_at(&fixture.index_dir()).unwrap();
    assert!(database::root(&connection, &trip).unwrap().unwrap().offline);

    // Plugged back in, it lists as before.
    std::fs::rename(&unplugged, &drive).unwrap();
    table
        .lock()
        .unwrap()
        .push(crate::index::volumes::tests::mount_at(
            &drive,
            "Photos SSD",
            2,
            false,
        ));
    let report = refresh(
        owner,
        client,
        json!({"kind": "indexed-folder", "path": trip}),
    );
    assert_eq!(
        (&report["files"], &report["headers_read"]),
        (&json!(1), &json!(0))
    );
    assert!(!database::root(&connection, &trip).unwrap().unwrap().offline);
}

/// Call `method` as `client` on a thread of its own, which blocks until it is answered.
fn call_on_thread(
    owner: &OwnerHandle,
    client: ClientId,
    method: &'static str,
    params: Value,
) -> JoinHandle<Result<ApiResponse, crate::Error>> {
    let owner = owner.clone();
    std::thread::spawn(move || {
        owner.call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
    })
}

/// Hold every question the query thread takes from now on, as a volume that does not answer would.
fn hold_queries(owner: &OwnerHandle) -> Arc<Gate> {
    let gate = Arc::new(Gate::new());
    gate.shut();
    tell(owner, FilesMessage::HoldQueries(gate.clone()));
    gate
}

/// How many calls wait behind the question the query thread is answering.
fn queries_waiting(owner: &OwnerHandle) -> usize {
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    tell(owner, FilesMessage::QueriesWaiting(reply));
    answer.recv().unwrap()
}

/// The labels of the mounted volumes `volume.list` names, in its order.
fn mounted_labels(owner: &OwnerHandle, client: ClientId) -> Vec<Value> {
    ok(owner, client, "volume.list", json!({}))["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|volume| volume["offline"] == false)
        .map(|volume| volume["volume"]["label"].clone())
        .collect()
}

/// A question held on the query thread, as a hung network volume holds one, holds only the call
/// that asked it: the owner answers every other call meanwhile — lane A's lists from what the
/// survey learned, an index job, other lanes' reads — and the held call is answered once its
/// question returns. Reading the lists opens no index on the owner.
#[test]
fn a_question_stuck_on_the_disk_never_holds_the_owner() {
    let fixture = Fixture::new("held");
    let owner = fixture.owner();
    let client = owner.register();
    let (_, _, drive) = mounts(&fixture);
    std::fs::create_dir_all(drive.join("2025")).unwrap();
    // The first list waits for the survey; from then on the owner knows the volumes.
    let volumes = ok(owner, client, "volume.list", json!({}))["volumes"].clone();
    assert_eq!(volumes.as_array().unwrap().len(), 2, "{volumes}");
    let card_id = volumes[0]["volume"]["id"].clone();

    let gate = hold_queries(owner);
    let asker = owner.register();
    let held = call_on_thread(owner, asker, "disk.folders", json!({"path": drive}));
    gate.wait_reached(1, "the disk.folders question");
    for (method, params) in [
        ("index.folders", json!({})),
        ("volume.list", json!({})),
        ("card.list", json!({})),
        ("pick.list", json!({})),
        ("activity.list", json!({})),
    ] {
        ok(owner, client, method, params);
    }
    assert!(
        !fixture.index_dir().join(crate::INDEX_FILE).exists(),
        "lane A's reads opened no index"
    );
    ok(owner, client, "catalog.info", json!({}));
    for source in [
        json!({"kind": "all-indexed"}),
        json!({"kind": "card", "volume_id": card_id}),
    ] {
        ok(owner, client, "index.refresh", json!({"source": source}));
    }
    assert!(gate.holding(), "the question is still held");
    assert_eq!(queries_waiting(owner), 0);
    gate.open();
    let answer = held.join().unwrap().expect("the owner answered");
    assert!(answer.error.is_none(), "{:?}", answer.error);
    let names: Vec<Value> = answer.result.unwrap()["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|folder| folder["name"].clone())
        .collect();
    assert_eq!(names, [json!("2025"), json!("2026")]);
}

/// Behind a held question at most `MAX_WAITING_QUERIES` calls wait, and one more is refused with
/// `resource-limit` at once. A client that leaves takes its waiting call with it, and the call
/// whose question is held is dropped too; the others are answered once the question returns, and
/// the thread answers on.
#[test]
fn a_full_query_queue_refuses_and_a_client_that_leaves_takes_its_call() {
    use super::queries::MAX_WAITING_QUERIES;
    let fixture = Fixture::new("queue");
    let owner = fixture.owner();
    let client = owner.register();
    let folder = fixture.dir.join("photos");
    std::fs::create_dir_all(folder.join("a")).unwrap();
    let gate = hold_queries(owner);
    let held_client = owner.register();
    let held = call_on_thread(owner, held_client, "disk.folders", json!({"path": folder}));
    gate.wait_reached(1, "the first question");
    let mut waiting: Vec<_> = (0..MAX_WAITING_QUERIES)
        .map(|_| {
            let waiter = owner.register();
            let call = call_on_thread(owner, waiter, "disk.folders", json!({"path": folder}));
            (waiter, call)
        })
        .collect();
    wait_for("every call to wait for the thread", || {
        (queries_waiting(owner) == MAX_WAITING_QUERIES).then_some(())
    });
    let (code, _) = refused(owner, client, "disk.folders", json!({"path": folder}));
    assert_eq!(code, "resource-limit");
    let (code, _) = refused(
        owner,
        client,
        "index.add-folder",
        json!({"path": folder, "mutation": envelope("full")}),
    );
    assert_eq!(
        code, "resource-limit",
        "every question waits in the one queue"
    );

    let (gone, gone_call) = waiting.remove(0);
    owner.disconnect(gone);
    assert!(gone_call.join().unwrap().is_err(), "dropped unanswered");
    assert_eq!(queries_waiting(owner), MAX_WAITING_QUERIES - 1);
    owner.disconnect(held_client);
    assert!(held.join().unwrap().is_err(), "the held call is dropped");
    gate.open();
    for (_, call) in waiting {
        let answer = call.join().unwrap().expect("the owner answered");
        assert!(answer.error.is_none(), "{:?}", answer.error);
    }
    assert_eq!(
        ok(owner, client, "disk.folders", json!({"path": folder}))["folders"][0]["name"],
        "a"
    );
}

/// Stopping the owner while a question is held does not wait for it: the owner's thread ends and
/// the held call is dropped unanswered, while the question's thread ends by itself once the
/// question returns.
#[test]
fn stopping_the_owner_does_not_wait_for_a_held_question() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut fixture = Fixture::new("stop");
    let (owner, join) = fixture.owner.take().unwrap();
    let gate = hold_queries(&owner);
    let client = owner.register();
    let held = call_on_thread(&owner, client, "disk.folders", json!({"path": fixture.dir}));
    gate.wait_reached(1, "the question");
    let stopped = Arc::new(AtomicBool::new(false));
    let stopper = {
        let stopped = stopped.clone();
        std::thread::spawn(move || {
            owner.stop();
            join.join().unwrap();
            stopped.store(true, Ordering::SeqCst);
        })
    };
    luxforge_testbase::wait_until("the owner to stop with the question held", || {
        stopped.load(Ordering::SeqCst)
    });
    assert!(gate.holding(), "the question was not waited for");
    assert!(held.join().unwrap().is_err(), "dropped unanswered");
    gate.open();
    stopper.join().unwrap();
}

/// The volume list answers from what the survey learned against the mount table read now: a
/// volume taken out is gone from the very next answer, one mounted since appears once a survey has
/// learned it, and its card can be listed before that, found on the query thread.
#[test]
fn the_volume_list_follows_the_mount_table_and_a_new_card_is_found_on_the_disk() {
    let fixture = Fixture::new("survey");
    let owner = fixture.owner();
    let client = owner.register();
    let (table, _, drive) = mounts(&fixture);
    assert_eq!(
        mounted_labels(owner, client),
        [json!("NIKON Z 6"), json!("Photos SSD")]
    );
    let lumix = fixture.dir.join("LUMIX");
    put(&lumix.join("DCIM/100_PANA/P1000001.JPG"), &camera_jpeg());
    {
        let mut table = table.lock().unwrap();
        table.retain(|mount| mount.mount_point != drive);
        table.push(crate::index::volumes::tests::mount_at(
            &lumix, "LUMIX", 4, true,
        ));
    }
    // This answer asks for a survey and answers before it: the drive is gone, the card not yet
    // learned.
    assert_eq!(mounted_labels(owner, client), [json!("NIKON Z 6")]);
    let report = refresh(
        owner,
        client,
        json!({"kind": "card", "volume_id": format!("volume-{}", "04".repeat(16))}),
    );
    assert_eq!(report["files"], 1, "{report}");
    wait_for("the survey to learn the new card", || {
        (mounted_labels(owner, client) == [json!("NIKON Z 6"), json!("LUMIX")]).then_some(())
    });
    let cards = ok(owner, client, "card.list", json!({}))["cards"].clone();
    assert_eq!(cards.as_array().unwrap().len(), 2, "{cards}");
    assert_eq!(cards[1]["files"], 1);
}

/// Run `hdiutil` with `args`, failing the test with its output when it fails.
#[cfg(target_os = "macos")]
fn hdiutil(args: &[&std::ffi::OsStr]) {
    let output = std::process::Command::new("hdiutil")
        .args(args)
        .output()
        .expect("hdiutil runs");
    assert!(
        output.status.success(),
        "hdiutil {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The platform's own entry for the disk image mounted at `mount`: its removability, name and UUID
/// as the kernel reports them. `-nobrowse` keeps it out of the Finder, which also marks it not
/// meant for browsing, so the test lists it as a card would be.
#[cfg(target_os = "macos")]
fn image_mount(mount: &Path) -> Mount {
    let mut entry = luxforge_watch::mounts()
        .unwrap()
        .into_iter()
        .find(|entry| entry.mount_point == mount)
        .expect("the image is mounted");
    entry.browsable = true;
    entry
}

/// A FAT32 disk image standing in for a camera card: the kernel reports it removable with a UUID,
/// so it is the same volume when mounted again under another device number. Listed, taken out
/// (offline: its rows stay, a refresh of it is `source-unavailable`, its indexed folder and its
/// volume are offline) and put back, when nothing is read again and every row keeps its identity.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image with hdiutil; run by hand on macOS"]
fn a_disk_image_card_goes_offline_and_comes_back() {
    let fixture = Fixture::new("disk-image");
    let owner = fixture.owner();
    let client = owner.register();
    let image = fixture.dir.join("card.dmg");
    let mount = fixture.dir.join("mnt");
    std::fs::create_dir_all(&mount).unwrap();
    hdiutil(&[
        "create".as_ref(),
        "-size".as_ref(),
        "40m".as_ref(),
        "-fs".as_ref(),
        "MS-DOS FAT32".as_ref(),
        "-volname".as_ref(),
        "LFCARD".as_ref(),
        image.as_os_str(),
    ]);
    let attach = || {
        hdiutil(&[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            image.as_os_str(),
        ])
    };
    let detach = || hdiutil(&["detach".as_ref(), mount.as_os_str()]);
    /// Detaches the image however the test ends.
    struct Attached<'a>(&'a Path);
    impl Drop for Attached<'_> {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach".as_ref(), "-force".as_ref(), self.0.as_os_str()])
                .output();
        }
    }
    let _attached = Attached(&mount);
    attach();
    let photos = mount.join("DCIM/100TEST");
    put(&photos.join("DSC_0001.jpg"), &camera_jpeg());
    put(&photos.join("DSC_0002.jpg"), &camera_jpeg());
    let entry = image_mount(&mount);
    assert!(entry.removable, "{entry:?}");
    assert!(entry.uuid.is_some(), "{entry:?}");
    let table = Arc::new(Mutex::new(vec![entry]));
    tell(
        owner,
        FilesMessage::Mounts(MountSource::Fixed(table.clone())),
    );

    let cards = ok(owner, client, "card.list", json!({}))["cards"].clone();
    assert_eq!(cards.as_array().unwrap().len(), 1, "{cards}");
    assert_eq!(cards[0]["volume"]["removable"], true);
    let volume_id = cards[0]["volume"]["id"].clone();
    let before = manifest(&mount);
    let report = refresh(
        owner,
        client,
        json!({"kind": "card", "volume_id": volume_id}),
    );
    assert_eq!(
        (&report["files"], &report["headers_read"]),
        (&json!(2), &json!(2))
    );
    let added = ok(
        owner,
        client,
        "index.add-folder",
        json!({"path": photos, "mutation": envelope("add")}),
    );
    finished(owner, client, &added["job_id"]);
    assert_eq!(
        manifest(&mount),
        before,
        "listing changed nothing on the card"
    );
    let ids: Vec<i64> = fixture.rows().into_iter().map(|(_, id, _)| id).collect();

    detach();
    table.lock().unwrap().clear();
    let (code, _) = refused(
        owner,
        client,
        "index.refresh",
        json!({"source": {"kind": "card", "volume_id": volume_id}}),
    );
    assert_eq!(code, "source-unavailable");
    let folders = ok(owner, client, "index.folders", json!({}))["folders"].clone();
    assert_eq!(folders[0]["offline"], true, "{folders}");
    let volumes = ok(owner, client, "volume.list", json!({}))["volumes"].clone();
    assert!(
        volumes
            .as_array()
            .unwrap()
            .iter()
            .any(|volume| volume["volume"]["id"] == volume_id && volume["offline"] == true),
        "{volumes}"
    );
    assert_eq!(fixture.rows().len(), 2, "its rows stay");

    attach();
    table.lock().unwrap().push(image_mount(&mount));
    let report = refresh(
        owner,
        client,
        json!({"kind": "card", "volume_id": volume_id}),
    );
    assert_eq!(
        (&report["files"], &report["headers_read"]),
        (&json!(2), &json!(0)),
        "{report}"
    );
    let again: Vec<i64> = fixture.rows().into_iter().map(|(_, id, _)| id).collect();
    assert_eq!(again, ids, "every row keeps its identity");
}
