//! Catalog folders and collections through the catalog owner, as clients call them: the folder
//! tree made, renamed, nested, moved, merged and deleted, photographs moved, collections, groups and
//! smart collections with their members, each one journaled library change with one event, undone
//! and redone exactly, refused where the design refuses, retried once, and never touching a file.
use crate::{
    AssetId, ModuleRegistry,
    api::{ApiRequest, ApiResponse, ClientId, OwnerHandle},
    catalog_types::{
        CaptureTime, CatalogFolder, CatalogFolderId, Collection, CollectionId, CollectionKind,
        EventSpan, FileAvailability, HeaderMetadata, Volume, VolumeId,
    },
    seed::{CatalogSeeder, SeedAsset, SeedKind},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::SystemTime};

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

/// The refusal of a call, as `(code, message)`.
fn refused(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> (String, String) {
    let error = call(owner, client, method, params)
        .error
        .unwrap_or_else(|| panic!("{method} was accepted"));
    (error.code, error.message)
}

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "agent"})
}

/// The library events after `after` as (method, library sequence), and the log's sequence.
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

fn labels(owner: &OwnerHandle, client: ClientId) -> Vec<String> {
    ok(owner, client, "library.journal", json!({}))["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["label"].as_str().unwrap().to_owned())
        .collect()
}

fn undo(owner: &OwnerHandle, client: ClientId, request_id: &str) -> Value {
    ok(
        owner,
        client,
        "library.undo",
        json!({"mutation": envelope(request_id)}),
    )
}

fn redo(owner: &OwnerHandle, client: ClientId, request_id: &str) -> Value {
    ok(
        owner,
        client,
        "library.redo",
        json!({"mutation": envelope(request_id)}),
    )
}

/// The folder tree as (name, parent's name, count, year), parents first.
fn tree(owner: &OwnerHandle, client: ClientId) -> Vec<(String, Option<String>, u64, Option<i64>)> {
    let list = ok(owner, client, "folder.list", json!({}));
    let folders = list["folders"].as_array().unwrap();
    let name_of = |id: &Value| {
        folders
            .iter()
            .find(|folder| &folder["id"] == id)
            .map(|folder| folder["name"].as_str().unwrap().to_owned())
    };
    folders
        .iter()
        .map(|folder| {
            (
                folder["name"].as_str().unwrap().to_owned(),
                folder.get("parent_id").and_then(name_of),
                folder["count"].as_u64().unwrap(),
                folder.get("year").and_then(Value::as_i64),
            )
        })
        .collect()
}

fn named(
    name: &str,
    parent: Option<&str>,
    count: u64,
    year: Option<i64>,
) -> (String, Option<String>, u64, Option<i64>) {
    (name.into(), parent.map(str::to_owned), count, year)
}

/// The collections as (name, parent's name, kind, count), parents first.
fn shelves(
    owner: &OwnerHandle,
    client: ClientId,
) -> Vec<(String, Option<String>, String, Option<u64>)> {
    let list = ok(owner, client, "collection.list", json!({}));
    let collections = list["collections"].as_array().unwrap();
    let name_of = |id: &Value| {
        collections
            .iter()
            .find(|collection| &collection["id"] == id)
            .map(|collection| collection["name"].as_str().unwrap().to_owned())
    };
    collections
        .iter()
        .map(|collection| {
            (
                collection["name"].as_str().unwrap().to_owned(),
                collection.get("parent_id").and_then(name_of),
                collection["kind"].as_str().unwrap().to_owned(),
                collection.get("count").and_then(Value::as_u64),
            )
        })
        .collect()
}

fn folder_id(owner: &OwnerHandle, client: ClientId, name: &str) -> Value {
    ok(owner, client, "folder.list", json!({}))["folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|folder| folder["name"] == name)
        .unwrap_or_else(|| panic!("no folder {name}"))["id"]
        .clone()
}

/// A seeded catalog of four photographs whose originals are real files in a scratch folder, all in
/// the catalog folder "Konstanz · Sep 2026" (made from an event), beside an empty "Travel", and a
/// collection "Portfolio" holding the first two since known times. The last photograph is removed.
struct Fixture {
    dir: PathBuf,
    catalog: PathBuf,
    photos: Vec<PathBuf>,
    assets: Vec<AssetId>,
    konstanz: CatalogFolderId,
    travel: CatalogFolderId,
    portfolio: CollectionId,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("catalog-folder-{name}"))
            .canonicalize()
            .unwrap();
        let photos_dir = dir.join("Photos");
        std::fs::create_dir_all(&photos_dir).unwrap();
        let catalog = dir.join("catalog.sqlite");
        let volume = Volume {
            id: VolumeId::parse("volume-catalog-folder-tests").unwrap(),
            mount_point: "/".into(),
            label: "Test disk".into(),
            removable: false,
            platform_id: None,
            last_seen_ms: 1,
        };
        let konstanz = CatalogFolder {
            id: CatalogFolderId::new(),
            name: "Konstanz · Sep 2026".into(),
            parent_id: None,
            created_ms: 10,
            event: Some(EventSpan {
                start_ms: 100,
                end_ms: 200,
                event_id: None,
            }),
            count: 0,
            year: None,
        };
        let travel = CatalogFolder {
            id: CatalogFolderId::new(),
            name: "Travel".into(),
            parent_id: None,
            created_ms: 11,
            event: None,
            count: 0,
            year: None,
        };
        // The earliest instant is the second photograph's, on 1 January 2026 at +14:00 (still 31
        // December in UTC); the third, captured later, is on a camera-local 31 December 2025. The
        // fourth is dated earliest of all but removed.
        let captures = [
            CaptureTime::from_exif("2026:09:12 10:15:00", None, Some("+02:00")),
            CaptureTime::from_exif("2026:01:01 00:30:00", None, Some("+14:00")),
            CaptureTime::from_exif("2025:12:31 20:00:00", None, Some("-10:00")),
            CaptureTime::from_exif("2024:05:01 12:00:00", None, Some("+00:00")),
        ];
        let mut photos = Vec::new();
        let mut seeded = Vec::new();
        for (index, capture) in captures.into_iter().enumerate() {
            let photo = photos_dir.join(format!("DSC_{index:04}.JPG"));
            std::fs::write(&photo, vec![index as u8; 64 + index]).unwrap();
            let asset = AssetId::parse(format!("asset-{:032x}", index + 1)).unwrap();
            seeded.push(SeedAsset {
                id: asset,
                kind: SeedKind::Jpeg,
                locator: photo.clone(),
                fingerprint: format!("{:064x}", index + 1),
                file_identity: format!("unix:7:{}", index + 1),
                byte_len: 64 + index as u64,
                width: 60,
                height: 40,
                catalog_folder_id: konstanz.id.clone(),
                volume_id: volume.id.clone(),
                developed_ms: 1_000 + index as i64,
                removed_ms: (index == 3).then_some(5_000),
                availability: FileAvailability::Available,
                checked_ms: 5,
                develop_moment: None,
                header: HeaderMetadata {
                    capture,
                    ..HeaderMetadata::default()
                },
                place: None,
            });
            photos.push(photo);
        }
        let portfolio = Collection {
            id: CollectionId::new(),
            name: "Portfolio".into(),
            parent_id: None,
            kind: CollectionKind::Collection,
            query: None,
            created_ms: 12,
            count: None,
        };
        let mut seeder = CatalogSeeder::create(&catalog, "catalog-folder-tests").unwrap();
        seeder.volumes(&[volume]).unwrap();
        seeder.folders(&[konstanz.clone(), travel.clone()]).unwrap();
        let rows = seeder.assets(&seeded).unwrap();
        seeder
            .collections(std::slice::from_ref(&portfolio))
            .unwrap();
        seeder
            .members(&[
                (portfolio.id.clone(), rows[0], 111),
                (portfolio.id.clone(), rows[1], 222),
            ])
            .unwrap();
        seeder.finish().unwrap();
        Self {
            dir,
            catalog,
            photos,
            assets: seeded.into_iter().map(|asset| asset.id).collect(),
            konstanz: konstanz.id,
            travel: travel.id,
            portfolio: portfolio.id,
        }
    }

    fn start(&self) -> (OwnerHandle, std::thread::JoinHandle<()>) {
        OwnerHandle::start_with(&self.catalog, Arc::new(ModuleRegistry::builtin())).unwrap()
    }

    /// Each original's bytes and modification time.
    fn originals(&self) -> Vec<(Vec<u8>, SystemTime)> {
        self.photos
            .iter()
            .map(|photo| {
                (
                    std::fs::read(photo).unwrap(),
                    std::fs::metadata(photo).unwrap().modified().unwrap(),
                )
            })
            .collect()
    }

    /// Every membership as (collection, photograph, added_ms), read with the owner stopped.
    fn memberships(&self) -> Vec<(String, String, i64)> {
        let connection = rusqlite::Connection::open(&self.catalog).unwrap();
        connection
            .prepare(
                "SELECT m.collection_id, a.id, m.added_ms FROM collection_members m
                 JOIN assets a ON a.row_id = m.asset_row ORDER BY m.collection_id, a.id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stop(owner: OwnerHandle, join: std::thread::JoinHandle<()>) {
    owner.stop();
    join.join().unwrap();
}

/// The folder tree is made, renamed, nested, moved, merged and deleted, each one journaled change
/// with one event; undoing every change restores the exact tree and redoing them makes it again.
#[test]
fn catalog_folders_are_created_renamed_nested_merged_and_deleted_with_undo_and_redo() {
    let fixture = Fixture::new("tree");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let initial = ok(&owner, client, "folder.list", json!({}));
    assert_eq!(
        tree(&owner, client),
        [
            named("Konstanz · Sep 2026", None, 3, Some(2026)),
            named("Travel", None, 0, None),
        ],
        "three photographs are not removed, the earliest instant is on 1 January 2026 local time"
    );
    assert_eq!(
        initial["folders"][0]["event"],
        json!({"start_ms": 100, "end_ms": 200})
    );
    let (_, start) = library_events(&owner, client, 0);

    let created = ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "  Lakes ", "parent_id": fixture.travel, "mutation": envelope("f1")}),
    );
    assert_eq!(created["folder"]["name"], "Lakes", "the name is trimmed");
    assert_eq!(created["folder"]["parent_id"], json!(fixture.travel));
    assert_eq!(
        (&created["folder"]["count"], created["folder"].get("year")),
        (&json!(0), None)
    );
    assert_eq!(
        (
            &created["change"]["outcome"],
            &created["change"]["items"],
            &created["deduplicated"]
        ),
        (&json!("applied"), &json!(1), &json!(false))
    );
    let lakes = created["folder"]["id"].clone();
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Alps", "parent_id": lakes, "mutation": envelope("f2")}),
    );
    let renamed = ok(
        &owner,
        client,
        "folder.rename",
        json!({"folder_id": lakes, "name": "Lakes and mountains", "mutation": envelope("f3")}),
    );
    assert_eq!(renamed["items"], 1);
    // The same name, and a move to where a folder already is, change nothing.
    let same = ok(
        &owner,
        client,
        "folder.rename",
        json!({"folder_id": lakes, "name": "Lakes and mountains ", "mutation": envelope("f3b")}),
    );
    assert_eq!(
        (&same["outcome"], same.get("change")),
        (&json!("no-op"), None)
    );
    ok(
        &owner,
        client,
        "folder.move",
        json!({"folder_id": fixture.konstanz, "parent_id": lakes, "mutation": envelope("f4")}),
    );
    assert_eq!(
        tree(&owner, client),
        [
            named("Travel", None, 0, None),
            named("Lakes and mountains", Some("Travel"), 0, None),
            named("Alps", Some("Lakes and mountains"), 0, None),
            named(
                "Konstanz · Sep 2026",
                Some("Lakes and mountains"),
                3,
                Some(2026)
            ),
        ]
    );
    // A move into itself or a folder inside it is refused, and so is an unknown folder.
    let alps = folder_id(&owner, client, "Alps");
    for (parent, words) in [
        (fixture.travel.to_string(), "cannot be moved into itself"),
        (alps.as_str().unwrap().to_owned(), "which is inside it"),
    ] {
        let (code, message) = refused(
            &owner,
            client,
            "folder.move",
            json!({"folder_id": fixture.travel, "parent_id": parent, "mutation": envelope(&format!("bad-{parent}"))}),
        );
        assert_eq!(code, "validation", "{message}");
        assert!(message.contains(words), "{message}");
    }
    let (code, _) = refused(
        &owner,
        client,
        "folder.move",
        json!({"folder_id": CatalogFolderId::new(), "mutation": envelope("bad-unknown")}),
    );
    assert_eq!(code, "validation");
    // Merging Konstanz into Alps moves its photographs, removed one included, then deletes it.
    let merged = ok(
        &owner,
        client,
        "folder.merge",
        json!({"folder_id": fixture.konstanz, "into_id": alps, "mutation": envelope("f5")}),
    );
    assert_eq!(merged["items"], 5, "four photographs and the folder");
    let detail = ok(
        &owner,
        client,
        "library.inspect",
        json!({"sequence": merged["change"]}),
    );
    let kinds: Vec<&str> = detail["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["item"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "asset-folder",
            "asset-folder",
            "asset-folder",
            "asset-folder",
            "catalog-folder"
        ]
    );
    assert_eq!(
        tree(&owner, client),
        [
            named("Travel", None, 0, None),
            named("Lakes and mountains", Some("Travel"), 0, None),
            named("Alps", Some("Lakes and mountains"), 3, Some(2026)),
        ]
    );
    // An empty folder is deleted.
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Scratch", "mutation": envelope("f6")}),
    );
    let scratch = folder_id(&owner, client, "Scratch");
    ok(
        &owner,
        client,
        "folder.delete",
        json!({"folder_id": scratch, "mutation": envelope("f7")}),
    );
    let (events, _) = library_events(&owner, client, start);
    let methods: Vec<&str> = events.iter().map(|(method, _)| method.as_str()).collect();
    assert_eq!(
        methods,
        [
            "folder.create",
            "folder.create",
            "folder.rename",
            "folder.move",
            "folder.merge",
            "folder.create",
            "folder.delete"
        ],
        "one event per change, the merge's five items included"
    );
    let changed = tree(&owner, client);
    let changed_list = ok(&owner, client, "folder.list", json!({}));
    assert_eq!(
        labels(&owner, client),
        [
            "Created folder Lakes",
            "Created folder Alps",
            "Renamed folder Lakes to Lakes and mountains",
            "Moved folder Konstanz · Sep 2026 into Lakes and mountains",
            "Merged Konstanz · Sep 2026 into Alps",
            "Created folder Scratch",
            "Deleted folder Scratch",
        ]
    );

    // Undoing all seven restores the exact tree, the merged folder's event span included.
    for step in 0..7 {
        let undone = undo(&owner, client, &format!("u{step}"));
        assert_eq!(undone["outcome"], "applied", "undo {step}");
    }
    assert_eq!(ok(&owner, client, "folder.list", json!({})), initial);
    // Redoing all seven makes the same tree again, with the same identities.
    for step in 0..7 {
        redo(&owner, client, &format!("r{step}"));
    }
    assert_eq!(tree(&owner, client), changed);
    assert_eq!(ok(&owner, client, "folder.list", json!({})), changed_list);
    stop(owner, join);
}

/// Deleting a folder that holds photographs, removed ones too, or folders is refused saying what it
/// holds; a merge into itself or a folder inside it is refused; an unknown parent is refused.
#[test]
fn catalog_folder_delete_of_a_folder_that_holds_anything_is_refused() {
    let fixture = Fixture::new("delete");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let (_, start) = library_events(&owner, client, 0);
    let (code, message) = refused(
        &owner,
        client,
        "folder.delete",
        json!({"folder_id": fixture.konstanz, "mutation": envelope("d1")}),
    );
    assert_eq!(code, "conflict");
    assert_eq!(
        message,
        "folder Konstanz · Sep 2026 cannot be deleted: it holds 4 photographs (1 of them in Removed)"
    );
    // Only the removed photograph left: still refused, and said so.
    ok(
        &owner,
        client,
        "asset.move",
        json!({
            "targets": {"kind": "assets", "asset_ids": &fixture.assets[..3]},
            "folder_id": fixture.travel,
            "mutation": envelope("m1"),
        }),
    );
    let (_, message) = refused(
        &owner,
        client,
        "folder.delete",
        json!({"folder_id": fixture.konstanz, "mutation": envelope("d2")}),
    );
    assert!(
        message.ends_with("it holds 1 photograph (all in Removed)"),
        "{message}"
    );
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Inside", "parent_id": fixture.travel, "mutation": envelope("c1")}),
    );
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Also inside", "parent_id": fixture.travel, "mutation": envelope("c2")}),
    );
    let (code, message) = refused(
        &owner,
        client,
        "folder.delete",
        json!({"folder_id": fixture.travel, "mutation": envelope("d3")}),
    );
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "conflict",
            "folder Travel cannot be deleted: it holds 3 photographs and 2 folders"
        )
    );
    let inside = folder_id(&owner, client, "Inside");
    for (index, (into, words)) in [
        (json!(fixture.travel), "cannot be merged into itself"),
        (inside, "which is inside it"),
    ]
    .into_iter()
    .enumerate()
    {
        let (code, message) = refused(
            &owner,
            client,
            "folder.merge",
            json!({"folder_id": fixture.travel, "into_id": into, "mutation": envelope(&format!("g{index}"))}),
        );
        assert_eq!(code, "validation");
        assert!(message.contains(words), "{message}");
    }
    let (code, message) = refused(
        &owner,
        client,
        "folder.create",
        json!({"name": "Orphan", "parent_id": CatalogFolderId::new(), "mutation": envelope("c3")}),
    );
    assert_eq!(code, "validation");
    assert!(message.starts_with("unknown catalog folder"), "{message}");
    let (events, _) = library_events(&owner, client, start);
    assert_eq!(
        events.len(),
        3,
        "only the move and the two creates were recorded"
    );
    stop(owner, join);
}

/// A name is unique among its siblings ignoring case, at the top level too, for a create, a rename,
/// a move and a merge's subfolders, and the refusal names the clash; the same holds for
/// collections, whatever their kinds.
#[test]
fn catalog_folder_and_collection_name_clashes_are_refused_naming_them() {
    let fixture = Fixture::new("clash");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let conflict = |method: &str, params: Value| {
        let (code, message) = refused(&owner, client, method, params);
        assert_eq!(code, "conflict", "{method}: {message}");
        message
    };
    assert_eq!(
        conflict(
            "folder.create",
            json!({"name": "TRAVEL", "mutation": envelope("n1")})
        ),
        "there is already a folder named Travel at the top level"
    );
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Lakes", "parent_id": fixture.travel, "mutation": envelope("n2")}),
    );
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "lakes", "mutation": envelope("n3")}),
    );
    let top_lakes = folder_id(&owner, client, "lakes");
    assert_eq!(
        conflict(
            "folder.move",
            json!({"folder_id": top_lakes, "parent_id": fixture.travel, "mutation": envelope("n4")})
        ),
        "there is already a folder named Lakes in Travel"
    );
    assert_eq!(
        conflict(
            "folder.rename",
            json!({"folder_id": top_lakes, "name": "konstanz · sep 2026", "mutation": envelope("n5")})
        ),
        "there is already a folder named Konstanz · Sep 2026 at the top level"
    );
    // A rename that changes only the case is not a clash with itself.
    ok(
        &owner,
        client,
        "folder.rename",
        json!({"folder_id": top_lakes, "name": "LAKES", "mutation": envelope("n6")}),
    );
    // Merging a folder whose subfolder's name the receiving folder holds is refused naming it.
    ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "Lakes", "parent_id": fixture.konstanz, "mutation": envelope("n7")}),
    );
    assert_eq!(
        conflict(
            "folder.merge",
            json!({"folder_id": fixture.konstanz, "into_id": fixture.travel, "mutation": envelope("n8")})
        ),
        "folder Konstanz · Sep 2026 cannot be merged into Travel: Travel already holds a folder named Lakes"
    );
    let (code, _) = refused(
        &owner,
        client,
        "folder.create",
        json!({"name": "   ", "mutation": envelope("n9")}),
    );
    assert_eq!(code, "validation", "an empty name");

    ok(
        &owner,
        client,
        "collection.create",
        json!({"name": "Prints", "kind": "group", "mutation": envelope("k1")}),
    );
    assert_eq!(
        conflict(
            "collection.create",
            json!({"name": "portfolio", "kind": "group", "mutation": envelope("k2")})
        ),
        "there is already a collection named Portfolio at the top level"
    );
    assert_eq!(
        conflict(
            "collection.create-smart",
            json!({"name": "PRINTS", "query": {"source": {"kind": "all-photographs"}}, "mutation": envelope("k3")})
        ),
        "there is already a group named Prints at the top level"
    );
    assert_eq!(
        conflict(
            "collection.rename",
            json!({"collection_id": fixture.portfolio, "name": "prints", "mutation": envelope("k4")})
        ),
        "there is already a group named Prints at the top level"
    );
    stop(owner, join);
}

/// Photographs move between folders as one change that leaves out those already there, labelled
/// by what moved; undo puts each back where it was; and no move, merge or undo touches a file.
#[test]
fn catalog_folder_moves_of_photographs_are_journaled_and_never_touch_a_file() {
    let fixture = Fixture::new("move");
    let originals = fixture.originals();
    let (owner, join) = fixture.start();
    let client = owner.register();
    let one = ok(
        &owner,
        client,
        "asset.move",
        json!({
            "targets": {"kind": "paths", "paths": [&fixture.photos[0]]},
            "folder_id": fixture.travel,
            "mutation": envelope("a1"),
        }),
    );
    assert_eq!(one["items"], 1);
    let several = ok(
        &owner,
        client,
        "asset.move",
        json!({
            "targets": {"kind": "assets", "asset_ids": &fixture.assets},
            "folder_id": fixture.travel,
            "mutation": envelope("a2"),
        }),
    );
    assert_eq!(several["items"], 3, "the first was already there");
    let nothing = ok(
        &owner,
        client,
        "asset.move",
        json!({
            "targets": {"kind": "assets", "asset_ids": &fixture.assets[..2]},
            "folder_id": fixture.travel,
            "mutation": envelope("a3"),
        }),
    );
    assert_eq!(nothing["outcome"], "no-op");
    assert_eq!(
        tree(&owner, client),
        [
            named("Konstanz · Sep 2026", None, 0, None),
            named("Travel", None, 3, Some(2026)),
        ]
    );
    for (targets, folder) in [
        (
            json!({"kind": "assets", "asset_ids": [AssetId::new()]}),
            json!(fixture.travel),
        ),
        (
            json!({"kind": "assets", "asset_ids": &fixture.assets[..1]}),
            json!(CatalogFolderId::new()),
        ),
        (
            json!({"kind": "paths", "paths": [fixture.dir.join("elsewhere.JPG")]}),
            json!(fixture.travel),
        ),
    ] {
        let (code, message) = refused(
            &owner,
            client,
            "asset.move",
            json!({"targets": targets, "folder_id": folder, "mutation": envelope("bad")}),
        );
        assert_eq!(code, "validation", "{message}");
    }
    // Merge Travel back into Konstanz, then undo the merge and the second move.
    ok(
        &owner,
        client,
        "folder.merge",
        json!({"folder_id": fixture.travel, "into_id": fixture.konstanz, "mutation": envelope("a4")}),
    );
    assert_eq!(
        tree(&owner, client),
        [named("Konstanz · Sep 2026", None, 3, Some(2026))]
    );
    undo(&owner, client, "u1");
    undo(&owner, client, "u2");
    assert_eq!(
        tree(&owner, client),
        [
            named("Konstanz · Sep 2026", None, 2, Some(2026)),
            named("Travel", None, 1, Some(2026)),
        ],
        "the first photograph stays where the first move put it"
    );
    assert_eq!(
        labels(&owner, client),
        [
            "Moved DSC_0000.JPG to Travel",
            "Moved 3 photographs to Travel",
            "Merged Travel into Konstanz · Sep 2026",
            "Undo Merged Travel into Konstanz · Sep 2026",
            "Undo Moved 3 photographs to Travel",
        ]
    );
    stop(owner, join);
    assert_eq!(
        fixture.originals(),
        originals,
        "every original's bytes and modification time are unchanged"
    );
}

/// Collections and groups are made, nested, renamed, moved and deleted, and photographs added and
/// removed, each one change with undo; a collection's delete takes its memberships and its undo
/// restores them exactly; a group that holds anything is not deleted.
#[test]
fn catalog_folder_collections_change_membership_and_undo_exactly() {
    let fixture = Fixture::new("collections");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let seeded_members = vec![
        (
            fixture.portfolio.to_string(),
            fixture.assets[0].to_string(),
            111,
        ),
        (
            fixture.portfolio.to_string(),
            fixture.assets[1].to_string(),
            222,
        ),
    ];
    let prints = ok(
        &owner,
        client,
        "collection.create",
        json!({"name": "Prints", "kind": "group", "mutation": envelope("c1")}),
    );
    assert_eq!(prints["collection"]["kind"], "group");
    assert_eq!(
        prints["collection"].get("count"),
        None,
        "a group has no members"
    );
    let prints_id = prints["collection"]["id"].clone();
    let landscapes = ok(
        &owner,
        client,
        "collection.create",
        json!({"name": "Landscapes", "kind": "collection", "parent_id": prints_id, "mutation": envelope("c2")}),
    );
    assert_eq!(landscapes["collection"]["count"], 0);
    let landscapes_id = landscapes["collection"]["id"].clone();
    // Only a group holds collections, and a group never goes inside itself.
    let (code, message) = refused(
        &owner,
        client,
        "collection.create",
        json!({"name": "Nested", "kind": "collection", "parent_id": landscapes_id, "mutation": envelope("c3")}),
    );
    assert_eq!(code, "validation");
    assert_eq!(
        message,
        "Landscapes is a collection, and only a group holds collections"
    );
    ok(
        &owner,
        client,
        "collection.create",
        json!({"name": "Inner", "kind": "group", "parent_id": prints_id, "mutation": envelope("c4")}),
    );
    let inner = shelves(&owner, client);
    assert_eq!(inner.len(), 4);
    let inner_id = ok(&owner, client, "collection.list", json!({}))["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["name"] == "Inner")
        .unwrap()["id"]
        .clone();
    let (code, message) = refused(
        &owner,
        client,
        "collection.move",
        json!({"collection_id": prints_id, "parent_id": inner_id, "mutation": envelope("c5")}),
    );
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "validation",
            "group Prints cannot be moved into Inner, which is inside it"
        )
    );
    ok(
        &owner,
        client,
        "collection.move",
        json!({"collection_id": fixture.portfolio, "parent_id": prints_id, "mutation": envelope("c6")}),
    );
    // Add three to Landscapes (one of them removed), then remove one; each a change with undo.
    let added = ok(
        &owner,
        client,
        "collection.add",
        json!({
            "collection_id": landscapes_id,
            "targets": {"kind": "assets", "asset_ids": [&fixture.assets[0], &fixture.assets[2], &fixture.assets[3]]},
            "mutation": envelope("c7"),
        }),
    );
    assert_eq!(added["items"], 3);
    let again = ok(
        &owner,
        client,
        "collection.add",
        json!({
            "collection_id": landscapes_id,
            "targets": {"kind": "assets", "asset_ids": &fixture.assets[..1]},
            "mutation": envelope("c8"),
        }),
    );
    assert_eq!(again["outcome"], "no-op", "a member keeps when it joined");
    ok(
        &owner,
        client,
        "collection.remove",
        json!({
            "collection_id": landscapes_id,
            "targets": {"kind": "paths", "paths": [&fixture.photos[2]]},
            "mutation": envelope("c9"),
        }),
    );
    assert_eq!(
        shelves(&owner, client),
        [
            ("Prints".into(), None, "group".into(), None),
            ("Inner".into(), Some("Prints".into()), "group".into(), None),
            (
                "Landscapes".into(),
                Some("Prints".into()),
                "collection".into(),
                Some(1)
            ),
            (
                "Portfolio".into(),
                Some("Prints".into()),
                "collection".into(),
                Some(2)
            ),
        ],
        "a removed member is not counted"
    );
    let (code, message) = refused(
        &owner,
        client,
        "collection.delete",
        json!({"collection_id": prints_id, "mutation": envelope("c10")}),
    );
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "conflict",
            "group Prints cannot be deleted: it holds 3 collections"
        )
    );
    for (method, collection, words) in [
        ("collection.add", &prints_id, "is a group"),
        ("collection.remove", &prints_id, "is a group"),
    ] {
        let (code, message) = refused(
            &owner,
            client,
            method,
            json!({
                "collection_id": collection,
                "targets": {"kind": "assets", "asset_ids": &fixture.assets[..1]},
                "mutation": envelope("c11"),
            }),
        );
        assert_eq!(code, "validation");
        assert!(message.contains(words), "{message}");
    }
    let deleted = ok(
        &owner,
        client,
        "collection.delete",
        json!({"collection_id": fixture.portfolio, "mutation": envelope("c12")}),
    );
    assert_eq!(deleted["items"], 3, "two memberships and the collection");
    ok(
        &owner,
        client,
        "collection.rename",
        json!({"collection_id": inner_id, "name": "Inside", "mutation": envelope("c13")}),
    );
    ok(
        &owner,
        client,
        "collection.delete",
        json!({"collection_id": inner_id, "mutation": envelope("c14")}),
    );
    assert_eq!(
        labels(&owner, client),
        [
            "Created group Prints",
            "Created collection Landscapes",
            "Created group Inner",
            "Moved collection Portfolio into Prints",
            "Added 3 to Prints › Landscapes",
            "Removed DSC_0002.JPG from Prints › Landscapes",
            "Deleted collection Portfolio",
            "Renamed group Inner to Inside",
            "Deleted group Inside",
        ]
    );
    // Undo the last three: Inner is back, and so is Portfolio with both its members.
    for step in 0..3 {
        undo(&owner, client, &format!("u{step}"));
    }
    assert_eq!(
        shelves(&owner, client),
        [
            ("Prints".into(), None, "group".into(), None),
            ("Inner".into(), Some("Prints".into()), "group".into(), None),
            (
                "Landscapes".into(),
                Some("Prints".into()),
                "collection".into(),
                Some(1)
            ),
            (
                "Portfolio".into(),
                Some("Prints".into()),
                "collection".into(),
                Some(2)
            ),
        ]
    );
    // Undo the removal and the addition: Landscapes is empty again.
    undo(&owner, client, "u3");
    undo(&owner, client, "u4");
    stop(owner, join);
    assert_eq!(
        fixture.memberships(),
        seeded_members,
        "Portfolio's memberships are restored with the times they joined"
    );
    let (owner, join) = fixture.start();
    let client = owner.register();
    redo(&owner, client, "r1");
    let landscapes_count = shelves(&owner, client)
        .into_iter()
        .find(|(name, ..)| name == "Landscapes")
        .unwrap()
        .3;
    assert_eq!(landscapes_count, Some(2), "the redo survives a restart");
    stop(owner, join);
}

/// A smart collection stores a query over photographs with its defaults; a query over files, or
/// naming a smart collection, a group or an unknown collection or folder, is refused, when made and
/// when updated.
#[test]
fn catalog_folder_smart_collection_queries_are_validated() {
    let fixture = Fixture::new("smart");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let smart = ok(
        &owner,
        client,
        "collection.create-smart",
        json!({
            "name": "From Konstanz",
            "query": {"source": {"kind": "catalog-folder", "folder_id": fixture.konstanz}, "filter": {"text": "DSC"}},
            "mutation": envelope("s1"),
        }),
    );
    let collection = &smart["collection"];
    assert_eq!(collection["kind"], "smart");
    assert_eq!(collection.get("count"), None);
    assert_eq!(
        collection["query"]["source"],
        json!({"kind": "catalog-folder", "folder_id": fixture.konstanz, "subfolders": true}),
        "stored with its defaults"
    );
    assert_eq!(collection["query"]["sort"], json!({"key": "capture-time"}));
    let smart_id = collection["id"].clone();
    let group = ok(
        &owner,
        client,
        "collection.create",
        json!({"name": "Prints", "kind": "group", "mutation": envelope("s2")}),
    )["collection"]["id"]
        .clone();
    let refusals = [
        (
            json!({"kind": "collection", "collection_id": smart_id}),
            "a smart collection may not refer to another smart collection (From Konstanz)",
        ),
        (
            json!({"kind": "collection", "collection_id": group}),
            "Prints is a group, which holds no photographs",
        ),
        (
            json!({"kind": "collection", "collection_id": CollectionId::new()}),
            "unknown collection",
        ),
        (
            json!({"kind": "catalog-folder", "folder_id": CatalogFolderId::new()}),
            "unknown catalog folder",
        ),
        (
            json!({"kind": "folder", "path": fixture.dir}),
            "a smart collection's query is over photographs",
        ),
        (
            json!({"kind": "card", "volume_id": "volume-catalog-folder-tests"}),
            "a smart collection's query is over photographs",
        ),
    ];
    for (index, (source, words)) in refusals.iter().enumerate() {
        for (method, params) in [
            (
                "collection.create-smart",
                json!({"name": format!("Bad {index}"), "query": {"source": source}, "mutation": envelope(&format!("b{index}"))}),
            ),
            (
                "collection.update-smart",
                json!({"collection_id": smart_id, "query": {"source": source}, "mutation": envelope(&format!("u{index}"))}),
            ),
        ] {
            let (code, message) = refused(&owner, client, method, params);
            assert_eq!(code, "validation", "{method} {source}: {message}");
            assert!(message.starts_with(words), "{method} {source}: {message}");
        }
    }
    // A condition that does not apply to photographs is refused as browse.view refuses it.
    let (code, _) = refused(
        &owner,
        client,
        "collection.create-smart",
        json!({"name": "Picked", "query": {"source": {"kind": "all-photographs"}, "filter": {"picked": true}}, "mutation": envelope("b9")}),
    );
    assert_eq!(code, "validation");
    // A smart collection may name a plain collection; only a smart collection's query changes.
    let updated = ok(
        &owner,
        client,
        "collection.update-smart",
        json!({"collection_id": smart_id, "query": {"source": {"kind": "collection", "collection_id": fixture.portfolio}}, "mutation": envelope("s3")}),
    );
    assert_eq!(updated["items"], 1);
    let (code, message) = refused(
        &owner,
        client,
        "collection.update-smart",
        json!({"collection_id": fixture.portfolio, "query": {"source": {"kind": "all-photographs"}}, "mutation": envelope("s4")}),
    );
    assert_eq!(code, "validation");
    assert!(
        message.contains("only a smart collection has a query"),
        "{message}"
    );
    let (code, message) = refused(
        &owner,
        client,
        "collection.add",
        json!({"collection_id": smart_id, "targets": {"kind": "assets", "asset_ids": &fixture.assets[..1]}, "mutation": envelope("s5")}),
    );
    assert_eq!(code, "validation");
    assert!(
        message.contains("its query decides what it holds"),
        "{message}"
    );
    let listed = ok(&owner, client, "collection.list", json!({}));
    let stored = listed["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["id"] == smart_id)
        .unwrap();
    assert_eq!(
        stored["query"]["source"],
        json!({"kind": "collection", "collection_id": fixture.portfolio})
    );
    undo(&owner, client, "s6");
    undo(&owner, client, "s7");
    undo(&owner, client, "s8");
    assert_eq!(
        shelves(&owner, client),
        [("Portfolio".into(), None, "collection".into(), Some(2))]
    );
    assert_eq!(
        labels(&owner, client)[..3],
        [
            "Created smart collection From Konstanz",
            "Created group Prints",
            "Changed the query of smart collection From Konstanz",
        ]
    );
    stop(owner, join);
}

/// A retried create is answered with its first answer, the same folder or collection, once in the
/// session and again after a restart, and records nothing and announces nothing a second time.
#[test]
fn catalog_folder_retries_are_answered_once() {
    let fixture = Fixture::new("retry");
    let (owner, join) = fixture.start();
    let client = owner.register();
    let create = json!({"name": "Once", "mutation": envelope("once")});
    let first = ok(&owner, client, "folder.create", create.clone());
    let (_, after_first) = library_events(&owner, client, 0);
    let retry = ok(&owner, client, "folder.create", create.clone());
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(retry["folder"], first["folder"]);
    assert_eq!(retry["change"]["change"], first["change"]["change"]);
    let collection = json!({"name": "Once", "kind": "collection", "mutation": envelope("once")});
    let made = ok(&owner, client, "collection.create", collection.clone());
    let merge = json!({"folder_id": fixture.travel, "into_id": fixture.konstanz, "mutation": envelope("merge")});
    let merged = ok(&owner, client, "folder.merge", merge.clone());
    let (_, settled) = library_events(&owner, client, 0);
    assert!(settled > after_first);
    stop(owner, join);

    // After a restart the catalog's journal answers each, before anything is checked again: the
    // folder exists now (a name clash) and Travel is gone (an unknown folder).
    let (owner, join) = fixture.start();
    let client = owner.register();
    let (_, restarted) = library_events(&owner, client, 0);
    let again = ok(&owner, client, "folder.create", create);
    assert_eq!(
        (
            &again["change"]["change"],
            &again["change"]["deduplicated"],
            &again["folder"]
        ),
        (&first["change"]["change"], &json!(true), &first["folder"])
    );
    let again = ok(&owner, client, "collection.create", collection);
    assert_eq!(again["collection"], made["collection"]);
    assert_eq!(again["change"]["deduplicated"], true);
    let again = ok(&owner, client, "folder.merge", merge);
    assert_eq!(
        (&again["change"], &again["items"]),
        (&merged["change"], &merged["items"])
    );
    assert_eq!(
        library_events(&owner, client, 0).1,
        restarted,
        "nothing announced"
    );
    assert_eq!(labels(&owner, client).len(), 3, "nothing recorded twice");
    stop(owner, join);
}

/// `folder.list` counts the photographs directly in each folder, removed ones excepted, and names
/// the capture year of its earliest photograph by instant, in one query whatever the tree.
#[test]
fn catalog_folder_list_counts_and_years_follow_the_photographs() {
    let fixture = Fixture::new("list");
    let (owner, join) = fixture.start();
    let client = owner.register();
    // The first photograph alone, dated September 2026, and the third alone, 31 December 2025 local.
    ok(
        &owner,
        client,
        "asset.move",
        json!({"targets": {"kind": "assets", "asset_ids": [&fixture.assets[0]]}, "folder_id": fixture.travel, "mutation": envelope("l1")}),
    );
    let nested = ok(
        &owner,
        client,
        "folder.create",
        json!({"name": "A", "parent_id": fixture.travel, "mutation": envelope("l2")}),
    )["folder"]["id"]
        .clone();
    ok(
        &owner,
        client,
        "asset.move",
        json!({"targets": {"kind": "assets", "asset_ids": [&fixture.assets[2]]}, "folder_id": nested, "mutation": envelope("l3")}),
    );
    assert_eq!(
        tree(&owner, client),
        [
            named("Konstanz · Sep 2026", None, 1, Some(2026)),
            named("Travel", None, 1, Some(2026)),
            named("A", Some("Travel"), 1, Some(2025)),
        ],
        "Konstanz keeps the second photograph and the removed fourth, which is neither counted nor dated"
    );
    stop(owner, join);
}
