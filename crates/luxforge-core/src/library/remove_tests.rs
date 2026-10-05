//! Emptying Removed against a seeded catalog, without the owner: the earliest removed first within
//! its bound, what is left counted; the triggers it lifts created again exactly as the catalog
//! stored them, and a failure inside the transaction leaving every row and trigger as it was; the
//! entries auto-collapse hid deleted with their photograph; and the stroke addresses an entry's
//! text is searched for.
use super::remove::{LIFTED_TRIGGERS, stroke_addresses};
use crate::{
    AssetId, EditorService, ErrorKind,
    catalog_types::{
        CatalogFolder, CatalogFolderId, Collection, CollectionId, CollectionKind, FileAvailability,
        HeaderMetadata, Volume, VolumeId,
    },
    seed::{CatalogSeeder, SeedAsset, SeedKind},
};
use luxforge_testbase::paths::temp_path;
use std::path::PathBuf;

/// A seeded catalog of six photographs in one folder, the first two in a collection. Photographs
/// 1, 3, 4 and 5 are removed, at 400, 100, 300 and 200: removed earliest first, that is 3, 5, 4, 1.
fn seeded(name: &str) -> (PathBuf, Vec<AssetId>) {
    let path = temp_path(&format!("remove-{name}.sqlite"));
    let _ = std::fs::remove_file(&path);
    let volume = Volume {
        id: VolumeId::parse("volume-remove-tests").unwrap(),
        mount_point: "/".into(),
        label: "Test disk".into(),
        removable: false,
        platform_id: None,
        last_seen_ms: 1,
    };
    let folder = CatalogFolder {
        id: CatalogFolderId::new(),
        name: "Konstanz · Sep 2026".into(),
        parent_id: None,
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    };
    let removed = [None, Some(400), None, Some(100), Some(300), Some(200)];
    let assets: Vec<SeedAsset> = removed
        .iter()
        .enumerate()
        .map(|(index, removed_ms)| SeedAsset {
            id: AssetId::parse(format!("asset-{:032x}", index + 1)).unwrap(),
            kind: SeedKind::Jpeg,
            locator: PathBuf::from(format!("/Photos/DSC_{index:04}.JPG")),
            fingerprint: format!("{:064x}", index + 1),
            file_identity: format!("unix:9:{}", index + 1),
            byte_len: 64,
            width: 60,
            height: 40,
            catalog_folder_id: folder.id.clone(),
            volume_id: volume.id.clone(),
            developed_ms: 1_000,
            removed_ms: *removed_ms,
            availability: FileAvailability::Available,
            checked_ms: 1,
            develop_moment: None,
            header: HeaderMetadata::default(),
            place: None,
        })
        .collect();
    let collection = Collection {
        id: CollectionId::new(),
        name: "Portfolio".into(),
        parent_id: None,
        kind: CollectionKind::Collection,
        query: None,
        created_ms: 1,
        count: None,
    };
    let mut seeder = CatalogSeeder::create(&path, "remove-tests").unwrap();
    seeder.volumes(&[volume]).unwrap();
    seeder.folders(&[folder]).unwrap();
    let rows = seeder.assets(&assets).unwrap();
    seeder
        .collections(std::slice::from_ref(&collection))
        .unwrap();
    seeder
        .members(&[
            (collection.id.clone(), rows[0], 1),
            (collection.id.clone(), rows[1], 2),
        ])
        .unwrap();
    seeder.finish().unwrap();
    (path, assets.into_iter().map(|asset| asset.id).collect())
}

/// Each photograph's rows in every table that holds its record, in a fixed order.
fn records(service: &EditorService, asset: &AssetId) -> [i64; 6] {
    let count = |sql: &str| -> i64 {
        service
            .connection
            .query_row(sql, [asset.as_str()], |row| row.get(0))
            .unwrap()
    };
    [
        count("SELECT count(*) FROM assets WHERE id = ?1"),
        count("SELECT count(*) FROM entries WHERE asset_id = ?1"),
        count("SELECT count(*) FROM asset_state WHERE asset_id = ?1"),
        count(
            "SELECT count(*) FROM capture WHERE asset_row = (SELECT row_id FROM assets WHERE id = ?1)",
        ),
        count(
            "SELECT count(*) FROM collection_members
             WHERE asset_row = (SELECT row_id FROM assets WHERE id = ?1)",
        ),
        count("SELECT count(*) FROM requests WHERE asset_id = ?1"),
    ]
}

/// Every trigger the catalog holds, by name, with its stored SQL.
fn triggers(service: &EditorService) -> Vec<(String, String)> {
    service
        .connection
        .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'trigger' ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Emptying takes the earliest removed first, at most its bound, deletes exactly their records and
/// says how many removed photographs are left; the next emptying takes the rest, and one with none
/// removed deletes nothing. The photographs not removed keep every row.
#[test]
fn remove_emptying_takes_the_earliest_removed_first_within_its_bound() {
    let (path, assets) = seeded("bound");
    let mut service = EditorService::open(&path).unwrap();
    let kept_before: Vec<[i64; 6]> = [0, 2]
        .iter()
        .map(|&at| records(&service, &assets[at]))
        .collect();
    assert_eq!(records(&service, &assets[1]), [1, 1, 1, 1, 1, 0]);

    let (emptied, collection) = service.empty_removed(2).unwrap();
    assert_eq!(emptied.assets, [assets[3].clone(), assets[5].clone()]);
    assert_eq!(emptied.remaining, 2);
    assert!(emptied.artifacts.is_empty());
    assert!(
        collection.is_none(),
        "no artifact row went, so nothing to collect"
    );
    for gone in [3, 5] {
        assert_eq!(records(&service, &assets[gone]), [0; 6]);
    }
    assert_eq!(records(&service, &assets[4]), [1, 1, 1, 1, 0, 0]);

    let (emptied, _) = service.empty_removed(2).unwrap();
    assert_eq!(emptied.assets, [assets[4].clone(), assets[1].clone()]);
    assert_eq!(emptied.remaining, 0);
    assert_eq!(records(&service, &assets[1]), [0; 6], "its membership too");
    let (emptied, _) = service.empty_removed(2).unwrap();
    assert!(emptied.assets.is_empty());
    assert_eq!(emptied.remaining, 0);
    let kept_after: Vec<[i64; 6]> = [0, 2]
        .iter()
        .map(|&at| records(&service, &assets[at]))
        .collect();
    assert_eq!(kept_after, kept_before);
    let _ = std::fs::remove_file(path);
}

/// The trigger emptying lifts is created again from the SQL the catalog stored for it, so the
/// catalog's triggers are exactly what they were and the permanence holds again after it; a failure
/// after it was dropped rolls the whole transaction back, the trigger with it, and every record
/// stays. A removed photograph's artifact reference goes with it; the artifact another photograph
/// still references stays.
#[test]
fn remove_emptying_restores_the_triggers_it_lifts_and_a_failure_keeps_everything() {
    let (path, assets) = seeded("triggers");
    let mut service = EditorService::open(&path).unwrap();
    let artifact = format!("artifact-{}", "ab".repeat(32));
    service
        .connection
        .execute(
            "INSERT INTO artifacts (id, sha256, bytes, kind, module_id, created_ms)
             VALUES (?1, ?2, 1, 'k', 'm', 1)",
            [&artifact, &"ab".repeat(32)],
        )
        .unwrap();
    for asset in [&assets[0], &assets[3]] {
        service
            .connection
            .execute(
                "INSERT INTO artifact_refs (entry_id, artifact_id)
                 SELECT current_entry_id, ?2 FROM asset_state WHERE asset_id = ?1",
                [asset.as_str(), &artifact],
            )
            .unwrap();
    }
    let references = |service: &EditorService| -> i64 {
        service
            .connection
            .query_row("SELECT count(*) FROM artifact_refs", [], |row| row.get(0))
            .unwrap()
    };
    let permanent = |service: &EditorService| {
        let refused = service
            .connection
            .execute("DELETE FROM artifact_refs", [])
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("artifact references are permanent"),
            "{refused}"
        );
    };
    permanent(&service);
    let before = triggers(&service);
    for lifted in LIFTED_TRIGGERS {
        assert!(before.iter().any(|(name, _)| name == lifted), "{lifted}");
    }

    // A failure at the last delete, after the trigger was dropped: nothing changes.
    service
        .connection
        .execute_batch(
            "CREATE TRIGGER injected_failure BEFORE DELETE ON assets BEGIN
                 SELECT RAISE(ABORT, 'injected failure');
             END;",
        )
        .unwrap();
    let with_injected = triggers(&service);
    let records_before: Vec<[i64; 6]> = assets.iter().map(|a| records(&service, a)).collect();
    let error = service.empty_removed(10).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Catalog, "{}", error.detail);
    assert!(
        error.detail.contains("injected failure"),
        "{}",
        error.detail
    );
    let records_after: Vec<[i64; 6]> = assets.iter().map(|a| records(&service, a)).collect();
    assert_eq!(records_after, records_before);
    assert_eq!(references(&service), 2);
    assert_eq!(
        triggers(&service),
        with_injected,
        "the lifted trigger is back"
    );
    permanent(&service);

    service
        .connection
        .execute_batch("DROP TRIGGER injected_failure")
        .unwrap();
    let (emptied, collection) = service.empty_removed(10).unwrap();
    assert_eq!(emptied.assets.len(), 4);
    assert!(
        emptied.artifacts.is_empty() && collection.is_none(),
        "the artifact is still referenced"
    );
    assert_eq!(
        references(&service),
        1,
        "the removed photograph's reference went"
    );
    assert_eq!(
        triggers(&service),
        before,
        "created again exactly as stored"
    );
    permanent(&service);
    let _ = std::fs::remove_file(path);
}

/// Every stroke address in an entry's text is found, whatever surrounds it; a string of any other
/// shape is not.
#[test]
fn remove_finds_every_stroke_address_in_an_entry() {
    let first = "0123456789abcdef0123456789abcdef";
    let second = "fedcba9876543210fedcba9876543210";
    let text = serde_json::json!({
        "masks": [{"components": [{"payload": {"strokes": [first, second]}}]}],
        "label": "Add brush",
        "id": "entry-0123456789abcdef0123456789abcdef",
        "upper": "0123456789ABCDEF0123456789ABCDEF",
        "short": "0123456789abcdef0123456789abcde",
        "quoted": format!("a\"{first}"),
    })
    .to_string();
    let found: Vec<&str> = stroke_addresses(&text).collect();
    assert!(
        found.contains(&first) && found.contains(&second),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .all(|address| *address == first || *address == second),
        "{found:?}"
    );
}

/// An entry auto-collapse hid is history its photograph keeps: sending the photograph back is
/// refused, as for any history beyond its Original, though the visible history shows only the
/// Original, and emptying Removed deletes the hidden entry with the rest of the record, lifting the
/// collapsed entries' permanence for its own transaction only.
#[test]
fn remove_emptying_deletes_the_entries_auto_collapse_hid() {
    let path = temp_path("remove-collapsed.sqlite");
    let _ = std::fs::remove_file(&path);
    let mut service = EditorService::open(&path).unwrap();
    service.set_auto_collapse(true);
    let asset = service
        .import(&luxforge_testbase::paths::jpeg())
        .unwrap()
        .asset
        .id;
    // Contrast +15, then back to where it began: the head returns to the Original and the
    // +15 entry is hidden.
    for (request, contrast) in [("up", 15.0), ("back", 0.0)] {
        let revision = service.state(&asset).unwrap().revision;
        service
            .apply_action(
                &asset,
                crate::editor::mutation(revision, request),
                "set-basic",
                serde_json::json!({"contrast": contrast}),
            )
            .unwrap();
    }
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 1);
    let collapsed = |service: &EditorService| -> i64 {
        service
            .connection
            .query_row(
                "SELECT count(*) FROM collapsed_entries WHERE asset_id = ?1",
                [asset.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(collapsed(&service), 1);
    assert_eq!(
        crate::editor::library_rows::kept_by(&service.connection, &asset).unwrap(),
        Some("has been edited since it was developed"),
        "a hidden entry is history sending back would erase"
    );
    let refused = service
        .connection
        .execute("DELETE FROM collapsed_entries", [])
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("collapsed entries stay collapsed"),
        "{refused}"
    );

    let before = triggers(&service);
    service
        .connection
        .execute(
            "UPDATE assets SET removed_ms = 1 WHERE id = ?1",
            [asset.as_str()],
        )
        .unwrap();
    let (emptied, _) = service.empty_removed(10).unwrap();
    assert_eq!(emptied.assets, std::slice::from_ref(&asset));
    assert_eq!(records(&service, &asset), [0; 6]);
    assert_eq!(collapsed(&service), 0);
    assert_eq!(triggers(&service), before, "both lifted triggers are back");
    drop(service);
    let _ = std::fs::remove_file(path);
}
