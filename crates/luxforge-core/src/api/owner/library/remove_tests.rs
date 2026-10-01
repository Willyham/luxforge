//! Removing, putting back and emptying Removed through the catalog owner, as clients call them:
//! removal and Put back as library changes undone and redone, keeping each photograph's history,
//! folder and collections; every other view and count leaving removed photographs out and Put back
//! returning them exactly; `catalog.empty-removed` forbidden without permission authority and
//! otherwise deleting exactly the removed photographs with their history, their own strokes and
//! artifacts, and nothing another photograph still names; no other path deleting an edited
//! photograph's entries; and the originals never touched. Every file is written in a scratch
//! directory.
use super::super::{Owner, OwnerMessage, catalog::CatalogMessage};
use super::LibraryMessage;
use crate::{
    EditorService,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientAuthority, ClientId, OwnerHandle},
    artifacts::{
        ArtifactId, object_path,
        testing::{registry, tint_bytes, tint_meta},
    },
    editor::{mutation_json, write},
    index::{upsert_file, volume_of},
    library::develop::develop_picks_tests::{Shot, photo, record, untouched},
    modules::PROOF_MODULE,
};
use luxforge_testbase::paths::temp_dir;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

/// The actor every client request here is made as.
const ACTOR: &str = "remove-client";

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

/// `{targets, mutation}` naming `assets`.
fn targets(assets: &[&Value], request_id: &str) -> Value {
    json!({
        "targets": {"kind": "assets", "asset_ids": assets},
        "mutation": envelope(request_id),
    })
}

/// One owner over a catalog in its own scratch directory, serving the proof module's tints, with
/// an edit client and a client with permission authority, as the desktop's own is.
struct Harness {
    dir: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    admin: ClientId,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("remove-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        Self::start(dir, catalog)
    }

    fn start(dir: PathBuf, catalog: PathBuf) -> Self {
        let (owner, join) = OwnerHandle::start_with(&catalog, registry()).unwrap();
        let client = owner.register();
        let admin = owner.register_with(ClientAuthority::Permissions);
        Self {
            dir,
            catalog,
            owner,
            join: Some(join),
            client,
            admin,
        }
    }

    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    /// Stop the owner and start a new one on the same catalog, so nothing published before is
    /// live any more.
    fn restart(mut self) -> Self {
        self.stop();
        let (dir, catalog) = (std::mem::take(&mut self.dir), self.catalog.clone());
        Self::start(dir, catalog)
    }

    fn send_as(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        self.send_as(self.client, method, params)
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

    /// Empty Removed as the client with permission authority.
    fn empty(&self, request_id: &str) -> Value {
        let response = self.send_as(
            self.admin,
            "catalog.empty-removed",
            json!({"mutation": envelope(request_id)}),
        );
        assert!(response.error.is_none(), "{:?}", response.error);
        response.result.expect("a result")
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

    /// Run `f` on the owner, between requests, and answer what it answers.
    fn run<T: Send + 'static>(&self, f: impl FnOnce(&mut Owner) -> T + Send + 'static) -> T {
        let (sender, receiver) = mpsc::channel();
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(
                LibraryMessage::Run(Box::new(move |owner| {
                    let _ = sender.send(f(owner));
                })),
            )))
            .unwrap();
        receiver.recv().expect("the owner ran it")
    }

    /// Write in the catalog, outside any library change, as a test arranges it.
    fn arrange(&self, sql: &'static str, asset: &Value) {
        let asset = asset.as_str().unwrap().to_owned();
        self.run(move |owner| {
            write(&mut owner.service.connection, |tx| {
                tx.execute(sql, [&asset])?;
                Ok(())
            })
            .unwrap()
        });
    }

    /// One integer the catalog answers for `sql` with `param`.
    fn count(&self, sql: &'static str, param: &str) -> i64 {
        let param = param.to_owned();
        self.run(move |owner| {
            owner
                .service
                .connection
                .query_row(sql, [&param], |row| row.get(0))
                .unwrap()
        })
    }

    /// Every value of the first column the catalog answers for `sql`, in order.
    fn strings(&self, sql: &'static str) -> Vec<String> {
        self.run(move |owner| {
            owner
                .service
                .connection
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        })
    }

    /// `count` photographs `k0.jpg`… of one day in Konstanz, in `trip/`.
    fn photos(&self, count: usize) -> Vec<PathBuf> {
        let trip = self.dir.join("trip");
        (0..count)
            .map(|at| {
                photo(
                    &trip.join(format!("k{at}.jpg")),
                    &Shot::at(&format!("2026:09:12 1{at}:00:00")),
                )
            })
            .collect()
    }

    /// List `paths` in the index and develop them into the plan's folder, answering each one's
    /// photograph in their order.
    fn develop(&self, request_id: &str, paths: &[PathBuf]) -> Vec<Value> {
        let records: Vec<_> = paths
            .iter()
            .map(|path| record(path, &volume_of(path, 0).unwrap().id))
            .collect();
        self.run(move |owner| {
            let mut index = owner.service.index().unwrap();
            let tx = index.connection_mut().transaction().unwrap();
            for file in &records {
                upsert_file(&tx, file).unwrap();
            }
            tx.commit().unwrap();
        });
        let started = self.ok(
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "into": [],
                "mutation": envelope(request_id),
            }),
        );
        let job = self.settle(&started["job_id"]);
        assert_eq!(job["status"], "ready", "{job}");
        let developed = job["result"]["developed"].as_array().unwrap().clone();
        paths
            .iter()
            .map(|path| {
                developed
                    .iter()
                    .find(|row| row["path"] == json!(path))
                    .unwrap_or_else(|| panic!("{} was not developed", path.display()))["asset_id"]
                    .clone()
            })
            .collect()
    }

    fn revision(&self, asset: &Value) -> u64 {
        self.ok("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// Edit `asset`'s exposure: an entry beyond its Original.
    fn edit(&self, asset: &Value, request_id: &str) {
        let revision = self.revision(asset);
        self.prepared(
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation_json(revision, request_id), "exposure": 0.5}),
        );
    }

    /// Paint one brush stroke along `points` on a new mask of `asset`.
    fn paint(&self, asset: &Value, points: Value, request_id: &str) {
        let revision = self.revision(asset);
        self.prepared(
            "mask.add-stroke",
            json!({
                "asset_id": asset,
                "mutation": mutation_json(revision, request_id),
                "points": points,
                "size": 0.1,
                "feather": 50.0,
                "flow": 100.0,
                "erase": false,
            }),
        );
    }

    /// Tint `asset` with `artifact` through the proof module.
    fn tint(&self, asset: &Value, artifact: &ArtifactId, request_id: &str) {
        let revision = self.revision(asset);
        self.prepared(
            "edit.apply-proof-tint",
            json!({
                "asset_id": asset,
                "artifact": artifact.as_str(),
                "mutation": mutation_json(revision, request_id),
            }),
        );
    }

    /// Publish a tint of `gains` as an artifact of this catalog, as the proof's task would.
    fn publish(&self, gains: [f32; 3]) -> ArtifactId {
        self.run(move |owner| {
            let (record, bytes) = owner
                .service
                .artifact_writer()
                .unwrap()
                .write(&tint_bytes(gains), tint_meta(), PROOF_MODULE)
                .unwrap();
            let id = record.id.clone();
            owner
                .service
                .register_artifact(record, bytes, true)
                .unwrap();
            id
        })
    }

    fn collection(&self, name: &str, members: &[&Value]) -> Value {
        let created = self.ok(
            "collection.create",
            json!({"name": name, "kind": "collection", "mutation": envelope(&format!("create-{name}"))}),
        );
        let id = created["collection"]["id"].clone();
        if !members.is_empty() {
            let mut params = targets(members, &format!("add-{name}"));
            params["collection_id"] = id.clone();
            self.ok("collection.add", params);
        }
        id
    }

    fn counts(&self) -> Value {
        self.ok("catalog.info", json!({}))["counts"].clone()
    }

    fn inspect(&self, sequence: &Value) -> Value {
        self.ok("library.inspect", json!({"sequence": sequence}))
    }

    fn sequence(&self) -> u64 {
        self.ok("events.since", json!({"after": 0}))["current_sequence"]
            .as_u64()
            .unwrap()
    }

    /// The methods of the events recorded after `after`.
    fn events_after(&self, after: u64) -> Vec<String> {
        self.ok("events.since", json!({"after": after}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["method"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A photograph's entries, oldest first.
    fn entries(&self, asset: &Value) -> Vec<String> {
        let asset = asset.as_str().unwrap().to_owned();
        self.run(move |owner| {
            owner
                .service
                .connection
                .prepare("SELECT id FROM entries WHERE asset_id = ?1 ORDER BY sequence")
                .unwrap()
                .query_map([&asset], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        })
    }

    /// Every row the catalog holds of a photograph, table by table: its asset row, entries,
    /// state, requests, versions, capture row, memberships and its entries' artifact references.
    fn records(&self, asset: &Value) -> [i64; 8] {
        let id = asset.as_str().unwrap();
        [
            self.count("SELECT count(*) FROM assets WHERE id = ?1", id),
            self.count("SELECT count(*) FROM entries WHERE asset_id = ?1", id),
            self.count("SELECT count(*) FROM asset_state WHERE asset_id = ?1", id),
            self.count("SELECT count(*) FROM requests WHERE asset_id = ?1", id),
            self.count("SELECT count(*) FROM versions WHERE asset_id = ?1", id),
            self.count(
                "SELECT count(*) FROM capture
                 WHERE asset_row = (SELECT row_id FROM assets WHERE id = ?1)",
                id,
            ),
            self.count(
                "SELECT count(*) FROM collection_members
                 WHERE asset_row = (SELECT row_id FROM assets WHERE id = ?1)",
                id,
            ),
            self.count(
                "SELECT count(*) FROM artifact_refs r JOIN entries e ON e.id = r.entry_id
                 WHERE e.asset_id = ?1",
                id,
            ),
        ]
    }

    /// A view of `source` as a client reads it: its summary's counts and groups, and every row
    /// but its preview state, which the preview lane moves on by itself.
    fn view(&self, source: Value) -> Value {
        let summary = self.ok("browse.view", json!({"source": source}));
        let count = summary["count"].as_u64().unwrap();
        let rows: Vec<Value> = if count == 0 {
            Vec::new()
        } else {
            self.ok("browse.rows", json!({"from": 0, "count": count}))["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let mut row = row.clone();
                    row.as_object_mut().unwrap().remove("preview");
                    row
                })
                .collect()
        };
        json!({
            "count": summary["count"],
            "in_catalog": summary["in_catalog"],
            "unavailable": summary["unavailable"],
            "groups": summary["groups"],
            "rows": rows,
        })
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

/// The photographs a view's rows are.
fn photographs_of(view: &Value) -> BTreeSet<String> {
    view["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["asset_id"].as_str().unwrap().to_owned())
        .collect()
}

/// The photographs `assets` names, as [`photographs_of`] answers them.
fn named(assets: &[&Value]) -> BTreeSet<String> {
    assets
        .iter()
        .map(|asset| asset.as_str().unwrap().to_owned())
        .collect()
}

/// `asset.remove` moves photographs to Removed as one library change, keeping each one's history,
/// catalog folder and collections, and changing nothing for one already removed; a retry answers
/// the first change. `library.undo` puts them back and `library.redo` removes them again, with the
/// same removal time; `asset.restore` puts one back as its own change. No original is touched.
#[test]
fn remove_and_put_back_are_library_changes_undone_and_redone() {
    let harness = Harness::new("put-back");
    let paths = harness.photos(3);
    let assets = harness.develop("develop", &paths);
    let originals: Vec<_> = paths.iter().map(|path| untouched(path)).collect();
    let portfolio = harness.collection("Portfolio", &[&assets[0], &assets[1]]);
    harness.edit(&assets[1], "edit-k1");
    let history = harness.ok("history.list", json!({"asset_id": assets[1]}));
    let records: Vec<_> = assets.iter().map(|asset| harness.records(asset)).collect();
    let folders = harness.strings("SELECT catalog_folder_id FROM assets ORDER BY row_id");
    let portfolio_count = || {
        harness.ok("collection.list", json!({}))["collections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|collection| collection["id"] == portfolio)
            .unwrap()["count"]
            .clone()
    };
    assert_eq!(portfolio_count(), 2);

    let sequence = harness.sequence();
    let removed = harness.ok(
        "asset.remove",
        targets(&[&assets[0], &assets[1]], "remove-1"),
    );
    assert_eq!(removed["outcome"], "applied", "{removed}");
    assert_eq!(removed["items"], 2);
    assert_eq!(harness.events_after(sequence), ["asset.remove"]);
    let detail = harness.inspect(&removed["change"]);
    assert_eq!(detail["change"]["label"], "Removed 2 photographs");
    assert_eq!(detail["change"]["method"], "asset.remove");
    let removed_ms = detail["rows"][0]["after"]["removed_ms"].clone();
    assert!(removed_ms.as_i64().unwrap() > 0);
    for (row, asset) in detail["rows"].as_array().unwrap().iter().zip(&assets) {
        assert_eq!(
            row["item"],
            json!({"kind": "asset-removal", "asset_id": asset})
        );
        assert_eq!(row["before"], Value::Null);
    }
    let counts = harness.counts();
    assert_eq!(
        (counts["photographs"].clone(), counts["removed"].clone()),
        (json!(1), json!(2))
    );
    assert_eq!(
        portfolio_count(),
        0,
        "a collection counts no removed member"
    );
    // Nothing else about them changed: history, folder, memberships and every other row.
    assert_eq!(
        harness.ok("history.list", json!({"asset_id": assets[1]})),
        history
    );
    assert_eq!(
        assets
            .iter()
            .map(|asset| harness.records(asset))
            .collect::<Vec<_>>(),
        records
    );
    assert_eq!(
        harness.strings("SELECT catalog_folder_id FROM assets ORDER BY row_id"),
        folders
    );

    // Removing one already removed keeps when it was removed.
    let again = harness.ok("asset.remove", targets(&[&assets[0]], "remove-2"));
    assert_eq!(again["outcome"], "no-op", "{again}");
    let retried = harness.ok(
        "asset.remove",
        targets(&[&assets[0], &assets[1]], "remove-1"),
    );
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["change"], removed["change"]);
    let removal_times = || harness.strings("SELECT quote(removed_ms) FROM assets ORDER BY row_id");
    let times = removal_times();
    assert_eq!(times[0], removed_ms.to_string());

    let undone = harness.ok("library.undo", json!({"mutation": envelope("undo-1")}));
    assert_eq!(undone["items"], 2, "{undone}");
    assert_eq!(
        harness.inspect(&undone["change"])["change"]["label"],
        "Undo Removed 2 photographs"
    );
    assert_eq!(harness.counts()["removed"], 0);
    assert_eq!(portfolio_count(), 2);
    let redone = harness.ok("library.redo", json!({"mutation": envelope("redo-1")}));
    assert_eq!(redone["items"], 2, "{redone}");
    assert_eq!(removal_times(), times, "removed again at the same time");

    let restored = harness.ok("asset.restore", targets(&[&assets[1]], "restore-1"));
    assert_eq!(restored["outcome"], "applied");
    let detail = harness.inspect(&restored["change"]);
    assert_eq!(detail["change"]["label"], "Put back k1.jpg");
    assert_eq!(detail["rows"][0]["before"]["removed_ms"], removed_ms);
    assert_eq!(detail["rows"][0]["after"], Value::Null);
    let again = harness.ok("asset.restore", targets(&[&assets[1]], "restore-2"));
    assert_eq!(again["outcome"], "no-op");
    let counts = harness.counts();
    assert_eq!(
        (counts["photographs"].clone(), counts["removed"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(
        harness.ok("history.list", json!({"asset_id": assets[1]})),
        history
    );

    let refused = harness.refused(
        "asset.remove",
        json!({"targets": {"kind": "assets", "asset_ids": ["asset-00000000000000000000000000000000"]}, "mutation": envelope("remove-unknown")}),
    );
    assert_eq!(refused.code, "validation");
    let after: Vec<_> = paths.iter().map(|path| untouched(path)).collect();
    assert_eq!(after, originals, "no original was touched");
}

/// Removed photographs leave every source but Removed — all photographs, recently developed, a
/// catalog folder with and without its subfolders, a collection, a smart collection, missing
/// originals, and a folder on disk's files, which no longer show them as in the catalog — and
/// every count: `catalog.info`, `folder.list`, `collection.list`, `source.missing` and the facets.
/// Removed shows only them. Put back returns every view and count exactly as it was.
#[test]
fn removed_photographs_leave_every_other_view_and_count_and_put_back_returns_them() {
    let harness = Harness::new("views");
    let paths = harness.photos(4);
    let assets = harness.develop("develop", &paths);
    let folder = harness.ok("folder.list", json!({}))["folders"][0]["id"].clone();
    let best = harness.ok(
        "folder.create",
        json!({"name": "Best", "parent_id": folder, "mutation": envelope("best")}),
    )["folder"]["id"]
        .clone();
    let mut moved = targets(&[&assets[3]], "move-k3");
    moved["folder_id"] = best.clone();
    harness.ok("asset.move", moved);
    let portfolio = harness.collection("Portfolio", &[&assets[0], &assets[2]]);
    let everything = harness.ok(
        "collection.create-smart",
        json!({"name": "Everything", "query": {"source": {"kind": "all-photographs"}}, "mutation": envelope("smart")}),
    )["collection"]["id"]
        .clone();
    // k1 was developed long ago, and k2's original was last seen missing: observations a test
    // arranges, outside the journal.
    harness.arrange(
        "UPDATE assets SET developed_ms = 1 WHERE id = ?1",
        &assets[1],
    );
    harness.arrange(
        "UPDATE assets SET availability = 'missing' WHERE id = ?1",
        &assets[2],
    );
    let trip = harness.dir.join("trip");
    // Every count but the journal's, which grows by the removal and Put back.
    let info = || {
        let mut counts = harness.counts();
        counts.as_object_mut().unwrap().remove("library_changes");
        counts
    };
    let state = || {
        json!({
            "all": harness.view(json!({"kind": "all-photographs"})),
            "recent": harness.view(json!({"kind": "recently-developed"})),
            "folder": harness.view(json!({"kind": "catalog-folder", "folder_id": folder})),
            "folder alone": harness.view(json!({"kind": "catalog-folder", "folder_id": folder, "subfolders": false})),
            "best": harness.view(json!({"kind": "catalog-folder", "folder_id": best})),
            "portfolio": harness.view(json!({"kind": "collection", "collection_id": portfolio})),
            "everything": harness.view(json!({"kind": "collection", "collection_id": everything})),
            "missing": harness.view(json!({"kind": "missing-originals"})),
            "removed": harness.view(json!({"kind": "removed"})),
            "files": harness.view(json!({"kind": "folder", "path": trip})),
            "facets": harness.ok("browse.facets", json!({"source": {"kind": "all-photographs"}, "facets": ["camera", "kind"]})),
            "info": info(),
            "folders": harness.ok("folder.list", json!({})),
            "collections": harness.ok("collection.list", json!({})),
            "source.missing": harness.ok("source.missing", json!({})),
        })
    };
    let before = state();
    let (k0, k1, k2, k3) = (&assets[0], &assets[1], &assets[2], &assets[3]);
    assert_eq!(photographs_of(&before["all"]), named(&[k0, k1, k2, k3]));
    assert_eq!(photographs_of(&before["recent"]), named(&[k0, k2, k3]));
    assert_eq!(photographs_of(&before["missing"]), named(&[k2]));
    assert_eq!(photographs_of(&before["removed"]), named(&[]));
    assert_eq!(before["files"]["in_catalog"], 4);
    assert_eq!(before["source.missing"]["count"], 1);

    let removed = harness.ok("asset.remove", targets(&[k2, k3], "remove-k2-k3"));
    assert_eq!(removed["items"], 2);
    let during = state();
    for (source, shown) in [
        ("all", named(&[k0, k1])),
        ("recent", named(&[k0])),
        ("folder", named(&[k0, k1])),
        ("folder alone", named(&[k0, k1])),
        ("best", named(&[])),
        ("portfolio", named(&[k0])),
        ("everything", named(&[k0, k1])),
        ("missing", named(&[])),
        ("removed", named(&[k2, k3])),
    ] {
        assert_eq!(photographs_of(&during[source]), shown, "{source}");
        assert_eq!(during[source]["count"], shown.len(), "{source}");
    }
    // Their files are no longer a photograph's in the folder on disk.
    assert_eq!(during["files"]["count"], 4);
    assert_eq!(during["files"]["in_catalog"], 2);
    let gone = named(&[k2, k3]);
    for row in during["files"]["rows"].as_array().unwrap() {
        if let Some(developed) = row.get("developed_as").and_then(Value::as_str) {
            assert!(!gone.contains(developed), "{row}");
        }
    }
    let info = &during["info"];
    assert_eq!(
        (
            info["photographs"].clone(),
            info["recently_developed"].clone(),
            info["removed"].clone(),
            info["unavailable"].clone()
        ),
        (json!(2), json!(1), json!(2), json!(0))
    );
    let counts: Vec<(Value, Value)> = during["folders"]["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|folder| (folder["name"].clone(), folder["count"].clone()))
        .collect();
    assert_eq!(counts[1], (json!("Best"), json!(0)), "{counts:?}");
    assert_eq!(counts[0].1, json!(2), "{counts:?}");
    let portfolio_count = during["collections"]["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["id"] == portfolio)
        .unwrap()["count"]
        .clone();
    assert_eq!(portfolio_count, 1);
    assert_eq!(during["source.missing"]["count"], 0);
    assert_ne!(during["facets"], before["facets"]);

    let restored = harness.ok(
        "asset.restore",
        targets(&[&assets[2], &assets[3]], "restore-k2-k3"),
    );
    assert_eq!(restored["items"], 2);
    let after = state();
    for (part, was) in before.as_object().unwrap() {
        assert_eq!(&after[part], was, "{part}: Put back returns it exactly");
    }
}

/// `catalog.empty-removed` is forbidden to an edit client, changing nothing and announcing nothing;
/// the client with permission authority empties Removed, recording one event, and its retry is
/// answered with the first answer and records none. With nothing removed it is a no-op.
#[test]
fn remove_empty_is_forbidden_without_permission_authority() {
    let harness = Harness::new("authority");
    let paths = harness.photos(2);
    let assets = harness.develop("develop", &paths);
    harness.ok("asset.remove", targets(&[&assets[0]], "remove-k0"));
    let sequence = harness.sequence();

    let refused = harness.refused(
        "catalog.empty-removed",
        json!({"mutation": envelope("empty-1")}),
    );
    assert_eq!(refused.code, "forbidden");
    assert!(
        refused.message.contains("permission authority"),
        "{}",
        refused.message
    );
    assert_eq!(harness.counts()["removed"], 1);
    assert_eq!(harness.records(&assets[0])[0], 1);
    assert!(harness.events_after(sequence).is_empty());

    let emptied = harness.empty("empty-1");
    assert_eq!(
        emptied,
        json!({"outcome": "applied", "deleted": 1, "remaining": 0, "deduplicated": false})
    );
    assert_eq!(harness.events_after(sequence), ["catalog.empty-removed"]);
    let after_first = harness.sequence();
    let retried = harness.empty("empty-1");
    assert_eq!(
        retried,
        json!({"outcome": "applied", "deleted": 1, "remaining": 0, "deduplicated": true})
    );
    assert_eq!(harness.sequence(), after_first, "a retry records no event");
    let counts = harness.counts();
    assert_eq!(
        (counts["photographs"].clone(), counts["removed"].clone()),
        (json!(1), json!(0))
    );

    let nothing = harness.empty("empty-2");
    assert_eq!(
        nothing,
        json!({"outcome": "no-op", "deleted": 0, "remaining": 0, "deduplicated": false})
    );
    assert_eq!(harness.sequence(), after_first);
    // An edit client is refused even with nothing to delete.
    assert_eq!(
        harness
            .refused(
                "catalog.empty-removed",
                json!({"mutation": envelope("empty-3")})
            )
            .code,
        "forbidden"
    );
}

/// Emptying Removed deletes photographs outside the journal, so it moves neither the library
/// sequence nor the index revision a view is stamped with; it still leaves every view stale, the
/// caller's and another client's, so neither acts on or shows the deleted photographs until it is
/// evaluated again. A no-op empties nothing and leaves views as they were.
#[test]
fn remove_empty_leaves_every_view_stale() {
    let harness = Harness::new("empty-views");
    let paths = harness.photos(2);
    let assets = harness.develop("develop", &paths);
    harness.ok("asset.remove", targets(&[&assets[0]], "remove-k0"));
    let removed = json!({"kind": "removed"});
    let stale = |client: ClientId| {
        harness
            .send_as(client, "session.state", json!({}))
            .result
            .unwrap()["browse"]["stale"]
            .clone()
    };
    let view = |client: ClientId, source: &Value| {
        let summary = harness
            .send_as(client, "browse.view", json!({"source": source}))
            .result
            .expect("a view");
        summary["count"].clone()
    };
    assert_eq!(view(harness.client, &removed), 1);
    assert_eq!(view(harness.admin, &json!({"kind": "all-photographs"})), 1);
    harness.ok("browse.select", json!({"all": true}));
    assert_eq!(
        (stale(harness.client), stale(harness.admin)),
        (json!(false), json!(false))
    );

    let emptied = harness.empty("empty-1");
    assert_eq!(emptied["deleted"], 1);
    assert_eq!(
        (stale(harness.client), stale(harness.admin)),
        (json!(true), json!(true)),
        "both views are stale"
    );
    // The stale Removed view's selection named the deleted photograph, so a request on the selection
    // is refused rather than acting on what is left of it; evaluated again it is empty.
    assert_eq!(
        harness
            .refused(
                "asset.restore",
                json!({"targets": {"kind": "selection"}, "mutation": envelope("restore")})
            )
            .code,
        "conflict"
    );
    assert_eq!(view(harness.client, &removed), 0);
    assert_eq!(stale(harness.client), json!(false));

    // Emptying nothing leaves a fresh view fresh.
    assert_eq!(harness.empty("empty-2")["outcome"], "no-op");
    assert_eq!(stale(harness.client), json!(false));
}

/// Emptying deletes exactly the removed photographs' records — an edited one with a named version,
/// a collection, brush strokes and tints, and an unedited one — with the stroke and the artifact only
/// they named, and keeps the stroke and the artifact a remaining photograph shares, which still
/// renders; the artifact's file is collected. The journal is kept, so undoing the removal is refused
/// by its value check, and the catalog's triggers are exactly as they were. No original is touched.
#[test]
fn remove_empty_deletes_exactly_the_removed_photographs_with_their_strokes_and_artifacts() {
    let harness = Harness::new("empty");
    let paths = harness.photos(3);
    let assets = harness.develop("develop", &paths);
    let originals: Vec<_> = paths.iter().map(|path| untouched(path)).collect();
    let (k0, k1, k2) = (&assets[0], &assets[1], &assets[2]);
    harness.edit(k0, "edit-k0");
    harness.paint(k0, json!([[0.2, 0.2], [0.4, 0.3]]), "paint-own");
    let own = harness.strings("SELECT id FROM strokes");
    assert_eq!(own.len(), 1);
    let shared_path = json!([[0.6, 0.6], [0.7, 0.5]]);
    harness.paint(k0, shared_path.clone(), "paint-shared-k0");
    harness.paint(k2, shared_path, "paint-shared-k2");
    let strokes: BTreeSet<String> = harness
        .strings("SELECT id FROM strokes")
        .into_iter()
        .collect();
    assert_eq!(strokes.len(), 2, "the shared stroke is stored once");
    let shared: Vec<String> = strokes
        .iter()
        .filter(|id| !own.contains(id))
        .cloned()
        .collect();
    let only_k0 = harness.publish([0.5, 1.0, 1.0]);
    let common = harness.publish([1.0, 0.5, 1.0]);
    harness.tint(k0, &only_k0, "tint-k0-own");
    harness.tint(k0, &common, "tint-k0-common");
    harness.tint(k2, &common, "tint-k2-common");
    harness.ok(
        "version.create",
        json!({"asset_id": k0, "name": "Best", "mutation": envelope("version-k0")}),
    );
    harness.collection("Portfolio", &[k0, k2]);
    // Restarted, nothing published before is live, so only references keep an artifact.
    let harness = harness.restart();
    let k2_records = harness.records(k2);
    let k2_entries = harness.entries(k2);
    assert!(
        harness.records(k0).iter().all(|&rows| rows > 0),
        "{:?}",
        harness.records(k0)
    );
    let removed = harness.ok("asset.remove", targets(&[k0, k1], "remove-k0-k1"));
    assert_eq!(removed["items"], 2);
    let journal = harness.ok("library.journal", json!({}));
    let triggers = || {
        harness.strings(
            "SELECT name || ':' || sql FROM sqlite_schema WHERE type = 'trigger' ORDER BY name",
        )
    };
    let triggers_before = triggers();
    let sequence = harness.sequence();
    // A rendered preview the preview cache holds of a removed photograph, as lane B writes one.
    let index = rusqlite::Connection::open(
        crate::index::index_dir(&harness.catalog).join(crate::INDEX_FILE),
    )
    .unwrap();
    index
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    index
        .execute(
            "INSERT INTO photo_previews (asset_id, entry_id, tier, renderer, path, width, height,
                 bytes, origin, last_used_ms, approximate)
             VALUES (?1, 'entry-0', 'grid', 1, ?2, 512, 341, 4, 'rendered', 0, 0)",
            [
                k0.as_str().unwrap().to_owned(),
                harness
                    .dir
                    .join("k0-grid.jpg")
                    .to_string_lossy()
                    .into_owned(),
            ],
        )
        .unwrap();

    let emptied = harness.empty("empty-1");
    let cached: i64 = index
        .query_row(
            "SELECT count(*) FROM photo_previews WHERE asset_id = ?1",
            [k0.as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cached, 0, "the preview cache forgets an emptied photograph");
    assert_eq!(
        emptied,
        json!({"outcome": "applied", "deleted": 2, "remaining": 0, "deduplicated": false})
    );
    assert_eq!(harness.events_after(sequence), ["catalog.empty-removed"]);
    for gone in [k0, k1] {
        assert_eq!(harness.records(gone), [0; 8], "{gone}");
        assert_eq!(
            harness
                .refused("asset.state", json!({"asset_id": gone}))
                .code,
            "validation"
        );
    }
    assert_eq!(
        harness.records(k2),
        k2_records,
        "the other photograph is whole"
    );
    assert_eq!(harness.entries(k2), k2_entries);
    assert_eq!(
        harness.strings("SELECT id FROM strokes"),
        shared,
        "only the shared stroke is left"
    );
    assert_eq!(
        harness.strings("SELECT id FROM artifacts"),
        [common.as_str().to_owned()],
        "only the shared artifact is left"
    );
    assert_eq!(
        triggers(),
        triggers_before,
        "the lifted trigger is back as it was"
    );
    assert_eq!(
        harness.ok("library.journal", json!({})),
        journal,
        "the journal is as it was"
    );
    let counts = harness.counts();
    assert_eq!(
        (counts["photographs"].clone(), counts["removed"].clone()),
        (json!(1), json!(0))
    );
    // The deleted artifact's file is collected; the shared one's stays.
    let root = harness.dir.join("catalog.artifacts");
    luxforge_testbase::wait_for("the deleted artifact's file to be collected", || {
        (!object_path(&root, &only_k0).exists()).then_some(())
    });
    assert!(object_path(&root, &common).exists());
    // The remaining photograph still renders with the stroke and artifact it shares.
    harness.prepared("render.sample", json!({"asset_id": k2, "x": 1, "y": 1}));
    // Undoing the removal names photographs that are gone: the journal's value check refuses it.
    let undo = harness.refused(
        "library.undo",
        json!({"mutation": envelope("undo-removal")}),
    );
    assert_eq!(undo.code, "conflict", "{}", undo.message);
    assert_eq!(
        undo.data.unwrap()["items"],
        json!([
            {"kind": "asset-removal", "asset_id": k0},
            {"kind": "asset-removal", "asset_id": k1},
        ])
    );
    let after: Vec<_> = paths.iter().map(|path| untouched(path)).collect();
    assert_eq!(after, originals, "no original was touched");
}

/// No path but emptying Removed deletes an edited photograph's entries: sending it back and
/// undoing its Develop are refused, and picking its file, moving it, a collection's add, remove and
/// delete, removal with its undo and redo, Put back and a source check leave its entries as they
/// were. Sending back an unedited photograph deletes its record, which holds only its Original
/// entry: the one other deletion, P9's. Emptying Removed then deletes the edited one's entries.
#[test]
fn remove_no_other_path_deletes_an_entry() {
    let harness = Harness::new("no-other-path");
    let paths = harness.photos(2);
    let assets = harness.develop("develop", &paths);
    // The Develop committed k0 in its first batch and k1 in its second, its latest change.
    let (unedited, edited) = (&assets[0], &assets[1]);
    harness.edit(edited, "edit");
    let entries = harness.entries(edited);
    assert_eq!(entries.len(), 2);
    let unchanged = |step: &str| assert_eq!(harness.entries(edited), entries, "after {step}");

    let edited_since = |refused: ApiFailure| {
        assert_eq!(refused.code, "conflict", "{}", refused.message);
        assert!(
            refused
                .message
                .contains("k1.jpg has been edited since it was developed"),
            "{}",
            refused.message
        );
    };
    edited_since(harness.refused("asset.send-back", targets(&[edited], "send-back")));
    unchanged("asset.send-back");
    edited_since(harness.refused(
        "library.undo",
        json!({"mutation": envelope("undo-develop")}),
    ));
    unchanged("library.undo of its Develop");
    harness.ok(
        "pick.set",
        json!({"targets": {"kind": "paths", "paths": [paths[1]]}, "picked": true, "mutation": envelope("pick")}),
    );
    unchanged("pick.set");
    let elsewhere = harness.ok(
        "folder.create",
        json!({"name": "Elsewhere", "mutation": envelope("folder")}),
    )["folder"]["id"]
        .clone();
    let mut moved = targets(&[edited], "move");
    moved["folder_id"] = elsewhere;
    harness.ok("asset.move", moved);
    unchanged("asset.move");
    let collection = harness.collection("Portfolio", &[edited]);
    let mut remove = targets(&[edited], "collection-remove");
    remove["collection_id"] = collection.clone();
    harness.ok("collection.remove", remove);
    harness.ok(
        "collection.delete",
        json!({"collection_id": collection, "mutation": envelope("collection-delete")}),
    );
    unchanged("collection.add, collection.remove and collection.delete");
    harness.ok("asset.remove", targets(&[edited], "remove-1"));
    harness.ok("library.undo", json!({"mutation": envelope("undo-remove")}));
    harness.ok("library.redo", json!({"mutation": envelope("redo-remove")}));
    harness.ok("asset.restore", targets(&[edited], "restore"));
    unchanged("asset.remove, library.undo, library.redo and asset.restore");
    let check = harness.ok(
        "source.check",
        json!({"targets": {"kind": "assets", "asset_ids": [edited]}}),
    );
    assert_eq!(harness.settle(&check["job_id"])["status"], "ready");
    unchanged("source.check");

    // P9: an unedited photograph sent back leaves with its Original-only record.
    assert_eq!(harness.entries(unedited).len(), 1);
    harness.ok(
        "asset.send-back",
        targets(&[unedited], "send-back-unedited"),
    );
    assert!(harness.entries(unedited).is_empty());

    harness.ok("asset.remove", targets(&[edited], "remove-2"));
    unchanged("asset.remove");
    harness.empty("empty");
    assert!(
        harness.entries(edited).is_empty(),
        "emptying Removed deletes them"
    );
}
