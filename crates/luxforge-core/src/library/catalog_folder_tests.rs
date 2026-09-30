//! Catalog folders and collections at the batch limit, planned against a catalog of 50,000
//! photographs: a merge or a collection's delete that one library change cannot cover is refused
//! whole, before anything is written, and one that fits is planned in full.
use super::{collections, folders};
use crate::{
    EditorService, ErrorKind,
    catalog_types::{
        CatalogFolder, CatalogFolderId, Collection, CollectionId, CollectionKind,
        MAX_LIBRARY_BATCH, Volume, VolumeId,
    },
    editor::{self, insert_catalog_folder, insert_collection, upsert_volume},
};
use luxforge_testbase::paths::temp_path;

fn folder(name: &str) -> CatalogFolder {
    CatalogFolder {
        id: CatalogFolderId::new(),
        name: name.into(),
        parent_id: None,
        created_ms: 1,
        event: None,
        count: 0,
        year: None,
    }
}

/// Insert `count` bare photographs into `folder`, numbered from `from`, in one statement.
fn photographs(service: &mut EditorService, folder: &CatalogFolderId, from: usize, count: usize) {
    editor::write(&mut service.connection, |tx| {
        tx.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?2)
             INSERT INTO assets (id, source_root, locator, canonical_locator, file_identity,
                 fingerprint, byte_len, width, height, source_json, source_kind,
                 catalog_folder_id, source_folder, volume_id, file_name, developed_ms,
                 availability, checked_ms)
             SELECT printf('asset-%032x', i), '/x', '/x/' || i, '/x/' || i, 'unix:1:' || i,
                 'f', 1, 1, 1, '{}', 'jpeg', ?3, '/x', 'volume-catalog-folder-limit', 'P' || i, 1,
                 'available', 1
             FROM n",
            rusqlite::params![from as i64, (from + count) as i64, folder.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn catalog_folder_merges_and_collection_deletes_past_the_batch_limit_are_refused_whole() {
    let path = temp_path("catalog-folder-limit.sqlite");
    let _ = std::fs::remove_file(&path);
    let mut service = EditorService::open(&path).unwrap();
    let (full, other) = (folder("Full"), folder("Other"));
    let collection = Collection {
        id: CollectionId::new(),
        name: "Everything".into(),
        parent_id: None,
        kind: CollectionKind::Collection,
        query: None,
        created_ms: 1,
        count: None,
    };
    editor::write(&mut service.connection, |tx| {
        upsert_volume(
            tx,
            &Volume {
                id: VolumeId::parse("volume-catalog-folder-limit").unwrap(),
                mount_point: "/".into(),
                label: "Disk".into(),
                removable: false,
                platform_id: None,
                last_seen_ms: 1,
            },
        )?;
        insert_catalog_folder(tx, &full)?;
        insert_catalog_folder(tx, &other)?;
        insert_collection(tx, &collection)
    })
    .unwrap();
    // One photograph fewer than the limit: the merge moves them all and deletes the folder, the
    // limit's worth of items exactly.
    photographs(&mut service, &full.id, 0, MAX_LIBRARY_BATCH - 1);
    let planned = folders::merge(&service.connection, &full.id, &other.id).unwrap();
    assert_eq!(planned.changes.len(), MAX_LIBRARY_BATCH);
    let listed = folders::list(&service.connection).unwrap();
    assert_eq!(listed.folders[0].count as usize, MAX_LIBRARY_BATCH - 1);
    // One more is past it, and refused before anything is recorded.
    photographs(&mut service, &full.id, MAX_LIBRARY_BATCH - 1, 1);
    let error = folders::merge(&service.connection, &full.id, &other.id)
        .map(drop)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
    // A collection of the limit's worth of members cannot be deleted with them in one change.
    editor::write(&mut service.connection, |tx| {
        tx.execute(
            "INSERT INTO collection_members (collection_id, asset_row, added_ms)
             SELECT ?1, row_id, 1 FROM assets",
            [collection.id.as_str()],
        )?;
        Ok(())
    })
    .unwrap();
    let error = collections::delete(&service.connection, &collection.id)
        .map(drop)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
    assert_eq!(
        collections::list(&service.connection).unwrap().collections[0].count,
        Some(MAX_LIBRARY_BATCH as u32)
    );
    assert!(
        super::journal::page(&service.connection, None, 10)
            .unwrap()
            .changes
            .is_empty()
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}
