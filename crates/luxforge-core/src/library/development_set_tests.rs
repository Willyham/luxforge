//! The development set against a real catalog: replacement and retention, members as journal
//! items with item-level inverses, a merge's publication against a captured set, and the session
//! that retires the set at reopening. Every expected value is read back from the catalog's rows.
use super::*;
use crate::{
    EditorService,
    editor::{distinct_jpeg, write},
    library::journal::{self, Outcome, Request},
};
use luxforge_testbase::paths::{jpeg, temp_dir};
use std::path::PathBuf;

/// A catalog of `count` photographs.
struct Catalog {
    directory: PathBuf,
    path: PathBuf,
    service: EditorService,
    photos: Vec<AssetId>,
}

impl Catalog {
    fn new(name: &str, count: usize) -> Self {
        let directory = temp_dir(&format!("development-set-{name}"))
            .canonicalize()
            .unwrap();
        let path = directory.join("catalog.sqlite");
        let mut service = EditorService::open(&path).unwrap();
        let photos = (0..count)
            .map(|at| {
                let file = distinct_jpeg(&jpeg(), &directory.join(format!("IMG_{at:04}.JPG")));
                service.import(&file).unwrap().asset.id
            })
            .collect();
        Self {
            directory,
            path,
            service,
            photos,
        }
    }

    fn rows(&self, photos: &[usize]) -> Vec<AssetRowId> {
        let ids: Vec<AssetId> = photos.iter().map(|&at| self.photos[at].clone()).collect();
        rows_of(&self.service.connection, &ids).unwrap()
    }

    fn replace(&mut self, photos: Option<&[usize]>) -> Option<CurrentSet> {
        let rows = photos.map(|photos| self.rows(photos));
        write(&mut self.service.connection, |tx| {
            replace(tx, rows.as_deref())
        })
        .unwrap()
    }

    /// The current set's members as indices into `photos`, in order.
    fn members(&self) -> Vec<usize> {
        page(&self.service.connection, 0, MAX_SET_MEMBERS)
            .unwrap()
            .members
            .iter()
            .map(|member| {
                self.photos
                    .iter()
                    .position(|photo| *photo == member.asset_id)
                    .unwrap()
            })
            .collect()
    }

    fn change(
        &mut self,
        actor: &str,
        request_id: &str,
        changes: impl FnOnce(&CurrentSet) -> Vec<(LibraryItem, Desired)>,
    ) -> Result<Outcome, Error> {
        let set = current(&self.service.connection).unwrap().unwrap();
        let changes = changes(&set);
        self.service.library_write(|tx| {
            journal::apply(
                tx,
                request("development-set.add", actor, request_id),
                changes,
                |_| "members".into(),
            )
        })
    }

    fn add(&mut self, request_id: &str, photos: &[usize]) -> Outcome {
        let ids: Vec<AssetId> = photos.iter().map(|&at| self.photos[at].clone()).collect();
        self.change("person", request_id, |set| additions(set, &ids).unwrap())
            .unwrap()
    }

    fn remove(&mut self, actor: &str, request_id: &str, photos: &[usize]) -> Outcome {
        let ids: Vec<AssetId> = photos.iter().map(|&at| self.photos[at].clone()).collect();
        self.change(actor, request_id, |set| removals(set, &ids))
            .unwrap()
    }

    fn undo(&mut self, actor: &str, request_id: &str) -> Result<Outcome, Error> {
        self.service
            .library_write(|tx| journal::undo(tx, request("library.undo", actor, request_id)))
    }

    fn redo(&mut self, actor: &str, request_id: &str) -> Result<Outcome, Error> {
        self.service
            .library_write(|tx| journal::redo(tx, request("library.redo", actor, request_id)))
    }

    fn sets(&self) -> i64 {
        self.service
            .connection
            .query_row("SELECT count(*) FROM development_sets", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn kept_members(&self) -> i64 {
        self.service
            .connection
            .query_row("SELECT count(*) FROM development_set_members", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn finish(self) {
        drop(self.service);
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}

fn request<'a>(method: &'a str, actor: &'a str, request_id: &'a str) -> Request<'a> {
    Request {
        method,
        actor,
        request_id,
    }
}

#[test]
fn a_replacement_keeps_its_order_once_each_and_moves_the_revision_on() {
    let mut catalog = Catalog::new("replace", 4);
    assert_eq!(revision(&catalog.service.connection).unwrap(), 0);
    let first = catalog.replace(Some(&[2, 0, 2, 3])).unwrap();
    assert_eq!(catalog.members(), [2, 0, 3]);
    assert_eq!(first.next_position, 3);
    let after_first = revision(&catalog.service.connection).unwrap();
    assert!(after_first > 0);

    let second = catalog.replace(Some(&[1])).unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(catalog.members(), [1]);
    assert!(revision(&catalog.service.connection).unwrap() > after_first);
    // Nothing named the first set's members: it is gone.
    assert_eq!(catalog.sets(), 1);
    assert_eq!(catalog.kept_members(), 1);

    assert!(catalog.replace(None).is_none());
    assert!(current(&catalog.service.connection).unwrap().is_none());
    let empty = page(&catalog.service.connection, 0, 10).unwrap();
    assert_eq!((empty.set_id, empty.count), (None, 0));
    assert_eq!(catalog.sets(), 0);
    catalog.finish();
}

#[test]
fn members_are_undone_item_by_item_and_return_to_their_places() {
    let mut catalog = Catalog::new("members", 5);
    catalog.replace(Some(&[0, 1, 2]));
    assert!(matches!(
        catalog.add("add", &[3, 1, 3]),
        Outcome::Recorded { .. }
    ));
    assert_eq!(
        catalog.members(),
        [0, 1, 2, 3],
        "1 kept its place, 3 joined once"
    );

    catalog.remove("person", "remove", &[1]);
    catalog.add("add-4", &[4]);
    assert_eq!(catalog.members(), [0, 2, 3, 4]);

    // Another actor's later change to another member leaves the undo free.
    catalog.remove("agent", "agent-remove", &[0]);
    catalog.undo("person", "undo-1").unwrap();
    assert_eq!(catalog.members(), [2, 3]);
    catalog.undo("person", "undo-2").unwrap();
    assert_eq!(
        catalog.members(),
        [1, 2, 3],
        "1 is back between 0's place and 2"
    );
    catalog.redo("person", "redo-1").unwrap();
    assert_eq!(catalog.members(), [2, 3]);

    // A later change to the same member refuses the undo by name.
    catalog.add("add-4-again", &[4]);
    assert_eq!(catalog.members(), [2, 3, 4]);
    catalog.remove("agent", "agent-4", &[4]);
    let refused = catalog.undo("person", "undo-3").unwrap_err();
    assert_eq!(refused.kind, crate::ErrorKind::Conflict, "{refused}");
    assert_eq!(catalog.members(), [2, 3]);
    catalog.finish();
}

#[test]
fn a_retired_set_keeps_only_what_the_journal_names_and_an_undo_writes_there() {
    let mut catalog = Catalog::new("retention", 3);
    let first = catalog.replace(Some(&[0, 1])).unwrap();
    catalog.add("add", &[2]);
    catalog.replace(Some(&[0]));
    // The first set is kept for the one member the journal names.
    assert_eq!(catalog.sets(), 2);
    assert_eq!(catalog.kept_members(), 2);
    assert_eq!(
        member(&catalog.service.connection, &first.id, &catalog.photos[2]).unwrap(),
        Some(2)
    );
    // Undoing the addition takes it out of the retired set, and leaves the current one alone.
    catalog.undo("person", "undo").unwrap();
    assert_eq!(
        member(&catalog.service.connection, &first.id, &catalog.photos[2]).unwrap(),
        None
    );
    assert_eq!(catalog.members(), [0]);
    catalog.finish();
}

#[test]
fn a_publication_appends_to_the_captured_set_or_names_the_set_that_replaced_it() {
    let mut catalog = Catalog::new("publication", 3);
    // No set: the result starts one of its own.
    let none = capture(&catalog.service.connection).unwrap();
    let result = catalog.photos[2].clone();
    let changes = write(&mut catalog.service.connection, |tx| {
        membership_for_publication(tx, &none, &result)
    })
    .unwrap();
    assert_eq!(changes.len(), 1);

    let set = catalog.replace(Some(&[0, 1])).unwrap();
    let captured = capture(&catalog.service.connection).unwrap();
    assert_eq!(captured.set_id.as_ref(), Some(&set.id));
    let changes = write(&mut catalog.service.connection, |tx| {
        membership_for_publication(tx, &captured, &result)
    })
    .unwrap();
    assert_eq!(
        changes,
        vec![(
            item(&set.id, &result),
            Desired::UnlessPresent(serde_json::json!({"position": 2}))
        )]
    );

    // Replaced while the merge ran: a named conflict, nothing appended anywhere.
    let replacement = catalog.replace(Some(&[1])).unwrap();
    let error = write(&mut catalog.service.connection, |tx| {
        membership_for_publication(tx, &captured, &result)
    })
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Conflict);
    assert_eq!(error.data.as_ref().unwrap()["set_id"], set.id.as_str());
    assert_eq!(
        error.data.as_ref().unwrap()["current"],
        replacement.id.as_str()
    );
    // Opened while a merge captured none ran: also refused.
    let error = write(&mut catalog.service.connection, |tx| {
        membership_for_publication(tx, &none, &result)
    })
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Conflict);
    assert_eq!(catalog.members(), [1]);
    catalog.finish();
}

#[test]
fn reopening_the_catalog_retires_the_set_and_refuses_a_removed_member() {
    let mut catalog = Catalog::new("session", 2);
    catalog.replace(Some(&[0, 1]));
    let before = revision(&catalog.service.connection).unwrap();
    drop(std::mem::replace(
        &mut catalog.service,
        EditorService::open(&catalog.directory.join("placeholder.sqlite")).unwrap(),
    ));
    catalog.service = EditorService::open(&catalog.path).unwrap();
    assert!(current(&catalog.service.connection).unwrap().is_none());
    assert!(revision(&catalog.service.connection).unwrap() > before);
    assert_eq!(catalog.sets(), 0);

    catalog
        .service
        .connection
        .execute(
            "UPDATE assets SET removed_ms = 1 WHERE id = ?1",
            [catalog.photos[1].as_str()],
        )
        .unwrap();
    let refused = rows_of(&catalog.service.connection, &catalog.photos).unwrap_err();
    assert_eq!(refused.kind, crate::ErrorKind::Validation);
    catalog.finish();
}
