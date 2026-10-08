//! Reconciliation by signature: what a listed folder's files mean for the rows the index holds
//! (`docs/design/catalog.md`, "Keeping up with the disk").
//!
//! - A file whose row has the same length, modification time and file identity is **unchanged**:
//!   its row and header are kept and nothing is read. A card mounted again under another device
//!   number keeps its rows too, when its volume and inode are the same; the row takes the new
//!   device.
//! - A file whose signature changed, or whose header was never read, is **read again** into its row.
//! - A file at a path the index does not know whose file identity is a row's elsewhere, where that
//!   file no longer is, has **moved** or been renamed within its volume: the row is carried to the
//!   new path, keeping its [`FileId`], and read again only if its length or time changed too. A
//!   file system may give a deleted file's identity to the next file created (Linux's do), so the
//!   row must be the file's own by its birth time too ([`same_file`]): equal where both record one,
//!   and otherwise the same length and modification time, so a file that took a deleted file's
//!   identity is new, never that file moved.
//! - Anything else is **new**, a second hard link to a file whose first is still at its path
//!   included: every link has its own row. Every link shares its file's identity, so the rows
//!   with a new file's identity are looked through a page at a time, each once a listing at most
//!   ([`Reconciler::moved_from`]): a tree of links costs a query a file, not a look at every link.
//! - A row under the listed root that the listing never saw has **vanished**, once the listing is
//!   complete ([`Reconciler::vanished`]).
use super::{
    database::{self, KnownFile},
    walk::{ListedFile, ListedFolder},
};
use crate::{
    Error,
    catalog_types::{FileId, FileIdentity, FileSignature, VolumeId},
};
use rusqlite::Connection;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// What one listed file means for the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Its row is current.
    Unchanged,
    /// Its row is current, but its file identity or volume is not the one recorded.
    Identity(FileId),
    /// Its row's header is read again.
    Reread(FileId),
    /// Its row is carried here from where the file was; `reread` when its length or time changed.
    Moved { id: FileId, reread: bool },
    /// It has no row yet.
    New,
}

/// Whether the file whose row records `stored` is unchanged now that it signs `now`, on the same
/// volume (`same_volume`) or not. The rule is [`FileSignature::unchanged`], except that a file on
/// the same volume whose device number alone changed, as a card's does when it is mounted again, is
/// the same file.
pub(crate) fn unchanged(stored: &FileSignature, now: &FileSignature, same_volume: bool) -> bool {
    if stored.len != now.len || stored.modified_ns != now.modified_ns {
        return false;
    }
    match (stored.identity, now.identity) {
        (Some(was), Some(is)) => was == is || (same_volume && was.inode == is.inode),
        _ => true,
    }
}

/// Whether `row`, whose file identity the listed `file` has, is that file's own: their birth times
/// are equal where both record one, which a rename or a move keeps and a new file given a deleted
/// file's identity does not; where either has none, the file is unchanged by its length and
/// modification time too, so a file moved and changed is new there.
fn same_file(row: &KnownFile, file: &ListedFile) -> bool {
    match (row.born_ns, file.born_ns) {
        (Some(was), Some(is)) => was == is,
        _ => {
            row.signature.len == file.signature.len
                && row.signature.modified_ns == file.signature.modified_ns
        }
    }
}

/// The rows with one file identity a query answers at most, as a reconciliation looks through them
/// for a moved file's row: every row a moved file or a lone new file has, in one query.
const IDENTITY_PAGE: usize = 64;

/// One listing's reconciliation, folder by folder, with the rows it has seen. It reads the index
/// through the connection each call is given, which is the one the listing's batches commit on.
///
/// Its memory is the rows it has seen and, for each file identity of a new file whose rows it has
/// looked through, the last row it looked at: both at most one entry for each file the listing
/// takes, which its file limit bounds ([`MAX_INDEX_FILES`](super::walk::MAX_INDEX_FILES)), and
/// dropped with it.
pub(crate) struct Reconciler {
    volume: VolumeId,
    seen: HashSet<FileId>,
    /// The highest row the index held as this reconciliation first looked for a moved file's row:
    /// the rows above it were written since, by the listing (or the unit of watched changes it is
    /// part of) at paths it had just looked at, so none is a moved file's row.
    earlier: Option<FileId>,
    /// For each file identity of a new file whose rows it has looked through, the last row it
    /// looked at: every row with that identity up to it was seen, is still at its path (a hard
    /// link) or was carried to a new path, so none is looked at again.
    looked: HashMap<FileIdentity, FileId>,
    #[cfg(test)]
    looks: Looks,
}

/// What a reconciliation asked the index and the disk to tell moved files from new ones, for a
/// test that counts it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Looks {
    /// Queries of the rows with a file identity.
    pub queries: usize,
    /// The rows those queries answered.
    pub rows: usize,
    /// The rows whose path was looked at on disk (one `lstat` each).
    pub stats: usize,
}

impl Reconciler {
    /// A reconciliation of a root on `volume`.
    pub(crate) fn new(volume: VolumeId) -> Self {
        Self {
            volume,
            seen: HashSet::new(),
            earlier: None,
            looked: HashMap::new(),
            #[cfg(test)]
            looks: Looks::default(),
        }
    }

    /// What it has asked the index and the disk so far to find moved files.
    #[cfg(test)]
    pub(crate) fn looks(&self) -> Looks {
        self.looks
    }

    /// What each of `folder`'s files means, in its order.
    pub(crate) fn folder(
        &mut self,
        connection: &Connection,
        folder: &ListedFolder,
    ) -> Result<Vec<Decision>, Error> {
        let known = database::files_in_folder(connection, &folder.path)?;
        let by_name: HashMap<&str, &KnownFile> =
            known.iter().map(|row| (row.name.as_str(), row)).collect();
        let mut decisions = Vec::with_capacity(folder.files.len());
        for file in &folder.files {
            let decision = match by_name.get(file.name.as_str()) {
                Some(row) => {
                    self.seen.insert(row.id);
                    let same_volume = row.volume_id == self.volume.as_str();
                    if row.pending || !unchanged(&row.signature, &file.signature, same_volume) {
                        Decision::Reread(row.id)
                    } else if row.signature.identity != file.signature.identity || !same_volume {
                        Decision::Identity(row.id)
                    } else {
                        Decision::Unchanged
                    }
                }
                None => match self.moved_from(connection, &folder.path.join(&file.name), file)? {
                    Some(row) => {
                        self.seen.insert(row.id);
                        Decision::Moved {
                            id: row.id,
                            reread: row.pending
                                || row.signature.len != file.signature.len
                                || row.signature.modified_ns != file.signature.modified_ns,
                        }
                    }
                    None => Decision::New,
                },
            };
            decisions.push(decision);
        }
        Ok(decisions)
    }

    /// The row a file new at `path` was moved from: the first, in row order, with its file
    /// identity, which the index held when the listing first looked for a moved file, not seen by
    /// this listing, that is the file's own by its birth time ([`same_file`]) and whose own path no
    /// longer holds that file. A second link to the same file, still at its path, is not a move,
    /// and nor is a new file given a deleted file's identity.
    ///
    /// Each row is looked at once a listing at most: the rows with the identity are read a page at
    /// a time from the last one looked at, and one found still at its path is passed for good. So
    /// a file with no other row costs one query, a moved file one query and one `lstat`, and every
    /// further link of a file the listing met costs one query that finds nothing, where looking
    /// again at every row with the identity cost the listing of `N` links to a file `N²/2` `lstat`s.
    fn moved_from(
        &mut self,
        connection: &Connection,
        path: &Path,
        file: &ListedFile,
    ) -> Result<Option<KnownFile>, Error> {
        let Some(identity) = file.signature.identity else {
            return Ok(None);
        };
        let earlier = match self.earlier {
            Some(earlier) => earlier,
            None => *self.earlier.insert(database::last_file_id(connection)?),
        };
        let from = self.looked.get(&identity).copied();
        let mut after = from.unwrap_or(FileId(i64::MIN));
        let moved = 'pages: loop {
            let page =
                database::files_with_identity(connection, identity, after, earlier, IDENTITY_PAGE)?;
            #[cfg(test)]
            {
                self.looks.queries += 1;
                self.looks.rows += page.len();
            }
            let last = page.len() < IDENTITY_PAGE;
            for row in page {
                after = row.id;
                if row.path == path || self.seen.contains(&row.id) || !same_file(&row, file) {
                    continue;
                }
                #[cfg(test)]
                {
                    self.looks.stats += 1;
                }
                let there = row
                    .path
                    .symlink_metadata()
                    .is_ok_and(|metadata| FileSignature::of(&metadata).identity == Some(identity));
                if !there {
                    break 'pages Some(row);
                }
            }
            if last {
                break None;
            }
        };
        if from != Some(after) && after != FileId(i64::MIN) {
            self.looked.insert(identity, after);
        }
        Ok(moved)
    }

    /// The rows under `root` the listing never saw, among those last seen before it started
    /// (`started_ms`): the files that vanished. Only a complete listing may drop them.
    pub(crate) fn vanished(
        &self,
        connection: &Connection,
        root: &Path,
        started_ms: i64,
    ) -> Result<Vec<FileId>, Error> {
        Ok(
            database::files_under_seen_before(connection, root, started_ms)?
                .into_iter()
                .filter(|id| !self.seen.contains(id))
                .collect(),
        )
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
