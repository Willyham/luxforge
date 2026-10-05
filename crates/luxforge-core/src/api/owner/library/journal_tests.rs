//! Picks and the journal through the catalog owner, as clients call them: picks by path and by
//! index row that survive a restart, one event per change however large, two clients' undo, the
//! batch limit, retries and the journal's pages.
use crate::{
    ModuleRegistry,
    api::{ApiRequest, ApiResponse, ClientId, OwnerHandle},
    catalog_types::{FileRecord, FileSignature, HeaderState, MAX_LIBRARY_BATCH, VolumeId},
    seed::IndexSeeder,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
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

fn envelope(actor: &str, request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": actor})
}

/// The library events after `after`: (method, library sequence).
fn library_events(owner: &OwnerHandle, client: ClientId, after: u64) -> (Vec<(String, u64)>, u64) {
    let read = ok(owner, client, "events.since", json!({"after": after}));
    let events = read["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            (
                event["method"].as_str().unwrap().to_owned(),
                event["library_sequence"]
                    .as_u64()
                    .expect("a library sequence"),
            )
        })
        .collect();
    (events, read["current_sequence"].as_u64().unwrap())
}

/// A scratch folder with `files` small files, a catalog beside it and an index listing the first
/// `listed` of them, as the index lane would have.
struct Fixture {
    dir: PathBuf,
    catalog: PathBuf,
    files: Vec<PathBuf>,
    volume: VolumeId,
}

impl Fixture {
    fn new(name: &str, files: usize, listed: usize, create: bool) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("library-{name}"))
            .canonicalize()
            .unwrap();
        let photos = dir.join("DCIM");
        std::fs::create_dir_all(&photos).unwrap();
        let catalog = dir.join("catalog.sqlite");
        let catalog_id = crate::EditorService::open(&catalog)
            .unwrap()
            .catalog_id()
            .to_owned();
        let volume = crate::index::volume_of(&photos, 1).unwrap().id;
        let files: Vec<PathBuf> = (0..files)
            .map(|index| photos.join(format!("DSC_{index:05}.NEF")))
            .collect();
        if create {
            for (index, file) in files.iter().enumerate() {
                std::fs::write(file, vec![0_u8; 16 + index % 7]).unwrap();
            }
        }
        let mut seeder = IndexSeeder::create(&catalog, &catalog_id).unwrap();
        let records: Vec<FileRecord> = files[..listed]
            .iter()
            .enumerate()
            .map(|(index, path)| FileRecord {
                path: path.clone(),
                folder: photos.clone(),
                name: path.file_name().unwrap().to_string_lossy().into(),
                volume_id: volume.clone(),
                signature: FileSignature {
                    len: 1000 + index as u64,
                    modified_ns: 5,
                    identity: None,
                },
                kind: crate::SourceTag::Raw,
                header: HeaderState::Pending,
                last_seen_ms: 1,
            })
            .collect();
        seeder.files(&records).unwrap();
        seeder.finish().unwrap();
        Self {
            dir,
            catalog,
            files,
            volume,
        }
    }

    fn start(&self) -> (OwnerHandle, std::thread::JoinHandle<()>) {
        OwnerHandle::start_with(&self.catalog, Arc::new(ModuleRegistry::builtin())).unwrap()
    }

    fn path(&self, index: usize) -> &Path {
        &self.files[index]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn picked_paths(owner: &OwnerHandle, client: ClientId) -> Vec<String> {
    ok(owner, client, "pick.list", json!({}))["picks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pick| pick["path"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn pick_set_over_paths_and_index_rows_is_journaled_and_survives_a_restart() {
    let fixture = Fixture::new("paths", 3, 2, true);
    let (owner, join) = fixture.start();
    let client = owner.register();
    let (_, start) = library_events(&owner, client, 0);

    // The first file by its index row; the second by path, which the index lists; the third by a
    // path the index does not list, read from the file itself.
    let by_row = ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [1]},
            "picked": true,
            "mutation": envelope("desktop", "p1"),
        }),
    );
    assert_eq!(
        (
            &by_row["outcome"],
            &by_row["items"],
            &by_row["deduplicated"]
        ),
        (&json!("applied"), &json!(1), &json!(false))
    );
    let unlisted = fixture.dir.join("DCIM/../DCIM").join("DSC_00002.NEF");
    let by_path = ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [fixture.path(1), unlisted, fixture.path(0)]},
            "picked": true,
            "mutation": envelope("agent", "p2"),
        }),
    );
    assert_eq!(by_path["items"], 2, "the first file was already picked");
    let (events, _) = library_events(&owner, client, start);
    assert_eq!(
        events,
        [
            ("pick.set".into(), by_row["change"].as_u64().unwrap()),
            ("pick.set".into(), by_path["change"].as_u64().unwrap())
        ]
    );

    let list = ok(&owner, client, "pick.list", json!({}));
    let picks = list["picks"].as_array().unwrap();
    assert_eq!(picks.len(), 3);
    assert_eq!(picks[0]["path"], json!(fixture.path(0)));
    assert_eq!(
        (
            &picks[0]["actor"],
            &picks[0]["file_id"],
            &picks[0]["signature"]["len"]
        ),
        (&json!("desktop"), &json!(1), &json!(1000)),
        "an index row's signature is the index's"
    );
    assert_eq!(picks[1]["file_id"], 2);
    assert_eq!(
        picks[2]["path"],
        json!(fixture.path(2)),
        "the canonical path"
    );
    assert_eq!(picks[2].get("file_id"), None);
    assert_eq!(
        picks[2]["signature"]["len"], 18,
        "a file the index lacks is read"
    );
    assert_eq!(picks[2]["volume_id"], json!(fixture.volume));
    // A folder source lists that folder's picks; a card source its volume's; a page continues.
    let folder = ok(
        &owner,
        client,
        "pick.list",
        json!({
            "source": {"kind": "folder", "path": fixture.dir.join("DCIM")},
            "limit": 2,
        }),
    );
    assert_eq!(folder["picks"].as_array().unwrap().len(), 2);
    assert_eq!(folder["next_after"], json!(fixture.path(1)));
    let rest = ok(
        &owner,
        client,
        "pick.list",
        json!({
            "source": {"kind": "folder", "path": fixture.dir.join("DCIM")},
            "after": fixture.path(1),
        }),
    );
    assert_eq!(rest["picks"].as_array().unwrap().len(), 1);
    assert_eq!(rest.get("next_after"), None);
    let shallow = ok(
        &owner,
        client,
        "pick.list",
        json!({
            "source": {"kind": "folder", "path": fixture.dir, "subfolders": false},
        }),
    );
    assert!(shallow["picks"].as_array().unwrap().is_empty());
    let card = ok(
        &owner,
        client,
        "pick.list",
        json!({
            "source": {"kind": "card", "volume_id": fixture.volume},
        }),
    );
    assert_eq!(card["picks"].as_array().unwrap().len(), 3);
    let photos = call(
        &owner,
        client,
        "pick.list",
        json!({"source": {"kind": "removed"}}),
    );
    assert_eq!(photos.error.unwrap().code, "validation");
    owner.stop();
    join.join().unwrap();

    // After a restart the picks are all there, and the actor undoes its own change.
    let (owner, join) = fixture.start();
    let client = owner.register();
    assert_eq!(ok(&owner, client, "pick.list", json!({})), list);
    // A retry sent after the restart is answered by the journal, as a retry.
    let retried = ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [1]},
            "picked": true,
            "mutation": envelope("desktop", "p1"),
        }),
    );
    assert_eq!(
        (&retried["change"], &retried["deduplicated"]),
        (&by_row["change"], &json!(true))
    );
    let undone = ok(
        &owner,
        client,
        "library.undo",
        json!({"mutation": envelope("agent", "u1")}),
    );
    assert_eq!(undone["items"], 2);
    assert_eq!(
        picked_paths(&owner, client),
        [fixture.path(0).to_string_lossy()]
    );
    let journal = ok(&owner, client, "library.journal", json!({}));
    let labels: Vec<_> = journal["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["label"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        labels,
        [
            "Picked DSC_00000.NEF",
            "Picked 2 files",
            "Undo Picked 2 files"
        ]
    );
    assert_eq!(journal["changes"][1]["undone_by"], undone["change"]);
    owner.stop();
    join.join().unwrap();
}

#[test]
fn a_journal_change_of_5000_picks_records_one_event_and_undoes_as_one() {
    let fixture = Fixture::new("five-thousand", 5000, 5000, false);
    let (owner, join) = fixture.start();
    let client = owner.register();
    let (_, start) = library_events(&owner, client, 0);
    let ids: Vec<u64> = (1..=5000).collect();
    let picked = ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": ids},
            "picked": true,
            "mutation": envelope("agent", "big"),
        }),
    );
    assert_eq!(picked["items"], 5000);
    let (events, after_pick) = library_events(&owner, client, start);
    assert_eq!(
        events,
        [("pick.set".into(), picked["change"].as_u64().unwrap())]
    );
    let detail = ok(
        &owner,
        client,
        "library.inspect",
        json!({"sequence": picked["change"]}),
    );
    assert_eq!(detail["rows"].as_array().unwrap().len(), 5000);
    assert_eq!(detail["change"]["label"], "Picked 5000 files");
    assert_eq!(
        ok(&owner, client, "pick.list", json!({"limit": 5000}))["picks"]
            .as_array()
            .unwrap()
            .len(),
        5000
    );

    let undone = ok(
        &owner,
        client,
        "library.undo",
        json!({"mutation": envelope("agent", "undo")}),
    );
    assert_eq!(undone["items"], 5000);
    let (events, _) = library_events(&owner, client, after_pick);
    assert_eq!(
        events,
        [("library.undo".into(), undone["change"].as_u64().unwrap())]
    );
    assert!(picked_paths(&owner, client).is_empty());
    owner.stop();
    join.join().unwrap();
}

#[test]
fn two_clients_journal_undo_their_own_changes_and_are_refused_naming_the_other_s_items() {
    let fixture = Fixture::new("two-clients", 3, 3, false);
    let (owner, join) = fixture.start();
    let desktop = owner.register();
    let agent = owner.register();
    ok(
        &owner,
        desktop,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [1, 2]},
            "picked": true,
            "mutation": envelope("desktop", "d1"),
        }),
    );
    ok(
        &owner,
        agent,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [fixture.path(1)]},
            "picked": false,
            "mutation": envelope("agent", "a1"),
        }),
    );
    let refused = call(
        &owner,
        desktop,
        "library.undo",
        json!({"mutation": envelope("desktop", "d2")}),
    )
    .error
    .expect("the agent cleared one of the desktop's picks since");
    assert_eq!(refused.code, "conflict");
    assert_eq!(
        refused.data.unwrap()["items"],
        json!([{"kind": "pick", "path": fixture.path(1)}])
    );
    assert_eq!(
        picked_paths(&owner, desktop),
        [fixture.path(0).to_string_lossy()]
    );
    // The agent undoes its own clear; then the desktop's undo is allowed, and its redo too.
    ok(
        &owner,
        agent,
        "library.undo",
        json!({"mutation": envelope("agent", "a2")}),
    );
    ok(
        &owner,
        desktop,
        "library.undo",
        json!({"mutation": envelope("desktop", "d3")}),
    );
    assert!(picked_paths(&owner, agent).is_empty());
    let redone = ok(
        &owner,
        desktop,
        "library.redo",
        json!({"mutation": envelope("desktop", "d4")}),
    );
    assert_eq!(redone["items"], 2);
    assert_eq!(picked_paths(&owner, agent).len(), 2);
    // A retry is answered with its first answer and records no event.
    let (_, before) = library_events(&owner, desktop, 0);
    let retry = ok(
        &owner,
        desktop,
        "library.redo",
        json!({"mutation": envelope("desktop", "d4")}),
    );
    assert_eq!(retry["change"], redone["change"]);
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(library_events(&owner, desktop, 0).1, before);
    // With nothing left to redo, a redo changes nothing and announces nothing.
    let nothing = ok(
        &owner,
        desktop,
        "library.redo",
        json!({"mutation": envelope("desktop", "d5")}),
    );
    assert_eq!(
        (&nothing["outcome"], nothing.get("change")),
        (&json!("no-op"), None)
    );
    assert_eq!(library_events(&owner, desktop, 0).1, before);
    owner.stop();
    join.join().unwrap();
}

#[test]
fn a_journal_request_over_the_batch_limit_or_naming_nothing_is_refused() {
    let fixture = Fixture::new("limit", 1, 1, false);
    let (owner, join) = fixture.start();
    let client = owner.register();
    let (_, start) = library_events(&owner, client, 0);
    let too_many: Vec<u64> = (1..=MAX_LIBRARY_BATCH as u64 + 1).collect();
    let refusals = [
        (
            json!({"kind": "files", "file_ids": too_many}),
            "resource-limit",
        ),
        (json!({"kind": "files", "file_ids": []}), "validation"),
        (json!({"kind": "files", "file_ids": [2]}), "validation"),
        (
            json!({"kind": "paths", "paths": ["relative.NEF"]}),
            "validation",
        ),
        (
            json!({"kind": "assets", "asset_ids": [crate::AssetId::new()]}),
            "validation",
        ),
        (json!({"kind": "selection"}), "validation"),
    ];
    for (index, (targets, code)) in refusals.into_iter().enumerate() {
        let error = call(
            &owner,
            client,
            "pick.set",
            json!({
                "targets": targets,
                "picked": true,
                "mutation": envelope("agent", &format!("r{index}")),
            }),
        )
        .error
        .unwrap_or_else(|| panic!("{targets} was accepted"));
        assert_eq!(error.code, code, "{targets}: {}", error.message);
    }
    // A file that cannot be read and is not listed cannot be picked, but its pick can be cleared.
    let missing = fixture.dir.join("DCIM/gone.NEF");
    let error = call(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [missing]},
            "picked": true,
            "mutation": envelope("agent", "gone"),
        }),
    )
    .error
    .unwrap();
    assert_eq!(error.code, "source-unavailable");
    let cleared = ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "paths", "paths": [missing]},
            "picked": false,
            "mutation": envelope("agent", "gone-clear"),
        }),
    );
    assert_eq!(cleared["outcome"], "no-op");
    assert_eq!(library_events(&owner, client, start).0, []);
    assert!(
        ok(&owner, client, "library.journal", json!({}))["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    owner.stop();
    join.join().unwrap();
}

#[test]
fn catalog_info_reports_the_counts_behind_the_catalog_sources_and_the_journal() {
    let fixture = Fixture::new("info", 2, 2, false);
    let (owner, join) = fixture.start();
    let client = owner.register();
    ok(
        &owner,
        client,
        "pick.set",
        json!({
            "targets": {"kind": "files", "file_ids": [1, 2]},
            "picked": true,
            "mutation": envelope("agent", "p1"),
        }),
    );
    let info = ok(&owner, client, "catalog.info", json!({}));
    assert_eq!(info["path"], json!(fixture.catalog));
    assert_eq!(
        (&info["format"], &info["index_format"]),
        (&json!(13), &json!(crate::INDEX_FORMAT))
    );
    assert_eq!(
        info["counts"],
        json!({
            "photographs": 0, "recently_developed": 0, "removed": 0, "unavailable": 0,
            "folders": 0, "collections": 0, "picks": 2, "indexed_folders": 0,
            "library_changes": 1,
        })
    );
    assert_eq!(info["index"]["files"], 2);
    assert!(info["index"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        (&info["previews"]["bytes"], &info["previews"]["files"]),
        (&json!(0), &json!(0))
    );
    owner.stop();
    join.join().unwrap();
}
