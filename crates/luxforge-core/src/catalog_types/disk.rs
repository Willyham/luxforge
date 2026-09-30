//! What Luxforge knows about the filesystem it browses: volumes, mounted camera cards, the indexed
//! folders a person added, and the roots the index lists.
//!
//! None of it is Luxforge's to keep except the list of indexed folders and the volumes that picks
//! and photographs live on, which the catalog records (`volumes`, `indexed_folders`); the rest is
//! the index's cache.
use super::{FileSignature, HeaderMetadata, VolumeId};
use crate::SourceTag;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A volume as the catalog records it (`volumes`): where it is mounted, what it is called, whether
/// it is removable, the platform's own identifier where one exists, and when it was last seen
/// mounted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub id: VolumeId,
    pub mount_point: PathBuf,
    pub label: String,
    pub removable: bool,
    /// The platform's volume identifier (the volume UUID on macOS), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_id: Option<String>,
    pub last_seen_ms: i64,
}

/// A mounted volume with a `DCIM` folder, as `card.list` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub volume: Volume,
    pub dcim: PathBuf,
    /// Supported files listed, once the card has been listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u32>,
    /// The bodies' labels, once their headers have been read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<String>,
    /// How many events its files fall into, once organized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<u32>,
}

/// What `card.list` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cards {
    pub cards: Vec<Card>,
}

/// A volume as `volume.list` answers it: mounted now, or known to the catalog (a pick or a
/// photograph lives on it) and not mounted, which is offline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeState {
    pub volume: Volume,
    /// Not mounted now; its `last_seen_ms` says when it last was.
    pub offline: bool,
    /// Mounted with a `DCIM` folder: a camera card, listed by `card.list` too.
    pub card: bool,
    /// The startup disk.
    pub startup: bool,
}

/// What `volume.list` answers: the mounted volumes in the platform's order, the startup disk
/// first, then the known volumes that are offline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volumes {
    pub volumes: Vec<VolumeState>,
}

/// One subfolder, as `disk.folders` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskFolder {
    pub name: String,
    pub path: PathBuf,
}

/// What `disk.folders` answers: a folder's immediate subfolders in name order, without what
/// indexing skips, and whether the listing stopped at its bound.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskFolders {
    pub path: PathBuf,
    pub folders: Vec<DiskFolder>,
    #[serde(default)]
    pub truncated: bool,
}

/// A folder on disk the person added for events (`indexed_folders`), with its subfolders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedFolder {
    pub path: PathBuf,
    pub volume_id: VolumeId,
    pub added_ms: i64,
    pub actor: String,
}

/// An indexed folder as `index.folders` answers it: the record, whether its volume is mounted, what
/// its last listing found, and whether the index lane follows its changes as they happen, with why
/// not when it does not. (Not `deny_unknown_fields`, which serde does not support beside
/// `flatten`.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFolderState {
    #[serde(flatten)]
    pub folder: IndexedFolder,
    pub offline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed_ms: Option<i64>,
    /// Whether the platform's change notifications keep the folder's rows current now.
    #[serde(default)]
    pub watching: bool,
    /// Why they do not, when they do not: not watched yet, offline, on a network volume, or a
    /// platform limit reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unwatched: Option<String>,
}

/// What `index.add-folder` answers: the library change, the folder, the `index.refresh` job that
/// lists it, and whether this is a retry's answer (as `change.deduplicated` says too).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFolderAnswer {
    pub change: super::LibraryAnswer,
    pub folder: IndexedFolderState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<crate::JobId>,
    #[serde(default)]
    pub deduplicated: bool,
}

/// What `index.folders` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFolders {
    pub folders: Vec<IndexedFolderState>,
}

/// An `index.refresh` job's result: what the listing found against what the index held.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexReport {
    /// The roots listed.
    pub roots: Vec<PathBuf>,
    /// Supported files found.
    pub files: u32,
    /// New rows, rows whose signature changed and were read again, rows carried across a move by
    /// file identity, rows dropped because their file vanished, and files whose header could not
    /// be read.
    pub added: u32,
    pub changed: u32,
    pub moved: u32,
    pub removed: u32,
    pub unreadable: u32,
    /// Headers read: the new and changed files' (an unchanged file's header is never read again).
    #[serde(default)]
    pub headers_read: u32,
    /// Subfolders that could not be read (no permission) and were skipped.
    #[serde(default)]
    pub unreadable_folders: u32,
    /// Roots not listed because their volume is not mounted; their rows stay, offline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offline: Vec<PathBuf>,
    /// Roots not listed because they are gone from a mounted volume; their rows stay until they
    /// are listed again or removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<PathBuf>,
}

/// What `index.refresh` lists again: one indexed folder, a card, any folder being browsed, or every
/// indexed folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum IndexSource {
    IndexedFolder { path: PathBuf },
    Card { volume_id: VolumeId },
    Folder { path: PathBuf },
    AllIndexed,
}

/// What the index knows of one file's header (`files.header_state`).
#[derive(Clone, Debug, PartialEq)]
pub enum HeaderState {
    /// Read: what the header says.
    Ok(Box<HeaderMetadata>),
    /// Listed, header not read yet.
    Pending,
    /// The header could not be read; the error says why. The file is still listed, and shown as
    /// unreadable.
    Unreadable(String),
}

impl HeaderState {
    /// The state as the index stores it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok(_) => "ok",
            Self::Pending => "pending",
            Self::Unreadable(_) => "unreadable",
        }
    }

    /// The header when it has been read.
    pub fn header(&self) -> Option<&HeaderMetadata> {
        match self {
            Self::Ok(header) => Some(header),
            _ => None,
        }
    }
}

/// One file as the index records it (`files`): where it is, its signature, its kind and what its
/// header says.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRecord {
    pub path: PathBuf,
    /// The directory it is in.
    pub folder: PathBuf,
    /// Its file name.
    pub name: String,
    pub volume_id: VolumeId,
    pub signature: FileSignature,
    pub kind: SourceTag,
    pub header: HeaderState,
    /// When a listing last wrote its row: found it new, changed or moved. An unchanged file's row
    /// is not written again.
    pub last_seen_ms: i64,
}

/// One root the index lists (`roots`): an indexed folder, a card or a folder being browsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRoot {
    pub path: PathBuf,
    pub kind: RootKind,
    pub volume_id: VolumeId,
    /// When its last complete listing finished; none before the first.
    pub listed_ms: Option<i64>,
    /// Supported files that listing found.
    pub file_count: Option<u32>,
    /// Its volume is not mounted: its cached rows and previews stay browsable.
    pub offline: bool,
}

/// Why the index lists a root (`roots.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootKind {
    /// An indexed folder: organized into events, watched for changes.
    Indexed,
    /// A mounted camera card: organized into events, listed again when it mounts.
    Card,
    /// A folder opened from On disk: listed while it is browsed, in no event.
    Browsed,
}

impl RootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Card => "card",
            Self::Browsed => "browsed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Indexed, Self::Card, Self::Browsed]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}
