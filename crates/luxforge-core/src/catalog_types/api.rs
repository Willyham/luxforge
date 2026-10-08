//! Every catalog method, declared once: its parameters (the `host_params!` struct its handler
//! parses, so its schema is generated from the same declaration), its answer, its mutation
//! envelope, the job it starts and the error codes it answers with (`CATALOG_METHODS`, the table
//! the tests hold every registered method to).
//!
//! The one method table (`api/methods.rs`) registers each method with the struct declared below,
//! and a test holds every registered catalog method to its declaration here. A mutation carries
//! the `{request_id, actor}` envelope (`MutationRequest`) and its retries are answered by the
//! owner's request table (`retries: Owner`).
//! Every method may also answer `protocol` and `internal`, which are not listed.

use super::{
    CatalogFolderId, CollectionId, CollectionKind, DevelopInto, Dimensions, Facet, Grouping,
    IndexSource, ItemRef, MAX_FILTER_TEXT, MAX_JOURNAL_PAGE, MAX_LIBRARY_NAME, MAX_PICK_PAGE,
    MAX_VIEW_ROWS, MissingGrouping, Month, PixelRect, PositionRange, PreviewItem, PreviewPriority,
    PreviewTier, RelinkPair, SelectionMode, Targets, ViewFilter, ViewQuery, ViewSort, ViewSource,
};
use crate::{AssetId, PresetId, api::params::host_params};
use std::path::PathBuf;

/// What a target list is, as every `targets` parameter's notes say.
const TARGETS: &str = "{kind: paths, paths: [...]}, {kind: files, file_ids: [...]}, {kind: assets, asset_ids: [...]} or {kind: selection}, the caller's selection in its current view; at most 50,000 items";

// Files: the index, cards, volumes and folders on disk.

host_params! {
    /// `index.add-folder`.
    pub(crate) struct IndexAddFolder {
        path: PathBuf = path().notes("a folder on disk, organized into events with its subfolders"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `index.remove-folder`.
    pub(crate) struct IndexRemoveFolder {
        path: PathBuf = path().notes("an indexed folder; forgetting it forgets only its cache"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `index.refresh`.
    pub(crate) struct IndexRefresh {
        source: IndexSource = json("{kind: indexed-folder, path}, {kind: card, volume_id}, {kind: folder, path} or {kind: all-indexed}: what to list again"),
    }
}

host_params! {
    /// `disk.folders`.
    pub(crate) struct DiskFoldersParams {
        path: PathBuf = path().notes("an absolute folder, such as a volume's mount point from volume.list"),
    }
}

// Previews.

host_params! {
    /// `preview.read`.
    pub(crate) struct PreviewRead {
        item: PreviewItem = json("{kind: file, file_id} or {kind: photo, asset_id, entry_id?}: entry_id defaults to the current entry"),
        tier: PreviewTier = enumeration(PreviewTier::ALL.map(PreviewTier::as_str)),
        priority: Option<PreviewPriority> = enumeration(PreviewPriority::ALL.map(PreviewPriority::as_str)).default("visible").notes("the loupe's look-ahead first, then visible cells, then the rest"),
    }
}

host_params! {
    /// `preview.region`.
    pub(crate) struct PreviewRegion {
        item: PreviewItem = json("{kind: file, file_id} or {kind: photo, asset_id, entry_id?}"),
        rect: PixelRect = json("{x, y, width, height} in the image's upright full-resolution pixels, or in frame when it is named"),
        frame: Option<Dimensions> = json("{width, height}: the upright frame rect is in, such as the loupe's preview or the header's dimensions turned upright; rect's centre is mapped into the source's frame and its size kept at 1:1; default the source's own full-resolution frame"),
    }
}

// The library: picks, catalog folders, collections, developing, originals and batches.

host_params! {
    /// `pick.set`.
    pub(crate) struct PickSet {
        targets: Targets = json(TARGETS),
        picked: bool = boolean().notes("true picks, false clears"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `pick.list`.
    pub(crate) struct PickList {
        source: Option<ViewSource> = json("a view source over files: only the picks it holds; default every pick"),
        after: Option<PathBuf> = path().notes("the last path of the previous page"),
        limit: Option<usize> = integer(1, MAX_PICK_PAGE as i64).default(1000),
    }
}

host_params! {
    /// `pick.plan`.
    pub(crate) struct PickPlan {
        targets: Option<Targets> = json("the picks to plan for; default every pick in the caller's view"),
    }
}

host_params! {
    /// `pick.develop`.
    pub(crate) struct PickDevelop {
        into: Vec<DevelopInto> = json("[{event_id?, folder: {kind: existing, folder_id} or {kind: new, name, parent_id?}}]: one per event of the plan"),
        mutation: MutationRequest,
        targets: Option<Targets> = json("the picks to develop; default every pick in the caller's view"),
        use_copies: Option<bool> = boolean().default(false).notes("develop a card's pick from its fingerprint-verified copy in an indexed folder"),
        confirm_removable: Option<bool> = boolean().default(false).notes("develop picks with no copy from removable media, pointing the catalog at the card"),
    }
}

host_params! {
    /// `folder.create`.
    pub(crate) struct FolderCreate {
        name: String = string(MAX_LIBRARY_NAME).notes("unique among its siblings ignoring case"),
        mutation: MutationRequest,
        parent_id: Option<CatalogFolderId> = catalog_folder().notes("default the top level"),
    }
}

host_params! {
    /// `folder.rename`.
    pub(crate) struct FolderRename {
        folder_id: CatalogFolderId = catalog_folder(),
        name: String = string(MAX_LIBRARY_NAME),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `folder.move`.
    pub(crate) struct FolderMove {
        folder_id: CatalogFolderId = catalog_folder(),
        mutation: MutationRequest,
        parent_id: Option<CatalogFolderId> = catalog_folder().notes("default the top level; never the folder itself or one inside it"),
    }
}

host_params! {
    /// `folder.merge`.
    pub(crate) struct FolderMerge {
        folder_id: CatalogFolderId = catalog_folder().notes("the folder merged away"),
        into_id: CatalogFolderId = catalog_folder().notes("the folder that receives its photographs and subfolders"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `folder.delete`.
    pub(crate) struct FolderDelete {
        folder_id: CatalogFolderId = catalog_folder().notes("an empty folder"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `asset.move`.
    pub(crate) struct AssetMove {
        targets: Targets = json(TARGETS),
        folder_id: CatalogFolderId = catalog_folder(),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `asset.send-back`, `asset.remove` and `asset.restore`.
    pub(crate) struct AssetTargets {
        targets: Targets = json(TARGETS),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `collection.create`.
    pub(crate) struct CollectionCreate {
        name: String = string(MAX_LIBRARY_NAME).notes("unique among its siblings ignoring case"),
        kind: CollectionKind = enumeration(["collection", "group"]).notes("a collection holds photographs, a group holds collections"),
        mutation: MutationRequest,
        parent_id: Option<CollectionId> = collection().notes("a group; default the top level"),
    }
}

host_params! {
    /// `collection.create-smart`.
    pub(crate) struct CollectionCreateSmart {
        name: String = string(MAX_LIBRARY_NAME),
        query: ViewQuery = json("a browse query over photographs, never naming a smart collection"),
        mutation: MutationRequest,
        parent_id: Option<CollectionId> = collection().notes("a group; default the top level"),
    }
}

host_params! {
    /// `collection.update-smart`.
    pub(crate) struct CollectionUpdateSmart {
        collection_id: CollectionId = collection(),
        query: ViewQuery = json("a browse query over photographs, never naming a smart collection"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `collection.rename`.
    pub(crate) struct CollectionRename {
        collection_id: CollectionId = collection(),
        name: String = string(MAX_LIBRARY_NAME),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `collection.move`.
    pub(crate) struct CollectionMove {
        collection_id: CollectionId = collection(),
        mutation: MutationRequest,
        parent_id: Option<CollectionId> = collection().notes("a group; default the top level"),
    }
}

host_params! {
    /// `collection.delete`.
    pub(crate) struct CollectionDelete {
        collection_id: CollectionId = collection().notes("a group must be empty; a collection's memberships go with it"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `collection.add` and `collection.remove`.
    pub(crate) struct CollectionMembers {
        collection_id: CollectionId = collection().notes("a plain collection"),
        targets: Targets = json(TARGETS),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `library.journal`.
    pub(crate) struct LibraryJournalParams {
        after: Option<u64> = sequence().notes("the last change the client has read; default from the first"),
        limit: Option<usize> = integer(1, MAX_JOURNAL_PAGE as i64).default(100),
    }
}

host_params! {
    /// `library.inspect`.
    pub(crate) struct LibraryInspect {
        sequence: u64 = sequence(),
    }
}

host_params! {
    /// `library.undo`, `library.redo` and `catalog.empty-removed`.
    pub(crate) struct LibraryRequest {
        mutation: MutationRequest,
    }
}

host_params! {
    /// `source.check`.
    pub(crate) struct SourceCheck {
        targets: Targets = json(TARGETS),
    }
}

host_params! {
    /// `source.missing`.
    pub(crate) struct SourceMissing {
        grouping: Option<MissingGrouping> = enumeration(["source-folder"]).default("source-folder"),
    }
}

host_params! {
    /// `source.find`.
    pub(crate) struct SourceFind {
        search_root: PathBuf = path().notes("the one folder searched, with its subfolders"),
        targets: Option<Targets> = json("the photographs to look for; exactly one of targets and source_folder"),
        source_folder: Option<PathBuf> = path().notes("every missing photograph developed from this folder on disk"),
    }
}

host_params! {
    /// `source.locate`.
    pub(crate) struct SourceLocate {
        asset_id: AssetId = asset(),
        path: PathBuf = path().notes("the chosen file, verified against the photograph's fingerprint before anything changes"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `source.relink`.
    pub(crate) struct SourceRelink {
        pairs: Vec<RelinkPair> = json("[{asset_id, path}]: results a find or a locate verified"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `batch.apply-preset`.
    pub(crate) struct BatchApplyPreset {
        targets: Targets = json(TARGETS),
        preset_id: PresetId = preset(),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `batch.paste-settings`.
    pub(crate) struct BatchPasteSettings {
        targets: Targets = json(TARGETS),
        settings: serde_json::Map<String, serde_json::Value> = settings(),
        source: String = string(128).notes("source photograph display name; not empty"),
        source_asset_id: Option<String> = string(96).notes("source asset provenance only; never looked up"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `batch.export`.
    pub(crate) struct BatchExport {
        targets: Targets = json(TARGETS),
        destination: PathBuf = path().notes("an existing folder; each file is named by the export's naming rule and nothing is replaced"),
        mutation: MutationRequest,
        keep_metadata: Option<bool> = boolean().default(false),
    }
}

// Views: events, browsing and the selection.

host_params! {
    /// `event.list`.
    pub(crate) struct EventListParams {
        month: Option<Month> = string(7).notes("YYYY-MM: only that month's events; default every month"),
        query: Option<String> = string(MAX_FILTER_TEXT).notes("matches places, dates and cameras"),
    }
}

host_params! {
    /// `browse.view`: a [`ViewQuery`] as fields.
    pub(crate) struct BrowseView {
        source: ViewSource = json("{kind: event, event_id}, {kind: folder, path, subfolders?}, {kind: card, volume_id}, {kind: all-photographs}, {kind: recently-developed, days?}, {kind: catalog-folder, folder_id, subfolders?}, {kind: collection, collection_id}, {kind: missing-originals} or {kind: removed}"),
        filter: Option<ViewFilter> = json("{text?, picked?, without_pick?, cameras?, lenses?, kinds?, edited?, dates?: {from, to}, places?}"),
        sort: Option<ViewSort> = json("{key: capture-time | date-developed | file-name | last-edited, descending?}"),
        grouping: Option<Grouping> = enumeration(Grouping::ALL.map(Grouping::as_str)).default("day-camera-moment"),
        thresholds: Option<super::Thresholds> = json("any of the moment and event thresholds, each defaulting to the recorded value"),
    }
}

host_params! {
    /// `browse.rows`.
    pub(crate) struct BrowseRows {
        from: u32 = position(),
        count: u32 = integer(1, MAX_VIEW_ROWS as i64),
        revision: Option<u64> = sequence().notes("refused with conflict unless the view is still at this revision"),
    }
}

host_params! {
    /// `browse.facets`.
    pub(crate) struct BrowseFacets {
        source: ViewSource = json("as browse.view's source"),
        facets: Vec<Facet> = json("[date | place | camera | lens | kind | pick]"),
        filter: Option<ViewFilter> = json("as browse.view's filter"),
    }
}

host_params! {
    /// `browse.select`.
    pub(crate) struct BrowseSelect {
        mode: Option<SelectionMode> = enumeration(SelectionMode::ALL.map(SelectionMode::as_str)).default("replace"),
        items: Option<Vec<ItemRef>> = json("[{file_id} or {asset_id}]"),
        range: Option<PositionRange> = json("{start, len}: positions in the view"),
        all: Option<bool> = boolean().notes("every item in the view"),
        active: Option<u32> = position().notes("the active item's position"),
        revision: Option<u64> = sequence().notes("refused with conflict unless the view is still at this revision"),
    }
}

/// The contract table every registered catalog method is held to by the tests below: each
/// method's parameters, answer, job and errors, declared once.
#[cfg(test)]
mod contract_table {
    use super::super::jobs::{self, CatalogJob};
    use super::*;
    use crate::{
        ErrorKind,
        api::params::{HostParams, NoParams, ParamSchema},
    };
    use ErrorKind::{
        Cancelled, Catalog as CatalogError, Conflict, Decode, FileAccess, Forbidden, Incompatible,
        NotReady, ResourceLimit, SourceUnavailable, UnsupportedColor, UnsupportedInput,
        UnsupportedProfile, Validation,
    };

    /// One catalog method's contract.
    pub(crate) struct MethodContract {
        pub name: &'static str,
        /// Its parameters, generated with the struct its handler parses.
        pub params: &'static ParamSchema,
        /// What it answers: a type of `catalog_types`, or `session` for a method that answers with the
        /// caller's session, as `view.set` does.
        pub answer: &'static str,
        /// The job it starts, whose result `job.read` answers; its immediate answer is then
        /// [`JobStarted`](super::JobStarted).
        pub job: Option<&'static CatalogJob>,
        /// The error codes it answers with, besides `protocol` and `internal`.
        pub errors: &'static [ErrorKind],
        /// Only the desktop or `luxforge-json --permission-authority` may call it.
        pub permission_authority: bool,
        /// What it does, as its `schema.list` notes will say.
        pub notes: &'static str,
    }

    /// `name` with parameters `P`, answering `answer`.
    const fn method<P: HostParams>(
        name: &'static str,
        answer: &'static str,
        errors: &'static [ErrorKind],
        notes: &'static str,
    ) -> MethodContract {
        MethodContract {
            name,
            params: &P::SCHEMA,
            answer,
            job: None,
            errors,
            permission_authority: false,
            notes,
        }
    }

    impl MethodContract {
        const fn starts(self, job: &'static CatalogJob) -> Self {
            Self {
                job: Some(job),
                ..self
            }
        }

        const fn authority(self) -> Self {
            Self {
                permission_authority: true,
                ..self
            }
        }
    }

    /// Every catalog method of the design's API table, in its order.
    pub(crate) const CATALOG_METHODS: &[MethodContract] = &[
        method::<IndexAddFolder>(
            "index.add-folder",
            "IndexFolderAnswer",
            &[Validation, FileAccess, ResourceLimit, Conflict, CatalogError],
            "adds a folder on disk, with its subfolders, to the set organized into events, as one library change, and starts listing it; refused for a file, a package, Luxforge's own directories and a folder inside or around an indexed one",
        ),
        method::<IndexRemoveFolder>(
            "index.remove-folder",
            "LibraryAnswer",
            &[Validation, ResourceLimit, Conflict, CatalogError],
            "forgets an indexed folder as one library change; its files leave events and nothing on disk changes",
        ),
        method::<NoParams>(
            "index.folders",
            "IndexFolders",
            &[ResourceLimit, CatalogError],
            "the indexed folders, whether each is offline, watched or stale, and what its last listing found",
        ),
        method::<NoParams>(
            "card.list",
            "Cards",
            &[ResourceLimit, CatalogError],
            "mounted volumes with a DCIM folder",
        ),
        method::<IndexRefresh>(
            "index.refresh",
            "IndexReport",
            &[Validation, SourceUnavailable, FileAccess, ResourceLimit, Cancelled],
            "lists a source again and reads the headers of new and changed files, reconciling by signature; broad disk roots, home directories and user-parent folders are refused: navigate them with disk.folders and choose a specific subfolder; a job with progress; resource-limit past the file limit",
        )
        .starts(&jobs::INDEX_REFRESH),
        method::<NoParams>(
            "volume.list",
            "Volumes",
            &[ResourceLimit, CatalogError],
            "the mounted volumes, the startup disk first, each with whether it is removable and a card, then the volumes the catalog knows that are not mounted, offline",
        ),
        method::<DiskFoldersParams>(
            "disk.folders",
            "DiskFolders",
            &[Validation, FileAccess, ResourceLimit],
            "a folder's immediate subfolders in name order without what indexing skips (hidden and system folders, packages, other applications' caches, Luxforge's own directories), bounded",
        ),
        method::<EventListParams>(
            "event.list",
            "EventList",
            &[Validation, CatalogError],
            "events over the indexed folders and mounted cards with their names, dates, place, cameras, counts, picks and offline files, newest first, and the months that hold them; query matches places, dates and cameras",
        ),
        method::<BrowseView>(
            "browse.view",
            "ViewSummary",
            &[Validation, ResourceLimit, CatalogError],
            "evaluates a query into the caller's one view, held by the owner, and answers its count, revision and group layout without a row",
        ),
        method::<BrowseRows>(
            "browse.rows",
            "ViewRows",
            &[Validation, Conflict],
            "a window of the caller's view by position",
        ),
        method::<BrowseFacets>(
            "browse.facets",
            "Facets",
            &[Validation, ResourceLimit, CatalogError],
            "counts per date, place, camera, lens, kind and pick; each count is the size of the view that value would give",
        ),
        method::<BrowseSelect>(
            "browse.select",
            "session",
            &[Validation, Conflict],
            "the caller's selection and active item in its view, replaced, added to, removed from or toggled by items, a range or all; session.state reports them as browse.selection",
        ),
        method::<PickSet>(
            "pick.set",
            "LibraryAnswer",
            &[Validation, ResourceLimit, Conflict, CatalogError],
            "picks or clears files as one library change; a pick keeps the file's path and signature until it is developed or cleared",
        ),
        method::<PickList>(
            "pick.list",
            "PickPage",
            &[Validation, CatalogError],
            "the picks in path order, paged",
        ),
        method::<PickPlan>(
            "pick.plan",
            "DevelopPlan",
            &[Validation, CatalogError],
            "what developing these picks would do: the picks by event, each event's proposed catalog folder (the one an earlier Develop from its span made, or a new one named after it) and the picks on removable media with or without a copy of the same name and size",
        ),
        method::<PickDevelop>(
            "pick.develop",
            "DevelopReport",
            &[Validation, SourceUnavailable, ResourceLimit, Conflict, CatalogError, Cancelled],
            "brings the picks into the catalog folders into names, in batches, each a library change: an identical file links, a missing original relinks on a fingerprint match, a card's pick uses its verified copy when use_copies is set and needs confirm_removable otherwise; committed picks are cleared, and a cancelled or failed job keeps what it committed and nothing partial",
        )
        .starts(&jobs::DEVELOP_PICKS),
        method::<NoParams>(
            "folder.list",
            "CatalogFolders",
            &[CatalogError],
            "every catalog folder, parents first, with its count and year",
        ),
        method::<FolderCreate>(
            "folder.create",
            "FolderAnswer",
            &[Validation, Conflict, CatalogError],
            "makes an empty catalog folder",
        ),
        method::<FolderRename>(
            "folder.rename",
            "LibraryAnswer",
            &[Validation, Conflict, CatalogError],
            "renames a catalog folder; nothing on disk changes",
        ),
        method::<FolderMove>(
            "folder.move",
            "LibraryAnswer",
            &[Validation, Conflict, CatalogError],
            "nests a catalog folder in another, or at the top level",
        ),
        method::<FolderMerge>(
            "folder.merge",
            "LibraryAnswer",
            &[Validation, Conflict, ResourceLimit, CatalogError],
            "moves a folder's photographs and subfolders into another and deletes it, as one library change",
        ),
        method::<FolderDelete>(
            "folder.delete",
            "LibraryAnswer",
            &[Validation, Conflict, CatalogError],
            "deletes an empty catalog folder; conflict when it holds photographs or folders",
        ),
        method::<AssetMove>(
            "asset.move",
            "LibraryAnswer",
            &[Validation, ResourceLimit, CatalogError],
            "moves photographs into a catalog folder as one library change",
        ),
        method::<AssetTargets>(
            "asset.send-back",
            "LibraryAnswer",
            &[Validation, Conflict, ResourceLimit, CatalogError],
            "deletes unedited photographs' catalog records and picks their files again; conflict for a photograph with history beyond its Original",
        ),
        method::<NoParams>(
            "collection.list",
            "Collections",
            &[CatalogError],
            "every collection, smart collection and group, parents first",
        ),
        method::<CollectionCreate>(
            "collection.create",
            "CollectionAnswer",
            &[Validation, Conflict, CatalogError],
            "makes a collection or a group",
        ),
        method::<CollectionCreateSmart>(
            "collection.create-smart",
            "CollectionAnswer",
            &[Validation, Conflict, CatalogError],
            "saves a query over photographs as a smart collection; a query naming a smart collection is refused",
        ),
        method::<CollectionUpdateSmart>(
            "collection.update-smart",
            "LibraryAnswer",
            &[Validation, CatalogError],
            "replaces a smart collection's query",
        ),
        method::<CollectionRename>(
            "collection.rename",
            "LibraryAnswer",
            &[Validation, Conflict, CatalogError],
            "renames a collection, smart collection or group",
        ),
        method::<CollectionMove>(
            "collection.move",
            "LibraryAnswer",
            &[Validation, Conflict, CatalogError],
            "moves a collection into a group, or to the top level",
        ),
        method::<CollectionDelete>(
            "collection.delete",
            "LibraryAnswer",
            &[Validation, Conflict, ResourceLimit, CatalogError],
            "deletes a collection with its memberships, or an empty group",
        ),
        method::<CollectionMembers>(
            "collection.add",
            "LibraryAnswer",
            &[Validation, ResourceLimit, CatalogError],
            "adds photographs to a collection as one library change",
        ),
        method::<CollectionMembers>(
            "collection.remove",
            "LibraryAnswer",
            &[Validation, ResourceLimit, CatalogError],
            "removes photographs from a collection as one library change",
        ),
        method::<LibraryJournalParams>(
            "library.journal",
            "LibraryJournal",
            &[Validation, CatalogError],
            "library changes after a cursor, oldest first, without their rows",
        ),
        method::<LibraryInspect>(
            "library.inspect",
            "LibraryChangeDetail",
            &[Validation, CatalogError],
            "one library change with each item's value before and after",
        ),
        method::<LibraryRequest>(
            "library.undo",
            "LibraryAnswer",
            &[Validation, Conflict, ResourceLimit, CatalogError],
            "reverts the calling client's latest change not yet undone by appending its inverse; conflict, naming the items, when a later change touched them",
        ),
        method::<LibraryRequest>(
            "library.redo",
            "LibraryAnswer",
            &[Validation, Conflict, ResourceLimit, CatalogError],
            "reverts the calling client's latest undo under the same rule",
        ),
        method::<AssetTargets>(
            "asset.remove",
            "LibraryAnswer",
            &[Validation, ResourceLimit, CatalogError],
            "moves photographs to Removed with their edits, history and collections kept; their files are untouched",
        ),
        method::<AssetTargets>(
            "asset.restore",
            "LibraryAnswer",
            &[Validation, ResourceLimit, CatalogError],
            "puts removed photographs back",
        ),
        method::<LibraryRequest>(
            "catalog.empty-removed",
            "EmptyRemovedAnswer",
            &[Forbidden, CatalogError],
            "permanently deletes the removed photographs' catalog records, the only destructive catalog operation; forbidden to an edit-authority client",
        )
        .authority(),
        method::<SourceCheck>(
            "source.check",
            "AvailabilityReport",
            &[Validation, ResourceLimit, Cancelled],
            "checks and records where photographs' originals are: available, offline, missing or changed",
        )
        .starts(&jobs::SOURCE_CHECK),
        method::<SourceMissing>(
            "source.missing",
            "MissingOriginals",
            &[Validation, CatalogError],
            "photographs whose original is not where it was, grouped by the folder on disk each was developed from, with reasons and the catalog folders they are in",
        ),
        method::<SourceFind>(
            "source.find",
            "FindReport",
            &[Validation, FileAccess, SourceUnavailable, ResourceLimit, Cancelled],
            "looks for each photograph's file under search_root by name and size, then fingerprint, and reports a result per photograph; changes nothing",
        )
        .starts(&jobs::SOURCE_FIND),
        method::<SourceLocate>(
            "source.locate",
            "LibraryAnswer",
            &[Validation, FileAccess, SourceUnavailable, Conflict, ResourceLimit, Cancelled, CatalogError],
            "verifies one chosen file against a photograph's fingerprint and relinks it as one library change; a mismatch or a file another photograph names changes nothing",
        )
        .starts(&jobs::SOURCE_LOCATE),
        method::<SourceRelink>(
            "source.relink",
            "LibraryAnswer",
            &[Validation, SourceUnavailable, Conflict, ResourceLimit, CatalogError],
            "commits verified pairs in one transaction as one library change, updating each photograph's source folder; a file changed since it was verified is refused",
        ),
        method::<BatchApplyPreset>(
            "batch.apply-preset",
            "BatchReport",
            &[Validation, ResourceLimit, Cancelled],
            "applies a preset to each photograph as its own history entry, reporting every one skipped with its reason",
        )
        .starts(&jobs::BATCH_PRESET),
        method::<BatchPasteSettings>(
            "batch.paste-settings",
            "BatchReport",
            &[Validation, Incompatible, ResourceLimit, Cancelled],
            "pastes inline settings to each photograph as its own history entry, reporting every skipped photograph and setting",
        )
        .starts(&jobs::BATCH_PASTE),
        method::<BatchExport>(
            "batch.export",
            "BatchReport",
            &[Validation, FileAccess, ResourceLimit, Cancelled],
            "exports each photograph's current entry as the single export does, into one folder, reporting every one skipped with its reason",
        )
        .starts(&jobs::BATCH_EXPORT),
        method::<PreviewRead>(
            "preview.read",
            "PreviewAnswer",
            &[
                Validation,
                SourceUnavailable,
                FileAccess,
                UnsupportedInput,
                UnsupportedColor,
                UnsupportedProfile,
                Decode,
                Incompatible,
                ResourceLimit,
                NotReady,
                Cancelled,
            ],
            "a cached preview's path, size, origin, whether it is approximate (never) and, for a photograph's rendered tier, the renderer that drew it, or the job that makes it, shared by every request for the tier and read by the clients that asked, a cancel releasing only the caller's interest and another client's refused conflict while one waits, and the best preview cached meanwhile: a file's grid or loupe tier (job preview-extract; a RAW with no usable preview is developed for a visible or look-ahead request and not-ready for a background one), or a developed photograph's grid or large tier rendered from its current or named entry (job preview-render, one per tier; its camera preview is the fallback until its first render, and an edit's missing module is incompatible)",
        )
        .starts(&jobs::PREVIEW_EXTRACT),
        method::<PreviewRegion>(
            "preview.region",
            "RegionAnswer",
            &[
                Validation,
                SourceUnavailable,
                FileAccess,
                UnsupportedInput,
                UnsupportedColor,
                UnsupportedProfile,
                Decode,
                ResourceLimit,
                Cancelled,
            ],
            "a 100% region, decoded from the embedded full-size preview for that region alone or from a neutral development, labelled, with the frame its rectangle is in; a client's next region cancels its previous one",
        )
        .starts(&jobs::PREVIEW_REGION),
        method::<NoParams>(
            "catalog.info",
            "CatalogInfo",
            &[CatalogError],
            "the catalog's path, identity and formats, its counts, and its index and previews' sizes",
        ),
    ];

    /// The contract of the catalog method `name`, if it is one.
    pub(crate) fn contract(name: &str) -> Option<&'static MethodContract> {
        CATALOG_METHODS.iter().find(|method| method.name == name)
    }
}

#[cfg(test)]
pub(crate) use contract_table::{CATALOG_METHODS, contract};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ErrorKind::{Cancelled, Forbidden, ResourceLimit, Validation},
        api::params::Envelope,
    };
    use serde_json::{Value, json};
    use std::collections::HashSet;

    /// Every method says what it answers and does.
    #[test]
    fn catalog_contracts_name_an_answer_and_notes_for_every_method() {
        for method in CATALOG_METHODS {
            assert!(!method.answer.is_empty(), "{} names no answer", method.name);
            assert!(!method.notes.is_empty(), "{} has no notes", method.name);
        }
    }

    /// Every method is declared once, and every mutation carries the request envelope.
    #[test]
    fn catalog_contracts_name_each_method_once_with_its_envelope() {
        let names: HashSet<_> = CATALOG_METHODS.iter().map(|method| method.name).collect();
        assert_eq!(names.len(), CATALOG_METHODS.len(), "a name declared twice");
        for method in CATALOG_METHODS {
            assert_ne!(
                method.params.envelope,
                Envelope::Revision,
                "{}: a library change has no revision",
                method.name
            );
            assert!(!method.errors.is_empty(), "{}", method.name);
            if method.permission_authority {
                assert!(method.errors.contains(&Forbidden), "{}", method.name);
            }
            if method.job.is_some() {
                assert!(
                    method.errors.contains(&ResourceLimit) || method.errors.contains(&Cancelled),
                    "{}: a job is bounded or cancellable",
                    method.name
                );
            }
        }
        assert!(contract("pick.set").is_some());
        assert!(contract("catalog.import").is_none());
    }

    /// The design's API table and the contracts name the same methods: each contract's name is in
    /// the table, and each `family.*` row of the table is covered by at least one contract.
    #[test]
    fn catalog_contracts_match_the_designs_api_table() {
        let design = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/design/catalog.md"
        ))
        .unwrap();
        let api =
            &design[design.find("\n## API").unwrap()..design.find("\n## Architecture").unwrap()];
        for method in CATALOG_METHODS {
            assert!(
                api.contains(&format!("`{}", method.name)),
                "{} is not in the design's API table",
                method.name
            );
        }
    }

    /// A registered catalog method lists exactly the parameters and envelope declared
    /// here, so the method table and the contracts cannot drift apart.
    #[test]
    fn a_registered_catalog_method_matches_its_contract() {
        let schemas = crate::api::schemas(&crate::ModuleRegistry::builtin());
        let listed = schemas["methods"].as_object().expect("the methods");
        for method in CATALOG_METHODS {
            let Some(schema) = listed.get(method.name) else {
                continue;
            };
            assert_eq!(
                schema["parameters"],
                json!((method.params.parameters)()),
                "{}: parameters",
                method.name
            );
            assert_eq!(
                schema.get("mutation").cloned().unwrap_or(Value::Null),
                json!(method.params.envelope.name()),
                "{}: envelope",
                method.name
            );
        }
    }

    /// Each declared struct parses what its schema says it takes: its required fields and nothing
    /// undeclared.
    #[test]
    fn catalog_contracts_parse_their_declared_fields() {
        use crate::api::params::parse;
        let envelope = json!({"request_id": "r", "actor": "agent"});
        let folder = crate::catalog_types::CatalogFolderId::new();
        let parsed: FolderMove =
            parse(&json!({"folder_id": folder, "mutation": envelope})).unwrap();
        assert_eq!(parsed.folder_id, folder);
        assert_eq!(parsed.parent_id, None);
        let set: PickSet = parse(&json!({
            "targets": {"kind": "files", "file_ids": [1, 2]},
            "picked": true,
            "mutation": envelope,
        }))
        .unwrap();
        assert_eq!(set.targets.len(), Some(2));
        assert!(parse::<PickSet>(&json!({"picked": true, "mutation": envelope, "x": 1})).is_err());
        let view: BrowseView = parse(&json!({
            "source": {"kind": "all-photographs"},
            "grouping": "day",
            "sort": {"key": "date-developed", "descending": true},
        }))
        .unwrap();
        assert_eq!(view.grouping, Some(Grouping::Day));
        let error = parse::<BrowseView>(&json!({
            "source": {"kind": "all-photographs"},
            "grouping": "week",
        }))
        .map(|_| ())
        .unwrap_err();
        assert_eq!(error.kind, Validation);
        let rows: BrowseRows = parse(&json!({"from": 0, "count": MAX_VIEW_ROWS})).unwrap();
        assert_eq!(rows.count as usize, MAX_VIEW_ROWS);
        assert!(parse::<BrowseRows>(&json!({"from": 0, "count": MAX_VIEW_ROWS + 1})).is_err());
        let events: EventListParams = parse(&json!({"month": "2026-09"})).unwrap();
        assert_eq!(
            events.month.map(|month| month.to_string()).as_deref(),
            Some("2026-09")
        );
        let read: PreviewRead =
            parse(&json!({"item": {"kind": "file", "file_id": 7}, "tier": "loupe"})).unwrap();
        assert_eq!(read.tier, PreviewTier::Loupe);
        let region: PreviewRegion = parse(&json!({
            "item": {"kind": "file", "file_id": 7},
            "rect": {"x": 1, "y": 2, "width": 3, "height": 4},
        }))
        .unwrap();
        assert_eq!(region.frame, None);
        let region: PreviewRegion = parse(&json!({
            "item": {"kind": "photo", "asset_id": crate::AssetId::new()},
            "rect": {"x": 1, "y": 2, "width": 3, "height": 4},
            "frame": {"width": 2560, "height": 1707},
        }))
        .unwrap();
        assert_eq!(
            region.frame,
            Some(Dimensions {
                width: 2560,
                height: 1707
            })
        );
        assert!(
            parse::<PreviewRegion>(&json!({
                "item": {"kind": "file", "file_id": 7},
                "rect": {"x": 1, "y": 2, "width": 3, "height": 4},
                "frame": {"width": 2560},
            }))
            .is_err()
        );
    }
}
