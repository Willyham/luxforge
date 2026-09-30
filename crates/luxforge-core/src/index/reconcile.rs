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
//!   new path, keeping its [`FileId`], and read again only if its length or time changed too.
//! - Anything else is **new**.
//! - A row under the listed root that the listing never saw has **vanished**, once the listing is
//!   complete ([`Reconciler::vanished`]).
use super::{
    database::{self, KnownFile},
    walk::ListedFolder,
};
use crate::{
    Error,
    catalog_types::{FileId, FileSignature, VolumeId},
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

/// One listing's reconciliation, folder by folder, with the rows it has seen. It reads the index
/// through the connection each call is given, which is the one the listing's batches commit on.
pub(crate) struct Reconciler {
    volume: VolumeId,
    seen: HashSet<FileId>,
}

impl Reconciler {
    /// A reconciliation of a root on `volume`.
    pub(crate) fn new(volume: VolumeId) -> Self {
        Self {
            volume,
            seen: HashSet::new(),
        }
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
                None => match self.moved_from(
                    connection,
                    &folder.path.join(&file.name),
                    &file.signature,
                )? {
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

    /// The row a file new at `path` was moved from: one with its file identity, not seen by this
    /// listing, whose own path no longer holds that file. A second link to the same file, still at
    /// its path, is not a move.
    fn moved_from(
        &self,
        connection: &Connection,
        path: &Path,
        signature: &FileSignature,
    ) -> Result<Option<KnownFile>, Error> {
        let Some(identity) = signature.identity else {
            return Ok(None);
        };
        Ok(database::files_with_identity(connection, identity)?
            .into_iter()
            .find(|row| {
                row.path != path
                    && !self.seen.contains(&row.id)
                    && row.path.symlink_metadata().map_or(true, |metadata| {
                        FileSignature::of(&metadata).identity != Some(identity)
                    })
            }))
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
mod tests {
    use super::*;
    use crate::catalog_types::FileIdentity;

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
}
