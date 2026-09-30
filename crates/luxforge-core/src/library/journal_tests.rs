//! The journal on the service: changes, undo and redo of every item kind, the actor's scope, the
//! refusals, retries across a restart, the batch limit and the pages.
use super::*;
use crate::{
    EditorService, ErrorKind,
    catalog_types::{
        AssetFolderValue, AssetRemovalValue, CatalogFolderId, Collection, CollectionId,
        CollectionKind, CollectionValue, FileSignature, FolderValue, IndexedFolder,
        MembershipValue, Pick, Volume, VolumeId,
    },
    editor::upsert_volume,
    library::items,
};
use luxforge_testbase::paths::{jpeg, temp_dir, temp_path};
use serde_json::to_value;
use std::path::PathBuf;

fn volume() -> Volume {
    Volume {
        id: VolumeId::parse("volume-0123456789abcdef").unwrap(),
        mount_point: "/".into(),
        label: "Test disk".into(),
        removable: false,
        platform_id: None,
        last_seen_ms: 1,
    }
}

/// A new catalog with one volume recorded.
fn catalog(name: &str) -> (PathBuf, EditorService) {
    let path = temp_path(&format!("journal-{name}.sqlite"));
    let _ = std::fs::remove_file(&path);
    let mut service = EditorService::open(&path).unwrap();
    crate::editor::write(&mut service.connection, |tx| upsert_volume(tx, &volume())).unwrap();
    (path, service)
}

fn pick(path: &str, actor: &str, request: &str) -> Pick {
    Pick {
        path: path.into(),
        signature: FileSignature {
            len: 100,
            modified_ns: 7,
            identity: None,
        },
        volume_id: volume().id,
        actor: actor.into(),
        request_id: request.into(),
        picked_ms: 1_000,
        file_id: None,
    }
}

fn request<'a>(method: &'a str, actor: &'a str, request_id: &'a str) -> Request<'a> {
    Request {
        method,
        actor,
        request_id,
    }
}

/// Pick (`true`) or clear the paths as one change by `actor` under `request_id`.
fn picks(
    service: &mut EditorService,
    actor: &str,
    request_id: &str,
    paths: &[(&str, bool)],
) -> Result<Outcome, Error> {
    let changes = paths
        .iter()
        .map(|(path, picked)| {
            let item = LibraryItem::Pick {
                path: (*path).into(),
            };
            let desired = if *picked {
                Desired::UnlessPresent(to_value(pick(path, actor, request_id)).unwrap())
            } else {
                Desired::Value(None)
            };
            (item, desired)
        })
        .collect();
    let request = request("pick.set", actor, request_id);
    service.library_write(|tx| apply(tx, request, changes, crate::library::picks::label))
}

fn undo_as(service: &mut EditorService, actor: &str, request_id: &str) -> Result<Outcome, Error> {
    service.library_write(|tx| undo(tx, request("library.undo", actor, request_id)))
}

fn redo_as(service: &mut EditorService, actor: &str, request_id: &str) -> Result<Outcome, Error> {
    service.library_write(|tx| redo(tx, request("library.redo", actor, request_id)))
}

/// The picked paths with who picked them and when, in path order.
fn picked(service: &EditorService) -> Vec<(String, String, String)> {
    service
        .connection
        .prepare("SELECT path, actor, request_id FROM picks ORDER BY path")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn sequence(outcome: &Outcome) -> u64 {
    outcome.answer().change.expect("a recorded change").0
}

fn owned(paths: &[&str], actor: &str, request: &str) -> Vec<(String, String, String)> {
    paths
        .iter()
        .map(|path| ((*path).into(), actor.into(), request.into()))
        .collect()
}

#[test]
fn a_journal_change_records_each_items_value_before_and_after_and_a_no_op_records_nothing() {
    let (path, mut service) = catalog("record");
    let first = picks(
        &mut service,
        "agent",
        "r1",
        &[("/a.NEF", true), ("/b.NEF", true)],
    )
    .unwrap();
    let detail = inspect(&service.connection, sequence(&first)).unwrap();
    assert_eq!(detail.change.label, "Picked 2 files");
    assert_eq!(
        (detail.change.actor.as_str(), detail.change.method.as_str()),
        ("agent", "pick.set")
    );
    assert_eq!(detail.change.item_count, 2);
    assert_eq!(
        detail.rows,
        [
            LibraryChangeRow {
                item: LibraryItem::Pick {
                    path: "/a.NEF".into()
                },
                before: None,
                after: Some(to_value(pick("/a.NEF", "agent", "r1")).unwrap()),
            },
            LibraryChangeRow {
                item: LibraryItem::Pick {
                    path: "/b.NEF".into()
                },
                before: None,
                after: Some(to_value(pick("/b.NEF", "agent", "r1")).unwrap()),
            },
        ]
    );
    // Picking what is picked keeps the first pick and records nothing.
    let again = picks(&mut service, "other", "r2", &[("/a.NEF", true)]).unwrap();
    assert!(matches!(again, Outcome::NoOp));
    assert_eq!(again.answer().outcome, MutationOutcome::NoOp);
    assert_eq!(
        picked(&service),
        owned(&["/a.NEF", "/b.NEF"], "agent", "r1")
    );
    // A clear records the pick it removed, and one change of two kinds is labelled as both.
    let clear = picks(&mut service, "agent", "r3", &[("/a.NEF", false)]).unwrap();
    let rows = inspect(&service.connection, sequence(&clear)).unwrap();
    assert_eq!(rows.change.label, "Cleared the pick of a.NEF");
    assert_eq!(
        rows.rows[0].before,
        Some(to_value(pick("/a.NEF", "agent", "r1")).unwrap())
    );
    assert_eq!(rows.rows[0].after, None);
    let mixed = picks(
        &mut service,
        "agent",
        "r4",
        &[("/a.NEF", true), ("/b.NEF", false)],
    )
    .unwrap();
    assert_eq!(
        inspect(&service.connection, sequence(&mixed))
            .unwrap()
            .change
            .label,
        "Picked 1 files and cleared 1 picks"
    );
    assert_eq!(
        page(&service.connection, None, 10).unwrap().changes.len(),
        3
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn journal_undo_and_redo_restore_exact_prior_values_and_are_journaled() {
    let (path, mut service) = catalog("undo-redo");
    let first = picks(
        &mut service,
        "agent",
        "r1",
        &[("/a.NEF", true), ("/b.NEF", true)],
    )
    .unwrap();
    let second = picks(&mut service, "agent", "r2", &[("/a.NEF", false)]).unwrap();
    assert_eq!(picked(&service), owned(&["/b.NEF"], "agent", "r1"));

    let undo_second = undo_as(&mut service, "agent", "u1").unwrap();
    assert_eq!(
        picked(&service),
        owned(&["/a.NEF", "/b.NEF"], "agent", "r1"),
        "the pick comes back exactly as it was, not as a new pick"
    );
    let undo_first = undo_as(&mut service, "agent", "u2").unwrap();
    assert!(picked(&service).is_empty());
    assert!(matches!(
        undo_as(&mut service, "agent", "u3").unwrap(),
        Outcome::NoOp
    ));

    let redo_first = redo_as(&mut service, "agent", "d1").unwrap();
    assert_eq!(
        picked(&service),
        owned(&["/a.NEF", "/b.NEF"], "agent", "r1")
    );
    let redo_second = redo_as(&mut service, "agent", "d2").unwrap();
    assert_eq!(picked(&service), owned(&["/b.NEF"], "agent", "r1"));
    assert!(matches!(
        redo_as(&mut service, "agent", "d3").unwrap(),
        Outcome::NoOp
    ));
    // A redo is undone like any change.
    undo_as(&mut service, "agent", "u4").unwrap();
    assert_eq!(
        picked(&service),
        owned(&["/a.NEF", "/b.NEF"], "agent", "r1")
    );

    let journal = page(&service.connection, None, 100).unwrap().changes;
    let described: Vec<_> = journal
        .iter()
        .map(|change| {
            (
                change.label.as_str(),
                change.undoes.map(|seq| seq.0),
                change.redoes.map(|seq| seq.0),
                change.undone_by.map(|seq| seq.0),
            )
        })
        .collect();
    let (c1, c2, u2, u1, r1, r2) = (
        sequence(&first),
        sequence(&second),
        sequence(&undo_second),
        sequence(&undo_first),
        sequence(&redo_first),
        sequence(&redo_second),
    );
    assert_eq!(
        described,
        [
            ("Picked 2 files", None, None, Some(u1)),
            ("Cleared the pick of a.NEF", None, None, Some(u2)),
            ("Undo Cleared the pick of a.NEF", Some(c2), None, None),
            ("Undo Picked 2 files", Some(c1), None, None),
            ("Redo Picked 2 files", None, Some(u1), None),
            (
                "Redo Cleared the pick of a.NEF",
                None,
                Some(u2),
                Some(r2 + 1)
            ),
            ("Undo Cleared the pick of a.NEF", Some(r2), None, None),
        ]
    );
    assert_eq!(r1, u1 + 1);
    // An undo's rows are its change's, reversed and inverted.
    let original = inspect(&service.connection, c1).unwrap().rows;
    let inverse = inspect(&service.connection, u1).unwrap().rows;
    assert_eq!(inverse.len(), original.len());
    for (undone, done) in inverse.iter().zip(original.iter().rev()) {
        assert_eq!(undone.item, done.item);
        assert_eq!((&undone.before, &undone.after), (&done.after, &done.before));
    }
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_journal_undo_is_the_actors_own_and_refuses_naming_the_items_another_changed() {
    let (path, mut service) = catalog("two-actors");
    picks(
        &mut service,
        "a",
        "a1",
        &[("/x.NEF", true), ("/y.NEF", true)],
    )
    .unwrap();
    picks(&mut service, "b", "b1", &[("/y.NEF", false)]).unwrap();

    // a's latest change touched y, which b cleared since: refused, naming y alone, and nothing
    // changes.
    let error = undo_as(&mut service, "a", "a2").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    let y = LibraryItem::Pick {
        path: "/y.NEF".into(),
    };
    let x = LibraryItem::Pick {
        path: "/x.NEF".into(),
    };
    assert!(names(&error, &y), "{error:?}");
    assert!(!names(&error, &x), "{error:?}");
    assert_eq!(error.data.as_ref().unwrap()["count"], 1);
    assert!(error.detail.contains("pick /y.NEF"), "{}", error.detail);
    assert_eq!(picked(&service), owned(&["/x.NEF"], "a", "a1"));
    assert_eq!(
        page(&service.connection, None, 10).unwrap().changes.len(),
        2
    );

    // b undoes its own change, not a's; then b's change and its undo cancel out, so a may undo.
    undo_as(&mut service, "b", "b2").unwrap();
    assert_eq!(picked(&service), owned(&["/x.NEF", "/y.NEF"], "a", "a1"));
    undo_as(&mut service, "a", "a3").unwrap();
    assert!(picked(&service).is_empty());
    // Nothing is left for b to undo: its only change is undone.
    assert!(matches!(
        undo_as(&mut service, "b", "b3").unwrap(),
        Outcome::NoOp
    ));
    // And b's redo would bring back its clear of y, which a's undo has since touched.
    let error = redo_as(&mut service, "b", "b4").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(names(&error, &y));
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_journal_undo_refuses_an_item_whose_value_changed_outside_the_journal() {
    let (path, mut service) = catalog("outside");
    picks(&mut service, "agent", "r1", &[("/x.NEF", true)]).unwrap();
    service
        .connection
        .execute("UPDATE picks SET byte_len = 101 WHERE path = '/x.NEF'", [])
        .unwrap();
    let error = undo_as(&mut service, "agent", "u1").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(names(
        &error,
        &LibraryItem::Pick {
            path: "/x.NEF".into()
        }
    ));
    assert_eq!(
        picked(&service).len(),
        1,
        "the refused undo changed nothing"
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_new_journal_change_ends_what_can_be_redone() {
    let (path, mut service) = catalog("redo-ends");
    picks(&mut service, "agent", "r1", &[("/a.NEF", true)]).unwrap();
    undo_as(&mut service, "agent", "u1").unwrap();
    picks(&mut service, "agent", "r2", &[("/b.NEF", true)]).unwrap();
    assert!(matches!(
        redo_as(&mut service, "agent", "d1").unwrap(),
        Outcome::NoOp
    ));
    // Another actor's new change does not end this actor's redo.
    undo_as(&mut service, "agent", "u2").unwrap();
    picks(&mut service, "other", "o1", &[("/c.NEF", true)]).unwrap();
    redo_as(&mut service, "agent", "d2").unwrap();
    assert_eq!(
        picked(&service)
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        ["/b.NEF", "/c.NEF"]
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_journal_change_over_the_batch_limit_is_refused_whole() {
    let (path, mut service) = catalog("limit");
    let changes = (0..=MAX_LIBRARY_BATCH)
        .map(|index| {
            (
                LibraryItem::Pick {
                    path: format!("/f{index}.NEF").into(),
                },
                Desired::Value(None),
            )
        })
        .collect();
    let error = service
        .library_write(|tx| {
            apply(
                tx,
                request("pick.set", "agent", "r1"),
                changes,
                crate::library::picks::label,
            )
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        page(&service.connection, None, 10)
            .unwrap()
            .changes
            .is_empty()
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_journal_request_is_answered_by_its_first_change_across_a_restart() {
    let (path, mut service) = catalog("retry");
    let first = picks(&mut service, "agent", "r1", &[("/a.NEF", true)]).unwrap();
    let retry = picks(&mut service, "agent", "r1", &[("/b.NEF", true)]).unwrap();
    assert_eq!(retry.answer().change, first.answer().change);
    assert!(retry.answer().deduplicated && retry.announced().is_none());
    // The same request identity from another actor, or through another method, is its own.
    let other = picks(&mut service, "other", "r1", &[("/b.NEF", true)]).unwrap();
    assert_ne!(other.answer().change, first.answer().change);
    drop(service);

    // After a restart the picks are there, a retry is still answered, and the actor's undo scope
    // is its own: it undoes its own change, not the other actor's later one.
    let mut service = EditorService::open(&path).unwrap();
    assert_eq!(
        picked(&service),
        [
            ("/a.NEF".into(), "agent".into(), "r1".into()),
            ("/b.NEF".into(), "other".into(), "r1".into())
        ]
    );
    let retry = picks(&mut service, "agent", "r1", &[("/c.NEF", true)]).unwrap();
    assert_eq!(retry.answer().change, first.answer().change);
    let undone = undo_as(&mut service, "agent", "u1").unwrap();
    assert_eq!(
        picked(&service),
        [("/b.NEF".into(), "other".into(), "r1".into())]
    );
    let retried = undo_as(&mut service, "agent", "u1").unwrap();
    assert_eq!(retried.answer().change, undone.answer().change);
    assert_eq!(
        picked(&service).len(),
        1,
        "a retried undo undoes nothing more"
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn the_journal_pages_after_a_cursor() {
    let (path, mut service) = catalog("pages");
    for index in 0..5 {
        picks(
            &mut service,
            "agent",
            &format!("r{index}"),
            &[(&format!("/f{index}.NEF"), true)],
        )
        .unwrap();
    }
    let sequences = |journal: &LibraryJournal| {
        journal
            .changes
            .iter()
            .map(|change| change.sequence.0)
            .collect::<Vec<_>>()
    };
    let first = page(&service.connection, None, 2).unwrap();
    assert_eq!(
        (sequences(&first), first.next_after),
        (vec![1, 2], Some(LibraryChangeSeq(2)))
    );
    let second = page(&service.connection, Some(2), 2).unwrap();
    assert_eq!(
        (sequences(&second), second.next_after),
        (vec![3, 4], Some(LibraryChangeSeq(4)))
    );
    let last = page(&service.connection, Some(4), 2).unwrap();
    assert_eq!((sequences(&last), last.next_after), (vec![5], None));
    assert!(
        page(&service.connection, Some(u64::MAX), 2)
            .unwrap()
            .changes
            .is_empty()
    );
    assert_eq!(latest(&service.connection).unwrap(), LibraryChangeSeq(5));
    assert_eq!(
        inspect(&service.connection, 6).unwrap_err().kind,
        ErrorKind::Validation
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
}

/// One change of `items`, by the actor `agent`.
fn set(
    service: &mut EditorService,
    request_id: &str,
    items: Vec<(LibraryItem, Option<Value>)>,
) -> Result<Outcome, Error> {
    let changes = items
        .into_iter()
        .map(|(item, value)| (item, Desired::Value(value)))
        .collect();
    service.library_write(|tx| {
        apply(
            tx,
            request("test.set", "agent", request_id),
            changes,
            |rows| format!("Set {}", rows.len()),
        )
    })
}

fn values(service: &EditorService, items: &[LibraryItem]) -> Vec<Option<Value>> {
    items
        .iter()
        .map(|item| items::read(&service.connection, item).unwrap())
        .collect()
}

/// Every kind of item a method sets is written by the change and restored exactly by its undo,
/// and set again by the redo: the journal reverts any change the same way.
#[test]
fn journal_undo_and_redo_restore_every_item_kind_exactly() {
    let (path, mut service) = catalog("kinds");
    let dir = temp_dir("journal-kinds");
    let original = dir.join("original.jpg");
    std::fs::copy(jpeg(), &original).unwrap();
    let asset = service.import(&original).unwrap().asset.id;
    let moved = dir.join("moved.jpg");
    std::fs::copy(&original, &moved).unwrap();
    let moved = moved.canonicalize().unwrap();
    // The value a relocation to the copy writes, read back, and the original restored.
    service.relocate(&asset, &moved).unwrap();
    let moved_source = crate::editor::library_rows::asset_source(&service.connection, &asset)
        .unwrap()
        .unwrap();
    service.relocate(&asset, &original).unwrap();
    let home = rows_folder(&service, &asset);

    let folder = CatalogFolderId::new();
    let collection = CollectionId::new();
    let items = [
        LibraryItem::CatalogFolder {
            folder_id: folder.clone(),
        },
        LibraryItem::AssetFolder {
            asset_id: asset.clone(),
        },
        LibraryItem::AssetRemoval {
            asset_id: asset.clone(),
        },
        LibraryItem::AssetSource {
            asset_id: asset.clone(),
        },
        LibraryItem::Collection {
            collection_id: collection.clone(),
        },
        LibraryItem::Membership {
            collection_id: collection.clone(),
            asset_id: asset.clone(),
        },
        LibraryItem::IndexedFolder { path: dir.clone() },
    ];
    let before = values(&service, &items);
    let after = vec![
        Some(
            to_value(FolderValue {
                id: folder.clone(),
                name: "Konstanz · Sep 2026".into(),
                parent_id: Some(home.clone()),
                created_ms: 5,
                event: None,
            })
            .unwrap(),
        ),
        Some(to_value(AssetFolderValue { folder_id: folder }).unwrap()),
        Some(to_value(AssetRemovalValue { removed_ms: 9 }).unwrap()),
        Some(to_value(&moved_source).unwrap()),
        Some(
            to_value(CollectionValue::from(Collection {
                id: collection,
                name: "Portfolio".into(),
                parent_id: None,
                kind: CollectionKind::Collection,
                query: None,
                created_ms: 6,
                count: None,
            }))
            .unwrap(),
        ),
        Some(to_value(MembershipValue { added_ms: 7 }).unwrap()),
        Some(
            to_value(IndexedFolder {
                path: dir.clone(),
                volume_id: volume().id,
                added_ms: 8,
                actor: "agent".into(),
            })
            .unwrap(),
        ),
    ];
    assert_eq!(before[0], None);
    assert_eq!(before[2], None, "not removed");
    let change = set(
        &mut service,
        "r1",
        items.iter().cloned().zip(after.clone()).collect(),
    )
    .unwrap();
    assert_eq!(values(&service, &items), after);
    assert_eq!(change.sources().collect::<Vec<_>>(), [&asset]);
    assert_eq!(
        service.state(&asset).unwrap().asset.locator,
        moved,
        "the cached head follows the moved original"
    );
    undo_as(&mut service, "agent", "u1").unwrap();
    assert_eq!(values(&service, &items), before);
    assert_eq!(
        service.state(&asset).unwrap().asset.locator,
        original.canonicalize().unwrap()
    );
    redo_as(&mut service, "agent", "d1").unwrap();
    assert_eq!(values(&service, &items), after);

    // A folder that holds a photograph cannot be removed: the journal says which item refused.
    let error = set(&mut service, "r2", vec![(items[0].clone(), None)]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict, "{error:?}");
    assert!(
        error.detail.starts_with("catalog-folder folder-"),
        "{}",
        error.detail
    );
    drop(service);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

fn rows_folder(service: &EditorService, asset: &crate::AssetId) -> CatalogFolderId {
    crate::editor::library_rows::asset_folder(&service.connection, asset)
        .unwrap()
        .unwrap()
}
