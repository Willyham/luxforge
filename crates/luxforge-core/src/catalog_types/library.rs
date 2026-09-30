//! The library: picks, catalog folders, collections, the journal of library changes, developing
//! picks, availability and resolving missing originals, removal and batch jobs.
//!
//! Every pick and clear, folder and collection change, Develop, relink, removal and indexed-folder
//! change is one **library change**: a numbered journal row naming the actor, request, method and a
//! label, with each item's value before and after (`library_changes`, `library_change_rows`), at
//! most [`MAX_LIBRARY_BATCH`] items, never split silently. Library changes sit beside each
//! photograph's history, never inside it.
use super::{
    CatalogFolderId, CollectionId, EventId, FileId, FileSignature, LibraryChangeSeq, ViewQuery,
    VolumeId,
};
use crate::{AssetId, Error, JobId, MutationOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// The most items one library change covers: provisionally 50,000, to be set by measuring the owner
/// transaction. A larger request is refused with `resource-limit`.
pub const MAX_LIBRARY_BATCH: usize = 50_000;
/// The longest catalog folder or collection name, in characters, after trimming.
pub const MAX_LIBRARY_NAME: usize = 128;
/// The most changes one `library.journal` page answers.
pub const MAX_JOURNAL_PAGE: usize = 500;

/// A picked file (`picks`): kept by path and signature with who picked it and when, until it is
/// developed or cleared, so a culling session survives a crash or a restart. A picked file whose
/// signature changes keeps its pick and loses its cached previews.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pick {
    pub path: PathBuf,
    pub signature: FileSignature,
    pub volume_id: VolumeId,
    pub actor: String,
    pub request_id: String,
    pub picked_ms: i64,
    /// The file's row in the index when it has one, for a client that holds a view; answers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<FileId>,
}

/// What a library method acts on: files by path or index row, photographs by identity, or the
/// items selected in the calling session's view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Targets {
    Paths {
        paths: Vec<PathBuf>,
    },
    Files {
        file_ids: Vec<FileId>,
    },
    Assets {
        asset_ids: Vec<AssetId>,
    },
    /// The session's selection in its current view; refused when the view is stale.
    Selection,
}

impl Targets {
    /// How many items the request names, or none for the selection, whose size the owner knows.
    pub fn len(&self) -> Option<usize> {
        match self {
            Self::Paths { paths } => Some(paths.len()),
            Self::Files { file_ids } => Some(file_ids.len()),
            Self::Assets { asset_ids } => Some(asset_ids.len()),
            Self::Selection => None,
        }
    }

    /// Whether the request names nothing (never the selection).
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Refuse an empty list and one over [`MAX_LIBRARY_BATCH`].
    pub fn check(&self) -> Result<(), Error> {
        match self.len() {
            Some(0) => Err(Error::validation("targets name nothing")),
            Some(count) if count > MAX_LIBRARY_BATCH => Err(Error::resource_limit(format!(
                "{count} targets exceed the {MAX_LIBRARY_BATCH} a library change covers"
            ))),
            _ => Ok(()),
        }
    }
}

/// One library change as the journal lists it (`library_changes`). `undone_by` is not stored: it is
/// the change whose `undoes` names this one, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryChange {
    pub sequence: LibraryChangeSeq,
    pub actor: String,
    pub request_id: String,
    pub method: String,
    /// What a person reads: "Picked L1003206.DNG", "Added 5 to Portfolio › Landscapes",
    /// "Developed 18".
    pub label: String,
    pub time_ms: i64,
    pub item_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<LibraryChangeSeq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redoes: Option<LibraryChangeSeq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_by: Option<LibraryChangeSeq>,
}

/// What one row of a library change touched (`library_change_rows.item_kind` and `item_key`), and
/// what its `before` and `after` values hold, `null` meaning absent:
///
/// | Kind | Key | Value |
/// | --- | --- | --- |
/// | `pick` | the file's path | the [`Pick`] |
/// | `asset-folder` | the asset | `{folder_id}` |
/// | `asset-removal` | the asset | `{removed_ms}`, `null` while not removed |
/// | `asset-source` | the asset | `{locator, source_folder, volume_id, file_identity}` |
/// | `catalog-folder` | the folder | the [`CatalogFolder`] without its counts |
/// | `collection` | the collection | the [`Collection`] without its count |
/// | `membership` | `collection-…/asset-…` | `{added_ms}` |
/// | `indexed-folder` | the folder's path | the [`IndexedFolder`](super::IndexedFolder) |
/// | `developed-asset` | the asset | `{path}` of the file it was developed from; a Develop's undo sends it back |
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LibraryItem {
    Pick {
        path: PathBuf,
    },
    AssetFolder {
        asset_id: AssetId,
    },
    AssetRemoval {
        asset_id: AssetId,
    },
    AssetSource {
        asset_id: AssetId,
    },
    CatalogFolder {
        folder_id: CatalogFolderId,
    },
    Collection {
        collection_id: CollectionId,
    },
    Membership {
        collection_id: CollectionId,
        asset_id: AssetId,
    },
    IndexedFolder {
        path: PathBuf,
    },
    DevelopedAsset {
        asset_id: AssetId,
    },
}

impl LibraryItem {
    /// Every `item_kind` the journal stores, in the order the table documents them.
    pub const KINDS: [&'static str; 9] = [
        "pick",
        "asset-folder",
        "asset-removal",
        "asset-source",
        "catalog-folder",
        "collection",
        "membership",
        "indexed-folder",
        "developed-asset",
    ];

    /// The row's `item_kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pick { .. } => "pick",
            Self::AssetFolder { .. } => "asset-folder",
            Self::AssetRemoval { .. } => "asset-removal",
            Self::AssetSource { .. } => "asset-source",
            Self::CatalogFolder { .. } => "catalog-folder",
            Self::Collection { .. } => "collection",
            Self::Membership { .. } => "membership",
            Self::IndexedFolder { .. } => "indexed-folder",
            Self::DevelopedAsset { .. } => "developed-asset",
        }
    }

    /// The row's `item_key`: what an intervening change to the same item is found by.
    pub fn key(&self) -> String {
        match self {
            Self::Pick { path } | Self::IndexedFolder { path } => path.to_string_lossy().into(),
            Self::AssetFolder { asset_id }
            | Self::AssetRemoval { asset_id }
            | Self::AssetSource { asset_id }
            | Self::DevelopedAsset { asset_id } => asset_id.to_string(),
            Self::CatalogFolder { folder_id } => folder_id.to_string(),
            Self::Collection { collection_id } => collection_id.to_string(),
            Self::Membership {
                collection_id,
                asset_id,
            } => format!("{collection_id}/{asset_id}"),
        }
    }

    /// The item a stored row names, refused when the kind or key is not one this build writes.
    pub fn from_row(kind: &str, key: &str) -> Result<Self, Error> {
        let path = || PathBuf::from(key);
        Ok(match kind {
            "pick" => Self::Pick { path: path() },
            "indexed-folder" => Self::IndexedFolder { path: path() },
            "asset-folder" => Self::AssetFolder {
                asset_id: AssetId::parse(key)?,
            },
            "asset-removal" => Self::AssetRemoval {
                asset_id: AssetId::parse(key)?,
            },
            "asset-source" => Self::AssetSource {
                asset_id: AssetId::parse(key)?,
            },
            "developed-asset" => Self::DevelopedAsset {
                asset_id: AssetId::parse(key)?,
            },
            "catalog-folder" => Self::CatalogFolder {
                folder_id: CatalogFolderId::parse(key)?,
            },
            "collection" => Self::Collection {
                collection_id: CollectionId::parse(key)?,
            },
            "membership" => {
                let (collection, asset) = key
                    .split_once('/')
                    .ok_or_else(|| Error::incompatible("a membership key names no asset"))?;
                Self::Membership {
                    collection_id: CollectionId::parse(collection)?,
                    asset_id: AssetId::parse(asset)?,
                }
            }
            other => {
                return Err(Error::incompatible(format!(
                    "library item kind {other} is not supported"
                )));
            }
        })
    }
}

/// One item of a library change with its value before and after (`null` for absent), exactly as
/// undo restores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryChangeRow {
    pub item: LibraryItem,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// What every library mutation answers: whether it changed anything, the change it recorded, how
/// many items it touched, and whether this is a retry's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryAnswer {
    pub outcome: MutationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<LibraryChangeSeq>,
    pub items: u32,
    pub deduplicated: bool,
}

/// What `library.journal` answers: changes after the cursor, oldest first, and the cursor that
/// continues it when the page is full.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryJournal {
    pub changes: Vec<LibraryChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<LibraryChangeSeq>,
}

/// What `library.inspect` answers: one change with every row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryChangeDetail {
    pub change: LibraryChange,
    pub rows: Vec<LibraryChangeRow>,
}

/// Where a developed photograph's original is, as last observed (`assets.availability`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileAvailability {
    /// At its locator, with its recorded signature.
    #[default]
    Available,
    /// Its volume is not mounted.
    Offline,
    /// Its volume is mounted and the file is not at its locator.
    Missing,
    /// A file is at its locator but it is not the recorded one.
    Changed,
}

impl FileAvailability {
    pub const ALL: [Self; 4] = [Self::Available, Self::Offline, Self::Missing, Self::Changed];

    /// The value as the catalog stores it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Offline => "offline",
            Self::Missing => "missing",
            Self::Changed => "changed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// One photograph's availability and when it was checked, as `source.check` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityRow {
    pub asset_id: AssetId,
    pub availability: FileAvailability,
    pub checked_ms: i64,
}

/// The event span a catalog folder was made from (`catalog_folders.event_*`), so a later pick from
/// the same event finds the folder: the event's first and last capture instants and its identity
/// then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSpan {
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
}

/// A catalog folder (`catalog_folders`). Names are unique among siblings, ignoring case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFolder {
    pub id: CatalogFolderId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<CatalogFolderId>,
    pub created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventSpan>,
    /// Photographs directly in it, removed ones excepted; answers only.
    #[serde(default)]
    pub count: u32,
    /// The capture year of its earliest photograph, which the sources panel groups folders by;
    /// answers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
}

/// What `folder.list` answers: every catalog folder, parents before children.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFolders {
    pub folders: Vec<CatalogFolder>,
}

/// What a collection row is (`collections.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CollectionKind {
    /// A set of photographs a person adds to and removes from.
    Collection,
    /// A saved [`ViewQuery`] over photographs, which may not refer to another smart collection.
    Smart,
    /// A group that holds collections, smart collections and groups, and no photographs.
    Group,
}

impl CollectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Smart => "smart",
            Self::Group => "group",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Collection, Self::Smart, Self::Group]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

/// A collection, smart collection or group (`collections`). Names are unique among siblings,
/// ignoring case; only a group has children, and only a plain collection has members.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<CollectionId>,
    pub kind: CollectionKind,
    /// Present exactly for a smart collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<ViewQuery>,
    pub created_ms: i64,
    /// Members of a plain collection; answers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// What `collection.list` answers: every collection and group, parents before children.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collections {
    pub collections: Vec<Collection>,
}

/// The catalog folder an event's picks are developed into: an existing folder, or a new one with
/// this name (and parent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FolderChoice {
    Existing {
        folder_id: CatalogFolderId,
    },
    New {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<CatalogFolderId>,
    },
}

/// Where one event's picks go (`pick.develop`'s `into`). Picks in no event (a single opened file)
/// name no event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopInto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    pub folder: FolderChoice,
}

/// Picks on a removable volume: how many, and how many have a file of the same name and size in an
/// indexed folder (a copied card), which the develop job confirms by fingerprint before using it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovablePicks {
    pub volume_id: VolumeId,
    pub label: String,
    pub count: u32,
    pub with_copy: u32,
}

/// One event's share of a Develop, as `pick.plan` proposes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    /// The event's name, which a new folder takes by default ("Konstanz · Sep 2026").
    pub name: String,
    pub count: u32,
    /// The proposal: the folder an earlier Develop from the same event span made, or a new folder
    /// named after the event.
    pub folder: FolderChoice,
    /// The proposed existing folder's name, for the confirmation to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removable: Vec<RemovablePicks>,
}

/// What `pick.plan` answers: what a Develop of these picks would do, per event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopPlan {
    pub events: Vec<PlannedEvent>,
    pub count: u32,
    /// Picks whose file is offline, which a Develop waits for.
    pub offline: u32,
}

/// How one pick became a photograph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevelopOutcome {
    /// A new photograph.
    Created,
    /// Its bytes are already a photograph's: linked, not duplicated.
    Linked,
    /// A photograph whose original was missing is relinked to it, the fingerprint matching.
    Relinked,
}

/// One developed pick, in a `pick.develop` job's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopedPick {
    pub path: PathBuf,
    /// The file actually used: a verified copy in an indexed folder instead of a card's file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<PathBuf>,
    pub asset_id: AssetId,
    pub outcome: DevelopOutcome,
}

/// One pick a Develop did not bring in, with the refusal's code and words; it stays picked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFailure {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    pub code: String,
    pub message: String,
}

/// A `pick.develop` job's result: what it committed, in batches, and what it did not; a cancelled
/// job keeps what it committed and reports the rest as not done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopReport {
    pub developed: Vec<DevelopedPick>,
    pub failed: Vec<ItemFailure>,
    /// The library changes its batches recorded.
    pub changes: Vec<LibraryChangeSeq>,
}

/// Why a group of photographs is missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MissingReason {
    /// The volume is not connected: connecting it resolves the group.
    VolumeOffline { label: String },
    /// The folder is gone from its volume.
    FolderGone,
    /// The folder is there and these files are not.
    FilesGone,
    /// Files are at the locators but they are not the recorded ones.
    Changed,
}

/// A catalog folder as another answer names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRef {
    pub id: CatalogFolderId,
    pub name: String,
}

/// One group of `source.missing`: the photographs developed from one folder on disk whose
/// originals are not where they were.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissingGroup {
    pub source_folder: PathBuf,
    pub volume_id: VolumeId,
    pub count: u32,
    /// The catalog folders its photographs are in now.
    pub catalog_folders: Vec<FolderRef>,
    pub reason: MissingReason,
}

/// What `source.missing` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissingOriginals {
    pub groups: Vec<MissingGroup>,
    pub count: u32,
}

/// What a search found for one photograph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum FindResult {
    /// The same bytes, verified by fingerprint.
    Found {
        path: PathBuf,
    },
    /// A file of the same name whose bytes differ: left as it was.
    DifferentBytes {
        path: PathBuf,
    },
    /// Several files with the same bytes, to choose between.
    SeveralIdentical {
        paths: Vec<PathBuf>,
    },
    NotFound,
    /// The same bytes, but another photograph already points at that file: reported, never taken.
    Claimed {
        path: PathBuf,
        by: AssetId,
    },
    /// Still checking.
    Checking,
}

/// One photograph's row in a `source.find` job's result. (Not `deny_unknown_fields`, which serde
/// does not support beside `flatten`.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindRow {
    pub asset_id: AssetId,
    pub file_name: String,
    #[serde(flatten)]
    pub result: FindResult,
}

/// A `source.find` job's result, and its progress's partial answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindReport {
    pub rows: Vec<FindRow>,
}

/// One verified pair `source.relink` commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelinkPair {
    pub asset_id: AssetId,
    pub path: PathBuf,
}

/// One photograph a batch job left out, and why; the rest are done exactly as N single calls would.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSkip {
    pub asset_id: AssetId,
    pub code: String,
    pub reason: String,
}

/// A `batch.apply-preset` or `batch.export` job's result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchReport {
    /// The photographs done: each its own history entry, or its own exported file.
    pub done: Vec<AssetId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub written: Vec<PathBuf>,
    pub skipped: Vec<BatchSkip>,
}

/// What a method that starts a job answers at once: the job to read with `job.read` and cancel
/// with `job.cancel`, and whether this is a retry's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStarted {
    pub job_id: JobId,
    pub status: crate::JobStatus,
    #[serde(default)]
    pub deduplicated: bool,
}

/// Counts `catalog.info` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCounts {
    pub photographs: u64,
    pub removed: u64,
    pub unavailable: u64,
    pub folders: u64,
    pub collections: u64,
    pub picks: u64,
    pub indexed_folders: u64,
    pub library_changes: u64,
}

/// A cache directory's size, for `catalog.info`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheSize {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
}

/// What `catalog.info` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogInfo {
    pub path: PathBuf,
    pub catalog_id: String,
    pub format: i64,
    pub index_format: i64,
    pub counts: CatalogCounts,
    /// The index database, and the files it lists.
    pub index: CacheSize,
    pub previews: CacheSize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_library_item_round_trips_through_its_row() {
        let asset = AssetId::new();
        let collection = CollectionId::new();
        let items = [
            LibraryItem::Pick {
                path: "/card/DSC_0001.NEF".into(),
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
            LibraryItem::CatalogFolder {
                folder_id: CatalogFolderId::new(),
            },
            LibraryItem::Collection {
                collection_id: collection.clone(),
            },
            LibraryItem::Membership {
                collection_id: collection,
                asset_id: asset.clone(),
            },
            LibraryItem::IndexedFolder {
                path: "/Users/a/Pictures".into(),
            },
            LibraryItem::DevelopedAsset { asset_id: asset },
        ];
        for (item, kind) in items.iter().zip(LibraryItem::KINDS) {
            assert_eq!(item.kind(), kind);
            assert_eq!(&LibraryItem::from_row(kind, &item.key()).unwrap(), item);
            assert_eq!(serde_json::to_value(item).unwrap()["kind"], kind);
        }
        assert!(LibraryItem::from_row("rating", "x").is_err());
    }

    #[test]
    fn targets_refuse_nothing_and_too_much() {
        assert!(
            Targets::Paths { paths: vec![] }.check().is_err(),
            "an empty list names nothing"
        );
        let many = Targets::Files {
            file_ids: vec![FileId(1); MAX_LIBRARY_BATCH + 1],
        };
        assert_eq!(
            many.check().unwrap_err().kind,
            crate::ErrorKind::ResourceLimit
        );
        Targets::Selection.check().unwrap();
        let parsed: Targets =
            serde_json::from_value(json!({"kind": "files", "file_ids": [3, 4]})).unwrap();
        assert_eq!(parsed.len(), Some(2));
    }

    #[test]
    fn a_find_row_reads_flat() {
        let row = FindRow {
            asset_id: AssetId::new(),
            file_name: "L1003206.DNG".into(),
            result: FindResult::Found {
                path: "/Volumes/Photos SSD/2026/L1003206.DNG".into(),
            },
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["result"], "found");
        assert_eq!(serde_json::from_value::<FindRow>(json).unwrap(), row);
        for state in FileAvailability::ALL {
            assert_eq!(FileAvailability::parse(state.as_str()), Some(state));
        }
    }
}
