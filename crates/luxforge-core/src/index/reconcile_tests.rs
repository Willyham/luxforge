//! Reconciliation by signature: the remount rule, and listings over real scratch folders and a real
//! index as the lane runs them, without its threads, counting what the reconciliation asks of the
//! index and the disk to find moved files — hard links each their own row at a cost linear in the
//! files, and a move or a rename among them carrying its row.
use super::*;
use crate::catalog_types::{FileIdentity, FileRecord, HeaderState};
use crate::index::{
    database::IndexDb,
    exclude::Exclusions,
    lane::INDEX_BATCH,
    read::FileTask,
    walk::{Walk, WalkLimits},
};
use luxforge_testbase::paths::temp_dir;
use std::path::PathBuf;

#[test]
fn a_remounted_cards_device_alone_changing_keeps_its_file() {
    let identity = FileIdentity {
        device: 5,
        inode: 9,
    };
    let stored = FileSignature {
        len: 10,
        modified_ns: 20,
        identity: Some(identity),
    };
    let remounted = FileSignature {
        identity: Some(FileIdentity {
            device: 6,
            ..identity
        }),
        ..stored
    };
    assert!(unchanged(&stored, &stored, true));
    assert!(unchanged(&stored, &remounted, true));
    assert!(!unchanged(&stored, &remounted, false));
    let replaced = FileSignature {
        identity: Some(FileIdentity {
            inode: 10,
            ..identity
        }),
        ..stored
    };
    assert!(!unchanged(&stored, &replaced, true));
    assert!(!unchanged(
        &stored,
        &FileSignature { len: 11, ..stored },
        true
    ));
}

/// A file at a new path with the file identity of a row whose file is gone is that file moved only
/// when the row is its own by birth time: a file system that gives a deleted file's inode to the
/// next file created (Linux's do) makes a new file of another birth a new row, and the deleted
/// file's row vanishes. Where either has no birth time, the length and modification time decide.
#[cfg(unix)]
#[test]
fn a_new_file_given_a_deleted_files_identity_is_not_that_file_moved() {
    let dir = temp_dir("reconcile-reused-identity")
        .canonicalize()
        .unwrap();
    let root = dir.join("photos");
    std::fs::create_dir_all(root.join("new")).unwrap();
    let new = root.join("new/f.jpg");
    std::fs::write(&new, b"image").unwrap();
    let metadata = new.symlink_metadata().unwrap();
    let now = FileSignature::of(&metadata);
    let born = crate::catalog_types::born_ns(&metadata).expect("the file system records births");
    let (mut index, _) = IndexDb::open(&dir.join("catalog.index"), "catalog").unwrap();
    let connection = index.connection_mut();
    let earlier =
        |born_ns: Option<i64>, modified_ns: i64| (FileSignature { modified_ns, ..now }, born_ns);
    // Each case plants the gone file's row, lists, and answers whether the row was carried.
    let mut carried = |(stored, born_ns): (FileSignature, Option<i64>)| {
        let gone = root.join("c.jpg");
        let task = FileTask {
            path: gone.clone(),
            folder: root.clone(),
            name: "c.jpg".into(),
            kind: crate::SourceTag::Jpeg,
            volume_id: VolumeId::parse("volume-0123456789").unwrap(),
            seen_ms: 1,
        };
        let tx = connection.transaction().unwrap();
        database::delete_files(
            &tx,
            &rows(&tx).into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
        )
        .unwrap();
        let id = database::upsert_file(&tx, &task.pending(stored, born_ns)).unwrap();
        tx.commit().unwrap();
        let (tally, _) = list(connection, &root, 2);
        let moved = row_id(connection, &new) == Some(id);
        assert_eq!(
            (tally.moved, tally.new, tally.vanished),
            if moved { (1, 0, 0) } else { (0, 1, 1) },
            "{tally:?}"
        );
        assert_eq!(row_id(connection, &gone), None);
        moved
    };
    assert!(
        carried(earlier(Some(born), now.modified_ns)),
        "its own row, renamed"
    );
    assert!(
        carried(earlier(Some(born), now.modified_ns - 1)),
        "its own row, moved and changed"
    );
    assert!(
        !carried(earlier(Some(born - 1), now.modified_ns)),
        "another file's row, born before it"
    );
    assert!(carried(earlier(None, now.modified_ns)), "no birth to tell");
    assert!(
        !carried(earlier(None, now.modified_ns - 1)),
        "no birth to tell, and changed"
    );
    drop(index);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What one listing decided, by kind, and the rows it dropped as vanished.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    unchanged: usize,
    identity: usize,
    reread: usize,
    moved: usize,
    new: usize,
    vanished: usize,
}

/// List `root` as the lane does, without its threads: walk it, reconcile each folder, write each
/// decision's row as the lane's batch does, committing every [`INDEX_BATCH`] writes, and once the
/// walk is complete drop the rows it never saw. A new or changed file's row is written as if its
/// header had been read, and every row it writes is last seen at `stamp`.
fn list(connection: &mut Connection, root: &Path, stamp: i64) -> (Tally, Looks) {
    let volume = VolumeId::parse("volume-0123456789").unwrap();
    let exclusions = Exclusions::default();
    let mut walk = Walk::new(root, Path::new("/"), &exclusions, WalkLimits::default()).unwrap();
    let mut reconciler = Reconciler::new(volume.clone());
    let mut tally = Tally::default();
    let mut batch: Vec<(Decision, FileRecord)> = Vec::new();
    while let Some(folder) = walk.next_folder(&|| Ok(())).unwrap() {
        let decisions = reconciler.folder(connection, &folder).unwrap();
        for (file, decision) in folder.files.iter().zip(decisions) {
            match decision {
                Decision::Unchanged => tally.unchanged += 1,
                Decision::Identity(_) => tally.identity += 1,
                Decision::Reread(_) => tally.reread += 1,
                Decision::Moved { .. } => tally.moved += 1,
                Decision::New => tally.new += 1,
            }
            if decision == Decision::Unchanged {
                continue;
            }
            let task = FileTask {
                path: folder.path.join(&file.name),
                folder: folder.path.clone(),
                name: file.name.clone(),
                kind: file.kind,
                volume_id: volume.clone(),
                seen_ms: stamp,
            };
            let record = FileRecord {
                header: HeaderState::Unreadable("not a photograph".into()),
                ..task.pending(file.signature, file.born_ns)
            };
            batch.push((decision, record));
            if batch.len() >= INDEX_BATCH {
                commit(connection, &mut batch, &[]);
            }
        }
    }
    let vanished = reconciler.vanished(connection, root, stamp).unwrap();
    tally.vanished = vanished.len();
    commit(connection, &mut batch, &vanished);
    (tally, reconciler.looks())
}

/// Write `batch` and drop the rows `vanished` in one transaction, as the lane's batch commits.
fn commit(
    connection: &mut Connection,
    batch: &mut Vec<(Decision, FileRecord)>,
    vanished: &[FileId],
) {
    let tx = connection.transaction().unwrap();
    for (decision, record) in batch.drain(..) {
        match decision {
            Decision::New | Decision::Reread(_) => {
                database::upsert_file(&tx, &record).unwrap();
            }
            Decision::Moved { id, reread } => {
                database::move_file(&tx, id, &record, reread).unwrap();
            }
            Decision::Identity(id) => {
                database::refresh_identity(&tx, id, record.signature.identity, &record.volume_id)
                    .unwrap();
            }
            Decision::Unchanged => {}
        }
    }
    database::delete_files(&tx, vanished).unwrap();
    tx.commit().unwrap();
}

/// Every row of the index: its path and id, in path order.
fn rows(connection: &Connection) -> Vec<(PathBuf, FileId)> {
    let mut statement = connection
        .prepare("SELECT path, id FROM files ORDER BY path")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((PathBuf::from(row.get::<_, String>(0)?), FileId(row.get(1)?)))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn row_id(connection: &Connection, path: &Path) -> Option<FileId> {
    rows(connection)
        .into_iter()
        .find(|(row, _)| row == path)
        .map(|(_, id)| id)
}

/// Files the hard links of a [`Linked`] tree are links to.
const SOURCES: usize = 5;

/// A scratch directory with [`SOURCES`] files outside `photos/`, hard links to them in turn under
/// `photos/` ([`Linked::link`]), and an index beside them.
struct Linked {
    dir: PathBuf,
    /// `photos/`, canonical.
    root: PathBuf,
    sources: Vec<PathBuf>,
    per_folder: usize,
    index: IndexDb,
}

impl Linked {
    /// `links` hard links in folders of `per_folder`.
    fn new(name: &str, links: usize, per_folder: usize) -> Self {
        let dir = temp_dir(name).canonicalize().unwrap();
        let sources = (0..SOURCES)
            .map(|index| {
                let path = dir.join(format!("sources/source-{index}.jpg"));
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, format!("image {index}")).unwrap();
                path
            })
            .collect();
        let (index, _) = IndexDb::open(&dir.join("catalog.index"), "catalog").unwrap();
        let tree = Self {
            root: dir.join("photos"),
            dir,
            sources,
            per_folder,
            index,
        };
        tree.add(1..=links, |number| (number - 1) % SOURCES);
        tree
    }

    /// The path of link `number` (from 1): `photos/0000/IMG_0001.jpg` on, in folders of its size.
    fn link(&self, number: usize) -> PathBuf {
        self.root
            .join(format!("{:04}", (number - 1) / self.per_folder))
            .join(format!("IMG_{number:04}.jpg"))
    }

    /// Links `numbers`, each to the source `source` names for it.
    fn add(&self, numbers: std::ops::RangeInclusive<usize>, source: impl Fn(usize) -> usize) {
        for number in numbers {
            let path = self.link(number);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::hard_link(&self.sources[source(number)], path).unwrap();
        }
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A first index of 2,000 hard links to five files lists every link as its own new row and moves
/// nothing, with one query a file that finds no row and no `lstat` of a row's path: the rows the
/// listing wrote are its own. Returning looks at nothing. 1,100 more links to one file read each
/// of its 400 earlier rows once, all of them seen by then, and never one of the listing's own,
/// though its first batch commits before the last new folder is reconciled.
///
/// Looking again at every row with a new file's identity, as the reconciliation did before, cost
/// the first index 153,600 rows read and as many `lstat`s, and the 1,100 more links 491,200 rows
/// read and 51,200 `lstat`s.
#[cfg(unix)]
#[test]
fn hard_links_are_each_their_own_row_found_at_a_cost_linear_in_the_files() {
    const LINKS: usize = 2_000;
    const MORE: usize = 1_100;
    let mut tree = Linked::new("reconcile-links", LINKS, 500);
    let paths: Vec<PathBuf> = (1..=LINKS).map(|number| tree.link(number)).collect();
    let root = tree.root.clone();
    let connection = tree.index.connection_mut();

    let (tally, looks) = list(connection, &root, 1);
    assert_eq!(
        tally,
        Tally {
            new: LINKS,
            ..Tally::default()
        }
    );
    assert_eq!(
        looks,
        Looks {
            queries: LINKS,
            rows: 0,
            stats: 0
        }
    );
    let listed: Vec<PathBuf> = rows(connection).into_iter().map(|(path, _)| path).collect();
    assert_eq!(listed, paths, "every link is its own row");

    let (tally, looks) = list(connection, &root, 2);
    assert_eq!(
        tally,
        Tally {
            unchanged: LINKS,
            ..Tally::default()
        }
    );
    assert_eq!(looks, Looks::default(), "nothing new, nothing looked for");

    // Three new folders: the batch commits as the second's rows are written, before the third is
    // reconciled.
    assert!((500..1_000).contains(&INDEX_BATCH));
    tree.add(LINKS + 1..=LINKS + MORE, |_| 0);
    let connection = tree.index.connection_mut();
    let (tally, looks) = list(connection, &root, 3);
    assert_eq!(
        tally,
        Tally {
            unchanged: LINKS,
            new: MORE,
            ..Tally::default()
        }
    );
    let earlier = LINKS / SOURCES;
    assert_eq!(
        looks,
        Looks {
            queries: MORE + earlier / IDENTITY_PAGE,
            rows: earlier,
            stats: 0
        }
    );
    assert_eq!(rows(connection).len(), LINKS + MORE);
}

/// Among hard links, a link renamed in its folder and one moved to another folder each keep their
/// row; a new link is its own row and moves nothing; a link removed has its row dropped; every
/// other row stays where it was. Each row with the three identities is read once at most.
#[cfg(unix)]
#[test]
fn a_move_or_a_rename_among_hard_links_carries_its_row() {
    const LINKS: usize = 400;
    let mut tree = Linked::new("reconcile-link-moves", LINKS, 100);
    let root = tree.root.clone();
    let [renamed, moved, linked, removed] = [101, 202, 3, 304].map(|number| tree.link(number));
    let connection = tree.index.connection_mut();
    list(connection, &root, 1);
    let before = rows(connection);
    let (renamed_id, moved_id, removed_id) = (
        row_id(connection, &renamed).unwrap(),
        row_id(connection, &moved).unwrap(),
        row_id(connection, &removed).unwrap(),
    );

    std::fs::rename(&renamed, root.join("0001/renamed.jpg")).unwrap();
    std::fs::rename(&moved, root.join("0000/moved.jpg")).unwrap();
    std::fs::hard_link(&linked, root.join("0003/new link.jpg")).unwrap();
    std::fs::remove_file(&removed).unwrap();
    let (tally, looks) = list(connection, &root, 2);
    assert_eq!(
        tally,
        Tally {
            unchanged: LINKS - 3,
            moved: 2,
            new: 1,
            vanished: 1,
            ..Tally::default()
        }
    );
    assert_eq!(
        row_id(connection, &root.join("0001/renamed.jpg")),
        Some(renamed_id)
    );
    assert_eq!(
        row_id(connection, &root.join("0000/moved.jpg")),
        Some(moved_id)
    );
    let new_link = row_id(connection, &root.join("0003/new link.jpg")).unwrap();
    assert!(before.iter().all(|(_, id)| *id != new_link), "its own row");
    let after = rows(connection);
    assert!(after.iter().all(|(_, id)| *id != removed_id), "dropped");
    assert_eq!(after.len(), LINKS);
    let changed = [&renamed, &moved, &removed];
    assert!(
        before
            .iter()
            .filter(|(path, _)| !changed.contains(&path))
            .all(|row| after.contains(row))
    );
    let per_source = LINKS / SOURCES;
    assert!(looks.rows <= 3 * per_source, "{looks:?}");
    assert!(
        looks.queries <= 3 * (per_source / IDENTITY_PAGE + 1),
        "{looks:?}"
    );
}
