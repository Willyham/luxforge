//! Identities: of files on disk, volumes, catalog folders, collections, events, moments and library
//! changes, and the owner's 16-byte view item.
//!
//! A **file** is identified by its path and its [`FileSignature`], never by a fingerprint: the index
//! lane reads no image data, and hashing a thousand RAW files would cost more than culling them. A
//! file's [`FileId`] is its row in the index database, valid while that database exists; every
//! public answer that names a file also names its path, so a client never depends on a row id
//! surviving a rebuilt index. A **photograph** (a developed pick) is an asset, named by its
//! [`AssetId`](crate::AssetId) in every public answer and by its [`AssetRowId`] only inside the
//! owner's view lists.
use crate::model::identifier;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::Metadata, path::Path};

/// A file's row in the index database (`files.id`). Valid while the index exists: deleting or
/// discarding the index renumbers every file, so it is never stored in the catalog, and every public
/// row that carries one also carries the file's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub i64);

/// A developed photograph's integer key: the catalog's `assets.row`, an `INTEGER PRIMARY KEY`, so it
/// is stable for the catalog's life (a vacuum never renumbers it) and costs 8 bytes in a view. Only
/// the owner's view lists hold it; every public answer names the photograph by its
/// [`AssetId`](crate::AssetId).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetRowId(pub i64);

/// The filesystem's own identity of a file: device and inode on Unix, volume serial number and file
/// index on Windows. A rename or move within a volume keeps it, which is how the index carries a
/// pick across a move and how a developed photograph whose file moved within its volume is found
/// again. Stored in SQLite's signed integers by bit pattern ([`Self::to_columns`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    /// The two columns (`device`, `inode`) a table stores it in: each `u64` as the `i64` with the
    /// same bits, so every value round-trips.
    pub fn to_columns(self) -> (i64, i64) {
        (self.device as i64, self.inode as i64)
    }

    /// The identity two stored columns hold, or none when either is null.
    pub fn from_columns(device: Option<i64>, inode: Option<i64>) -> Option<Self> {
        Some(Self {
            device: device? as u64,
            inode: inode? as u64,
        })
    }
}

/// What tells a file apart from its earlier self without reading it: its length, its modification
/// time in nanoseconds since the Unix epoch, and its [`FileIdentity`] where the platform gives one.
///
/// **The equality rule** ([`Self::unchanged`]): two signatures are the same file unchanged when their
/// lengths and modification times are equal and, when both carry an identity, their identities are
/// equal too. A file rewritten in place with the same length and modification time is not detected;
/// that is the price of reading no bytes, and a developed photograph's fingerprint still catches it
/// before anything is rendered. Derived `PartialEq` is exact, field by field, for storage round trips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSignature {
    pub len: u64,
    pub modified_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<FileIdentity>,
}

impl FileSignature {
    /// The signature a file's metadata gives now. On Unix the identity is its device and inode; on
    /// other platforms none, until the index lane reads the platform's own (a Windows file index
    /// needs an open handle).
    pub fn of(metadata: &Metadata) -> Self {
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos().min(i64::MAX as u128) as i64);
        Self {
            len: metadata.len(),
            modified_ns,
            identity: identity_of(metadata),
        }
    }

    /// Whether `other` is this file unchanged, by the rule the type documents.
    pub fn unchanged(&self, other: &Self) -> bool {
        self.len == other.len
            && self.modified_ns == other.modified_ns
            && match (self.identity, other.identity) {
                (Some(this), Some(that)) => this == that,
                _ => true,
            }
    }
}

/// A file's birth (creation) time in nanoseconds since the Unix epoch, where the file system
/// records one and the platform's stat reports it (Linux's through `statx`). A rename or a move
/// within the volume keeps it, and a file given a deleted file's [`FileIdentity`] (Linux's file
/// systems reuse inodes) has its own: what tells a file moved from another that took its identity.
pub fn born_ns(metadata: &Metadata) -> Option<i64> {
    let since = metadata
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(since.as_nanos().min(i64::MAX as u128) as i64)
}

#[cfg(unix)]
fn identity_of(metadata: &Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn identity_of(_: &Metadata) -> Option<FileIdentity> {
    None
}

identifier!(
    /// A volume: the internal disk, an external drive or a camera card, `volume-…`. The index lane
    /// derives it from the platform's volume identifier where one exists (so a card is the same
    /// volume every time it is mounted) and from the device otherwise; it is text so it can be kept
    /// in the catalog beside every pick and photograph that lives on the volume.
    VolumeId,
    "volume-"
);

identifier!(
    /// A catalog folder, `folder-…`: where a developed photograph lives in the catalog, one folder
    /// each. Not a folder on disk.
    CatalogFolderId,
    "folder-"
);

identifier!(
    /// A collection, smart collection or collection group, `collection-…`.
    CollectionId,
    "collection-"
);

identifier!(
    /// An event, `event-…`: computed, not stored, and stable for the same content — the hash of its
    /// first file's path and its start instant ([`Self::of`]) — so a client can hold one across
    /// re-evaluations that do not change the event. Files added before its first file, or an event
    /// that splits or merges, give it a new identity; a catalog folder made from an event keeps its
    /// span and key (`catalog_folders.event_*`) so later picks from it still find the folder.
    EventId,
    "event-"
);

identifier!(
    /// A moment (a burst or a bracket), `moment-…`: computed like an [`EventId`], from its first
    /// frame's path and capture instant. Photographs developed from one moment record it
    /// (`assets.develop_moment`), which is what links a bracket's developed frames.
    MomentId,
    "moment-"
);

/// The first 128 bits of the SHA-256 of `parts`, each length-prefixed so no two lists collide, as
/// 32 lowercase hex digits.
fn content_key(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl EventId {
    /// The identity of the event whose first file is `first` and which starts at `start_ms` (the
    /// sortable instant of [`CaptureTime`](super::CaptureTime); an undated event passes its
    /// folder's path as `first` and 0).
    pub fn of(first: &Path, start_ms: i64) -> Self {
        Self(format!(
            "{}{}",
            Self::PREFIX,
            content_key(&[
                b"event",
                first.as_os_str().as_encoded_bytes(),
                &start_ms.to_le_bytes()
            ])
        ))
    }
}

impl MomentId {
    /// The identity of the moment whose first frame is `first`, captured at `instant_ms`.
    pub fn of(first: &Path, instant_ms: i64) -> Self {
        Self(format!(
            "{}{}",
            Self::PREFIX,
            content_key(&[
                b"moment",
                first.as_os_str().as_encoded_bytes(),
                &instant_ms.to_le_bytes()
            ])
        ))
    }
}

/// One library change's number in the journal (`library_changes.sequence`), from 1, increasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LibraryChangeSeq(pub u64);

/// One entry of a client's view as the owner holds it: a file of the index or a developed
/// photograph, 16 bytes, so a view of 10,000 files is 160 KB and a view of 1,000,000 photographs 16
/// MB. A window, a range selection, the loupe's next frame and the filmstrip's neighbours read it
/// with no query. Never serialized: public rows name a file by `file_id` and `path` and a photograph
/// by `asset_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ViewItem {
    File(FileId),
    Photo(AssetRowId),
}

const _: () = assert!(std::mem::size_of::<ViewItem>() == 16);

/// A file or a photograph as a request names one: `{"file_id": 12}` or `{"asset_id": "asset-…"}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemRef {
    File { file_id: FileId },
    Photo { asset_id: crate::AssetId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_view_item_is_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<ViewItem>(), 16);
        assert_eq!(std::mem::size_of::<Option<ViewItem>>(), 16);
    }

    #[test]
    fn identities_parse_their_own_prefix_only() {
        let folder = CatalogFolderId::new();
        assert!(folder.as_str().starts_with("folder-"));
        assert_eq!(
            serde_json::from_value::<CatalogFolderId>(json!(folder.as_str())).unwrap(),
            folder
        );
        assert!(CollectionId::parse(folder.as_str()).is_err());
        assert!(VolumeId::parse("volume-0123456789abcdef").is_ok());
        assert!(VolumeId::parse("volume-x").is_err(), "too short");
    }

    #[test]
    fn an_event_and_a_moment_are_stable_for_the_same_content() {
        let path = Path::new("/Volumes/NIKON Z 8/DCIM/100NZ8_1/DSC_0001.NEF");
        assert_eq!(EventId::of(path, 1_000), EventId::of(path, 1_000));
        assert_ne!(EventId::of(path, 1_000), EventId::of(path, 1_001));
        assert_ne!(
            EventId::of(path, 1_000).as_str()[6..],
            MomentId::of(path, 1_000).as_str()[7..],
            "an event and a moment never share a key"
        );
        assert!(EventId::is_valid(EventId::of(path, 0).as_str()));
        assert!(MomentId::is_valid(MomentId::of(path, 0).as_str()));
    }

    #[test]
    fn a_signature_compares_identity_only_when_both_know_it() {
        let identity = FileIdentity {
            device: u64::MAX,
            inode: 7,
        };
        let known = FileSignature {
            len: 10,
            modified_ns: 5,
            identity: Some(identity),
        };
        let unknown = FileSignature {
            identity: None,
            ..known
        };
        let moved = FileSignature {
            identity: Some(FileIdentity {
                inode: 8,
                ..identity
            }),
            ..known
        };
        assert!(known.unchanged(&unknown) && unknown.unchanged(&known));
        assert!(!known.unchanged(&moved));
        assert!(!known.unchanged(&FileSignature { len: 11, ..known }));
        let (device, inode) = identity.to_columns();
        assert_eq!(
            FileIdentity::from_columns(Some(device), Some(inode)),
            Some(identity)
        );
        assert_eq!(FileIdentity::from_columns(None, Some(inode)), None);
    }

    #[test]
    fn an_item_reference_reads_either_shape() {
        let asset = crate::AssetId::new();
        assert_eq!(
            serde_json::from_value::<ItemRef>(json!({"file_id": 3})).unwrap(),
            ItemRef::File { file_id: FileId(3) }
        );
        assert_eq!(
            serde_json::from_value::<ItemRef>(json!({"asset_id": asset})).unwrap(),
            ItemRef::Photo { asset_id: asset }
        );
    }
}
