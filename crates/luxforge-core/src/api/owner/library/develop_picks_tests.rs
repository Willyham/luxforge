//! Developing picks through the catalog owner, as clients call it: `pick.plan` by event with its
//! folder proposals, removable picks and offline files; `pick.develop` bringing in exactly the
//! picked photographs in batches, one event a batch, clearing their picks, with every original
//! unchanged; linking identical files and relinking a missing original only on a fingerprint
//! match; card picks from verified copies, and removable media otherwise only when confirmed; a
//! cancel mid-job keeping exactly the committed photographs; a failing file reported and left
//! picked; a retry answered once, after a restart too; `library.undo` of a Develop sending its
//! photographs back and refused for an edited one; and `asset.send-back`. Every file is written in
//! a scratch directory.
use super::super::{Owner, OwnerMessage, catalog::CatalogMessage};
use super::{Hold, LibraryMessage};
use crate::{
    EditorService, ModuleRegistry,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientId, OwnerHandle},
    catalog_types::{
        CaptureTime, CatalogFolder, CatalogFolderId, EventSpan, FileRecord, IndexedFolder, Volume,
        VolumeId,
    },
    editor::{insert_catalog_folder, insert_indexed_folder, mutation_json, upsert_volume, write},
    index::{upsert_file, volume_of},
    library::{
        develop::develop_picks_tests::{Shot, broken_raw, photo, record, untouched},
        locate::Phase,
    },
};
use luxforge_testbase::{Gate, paths::temp_dir};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

/// The actor every client request here is made as.
const ACTOR: &str = "develop-client";

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

/// One owner over a catalog in its own scratch directory.
struct Harness {
    dir: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("develop-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        Self::start(dir, catalog)
    }

    fn start(dir: PathBuf, catalog: PathBuf) -> Self {
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
        let client = owner.register();
        Self {
            dir,
            catalog,
            owner,
            join: Some(join),
            client,
        }
    }

    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    /// Stop the owner and start a new one on the same catalog.
    fn restart(mut self) -> Self {
        self.stop();
        let (dir, catalog) = (std::mem::take(&mut self.dir), self.catalog.clone());
        Self::start(dir, catalog)
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

    /// Answer `method`, retrying through every preparation it asks for, as a client does.
    fn prepared(&self, method: &str, params: Value) -> Value {
        for _ in 0..4 {
            let response = self.send(method, params.clone());
            match response.error {
                None => return response.result.expect("a result"),
                Some(error) if error.code == "preparation-required" => {
                    let job = json!(error.job_id.expect("a preparation names its job"));
                    assert_eq!(self.settle(&job)["status"], "ready");
                }
                Some(error) => panic!("{method}: {error:?}"),
            }
        }
        panic!("{method} kept asking for preparation");
    }

    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    fn post(&self, message: LibraryMessage) {
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(message)))
            .unwrap();
    }

    /// Run `f` on the owner, between requests, and answer what it answers.
    fn run<T: Send + 'static>(&self, f: impl FnOnce(&mut Owner) -> T + Send + 'static) -> T {
        let (sender, receiver) = mpsc::channel();
        self.post(LibraryMessage::Run(Box::new(move |owner| {
            let _ = sender.send(f(owner));
        })));
        receiver.recv().expect("the owner ran it")
    }

    /// List `records` in the index, as the index lane would.
    fn index(&self, records: Vec<FileRecord>) {
        self.run(move |owner| {
            let mut index = owner.service.index().unwrap();
            let tx = index.connection_mut().transaction().unwrap();
            for file in &records {
                upsert_file(&tx, file).unwrap();
            }
            tx.commit().unwrap();
        });
    }

    /// List each of `paths` in the index on the volume it is on.
    fn index_paths(&self, paths: &[&PathBuf]) {
        self.index(
            paths
                .iter()
                .map(|path| record(path, &volume_of(path, 0).unwrap().id))
                .collect(),
        );
    }

    /// Write in the catalog, outside any library change, as a test arranges it.
    fn arrange(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), crate::Error> + Send + 'static,
    ) {
        self.run(move |owner| write(&mut owner.service.connection, f).unwrap());
    }

    fn query<T: Send + 'static>(
        &self,
        sql: &'static str,
        params: Vec<String>,
        read: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Vec<T> {
        self.run(move |owner| {
            let mut statement = owner.service.connection.prepare(sql).unwrap();
            statement
                .query_map(rusqlite::params_from_iter(params), read)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
    }

    fn pick(&self, paths: &[&PathBuf], request_id: &str) {
        let answer = self.ok(
            "pick.set",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "picked": true,
                "mutation": envelope(request_id),
            }),
        );
        assert_eq!(answer["outcome"], "applied", "{answer}");
    }

    fn picks(&self) -> Vec<PathBuf> {
        self.ok("pick.list", json!({}))["picks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pick| PathBuf::from(pick["path"].as_str().unwrap()))
            .collect()
    }

    /// Start a Develop with `params` (its envelope added), answering the start.
    fn develop(&self, request_id: &str, mut params: Value) -> ApiResponse {
        params["mutation"] = envelope(request_id);
        if params.get("into").is_none() {
            params["into"] = json!([]);
        }
        self.send("pick.develop", params)
    }

    /// Develop and wait for the job, answering the job as `job.read` reads it.
    fn developed(&self, request_id: &str, params: Value) -> Value {
        let response = self.develop(request_id, params);
        assert!(response.error.is_none(), "{:?}", response.error);
        let job = self.settle(&response.result.unwrap()["job_id"]);
        assert_eq!(job["kind"], "develop-picks", "{job}");
        job
    }

    /// Develop `paths` into the plan's folders, answering the finished job's report.
    fn develop_paths(&self, request_id: &str, paths: &[&PathBuf]) -> Value {
        let job = self.developed(
            request_id,
            json!({"targets": {"kind": "paths", "paths": paths}}),
        );
        assert_eq!(job["status"], "ready", "{job}");
        job["result"].clone()
    }

    fn state(&self, asset: &Value) -> Value {
        self.ok("asset.state", json!({"asset_id": asset}))
    }

    fn inspect(&self, sequence: &Value) -> Value {
        self.ok("library.inspect", json!({"sequence": sequence}))
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

    fn photographs(&self) -> u64 {
        self.ok("catalog.info", json!({}))["counts"]["photographs"]
            .as_u64()
            .unwrap()
    }

    /// Hold every library job dispatched from now on at the `nth` time it reaches `phase`.
    fn hold_at(&self, phase: Phase, nth: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let held = gate.clone();
        let reached = Mutex::new(0);
        let hold: Hold = Arc::new(move |at| {
            if at == phase {
                let mut count = reached.lock().unwrap();
                *count += 1;
                if *count == nth {
                    drop(count);
                    held.pass();
                }
            }
        });
        self.post(LibraryMessage::Hold(Some(hold)));
        gate
    }

    fn release_holds(&self) {
        self.post(LibraryMessage::Hold(None));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// The instant a camera at +02:00 records for `time`.
fn instant(time: &str) -> i64 {
    CaptureTime::from_exif(time, None, Some("+02:00"))
        .unwrap()
        .instant_ms()
}

/// A catalog folder at the top level, made from `span`.
fn folder(name: &str, span: Option<EventSpan>) -> CatalogFolder {
    CatalogFolder {
        id: CatalogFolderId::new(),
        name: name.into(),
        parent_id: None,
        created_ms: 1,
        event: span,
        count: 0,
        year: None,
    }
}

/// A removable volume the catalog records, mounted at `mount_point`.
fn card(mount_point: &Path) -> Volume {
    Volume {
        id: VolumeId::new(),
        mount_point: mount_point.to_path_buf(),
        label: "NIKON Z 8".into(),
        removable: true,
        platform_id: None,
        last_seen_ms: 1,
    }
}

/// Each developed file of a report as `(file name, outcome)`.
fn outcomes(report: &Value) -> Vec<(String, String)> {
    report["developed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                Path::new(row["path"].as_str().unwrap())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                row["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The asset a report developed from the file named `name`.
fn asset_of(report: &Value, name: &str) -> Value {
    report["developed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"].as_str().unwrap().ends_with(name))
        .unwrap_or_else(|| panic!("{name} was not developed: {report}"))["asset_id"]
        .clone()
}

/// A plan groups the files by event over every file its folders hold, chronologically and undated
/// last; proposes the folder an earlier Develop made from an event (by its span) or a new one named
/// after its place and month, unique among the top-level folders; counts the picks on a removable
/// volume and those with a copy in an indexed folder; and counts the offline file.
#[test]
fn develop_picks_plan_groups_by_event_and_proposes_folders() {
    let harness = Harness::new("plan");
    let trip = harness.dir.join("trip");
    let before_picks = photo(&trip.join("k0.jpg"), &Shot::at("2026:09:12 09:00:00"));
    let k1 = photo(
        &trip.join("k1.jpg"),
        &Shot::at("2026:09:12 10:00:00").subsec("10"),
    );
    let k2 = photo(
        &trip.join("k2.jpg"),
        &Shot::at("2026:09:12 10:00:00").subsec("60"),
    );
    let k3 = photo(&trip.join("k3.jpg"), &Shot::at("2026:09:12 14:00:00"));
    let z1 = photo(
        &trip.join("z1.jpg"),
        &Shot::at("2026:09:16 09:00:00").in_place(47.3769, 8.5417),
    );
    let loose = photo(
        &harness.dir.join("loose").join("anna.jpg"),
        &Shot::at("2026:09:18 09:00:00"),
    );
    // A card, with one of its files copied into an indexed folder.
    let dcim = harness.dir.join("card").join("DCIM").join("100NZ8_1");
    let lindau = |time: &str| Shot::at(time).in_place(47.5460, 9.6829);
    let c1 = photo(&dcim.join("c1.jpg"), &lindau("2026:09:20 10:00:00"));
    let c2 = photo(&dcim.join("c2.jpg"), &lindau("2026:09:20 10:05:00"));
    let dump = harness.dir.join("dump");
    fs::create_dir_all(&dump).unwrap();
    fs::copy(&c1, dump.join("c1.jpg")).unwrap();
    let copy = dump.join("c1.jpg").canonicalize().unwrap();
    let card = card(&harness.dir.join("card"));
    // A file on a volume that is not connected.
    let gone_volume = Volume {
        id: VolumeId::new(),
        mount_point: "/Volumes/Luxforge test drive that is not connected".into(),
        label: "Photos SSD".into(),
        removable: false,
        platform_id: None,
        last_seen_ms: 1,
    };
    let gone = gone_volume.mount_point.join("2026").join("gone.jpg");
    let was = photo(
        &harness.dir.join("scratch").join("gone.jpg"),
        &Shot::at("2026:10:01 10:00:00"),
    );
    let mut gone_record = record(&was, &gone_volume.id);
    gone_record.path = gone.clone();
    gone_record.folder = gone.parent().unwrap().to_path_buf();
    gone_record.name = "gone.jpg".into();

    harness.index_paths(&[&before_picks, &k1, &k2, &k3, &z1, &copy]);
    harness.index(vec![
        record(&c1, &card.id),
        record(&c2, &card.id),
        gone_record,
    ]);
    // An earlier Develop's folder from the Zürich event, and an unrelated one named as the new
    // Konstanz folder would be.
    let zurich = folder(
        "Zürich trip",
        Some(EventSpan {
            start_ms: instant("2026:09:16 08:00:00"),
            end_ms: instant("2026:09:16 09:30:00"),
            event_id: None,
        }),
    );
    let (zurich_id, volumes) = (zurich.id.clone(), [card.clone(), gone_volume.clone()]);
    let dump_folder = IndexedFolder {
        path: dump.canonicalize().unwrap(),
        volume_id: volume_of(&dump, 0).unwrap().id,
        added_ms: 1,
        actor: ACTOR.into(),
    };
    let dump_volume = volume_of(&dump, 0).unwrap();
    harness.arrange(move |tx| {
        for volume in volumes.iter().chain([&dump_volume]) {
            upsert_volume(tx, volume)?;
        }
        insert_catalog_folder(tx, &zurich)?;
        insert_catalog_folder(tx, &folder("Konstanz · Sep 2026", None))?;
        insert_indexed_folder(tx, &dump_folder)
    });

    let plan = harness.ok(
        "pick.plan",
        json!({"targets": {"kind": "paths", "paths": [&z1, &k3, &k1, &k2, &loose, &c1, &c2, &gone]}}),
    );
    assert_eq!(plan["count"], 8, "{plan}");
    assert_eq!(plan["offline"], 1, "{plan}");
    let events = plan["events"].as_array().unwrap();
    let summary: Vec<(&str, u64)> = events
        .iter()
        .map(|event| {
            (
                event["name"].as_str().unwrap(),
                event["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("Konstanz · 12 Sep", 3),
            ("Zürich · 16 Sep", 1),
            ("Lindau · 20 Sep", 2),
            ("Konstanz · 1 Oct", 1),
            ("Undated · loose", 1),
        ],
        "{plan}"
    );
    // The first event is identified by its first file, k0, which is not planned: the events are
    // its folder's.
    assert!(events.iter().all(|event| event["event_id"].is_string()));
    assert_eq!(
        events[0]["folder"],
        json!({"kind": "new", "name": "Konstanz · Sep 2026 2"}),
        "the name the unrelated folder has is taken"
    );
    assert_eq!(
        events[1]["folder"],
        json!({"kind": "existing", "folder_id": zurich_id})
    );
    assert_eq!(events[1]["folder_name"], "Zürich trip");
    assert_eq!(
        events[2]["folder"],
        json!({"kind": "new", "name": "Lindau · Sep 2026"})
    );
    assert_eq!(
        events[2]["removable"],
        json!([{"volume_id": card.id, "label": "NIKON Z 8", "count": 2, "with_copy": 1}])
    );
    assert_eq!(
        events[3]["folder"],
        json!({"kind": "new", "name": "Konstanz · Oct 2026"})
    );
    assert!(
        events[3].get("removable").is_none(),
        "an offline file is not developable"
    );
    assert_eq!(events[4]["folder"], json!({"kind": "new", "name": "loose"}));

    // Without targets a plan takes the view's picks, and there is no view yet.
    let refused = harness.refused("pick.plan", json!({}));
    assert_eq!(refused.code, "validation");
    assert!(refused.message.contains("no view"), "{}", refused.message);
}

/// Developing the picks of two events brings in exactly those photographs, in batches — the first
/// of one file, then the rest of its event, then the next event — each one library change announced
/// once: new folders named after the events with their spans, each photograph with its
/// fingerprint, interpretation, Original entry, capture row with its place, source folder and
/// burst, the committed picks cleared, and every original unchanged.
#[test]
fn develop_picks_brings_in_the_picked_photographs_in_batches_one_event_each() {
    let harness = Harness::new("batches");
    let trip = harness.dir.join("trip");
    let k1 = photo(
        &trip.join("k1.jpg"),
        &Shot::at("2026:09:12 10:00:00").subsec("10"),
    );
    let k2 = photo(
        &trip.join("k2.jpg"),
        &Shot::at("2026:09:12 10:00:00").subsec("60"),
    );
    let k3 = photo(&trip.join("k3.jpg"), &Shot::at("2026:09:12 14:00:00"));
    let zurich = |time: &str| Shot::at(time).in_place(47.3769, 8.5417);
    let z1 = photo(&trip.join("z1.jpg"), &zurich("2026:09:16 10:00:00"));
    let z2 = photo(&trip.join("z2.jpg"), &zurich("2026:09:16 11:00:00"));
    let unpicked = photo(&trip.join("k4.jpg"), &Shot::at("2026:09:12 15:00:00"));
    let all = [&k1, &k2, &k3, &z1, &z2, &unpicked];
    harness.index_paths(&all);
    let before: Vec<_> = all.iter().map(|path| untouched(path)).collect();
    harness.pick(&[&k1, &k2, &k3, &z1, &z2], "pick-1");
    let sequence = harness.sequence();

    let plan = harness.ok(
        "pick.plan",
        json!({"targets": {"kind": "paths", "paths": [&k1, &k2, &k3, &z1, &z2]}}),
    );
    let job = harness.developed(
        "develop-1",
        json!({"targets": {"kind": "paths", "paths": [&k1, &k2, &k3, &z1, &z2]}}),
    );
    assert_eq!(job["status"], "ready", "{job}");
    let report = &job["result"];
    assert_eq!(
        outcomes(report),
        [
            ("k1.jpg".into(), "created".into()),
            ("k2.jpg".into(), "created".into()),
            ("k3.jpg".into(), "created".into()),
            ("z1.jpg".into(), "created".into()),
            ("z2.jpg".into(), "created".into()),
        ]
    );
    assert_eq!(report["failed"], json!([]));
    let changes = report["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 3, "{report}");

    // Each batch is one change of one event, announced once, in order.
    let kinds = |sequence: &Value| -> Vec<String> {
        harness.inspect(sequence)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let item = &row["item"];
                let key = item["path"]
                    .as_str()
                    .map(|path| {
                        Path::new(path)
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_default();
                format!("{} {key}", item["kind"].as_str().unwrap())
            })
            .collect()
    };
    assert_eq!(
        kinds(&changes[0]),
        ["catalog-folder ", "developed-asset ", "pick k1.jpg"]
    );
    assert_eq!(
        kinds(&changes[1]),
        [
            "developed-asset ",
            "pick k2.jpg",
            "developed-asset ",
            "pick k3.jpg"
        ]
    );
    assert_eq!(
        kinds(&changes[2]),
        [
            "catalog-folder ",
            "developed-asset ",
            "pick z1.jpg",
            "developed-asset ",
            "pick z2.jpg"
        ]
    );
    let first = harness.inspect(&changes[0]);
    assert_eq!(first["change"]["label"], "Developed k1.jpg");
    assert_eq!(first["change"]["method"], "pick.develop");
    assert_eq!(first["change"]["actor"], ACTOR);
    assert_eq!(first["change"]["request_id"], "develop-1");
    assert_eq!(
        harness.inspect(&changes[1])["change"]["label"],
        "Developed 2"
    );
    let events = harness.events_after(sequence);
    let job = &events
        .iter()
        .rev()
        .find(|event| event["method"] == "pick.develop")
        .map(|event| event["job_id"].clone())
        .expect("the job's end");
    assert!(job.is_string(), "{events:?}");
    let announced: Vec<&Value> = events
        .iter()
        .filter(|event| event["method"] == "pick.develop")
        .map(|event| &event["library_sequence"])
        .collect();
    let mut expected: Vec<&Value> = changes.iter().collect();
    expected.push(&Value::Null);
    assert_eq!(
        announced, expected,
        "one event a batch, then the job's end: {events:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| event["method"] == "pick.develop")
            .all(|event| &event["job_id"] == job),
        "every one names the job: {events:?}"
    );

    // The folders the plan proposed, made from their events.
    let folders = harness.ok("folder.list", json!({}))["folders"].clone();
    let names: Vec<&str> = folders
        .as_array()
        .unwrap()
        .iter()
        .map(|folder| folder["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Konstanz · Sep 2026", "Zürich · Sep 2026"]);
    let konstanz = &folders[0];
    assert_eq!(konstanz["count"], 3);
    assert_eq!(konstanz["event"]["event_id"], plan["events"][0]["event_id"]);
    assert_eq!(
        konstanz["event"]["start_ms"],
        instant("2026:09:12 10:00:00") + 100
    );
    assert_eq!(
        konstanz["event"]["end_ms"],
        instant("2026:09:12 15:00:00"),
        "the event's span, its unpicked frame included"
    );

    // Each photograph as the catalog holds it.
    let rows = harness.query(
        "SELECT a.file_name, a.fingerprint, a.source_folder, f.name, a.availability,
             a.develop_moment, a.source_kind, c.place, c.make, c.model, c.iso, c.local_text
         FROM assets a JOIN capture c ON c.asset_row = a.row_id
         JOIN catalog_folders f ON f.id = a.catalog_folder_id ORDER BY a.file_name",
        vec![],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                (
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ),
            ))
        },
    );
    assert_eq!(rows.len(), 5);
    for (row, path) in rows.iter().zip([&k1, &k2, &k3, &z1, &z2]) {
        let (name, fingerprint, source_folder, folder, availability, _, kind, place, camera) = row;
        assert_eq!(*name, path.file_name().unwrap().to_string_lossy());
        assert_eq!(*fingerprint, untouched(path).0);
        assert_eq!(Path::new(source_folder), trip.canonicalize().unwrap());
        assert_eq!(availability, "available");
        assert_eq!(kind, "jpeg");
        let expected = if name.starts_with('k') {
            ("Konstanz · Sep 2026", "Konstanz")
        } else {
            ("Zürich · Sep 2026", "Zürich")
        };
        assert_eq!(
            (folder.as_str(), place.as_deref()),
            (expected.0, Some(expected.1))
        );
        assert_eq!(camera.1.as_deref(), Some("NIKON Z 8"));
        assert_eq!(camera.2, Some(100));
        assert!(camera.3.as_deref().unwrap().starts_with("2026-09-1"));
    }
    // The burst's two frames record their moment; the frame on its own records none.
    assert!(rows[0].5.is_some());
    assert_eq!(rows[0].5, rows[1].5);
    assert_eq!(rows[2].5, None);
    // Each photograph holds its Original only, its interpretation its header's upright size.
    let state = harness.state(&asset_of(report, "k1.jpg"));
    assert_eq!(state["revision"], 0);
    assert_eq!(state["current_entry"]["label"], "Original");
    assert_eq!(state["asset"]["locator"], json!(k1));
    assert_eq!(state["asset"]["width"], 480);
    assert_eq!(state["asset"]["height"], 320);

    // The committed picks are cleared, and no original changed.
    assert!(harness.picks().is_empty());
    let after: Vec<_> = all.iter().map(|path| untouched(path)).collect();
    assert_eq!(after, before);
    // A Develop with nothing to develop is refused.
    let refused = harness.refused(
        "pick.develop",
        json!({"into": [], "targets": {"kind": "paths", "paths": []}, "mutation": envelope("none")}),
    );
    assert_eq!(refused.code, "validation");
}

/// A file with a photograph's bytes links to it — a copy elsewhere, or the photograph's own
/// original — adding nothing; a photograph whose original is missing is relinked by a file of its
/// name, length and bytes, keeping its folder, and not by one of its name and length whose bytes
/// differ, which becomes a photograph of its own; a photograph whose file was renamed within its
/// volume is relinked by that file. A single file no index lists develops too.
#[test]
fn develop_picks_links_identical_files_and_relinks_a_missing_original_only_on_a_match() {
    let harness = Harness::new("link");
    let a = photo(
        &harness.dir.join("shoot").join("a.jpg"),
        &Shot::at("2026:09:12 10:00:00"),
    );
    let b = photo(
        &harness.dir.join("shoot").join("b.jpg"),
        &Shot::at("2026:09:12 11:00:00"),
    );
    let first = harness.develop_paths("develop-a", &[&a, &b]);
    let (asset_a, asset_b) = (asset_of(&first, "a.jpg"), asset_of(&first, "b.jpg"));
    let folder_b = harness.state(&asset_b)["asset"]["source_root"].clone();

    // A copy of a's bytes, and a's own file: both link to it.
    let copy = harness.dir.join("elsewhere").join("a-copy.jpg");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(&a, &copy).unwrap();
    let copy = copy.canonicalize().unwrap();
    let linked = harness.develop_paths("develop-copy", &[&copy, &a]);
    assert_eq!(
        outcomes(&linked),
        [
            ("a-copy.jpg".into(), "linked".into()),
            ("a.jpg".into(), "linked".into())
        ]
    );
    assert_eq!(linked["developed"][0]["asset_id"], asset_a);
    assert_eq!(linked["developed"][1]["asset_id"], asset_a);
    assert_eq!(
        linked["changes"],
        json!([]),
        "nothing was picked, nothing changed"
    );
    assert_eq!(harness.photographs(), 2);

    // b's original goes missing: copied to another folder, and the file it was removed.
    let moved = harness.dir.join("moved").join("b.jpg");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::copy(&b, &moved).unwrap();
    fs::remove_file(&b).unwrap();
    let moved = moved.canonicalize().unwrap();
    // A file of its name and length with other bytes (another second) is its own photograph.
    let lookalike = photo(
        &harness.dir.join("other").join("b.jpg"),
        &Shot::at("2026:09:12 11:00:01"),
    );
    assert_eq!(
        lookalike.metadata().unwrap().len(),
        moved.metadata().unwrap().len()
    );
    let created = harness.develop_paths("develop-lookalike", &[&lookalike]);
    assert_eq!(outcomes(&created), [("b.jpg".into(), "created".into())]);
    assert_ne!(created["developed"][0]["asset_id"], asset_b);
    assert_eq!(
        harness.state(&asset_b)["asset"]["locator"],
        json!(b),
        "b still points where it was"
    );

    // The moved file itself relinks b, keeping its catalog folder.
    let folder_of = |asset: &Value| {
        harness.query(
            "SELECT catalog_folder_id FROM assets WHERE id = ?1",
            vec![asset.as_str().unwrap().to_owned()],
            |row| row.get::<_, String>(0),
        )
    };
    let folder_before = folder_of(&asset_b);
    let relinked = harness.develop_paths("develop-moved", &[&moved]);
    assert_eq!(outcomes(&relinked), [("b.jpg".into(), "relinked".into())]);
    assert_eq!(relinked["developed"][0]["asset_id"], asset_b);
    let state = harness.state(&asset_b);
    assert_eq!(state["asset"]["locator"], json!(moved));
    assert_ne!(state["asset"]["source_root"], folder_b);
    assert_eq!(folder_of(&asset_b), folder_before);
    let detail = harness.inspect(&relinked["changes"][0]);
    assert_eq!(
        detail["rows"][0]["item"],
        json!({"kind": "asset-source", "asset_id": asset_b})
    );
    assert_eq!(detail["rows"][0]["after"]["locator"], json!(moved));
    assert_eq!(harness.photographs(), 3);

    // a's own file renamed within its volume is found again by its identity and bytes.
    let renamed = harness.dir.join("renamed").join("Lake.jpg");
    fs::create_dir_all(renamed.parent().unwrap()).unwrap();
    fs::rename(&a, &renamed).unwrap();
    let renamed = renamed.canonicalize().unwrap();
    let found = harness.develop_paths("develop-renamed", &[&renamed]);
    assert_eq!(outcomes(&found), [("Lake.jpg".into(), "relinked".into())]);
    assert_eq!(found["developed"][0]["asset_id"], asset_a);
    assert_eq!(harness.state(&asset_a)["asset"]["locator"], json!(renamed));
    assert_eq!(harness.photographs(), 3);
    assert_eq!(
        harness.ok("catalog.info", json!({}))["counts"]["unavailable"],
        0
    );
}

/// A card's picks are refused unless something covers them — naming the volume — then developed
/// from a copy in an indexed folder whose fingerprint matches, with `use_copies`; a copy with other
/// bytes leaves its pick refused and picked unless `confirm_removable` develops it from the card.
/// Neither the card's files nor the copies change.
#[test]
fn develop_picks_uses_verified_card_copies_and_otherwise_needs_confirm_removable() {
    let harness = Harness::new("card");
    let dcim = harness.dir.join("card").join("DCIM").join("100NZ8_1");
    let x = photo(&dcim.join("x.jpg"), &Shot::at("2026:09:12 10:00:00"));
    let y = photo(&dcim.join("y.jpg"), &Shot::at("2026:09:12 10:10:00"));
    let z = photo(&dcim.join("z.jpg"), &Shot::at("2026:09:12 10:20:00"));
    let dump = harness.dir.join("dump").join("2026-09-12");
    fs::create_dir_all(&dump).unwrap();
    fs::copy(&x, dump.join("x.jpg")).unwrap();
    let x_copy = dump.join("x.jpg").canonicalize().unwrap();
    // y's "copy" has its name and length and other bytes.
    let y_copy = photo(&dump.join("y.jpg"), &Shot::at("2026:09:12 10:10:01"));
    assert_eq!(
        y_copy.metadata().unwrap().len(),
        y.metadata().unwrap().len()
    );
    let card = card(&harness.dir.join("card"));
    harness.index(vec![
        record(&x, &card.id),
        record(&y, &card.id),
        record(&z, &card.id),
    ]);
    harness.index_paths(&[&x_copy, &y_copy]);
    let indexed = IndexedFolder {
        path: harness.dir.join("dump"),
        volume_id: volume_of(&dump, 0).unwrap().id,
        added_ms: 1,
        actor: ACTOR.into(),
    };
    let fixed = volume_of(&dump, 0).unwrap();
    let recorded = card.clone();
    harness.arrange(move |tx| {
        upsert_volume(tx, &recorded)?;
        upsert_volume(tx, &fixed)?;
        insert_indexed_folder(tx, &indexed)
    });
    let files = [&x, &y, &z, &x_copy, &y_copy];
    let before: Vec<_> = files.iter().map(|path| untouched(path)).collect();
    harness.pick(&[&x, &y, &z], "pick-card");
    let all = json!({"kind": "paths", "paths": [&x, &y, &z]});

    let refused = harness.develop("develop-refused", json!({"targets": all}));
    let error = refused.error.expect("removable picks are refused");
    assert_eq!(error.code, "conflict");
    assert!(error.message.contains("NIKON Z 8"), "{}", error.message);
    let data = error.data.unwrap();
    assert_eq!(data["volume_id"], json!(card.id));
    assert_eq!(
        (data["count"].clone(), data["with_copy"].clone()),
        (json!(3), json!(2))
    );
    let refused = harness.develop(
        "develop-refused-copies",
        json!({"targets": all, "use_copies": true}),
    );
    let data = refused.error.expect("z has no copy").data.unwrap();
    assert_eq!(
        (data["count"].clone(), data["with_copy"].clone()),
        (json!(1), json!(0))
    );
    assert_eq!(harness.photographs(), 0);

    // x from its verified copy; y's copy is not its bytes, so it stays picked.
    let job = harness.developed(
        "develop-copies",
        json!({"targets": {"kind": "paths", "paths": [&x, &y]}, "use_copies": true}),
    );
    let report = &job["result"];
    assert_eq!(outcomes(report), [("x.jpg".into(), "created".into())]);
    assert_eq!(report["developed"][0]["used"], json!(x_copy));
    assert_eq!(
        harness.state(&asset_of(report, "x.jpg"))["asset"]["locator"],
        json!(x_copy)
    );
    assert_eq!(report["failed"][0]["path"], json!(y));
    assert_eq!(report["failed"][0]["code"], "conflict");
    assert_eq!(harness.picks(), [y.clone(), z.clone()]);

    // Confirmed, y and z are developed from the card.
    let job = harness.developed(
        "develop-confirmed",
        json!({"targets": {"kind": "paths", "paths": [&y, &z]}, "use_copies": true, "confirm_removable": true}),
    );
    let report = &job["result"];
    assert_eq!(
        outcomes(report),
        [
            ("y.jpg".into(), "created".into()),
            ("z.jpg".into(), "created".into())
        ]
    );
    assert!(report["developed"][0].get("used").is_none());
    assert_eq!(
        harness.state(&asset_of(report, "y.jpg"))["asset"]["locator"],
        json!(y)
    );
    assert!(harness.picks().is_empty());
    let after: Vec<_> = files.iter().map(|path| untouched(path)).collect();
    assert_eq!(after, before);
}

/// Cancelled once its first batch has committed, a Develop keeps exactly that photograph, whole,
/// and its pick cleared; the rest stay picked and nothing of them is written. Its progress is on
/// the activity board meanwhile.
#[test]
fn develop_picks_cancelled_mid_job_keeps_exactly_the_committed_photographs() {
    let harness = Harness::new("cancel");
    let trip = harness.dir.join("trip");
    let paths: Vec<PathBuf> = (0..3)
        .map(|at| {
            photo(
                &trip.join(format!("k{at}.jpg")),
                &Shot::at(&format!("2026:09:12 1{at}:00:00")),
            )
        })
        .collect();
    let refs: Vec<&PathBuf> = paths.iter().collect();
    harness.index_paths(&refs);
    harness.pick(&refs, "pick-cancel");
    // Held as its second file is ready to commit: the first has committed alone.
    let gate = harness.hold_at(Phase::Verified, 2);
    let started = harness
        .develop(
            "develop-cancel",
            json!({"targets": {"kind": "paths", "paths": refs}}),
        )
        .result
        .unwrap();
    let job = &started["job_id"];
    gate.wait_reached(1, "the second file");
    assert_eq!(harness.photographs(), 1);
    let board = harness.ok("activity.list", json!({}));
    let entry = board["active"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "pick.develop")
        .cloned()
        .unwrap_or_else(|| panic!("the Develop is on the board: {board}"));
    assert_eq!(entry["label"], "Developing picks");
    assert_eq!(entry["detail"], "3 files");
    assert_eq!(entry["job_id"], *job);
    assert_eq!(
        entry["progress"],
        json!({"fraction": 1.0 / 3.0, "message": "1 of 3"}),
        "the first file is done, the second held before it is counted"
    );
    harness.ok("job.cancel", json!({"job_id": job}));
    gate.open();
    let settled = harness.settle(job);
    assert_eq!(settled["status"], "cancelled", "{settled}");
    harness.release_holds();
    assert_eq!(harness.photographs(), 1);
    assert_eq!(harness.picks(), [paths[1].clone(), paths[2].clone()]);
    let journal = harness.ok("library.journal", json!({}))["changes"].clone();
    let developed: Vec<&Value> = journal
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["method"] == "pick.develop")
        .collect();
    assert_eq!(developed.len(), 1);
    assert_eq!(developed[0]["label"], "Developed k0.jpg");
    // The one photograph is whole: its Original, its state and its capture row.
    let whole = harness.query(
        "SELECT (SELECT count(*) FROM entries), (SELECT count(*) FROM asset_state),
             (SELECT count(*) FROM capture), (SELECT count(*) FROM assets)",
        vec![],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    );
    assert_eq!(whole, [(1, 1, 1, 1)]);
}

/// A file that cannot be developed — a RAW its decoder refuses — is reported with its refusal and
/// stays picked; the others are developed.
#[test]
fn develop_picks_reports_a_failing_file_and_leaves_it_picked() {
    let harness = Harness::new("failing");
    let trip = harness.dir.join("trip");
    let k1 = photo(&trip.join("k1.jpg"), &Shot::at("2026:09:12 10:00:00"));
    let broken = broken_raw(&trip.join("DSC_0001.NEF"));
    let k2 = photo(&trip.join("k2.jpg"), &Shot::at("2026:09:12 11:00:00"));
    harness.index_paths(&[&k1, &broken, &k2]);
    harness.pick(&[&k1, &broken, &k2], "pick-failing");
    let report = harness.develop_paths("develop-failing", &[&k1, &broken, &k2]);
    assert_eq!(
        outcomes(&report),
        [
            ("k1.jpg".into(), "created".into()),
            ("k2.jpg".into(), "created".into())
        ]
    );
    let failed = report["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{report}");
    assert_eq!(failed[0]["path"], json!(broken));
    assert!(
        matches!(
            failed[0]["code"].as_str(),
            Some("invalid-input" | "unsupported-input")
        ),
        "{report}"
    );
    assert_eq!(harness.picks(), [broken]);
    assert_eq!(harness.photographs(), 2);
}

/// A retried Develop answers its first job and changes nothing again, and after a restart it is
/// answered as a finished job with the report its recorded changes give.
#[test]
fn develop_picks_answers_a_retry_with_the_first_result() {
    let harness = Harness::new("retry");
    let trip = harness.dir.join("trip");
    let k1 = photo(&trip.join("k1.jpg"), &Shot::at("2026:09:12 10:00:00"));
    let k2 = photo(&trip.join("k2.jpg"), &Shot::at("2026:09:12 11:00:00"));
    harness.index_paths(&[&k1, &k2]);
    harness.pick(&[&k1, &k2], "pick-retry");
    let params = json!({"targets": {"kind": "paths", "paths": [&k1, &k2]}});
    let first = harness
        .develop("develop-retry", params.clone())
        .result
        .unwrap();
    let job = harness.settle(&first["job_id"]);
    assert_eq!(job["status"], "ready");
    let sequence = harness.sequence();
    let retried = harness
        .develop("develop-retry", params.clone())
        .result
        .unwrap();
    assert_eq!(retried["job_id"], first["job_id"]);
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(harness.sequence(), sequence, "the retry records no event");
    assert_eq!(harness.photographs(), 2);

    // After a restart the journal answers it: a finished job, its report rebuilt from the
    // changes the first attempt recorded, and nothing developed again.
    let harness = harness.restart();
    let sequence = harness.sequence();
    let again = harness.develop("develop-retry", params).result.unwrap();
    assert_ne!(again["job_id"], first["job_id"]);
    assert_eq!(again["status"], "ready", "answered at once");
    let answered = harness.settle(&again["job_id"]);
    assert_eq!(answered["status"], "ready");
    assert_eq!(harness.sequence(), sequence, "the retry records no event");
    assert_eq!(answered["kind"], "develop-picks");
    assert_eq!(answered["result"], job["result"], "the first result");
    assert_eq!(harness.photographs(), 2);
}

/// `library.undo` of a Develop sends its unedited photograph back — its record deleted, its file
/// picked again as it was, its new folder gone — and redoing it is refused; an edited photograph
/// refuses the undo, naming it, and nothing changes.
#[test]
fn develop_picks_undo_sends_back_and_repicks_and_refuses_an_edited_photograph() {
    let harness = Harness::new("undo");
    let trip = harness.dir.join("trip");
    let k1 = photo(&trip.join("k1.jpg"), &Shot::at("2026:09:12 10:00:00"));
    harness.index_paths(&[&k1]);
    harness.pick(&[&k1], "pick-undo");
    let pick = harness.ok("pick.list", json!({}))["picks"][0].clone();
    let report = harness.develop_paths("develop-undo", &[&k1]);
    let asset = asset_of(&report, "k1.jpg");
    assert_eq!(
        harness.ok("folder.list", json!({}))["folders"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo-1")}));
    assert_eq!(undone["outcome"], "applied", "{undone}");
    assert_eq!(harness.photographs(), 0);
    assert_eq!(
        harness
            .refused("asset.state", json!({"asset_id": asset}))
            .code,
        "validation"
    );
    let repicked = harness.ok("pick.list", json!({}))["picks"][0].clone();
    assert_eq!(repicked, pick, "picked again exactly as it was");
    assert_eq!(harness.ok("folder.list", json!({}))["folders"], json!([]));
    let redo = harness.refused("library.redo", json!({"mutation": envelope("redo-1")}));
    assert_eq!(redo.code, "conflict");
    assert!(redo.message.contains("sent back"), "{}", redo.message);
    assert_eq!(harness.photographs(), 0);

    // Developed again and edited, it refuses the undo.
    let report = harness.develop_paths("develop-again", &[&k1]);
    let asset = asset_of(&report, "k1.jpg");
    let revision = harness.state(&asset)["revision"].as_u64().unwrap();
    harness.prepared(
        "edit.set-basic",
        json!({"asset_id": asset, "mutation": mutation_json(revision, "expose"), "exposure": 0.5}),
    );
    let refused = harness.refused("library.undo", json!({"mutation": envelope("undo-2")}));
    assert_eq!(refused.code, "conflict");
    assert!(
        refused
            .message
            .contains("k1.jpg has been edited since it was developed"),
        "{}",
        refused.message
    );
    assert_eq!(
        refused.data.unwrap()["items"],
        json!([{"kind": "developed-asset", "asset_id": asset}])
    );
    assert_eq!(harness.photographs(), 1);
    assert!(harness.picks().is_empty());
}

/// A Develop committed in several batches is undone as the one change it was: one `library.undo`
/// sends every photograph back and picks every file again, recorded as one undo per batch and
/// announced once, and a retry of that undo answers them all; redoing it is refused, as a
/// sent-back photograph is developed again with `pick.develop`.
#[test]
fn develop_picks_undo_of_a_develop_in_batches_is_one_step() {
    let harness = Harness::new("undo-batches");
    let trip = harness.dir.join("trip");
    let files: Vec<PathBuf> = (0..3)
        .map(|index| {
            photo(
                &trip.join(format!("b{index}.jpg")),
                &Shot::at(&format!("2026:09:12 10:00:0{index}")),
            )
        })
        .collect();
    let paths: Vec<&PathBuf> = files.iter().collect();
    harness.index_paths(&paths);
    harness.pick(&paths, "pick-batches");
    let picks = harness.ok("pick.list", json!({}))["picks"].clone();
    let report = harness.develop_paths("develop-batches", &paths);
    assert_eq!(
        report["changes"].as_array().unwrap().len(),
        2,
        "the first batch of one file, then the rest: {report}"
    );
    assert_eq!(harness.photographs(), 3);

    let sequence = harness.sequence();
    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo-all")}));
    assert_eq!(
        (&undone["outcome"], &undone["items"]),
        (&json!("applied"), &json!(7)),
        "three photographs sent back, three picks restored and the new folder gone: {undone}"
    );
    assert_eq!(harness.photographs(), 0);
    assert_eq!(harness.ok("folder.list", json!({}))["folders"], json!([]));
    assert_eq!(harness.ok("pick.list", json!({}))["picks"], picks);
    assert_eq!(harness.events_after(sequence).len(), 1, "announced once");
    let journal = harness.ok("library.journal", json!({}))["changes"].clone();
    let undos: Vec<&Value> = journal
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["method"] == "library.undo")
        .collect();
    assert_eq!(undos.len(), 2, "one undo per batch");
    let retry = harness.ok("library.undo", json!({"mutation": envelope("undo-all")}));
    assert_eq!(
        (&retry["change"], &retry["items"], &retry["deduplicated"]),
        (&undone["change"], &json!(7), &json!(true))
    );
    assert_eq!(harness.photographs(), 0, "a retry undoes nothing more");

    // A sent-back photograph is developed again with pick.develop, so redoing the undo is refused,
    // naming the first photograph it would bring back, and changes nothing.
    let sequence = harness.sequence();
    let refused = harness
        .send("library.redo", json!({"mutation": envelope("redo-all")}))
        .error
        .expect("a refusal");
    assert_eq!(refused.code, "conflict", "{refused:?}");
    assert!(refused.message.contains("develop it again"), "{refused:?}");
    assert_eq!(refused.data.unwrap()["items"][0]["kind"], "developed-asset");
    assert_eq!(harness.photographs(), 0);
    assert_eq!(harness.ok("pick.list", json!({}))["picks"], picks);
    assert!(harness.events_after(sequence).is_empty());
}

/// `asset.send-back` sends an unedited photograph back as one library change — its record gone,
/// its file picked again with its signature now — answered once for a retry, and not undone; it
/// refuses a photograph with an edit, one in a collection and one whose original is missing, each
/// saying why.
#[test]
fn develop_picks_send_back_returns_an_unedited_photograph_to_its_picks() {
    let harness = Harness::new("send-back");
    let trip = harness.dir.join("trip");
    let paths: Vec<PathBuf> = (0..4)
        .map(|at| {
            photo(
                &trip.join(format!("k{at}.jpg")),
                &Shot::at(&format!("2026:09:12 1{at}:00:00")),
            )
        })
        .collect();
    let refs: Vec<&PathBuf> = paths.iter().collect();
    let report = harness.develop_paths("develop-send", &refs);
    let assets: Vec<Value> = (0..4)
        .map(|at| asset_of(&report, &format!("k{at}.jpg")))
        .collect();
    let send = |asset: &Value, request_id: &str| {
        harness.send(
            "asset.send-back",
            json!({"targets": {"kind": "assets", "asset_ids": [asset]}, "mutation": envelope(request_id)}),
        )
    };

    // A rendered preview the preview cache holds of the photograph, as lane B writes one.
    let index = rusqlite::Connection::open(
        crate::index::index_dir(&harness.catalog).join(crate::INDEX_FILE),
    )
    .unwrap();
    index
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let previews_of = |asset: &Value| -> i64 {
        index
            .query_row(
                "SELECT count(*) FROM photo_previews WHERE asset_id = ?1",
                [asset.as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap()
    };
    index
        .execute(
            "INSERT INTO photo_previews (asset_id, entry_id, tier, renderer, path, width, height,
                 bytes, origin, last_used_ms, approximate)
             VALUES (?1, 'entry-0', 'grid', 1, ?2, 512, 341, 4, 'rendered', 0, 0)",
            [
                assets[0].as_str().unwrap().to_owned(),
                harness
                    .dir
                    .join("k0-grid.jpg")
                    .to_string_lossy()
                    .into_owned(),
            ],
        )
        .unwrap();
    assert_eq!(previews_of(&assets[0]), 1);

    let answer = send(&assets[0], "send-1").result.unwrap();
    assert_eq!(answer["outcome"], "applied");
    assert_eq!(answer["items"], 2);
    assert_eq!(harness.photographs(), 3);
    assert_eq!(
        previews_of(&assets[0]),
        0,
        "the preview cache forgets a photograph sent back"
    );
    assert_eq!(
        harness
            .refused("asset.state", json!({"asset_id": assets[0]}))
            .code,
        "validation"
    );
    let picks = harness.ok("pick.list", json!({}))["picks"].clone();
    assert_eq!(picks[0]["path"], json!(paths[0]));
    assert_eq!(
        picks[0]["signature"]["len"],
        paths[0].metadata().unwrap().len()
    );
    let detail = harness.inspect(&answer["change"]);
    assert_eq!(detail["change"]["label"], "Sent back k0.jpg");
    assert_eq!(detail["rows"][0]["after"], Value::Null);
    let retried = send(&assets[0], "send-1").result.unwrap();
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["change"], answer["change"]);
    let undo = harness.refused("library.undo", json!({"mutation": envelope("undo-send")}));
    assert_eq!(undo.code, "conflict");

    // Edited: refused.
    let revision = harness.state(&assets[1])["revision"].as_u64().unwrap();
    harness.prepared(
        "edit.set-basic",
        json!({"asset_id": assets[1], "mutation": mutation_json(revision, "expose"), "exposure": 0.5}),
    );
    let refused = send(&assets[1], "send-2").error.unwrap();
    assert_eq!(refused.code, "conflict");
    assert!(
        refused
            .message
            .contains("k1.jpg has been edited since it was developed"),
        "{}",
        refused.message
    );
    assert_eq!(
        refused.data.unwrap()["items"],
        json!([{"kind": "developed-asset", "asset_id": assets[1]}])
    );
    // In a collection: refused.
    let collection = harness.ok(
        "collection.create",
        json!({"name": "Portfolio", "kind": "collection", "mutation": envelope("portfolio")}),
    )["collection"]["id"]
        .clone();
    harness.ok(
        "collection.add",
        json!({"collection_id": collection, "targets": {"kind": "assets", "asset_ids": [assets[2]]}, "mutation": envelope("add")}),
    );
    let refused = send(&assets[2], "send-3").error.unwrap();
    assert!(
        refused.message.contains("k2.jpg is in a collection"),
        "{}",
        refused.message
    );
    // Its original missing: refused, since its file could not be picked again.
    fs::rename(&paths[3], harness.dir.join("k3-away.jpg")).unwrap();
    let refused = send(&assets[3], "send-4").error.unwrap();
    assert_eq!(refused.code, "conflict");
    assert!(
        refused.message.contains("its original is not at"),
        "{}",
        refused.message
    );
    assert_eq!(harness.photographs(), 3);
}

/// `into` sends an event to the folder its entry names, and every other event to the folder the
/// entry with no event names, made with that event's span; an unknown event, two default entries,
/// an unknown folder and a new folder whose name is taken are each refused, developing nothing.
#[test]
fn develop_picks_into_sends_each_event_to_the_folder_it_names() {
    let harness = Harness::new("into");
    let trip = harness.dir.join("trip");
    let k1 = photo(&trip.join("k1.jpg"), &Shot::at("2026:09:12 10:00:00"));
    let z1 = photo(
        &trip.join("z1.jpg"),
        &Shot::at("2026:09:16 10:00:00").in_place(47.3769, 8.5417),
    );
    harness.index_paths(&[&k1, &z1]);
    let targets = json!({"kind": "paths", "paths": [&k1, &z1]});
    let plan = harness.ok("pick.plan", json!({"targets": targets}));
    let (konstanz, zurich) = (
        plan["events"][0]["event_id"].clone(),
        plan["events"][1]["event_id"].clone(),
    );
    let portfolio = harness.ok(
        "folder.create",
        json!({"name": "Portfolio", "mutation": envelope("portfolio")}),
    )["folder"]["id"]
        .clone();

    for (into, code) in [
        (
            json!([{"event_id": "event-00000000000000000000000000000000", "folder": {"kind": "new", "name": "X"}}]),
            "conflict",
        ),
        (
            json!([{"folder": {"kind": "new", "name": "X"}}, {"folder": {"kind": "new", "name": "Y"}}]),
            "validation",
        ),
        (
            json!([{"folder": {"kind": "existing", "folder_id": CatalogFolderId::new()}}]),
            "validation",
        ),
        (
            json!([{"folder": {"kind": "new", "name": "portfolio"}}]),
            "conflict",
        ),
    ] {
        let refused = harness
            .develop("refused", json!({"targets": targets, "into": into}))
            .error
            .unwrap_or_else(|| panic!("{into} was accepted"));
        assert_eq!(refused.code, code, "{into}: {}", refused.message);
    }
    assert_eq!(harness.photographs(), 0);

    let job = harness.developed(
        "develop-into",
        json!({"targets": targets, "into": [
            {"event_id": konstanz, "folder": {"kind": "existing", "folder_id": portfolio}},
            {"folder": {"kind": "new", "name": "Rest"}},
        ]}),
    );
    let report = &job["result"];
    let folder_of = |asset: &Value| {
        harness.query(
            "SELECT f.name FROM assets a JOIN catalog_folders f ON f.id = a.catalog_folder_id
             WHERE a.id = ?1",
            vec![asset.as_str().unwrap().to_owned()],
            |row| row.get::<_, String>(0),
        )
    };
    assert_eq!(folder_of(&asset_of(report, "k1.jpg")), ["Portfolio"]);
    assert_eq!(folder_of(&asset_of(report, "z1.jpg")), ["Rest"]);
    let folders = harness.ok("folder.list", json!({}))["folders"].clone();
    let rest = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|folder| folder["name"] == "Rest")
        .unwrap();
    assert_eq!(rest["event"]["event_id"], zurich);
    assert_eq!(rest["event"]["start_ms"], instant("2026:09:16 10:00:00"));
    let portfolio = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|folder| folder["name"] == "Portfolio")
        .unwrap();
    assert_eq!(
        portfolio.get("event"),
        None,
        "an existing folder keeps its own"
    );
}

/// On the owner's Mac: copies of the owner's RAW fixtures develop into photographs whose stored
/// interpretation is the one their first preparation decodes again, so each opens in Develop, and
/// the copies are unchanged. Set `LUXFORGE_RAW_OWNER_DIR` to the directory holding
/// `nikon_z6.NEF`, `fujifilm_x100vi.RAF` and `mavic_air_2s.DNG`.
#[test]
#[ignore = "requires the owner's RAW fixtures: set LUXFORGE_RAW_OWNER_DIR"]
fn develop_picks_develops_the_owners_raw_files_that_then_open() {
    let owner = PathBuf::from(std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("RAW directory"));
    let harness = Harness::new("raw");
    let copies: Vec<PathBuf> = ["nikon_z6.NEF", "fujifilm_x100vi.RAF", "mavic_air_2s.DNG"]
        .iter()
        .map(|name| {
            let copy = harness.dir.join("raw").join(name);
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::copy(owner.join(name), &copy).unwrap();
            copy.canonicalize().unwrap()
        })
        .collect();
    let refs: Vec<&PathBuf> = copies.iter().collect();
    let before: Vec<_> = refs.iter().map(|path| untouched(path)).collect();
    let report = harness.develop_paths("develop-raw", &refs);
    assert_eq!(report["failed"], json!([]), "{report}");
    for path in &copies {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let asset = asset_of(&report, &name);
        let state = harness.state(&asset);
        assert_eq!(state["asset"]["source"]["kind"], "raw", "{name}");
        assert_eq!(state["asset"]["fingerprint"], json!(untouched(path).0));
        let started = harness.ok("source.prepare", json!({"asset_id": asset}));
        let prepared = harness.settle(&started["job_id"]);
        assert_eq!(prepared["status"], "ready", "{name}: {prepared}");
    }
    let after: Vec<_> = refs.iter().map(|path| untouched(path)).collect();
    assert_eq!(after, before);
}
