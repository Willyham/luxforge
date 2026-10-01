//! The catalog in the Select workspace ([catalog design](../../../../docs/design/catalog.md#the-catalog),
//! [catalog board](../../../../docs/design/catalog/catalog.png)): the Catalog sources'
//! folders by year and collections, the filter bar over the catalog (search, Kind and Edited, the
//! metadata conditions and the view's count), the Metadata browser, Save as smart collection…, and
//! the Info panel over photographs — one photograph's Organize band, or the batch form. **Lane D
//! (views and desktop)** owns it. Like every view model it names no framework type, no widget and no
//! view.
//!
//! The desktop holds no catalog logic. The folders and collections are `folder.list` and
//! `collection.list` as the owner answered them; every chip, column value and sort is a whole
//! [`ViewQuery`] for the owner to evaluate ([`changed`]), and each count is `browse.facets`'s; every
//! organizing gesture is the request an API client writes ([`folder_create_params`] and the rest),
//! one library change each. Apply preset… and Export… start the batch jobs an API client starts
//! ([`batch_preset_params`], [`batch_export_params`]), whose report ([`BatchReport`]) is shown as
//! the owner answered it; Remove from catalog… and Put back are library changes, and Empty
//! Removed… is `catalog.empty-removed`, each behind its confirmation ([`Confirm`]).
use super::select::{
    SelectState, SelectionModel, dates, item_info, month_label, photographs, shown_path, thousands,
};
use luxforge_core::{
    AssetId, MutationRequest, PresetId, SourceTag,
    catalog_types::{
        BatchReport, BodyKey, CatalogFolder, CatalogFolderId, CatalogFolders, Collection,
        CollectionId, CollectionKind, Collections, DateRange, Facet, FacetValue, FileAvailability,
        LocalDay, MAX_FILTER_TEXT, Month, RowItem, ViewFilter, ViewQuery, ViewRow, ViewSource,
    },
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

#[cfg(test)]
pub(crate) mod tests;

/// The most selected photographs whose rows the Info panel reads to show their folders and
/// collections: one `browse.rows` window's worth. A larger selection is organized all the same
/// (every gesture names the selection), and the panel says the chips are not shown.
pub(crate) const MAX_SELECTION_ROWS: u32 = 1000;

/// The most selected photographs whose previews head the batch form.
pub(crate) const BATCH_PREVIEWS: usize = 5;

/// The most rows a report's section lists; the rest are counted ("and 12 more").
pub(crate) const REPORT_ROWS: usize = 200;

/// What the Develop band says under Apply preset… and Export…, as the catalog board does: a batch
/// is not a library change, so `Cmd+Z` in Select does not undo it.
pub(crate) const BATCH_NOTE: &str = "Each photograph gets its own history entry; one that cannot \
    take the change is listed, not skipped silently.";

// -- What the catalog seam holds -------------------------------------------------------------------

/// A year the sources panel groups catalog folders under: a top-level folder's is the capture year
/// of the earliest photograph in it or its subfolders. A folder whose photographs are all undated,
/// and an empty one, have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum YearGroup {
    Year(i32),
    Undated,
    Empty,
}

impl YearGroup {
    pub(crate) fn label(self) -> String {
        match self {
            Self::Year(year) => year.to_string(),
            Self::Undated => "Undated".to_owned(),
            Self::Empty => "Empty".to_owned(),
        }
    }
}

/// A menu of the catalog's, open under what opened it. Only one is open at a time, with Select's
/// own chip and sort menus closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CatalogMenu {
    /// The Catalog heading's `+`.
    Add,
    /// A folder row's menu (a right-click).
    Folder(CatalogFolderId),
    /// A folder's Move to…: where it can be nested.
    FolderMove(CatalogFolderId),
    /// A folder's Merge into…: the folders it can be merged into.
    FolderMerge(CatalogFolderId),
    /// A collection's or group's row menu.
    Collection(CollectionId),
    /// The Edited chip.
    Edited,
    /// The Info panel's Move to… for the selected photographs.
    MovePhotos,
    /// The Info panel's Add to… for the selected photographs.
    AddTo,
    /// A collection chip in the Info panel: remove the selection from it, or add the rest.
    Member(CollectionId),
    /// The Develop band's Apply preset…: the library's presets.
    Presets,
}

/// A confirmation over the centre, before a gesture that removes photographs from view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirm {
    /// Remove from catalog… of the `count` selected photographs.
    Remove { count: u32 },
    /// Empty Removed… of the `count` photographs in Removed.
    Empty { count: u32 },
}

/// A library preset the Apply preset… menu offers: `preset.list`'s row as the menu names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetChoice {
    pub(crate) id: PresetId,
    pub(crate) name: String,
    pub(crate) group: String,
    /// Why this build cannot apply it: an action it holds is unavailable, as Develop's Presets
    /// section says.
    pub(crate) unavailable: Option<String>,
}

/// What a batch of this desktop's does to the photographs it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BatchKind {
    /// `batch.apply-preset` of the library preset named so.
    Preset { name: String },
    /// `batch.export` into the folder.
    Export { folder: PathBuf },
}

/// How a batch ended, as its `job.read` record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BatchEnd {
    Done(BatchReport),
    /// A cancelled batch keeps no report: what it finished is in the histories and the folder.
    Cancelled,
    Failed(String),
}

/// A batch job this desktop started for the selection, from its start to its end: one at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BatchRun {
    pub(crate) kind: BatchKind,
    /// How many photographs it names.
    pub(crate) count: u32,
    pub(crate) job: String,
    /// The file names of the photographs the desktop held rows for when it started, which the
    /// report names them by; any other is named by its identity.
    pub(crate) names: BTreeMap<AssetId, String>,
    /// How far it has got, from the activity board ("3 of 18").
    pub(crate) progress: Option<String>,
    pub(crate) end: Option<BatchEnd>,
}

impl BatchRun {
    /// The photograph's file name, or its identity when the desktop held no row for it.
    pub(crate) fn name(&self, asset: &AssetId) -> String {
        self.names
            .get(asset)
            .cloned()
            .unwrap_or_else(|| asset.as_str().to_owned())
    }

    /// What the status bar says once it has ended: what it did and how many it left out.
    pub(crate) fn sentence(&self, home: Option<&Path>) -> Option<String> {
        Some(match (self.end.as_ref()?, &self.kind) {
            (BatchEnd::Done(report), kind) => {
                let done = u32::try_from(report.done.len()).unwrap_or(u32::MAX);
                let mut text = match kind {
                    BatchKind::Preset { name } if done > 0 => {
                        format!("Applied {name} to {}", photographs(done))
                    }
                    BatchKind::Preset { name } => format!("Applied {name} to no photographs"),
                    BatchKind::Export { folder } if done > 0 => format!(
                        "Exported {} to {}",
                        photographs(done),
                        shown_path(folder, home)
                    ),
                    BatchKind::Export { .. } => "Exported no photographs".to_owned(),
                };
                if !report.skipped.is_empty() {
                    text.push_str(&format!(
                        " \u{b7} {} left out",
                        thousands(report.skipped.len() as u32)
                    ));
                }
                if !report.settings_skipped.is_empty() {
                    text.push_str(&format!(
                        " \u{b7} {} without some settings",
                        thousands(report.settings_skipped.len() as u32)
                    ));
                }
                text
            }
            (BatchEnd::Cancelled, BatchKind::Preset { name }) => {
                format!("Cancelled applying {name}: the photographs it reached keep their entries")
            }
            (BatchEnd::Cancelled, BatchKind::Export { folder }) => format!(
                "Cancelled exporting: the files it wrote stay in {}",
                shown_path(folder, home)
            ),
            (BatchEnd::Failed(reason), BatchKind::Preset { name }) => {
                format!("Applying {name} failed: {reason}")
            }
            (BatchEnd::Failed(reason), BatchKind::Export { .. }) => {
                format!("Exporting failed: {reason}")
            }
        })
    }

    /// What the Develop band says while it runs: what it does and how far it has got.
    pub(crate) fn running(&self) -> String {
        let doing = match &self.kind {
            BatchKind::Preset { name } => format!("Applying {name}"),
            BatchKind::Export { .. } => "Exporting".to_owned(),
        };
        match &self.progress {
            Some(progress) => format!("{doing} \u{b7} {progress}"),
            None => format!("{doing} to {}\u{2026}", photographs(self.count)),
        }
    }
}

/// `catalog.empty-removed`, called again while photographs remain: how many it has deleted so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Emptying {
    pub(crate) deleted: u32,
}

/// What a name being typed makes or renames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamingTarget {
    NewFolder {
        parent: Option<CatalogFolderId>,
    },
    NewCollection {
        kind: CollectionKind,
        parent: Option<CollectionId>,
    },
    RenameFolder(CatalogFolderId),
    RenameCollection(CollectionId),
    /// Save as smart collection…: the view's query under this name.
    SmartCollection,
}

/// A name being typed in place: a new folder's or collection's, a rename, or the smart collection
/// Save as smart collection… makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Naming {
    pub(crate) target: NamingTarget,
    pub(crate) text: String,
}

/// The rows the Info panel read for a selection larger than the rows near the screen: the view
/// revision and selection they were read for, by position.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectionRows {
    pub(crate) revision: u64,
    pub(crate) ranges: Vec<(u32, u32)>,
    pub(crate) rows: BTreeMap<u32, ViewRow>,
}

/// The size of a view's source with no filter, for the filter bar's "9 of 55": the source and the
/// library change it was counted at.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceTotal {
    pub(crate) source: ViewSource,
    pub(crate) sequence: u64,
    pub(crate) count: u32,
}

/// What the catalog seam holds that its model reads: what the owner last answered for the folders
/// and collections, and the desktop's own view state — the Metadata browser, the search text, which
/// years, folders and groups are open, the menu open and a name being typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogState {
    /// `folder.list` as last answered, read with the catalog's counts.
    pub(crate) folders: Option<CatalogFolders>,
    /// `collection.list` as last answered, read with the folders.
    pub(crate) collections: Option<Collections>,
    /// Why the last read failed, until one succeeds.
    pub(crate) lists_error: Option<String>,
    /// The Metadata browser is open over the grid.
    pub(crate) metadata: bool,
    /// The search field's text: the query's `filter.text` once evaluated.
    pub(crate) search: String,
    /// The source the search text belongs to; choosing another starts from its own query's text.
    pub(crate) searched: Option<ViewSource>,
    /// Years whose disclosure the person changed from the default (the newest open).
    pub(crate) years: BTreeMap<YearGroup, bool>,
    /// Catalog folders opened to their subfolders.
    pub(crate) open_folders: BTreeSet<CatalogFolderId>,
    /// Collection groups closed; a group is open by default.
    pub(crate) closed_groups: BTreeSet<CollectionId>,
    pub(crate) menu: Option<CatalogMenu>,
    pub(crate) naming: Option<Naming>,
    /// The view's source counted with no filter.
    pub(crate) total: Option<SourceTotal>,
    /// The selection's rows, read for the Info panel when they are not all near the screen.
    pub(crate) selection_rows: Option<SelectionRows>,
    /// `preset.list` as read for the Apply preset… menu when it opened, or why it could not be.
    pub(crate) presets: Option<Result<Vec<PresetChoice>, String>>,
    /// The batch this desktop started last: running, or ended with its report.
    pub(crate) batch: Option<BatchRun>,
    /// The confirmation shown over the centre.
    pub(crate) confirm: Option<Confirm>,
    /// The last batch's report is open over the centre.
    pub(crate) report: bool,
    /// Empty Removed… is deleting.
    pub(crate) emptying: Option<Emptying>,
}

impl CatalogState {
    /// A menu, a name being typed, a confirmation or a report is open, so Escape closes it.
    pub(crate) fn open(&self) -> bool {
        self.menu.is_some() || self.naming.is_some() || self.confirm.is_some() || self.report
    }

    /// Close the open menu, a confirmation and the report, and drop a name being typed.
    pub(crate) fn close(&mut self) {
        self.menu = None;
        self.naming = None;
        self.confirm = None;
        self.report = false;
    }

    /// The batch running now, if one is.
    pub(crate) fn running(&self) -> Option<&BatchRun> {
        self.batch.as_ref().filter(|batch| batch.end.is_none())
    }
}

// -- Gestures --------------------------------------------------------------------------------------

/// One change the catalog's filter bar or Metadata browser makes to the view's query. Each changes
/// its own part of the filter and nothing else ([`changed`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CatalogChange {
    /// The search text; empty clears it.
    Text(String),
    /// Edited photographs only (`true`), those never edited (`false`), or either.
    Edited(Option<bool>),
    /// The dates, a year or a month; none clears them.
    Dates(Option<DateRange>),
    /// One place, or every place.
    Place(Option<String>),
    /// One camera body, or every camera.
    Camera(Option<BodyKey>),
    /// One lens, or every lens.
    Lens(Option<String>),
}

/// What pressing something of the catalog's does, as plain data: the view sends it back as the
/// catalog's message, and the app answers it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CatalogAction {
    /// View a catalog folder or collection.
    View(ViewSource),
    ToggleYear(YearGroup),
    ToggleFolder(CatalogFolderId),
    ToggleGroup(CollectionId),
    /// Open a menu, or close the one open.
    Menu(Option<CatalogMenu>),
    /// Start typing a name for a new folder or collection, a rename or a smart collection.
    Name(NamingTarget),
    /// The name being typed changed.
    NameText(String),
    /// Return: make, rename or save with the name typed.
    Submit,
    MoveFolder {
        folder: CatalogFolderId,
        parent: Option<CatalogFolderId>,
    },
    MergeFolder {
        folder: CatalogFolderId,
        into: CatalogFolderId,
    },
    DeleteFolder(CatalogFolderId),
    DeleteCollection(CollectionId),
    /// The selected photographs, moved to a folder: `asset.move` of the selection.
    MovePhotos(CatalogFolderId),
    /// The selected photographs, added to or removed from a collection.
    AddPhotos(CollectionId),
    RemovePhotos(CollectionId),
    /// A chip, a column value or the search: one change of the query.
    Change(CatalogChange),
    /// The search field's text as typed.
    Search(String),
    /// Show or hide the Metadata browser.
    Metadata,
    /// `Cmd+F`: the search field takes the focus.
    FocusSearch,
    /// Apply preset…'s choice: `batch.apply-preset` of the selection with the library preset.
    ApplyPreset {
        id: PresetId,
        name: String,
    },
    /// Export…: the native folder dialog, then [`Self::ExportInto`].
    Export,
    /// The folder Export… writes into, or none when the dialog was dismissed: `batch.export` of
    /// the selection.
    ExportInto(Option<PathBuf>),
    /// Remove from catalog… (or `Delete` over a catalog view): its confirmation.
    Remove,
    /// Put back: `asset.restore` of the selection, over Removed.
    Restore,
    /// Empty Removed…: its confirmation.
    Empty,
    /// The confirmation's own button: the removal or the emptying it names.
    Confirmed,
    /// Open the last batch's report, or close it.
    Report(bool),
}

/// A library change the catalog's gestures make, which says how its answer is told in the status
/// bar. Select's own library gestures carry it ([`super::select::LibraryGesture::Catalog`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogGesture {
    CreateFolder,
    RenameFolder,
    MoveFolder,
    MergeFolder,
    DeleteFolder,
    MovePhotos,
    CreateCollection,
    CreateGroup,
    CreateSmart,
    RenameCollection,
    DeleteCollection,
    AddPhotos,
    RemovePhotos,
    /// Remove from catalog…: the selection moved to Removed.
    Remove,
    /// Put back: the selection returned from Removed.
    Restore,
}

impl CatalogGesture {
    /// The method the gesture sends.
    pub(crate) fn method(self) -> &'static str {
        match self {
            Self::CreateFolder => "folder.create",
            Self::RenameFolder => "folder.rename",
            Self::MoveFolder => "folder.move",
            Self::MergeFolder => "folder.merge",
            Self::DeleteFolder => "folder.delete",
            Self::MovePhotos => "asset.move",
            Self::CreateCollection | Self::CreateGroup => "collection.create",
            Self::CreateSmart => "collection.create-smart",
            Self::RenameCollection => "collection.rename",
            Self::DeleteCollection => "collection.delete",
            Self::AddPhotos => "collection.add",
            Self::RemovePhotos => "collection.remove",
            Self::Remove => "asset.remove",
            Self::Restore => "asset.restore",
        }
    }

    /// Whether the answer is a `FolderAnswer` or `CollectionAnswer`, whose library change is its
    /// `change`, rather than the change itself.
    pub(crate) fn creates(self) -> bool {
        matches!(
            self,
            Self::CreateFolder | Self::CreateCollection | Self::CreateGroup | Self::CreateSmart
        )
    }

    /// Whether the request names the selection, which a stale view refuses.
    pub(crate) fn of_selection(self) -> bool {
        matches!(
            self,
            Self::MovePhotos | Self::AddPhotos | Self::RemovePhotos | Self::Remove | Self::Restore
        )
    }

    /// What the status bar says when the owner changed nothing.
    pub(crate) fn nothing(self) -> &'static str {
        match self {
            Self::MovePhotos => "Already in that folder",
            Self::AddPhotos => "Already in that collection",
            Self::RemovePhotos => "Not in that collection",
            Self::Remove => "Already in Removed",
            Self::Restore => "Not in Removed",
            Self::RenameFolder | Self::RenameCollection => "The name is unchanged",
            Self::MoveFolder => "Already there",
            _ => "Nothing changed",
        }
    }

    /// What the status bar says of a recorded change whose label could not be read.
    pub(crate) fn done(self) -> &'static str {
        match self {
            Self::CreateFolder => "Created the folder \u{b7} Undo \u{2318}Z",
            Self::RenameFolder => "Renamed the folder \u{b7} Undo \u{2318}Z",
            Self::MoveFolder => "Moved the folder \u{b7} Undo \u{2318}Z",
            Self::MergeFolder => "Merged the folder \u{b7} Undo \u{2318}Z",
            Self::DeleteFolder => "Deleted the folder \u{b7} Undo \u{2318}Z",
            Self::MovePhotos => "Moved the photographs \u{b7} Undo \u{2318}Z",
            Self::CreateCollection => "Created the collection \u{b7} Undo \u{2318}Z",
            Self::CreateGroup => "Created the group \u{b7} Undo \u{2318}Z",
            Self::CreateSmart => "Saved the smart collection \u{b7} Undo \u{2318}Z",
            Self::RenameCollection => "Renamed the collection \u{b7} Undo \u{2318}Z",
            Self::DeleteCollection => "Deleted the collection \u{b7} Undo \u{2318}Z",
            Self::AddPhotos => "Added to the collection \u{b7} Undo \u{2318}Z",
            Self::RemovePhotos => "Removed from the collection \u{b7} Undo \u{2318}Z",
            Self::Remove => "Removed from the catalog \u{b7} Undo \u{2318}Z",
            Self::Restore => "Put back \u{b7} Undo \u{2318}Z",
        }
    }

    /// What a refusal is prefixed with.
    pub(crate) fn refused(self) -> &'static str {
        match self {
            Self::CreateFolder => "Could not create the folder",
            Self::RenameFolder => "Could not rename the folder",
            Self::MoveFolder => "Could not move the folder",
            Self::MergeFolder => "Could not merge the folder",
            Self::DeleteFolder => "Could not delete the folder",
            Self::MovePhotos => "Could not move the photographs",
            Self::CreateCollection | Self::CreateGroup => "Could not create the collection",
            Self::CreateSmart => "Could not save the smart collection",
            Self::RenameCollection => "Could not rename the collection",
            Self::DeleteCollection => "Could not delete the collection",
            Self::AddPhotos => "Could not add to the collection",
            Self::RemovePhotos => "Could not remove from the collection",
            Self::Remove => "Could not remove from the catalog",
            Self::Restore => "Could not put back",
        }
    }
}

// -- Requests --------------------------------------------------------------------------------------

/// The selection in the caller's view, as every batch gesture names its photographs.
fn selection() -> Value {
    json!({"kind": "selection"})
}

/// `folder.create`'s parameters: a folder named `name`, at the top level or in `parent`.
pub(crate) fn folder_create_params(
    name: &str,
    parent: Option<&CatalogFolderId>,
    mutation: &MutationRequest,
) -> Value {
    let mut params = json!({"name": name, "mutation": mutation});
    if let Some(parent) = parent {
        params["parent_id"] = json!(parent);
    }
    params
}

/// `folder.rename`'s parameters.
pub(crate) fn folder_rename_params(
    folder: &CatalogFolderId,
    name: &str,
    mutation: &MutationRequest,
) -> Value {
    json!({"folder_id": folder, "name": name, "mutation": mutation})
}

/// `folder.move`'s parameters: nested in `parent`, or to the top level without one.
pub(crate) fn folder_move_params(
    folder: &CatalogFolderId,
    parent: Option<&CatalogFolderId>,
    mutation: &MutationRequest,
) -> Value {
    let mut params = json!({"folder_id": folder, "mutation": mutation});
    if let Some(parent) = parent {
        params["parent_id"] = json!(parent);
    }
    params
}

/// `folder.merge`'s parameters: `folder` merged into `into`.
pub(crate) fn folder_merge_params(
    folder: &CatalogFolderId,
    into: &CatalogFolderId,
    mutation: &MutationRequest,
) -> Value {
    json!({"folder_id": folder, "into_id": into, "mutation": mutation})
}

/// `folder.delete`'s parameters.
pub(crate) fn folder_delete_params(folder: &CatalogFolderId, mutation: &MutationRequest) -> Value {
    json!({"folder_id": folder, "mutation": mutation})
}

/// `asset.move`'s parameters: the selection, moved to `folder`.
pub(crate) fn asset_move_params(folder: &CatalogFolderId, mutation: &MutationRequest) -> Value {
    json!({"targets": selection(), "folder_id": folder, "mutation": mutation})
}

/// `collection.create`'s parameters: a plain collection or a group named `name`, at the top level
/// or in the group `parent`.
pub(crate) fn collection_create_params(
    name: &str,
    kind: CollectionKind,
    parent: Option<&CollectionId>,
    mutation: &MutationRequest,
) -> Value {
    let mut params = json!({"name": name, "kind": kind, "mutation": mutation});
    if let Some(parent) = parent {
        params["parent_id"] = json!(parent);
    }
    params
}

/// `collection.create-smart`'s parameters: exactly `query`, the view's query as shown, under
/// `name`.
pub(crate) fn smart_create_params(
    name: &str,
    query: &ViewQuery,
    mutation: &MutationRequest,
) -> Value {
    json!({"name": name, "query": query, "mutation": mutation})
}

/// `collection.rename`'s parameters.
pub(crate) fn collection_rename_params(
    collection: &CollectionId,
    name: &str,
    mutation: &MutationRequest,
) -> Value {
    json!({"collection_id": collection, "name": name, "mutation": mutation})
}

/// `collection.delete`'s parameters.
pub(crate) fn collection_delete_params(
    collection: &CollectionId,
    mutation: &MutationRequest,
) -> Value {
    json!({"collection_id": collection, "mutation": mutation})
}

/// `collection.add`'s and `collection.remove`'s parameters: the selection, in `collection`.
pub(crate) fn members_params(collection: &CollectionId, mutation: &MutationRequest) -> Value {
    json!({"collection_id": collection, "targets": selection(), "mutation": mutation})
}

/// `asset.remove`'s and `asset.restore`'s parameters: the selection.
pub(crate) fn removal_params(mutation: &MutationRequest) -> Value {
    json!({"targets": selection(), "mutation": mutation})
}

/// `batch.apply-preset`'s parameters: the library preset `preset`, applied to the selection.
pub(crate) fn batch_preset_params(preset: &PresetId, mutation: &MutationRequest) -> Value {
    json!({"targets": selection(), "preset_id": preset, "mutation": mutation})
}

/// `batch.export`'s parameters: the selection, exported into the existing folder `destination`
/// with the export's own settings.
pub(crate) fn batch_export_params(destination: &Path, mutation: &MutationRequest) -> Value {
    json!({"targets": selection(), "destination": destination, "mutation": mutation})
}

/// `catalog.empty-removed`'s parameters. Each call is a request of its own: a call that finds
/// photographs `remaining` is followed by another, never a retry of the first.
pub(crate) fn empty_params(mutation: &MutationRequest) -> Value {
    json!({"mutation": mutation})
}

/// The Apply preset… menu's presets, from `preset.list`'s rows in its order (by group, then
/// name). A preset holding an action this build cannot apply is offered with why, never applied.
pub(crate) fn preset_choices(presets: &[luxforge_core::PresetSummary]) -> Vec<PresetChoice> {
    presets
        .iter()
        .map(|preset| PresetChoice {
            id: preset.id.clone(),
            name: preset.name.clone(),
            group: preset.group.clone(),
            unavailable: match preset.unavailable.as_slice() {
                [] => None,
                [one] => Some(format!("Cannot apply: {one} is unavailable")),
                many => Some(format!("Cannot apply: {} are unavailable", many.join(", "))),
            },
        })
        .collect()
}

/// The file names of the selected photographs whose rows the desktop holds, by identity: what a
/// batch's report names them by.
pub(crate) fn selected_names(
    state: &SelectState,
    selection: &SelectionModel,
) -> BTreeMap<AssetId, String> {
    if selection.count > MAX_SELECTION_ROWS {
        return BTreeMap::new();
    }
    positions(selection)
        .filter_map(|position| row_at(state, position))
        .filter_map(|row| match &row.item {
            RowItem::Photo { asset_id } => Some((asset_id.clone(), row.file_name.clone())),
            RowItem::File { .. } => None,
        })
        .collect()
}

/// `browse.facets`' parameters over the catalog: the query's source and filter, and the four
/// columns of the Metadata browser with Kind for its chip. Each column's counts are the views its
/// values give with its own condition replaced, so they are counted without it.
pub(crate) fn facets_params(query: &ViewQuery) -> Value {
    let facets = [
        Facet::Date,
        Facet::Place,
        Facet::Camera,
        Facet::Lens,
        Facet::Kind,
    ];
    json!({"source": query.source, "filter": query.filter, "facets": facets})
}

/// `browse.facets`' parameters that count `source` with no filter: the filter bar's "of 55".
pub(crate) fn total_params(source: &ViewSource) -> Value {
    json!({"source": source, "facets": [Facet::Kind]})
}

// -- Queries ---------------------------------------------------------------------------------------

/// A source's query as a catalog folder or collection row views it: newest first and ungrouped, as
/// every catalog source is ([`super::select::source_query`]), and a smart collection in the sort
/// it was saved with.
pub(crate) fn view_query(state: &CatalogState, source: ViewSource) -> ViewQuery {
    let stored = match &source {
        ViewSource::Collection { collection_id } => collection(state, collection_id)
            .and_then(|collection| collection.query.as_ref())
            .map(|query| query.sort),
        _ => None,
    };
    let mut query = super::select::source_query(source);
    if let Some(sort) = stored {
        query.sort = sort;
    }
    query
}

/// A catalog folder's or collection's own name, which the title bar calls its view by.
pub(crate) fn source_name(state: &CatalogState, source: &ViewSource) -> Option<String> {
    match source {
        ViewSource::CatalogFolder { folder_id, .. } => state
            .folders
            .as_ref()?
            .folders
            .iter()
            .find(|folder| &folder.id == folder_id)
            .map(|folder| folder.name.clone()),
        ViewSource::Collection { collection_id } => {
            collection(state, collection_id).map(|collection| collection.name.clone())
        }
        _ => None,
    }
}

/// `query` with one change made to its filter, and nothing else changed.
pub(crate) fn changed(query: &ViewQuery, change: &CatalogChange) -> ViewQuery {
    let mut query = query.clone();
    let filter = &mut query.filter;
    match change {
        CatalogChange::Text(text) => filter.text = search_text(text),
        CatalogChange::Edited(edited) => filter.edited = *edited,
        CatalogChange::Dates(dates) => filter.dates = *dates,
        CatalogChange::Place(place) => filter.places = place.iter().cloned().collect(),
        CatalogChange::Camera(camera) => filter.cameras = camera.iter().cloned().collect(),
        CatalogChange::Lens(lens) => filter.lenses = lens.iter().cloned().collect(),
    }
    query
}

/// The query's search text for what was typed: trimmed, at most [`MAX_FILTER_TEXT`] characters, and
/// none when nothing is left.
pub(crate) fn search_text(typed: &str) -> Option<String> {
    let text: String = typed.trim().chars().take(MAX_FILTER_TEXT).collect();
    (!text.is_empty()).then_some(text)
}

/// Whether a filter narrows the view at all.
fn filtered(filter: &ViewFilter) -> bool {
    filter != &ViewFilter::default()
}

/// The whole of `year`, or of `month` in it.
pub(crate) fn year_range(year: i32) -> Option<DateRange> {
    Some(DateRange {
        from: LocalDay::from_ymd(year, 1, 1)?,
        to: LocalDay::from_ymd(year, 12, 31)?,
    })
}

pub(crate) fn month_range(month: Month) -> Option<DateRange> {
    let from = LocalDay::from_ymd(month.year, month.month, 1)?;
    let next = if month.month == 12 {
        LocalDay::from_ymd(month.year + 1, 1, 1)?
    } else {
        LocalDay::from_ymd(month.year, month.month + 1, 1)?
    };
    Some(DateRange {
        from,
        to: LocalDay(next.0 - 1),
    })
}

/// A facet's day value (`YYYY-MM-DD`) as its month.
fn month_of(value: &str) -> Option<Month> {
    let year = value.get(0..4)?.parse().ok()?;
    let month: u32 = value.get(5..7)?.parse().ok()?;
    (value.as_bytes().get(4) == Some(&b'-') && (1..=12).contains(&month))
        .then_some(Month { year, month })
}

/// A date condition as the filter bar's chip says it: a year, a month, or the range's days.
pub(crate) fn dates_label(range: DateRange) -> String {
    if year_range(range.from.ymd().0) == Some(range) {
        return range.from.ymd().0.to_string();
    }
    if month_range(range.from.month()) == Some(range) {
        return month_label(range.from.month());
    }
    dates(Some(range.from), Some(range.to), true).unwrap_or_default()
}

// -- The model -------------------------------------------------------------------------------------

/// A catalog source row's glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogIcon {
    /// A year that groups folders.
    Year,
    Folder,
    Collection,
    Smart,
    Group,
}

/// One row of the Catalog sources' folders or collections, or a name being typed in place of one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogRow {
    pub(crate) icon: CatalogIcon,
    pub(crate) name: String,
    pub(crate) count: Option<String>,
    /// Nesting under its year or group: one level in, and one more per parent folder.
    pub(crate) indent: u8,
    /// `Some(open)` for a row that opens.
    pub(crate) open: Option<bool>,
    pub(crate) selected: bool,
    pub(crate) press: Option<CatalogAction>,
    pub(crate) toggle: Option<CatalogAction>,
    /// What a right-click on it opens.
    pub(crate) context: Option<CatalogAction>,
    /// Its menu, while open.
    pub(crate) menu: Option<Vec<ActionChoice>>,
    /// The name being typed for it: a rename, or a new folder or collection drawn in its place.
    pub(crate) naming: Option<String>,
}

impl CatalogRow {
    fn new(icon: CatalogIcon, name: String, indent: u8) -> Self {
        Self {
            icon,
            name,
            count: None,
            indent,
            open: None,
            selected: false,
            press: None,
            toggle: None,
            context: None,
            menu: None,
            naming: None,
        }
    }
}

/// One item of a catalog menu: `None` draws it disabled, with `reason` saying why.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionChoice {
    pub(crate) label: String,
    pub(crate) trailing: Option<String>,
    pub(crate) checked: bool,
    pub(crate) action: Option<CatalogAction>,
    pub(crate) reason: Option<String>,
    /// A rule above it.
    pub(crate) separated: bool,
}

impl ActionChoice {
    fn new(label: impl Into<String>, action: CatalogAction) -> Self {
        Self {
            label: label.into(),
            trailing: None,
            checked: false,
            action: Some(action),
            reason: None,
            separated: false,
        }
    }

    fn refused(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            trailing: None,
            checked: false,
            action: None,
            reason: Some(reason.into()),
            separated: false,
        }
    }

    fn separated(self) -> Self {
        Self {
            separated: true,
            ..self
        }
    }
}

/// The Catalog sources' folders and collections, between Recently developed and Missing
/// originals.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogSources {
    /// The years, each with its top-level folders and their open subfolders under it.
    pub(crate) folders: Vec<CatalogRow>,
    pub(crate) collections: Vec<CatalogRow>,
    /// Said in place of the rows while they are read, or when they cannot be.
    pub(crate) note: Option<String>,
    /// The `+` menu, while open.
    pub(crate) add_menu: Option<Vec<ActionChoice>>,
}

/// Which column a set condition is of, for its chip's glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConditionGlyph {
    Date,
    Place,
    Camera,
    Lens,
}

/// A set condition of the Metadata browser's, drawn as a chip in the accent with its clear ✕.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConditionChip {
    pub(crate) glyph: ConditionGlyph,
    pub(crate) label: String,
    pub(crate) clear: CatalogAction,
}

/// The Edited chip: its label, whether its condition is set (drawn in the accent with its clear ✕),
/// and its menu while open.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EditedChip {
    pub(crate) label: String,
    pub(crate) set: bool,
    pub(crate) menu: Option<Vec<ActionChoice>>,
}

/// The filter bar over the catalog: the search, Metadata, Kind (the shell's own chip), Edited, the
/// metadata conditions set, Save as smart collection… and the view's count.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogFilterBar {
    pub(crate) search: String,
    pub(crate) metadata_open: bool,
    pub(crate) edited: EditedChip,
    pub(crate) conditions: Vec<ConditionChip>,
    /// Why Save as smart collection… cannot save now; none while it can.
    pub(crate) save_refused: Option<String>,
    /// The smart collection's name being typed, while its field is open.
    pub(crate) naming: Option<String>,
    /// Over Removed, Empty Removed… in Save as smart collection…'s place.
    pub(crate) empty: Option<EmptyButton>,
    /// "9 of 55", or the view's size with no filter.
    pub(crate) count: String,
}

/// One value of a Metadata browser column, with its count.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FacetRow {
    pub(crate) label: String,
    pub(crate) count: String,
    /// A month under its year.
    pub(crate) indent: bool,
    pub(crate) selected: bool,
    /// What pressing it changes; `None` for a value no condition can name (no place, no lens).
    pub(crate) change: Option<CatalogChange>,
}

/// One column of the Metadata browser.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FacetColumnModel {
    pub(crate) facet: Facet,
    pub(crate) title: &'static str,
    pub(crate) rows: Vec<FacetRow>,
    /// Said in place of the rows while the counts are read, or when they cannot be.
    pub(crate) note: Option<String>,
}

/// A folder or collection a photograph is in, as the Info panel's Organize band shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OrganizeChip {
    pub(crate) collection: bool,
    pub(crate) label: String,
    /// "2 of 5" when only some of the selection is in it.
    pub(crate) partial: Option<String>,
    pub(crate) press: Option<CatalogAction>,
    pub(crate) menu: Option<Vec<ActionChoice>>,
}

/// The Info panel over photographs: one photograph's preview, Organize, Metadata and Develop bands,
/// or the batch form for several.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PhotoInfo {
    /// How many are selected.
    pub(crate) count: u32,
    /// One photograph's name, or "5 selected".
    pub(crate) title: String,
    /// Several: the active photograph's name.
    pub(crate) active: Option<String>,
    /// The view positions whose previews head the panel: the one, or up to five selected.
    pub(crate) previews: Vec<u32>,
    /// The one photograph's shape, for its preview's place.
    pub(crate) aspect: Option<f32>,
    pub(crate) folders: Vec<OrganizeChip>,
    pub(crate) collections: Vec<OrganizeChip>,
    /// Said in place of the chips when the selection's rows are not read.
    pub(crate) organize_note: Option<String>,
    /// Move to… and Add to…, and their menus while open.
    pub(crate) move_menu: Option<Vec<ActionChoice>>,
    pub(crate) add_menu: Option<Vec<ActionChoice>>,
    pub(crate) metadata: Vec<(String, String)>,
    /// "Yes", "No" or "5 of 5".
    pub(crate) edited: Option<String>,
    /// The Develop band's rows: whether they are edited.
    pub(crate) develop: Vec<(String, String)>,
    /// The Develop band's Apply preset… and Export…, and what the last batch says.
    pub(crate) batch: BatchBand,
    /// Remove from catalog…, or Put back over Removed.
    pub(crate) removal: Option<RemovalButton>,
}

/// The Develop band's batch actions, as the catalog board draws them: Apply preset… with its menu
/// of the library's presets, Export N…, the running batch's progress or the last one's sentence
/// with its report, and the note that each photograph gets its own history entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BatchBand {
    /// Why Apply preset… and Export… cannot start now; none while they can.
    pub(crate) refused: Option<String>,
    /// Apply preset…'s menu, while open.
    pub(crate) presets: Option<Vec<ActionChoice>>,
    /// Export's label: "Export 5…", or "Export…" for one.
    pub(crate) export: String,
    /// The running batch's progress, or the last batch's sentence.
    pub(crate) line: Option<String>,
    /// The last batch left a report to open.
    pub(crate) report: bool,
    pub(crate) note: &'static str,
}

/// The Info panel's Remove from catalog…, or Put back over Removed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemovalButton {
    pub(crate) label: String,
    pub(crate) action: CatalogAction,
}

/// What a sheet over the centre is: a confirmation or a batch's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetKind {
    Remove,
    Empty,
    Preset,
    Export,
}

/// One section of a report: a heading with its count, a row per photograph (its name and what
/// happened to it), and how many more there are past [`REPORT_ROWS`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SheetSection {
    pub(crate) heading: String,
    pub(crate) rows: Vec<(String, String)>,
    pub(crate) more: Option<String>,
}

/// A sheet over the centre: Remove from catalog…'s or Empty Removed…'s confirmation, or the last
/// batch's report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogSheet {
    pub(crate) kind: SheetKind,
    pub(crate) title: String,
    pub(crate) note: String,
    pub(crate) sections: Vec<SheetSection>,
    /// The confirming button's label; none for a report, which only closes.
    pub(crate) confirm: Option<String>,
}

/// The filter bar's Empty Removed… over Removed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EmptyButton {
    /// Why it cannot empty now; none while it can.
    pub(crate) refused: Option<String>,
}

/// What the catalog shows in Select, derived after every message.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogModel {
    /// Select's view is over the catalog: the filter bar, the Metadata browser and the Info panel
    /// are the catalog's.
    pub(crate) shown: bool,
    pub(crate) sources: CatalogSources,
    pub(crate) filter: Option<CatalogFilterBar>,
    /// The four columns, while the Metadata browser is open.
    pub(crate) metadata: Option<Vec<FacetColumnModel>>,
    pub(crate) info: Option<PhotoInfo>,
    /// A confirmation or a report over the centre.
    pub(crate) sheet: Option<CatalogSheet>,
    /// The status bar says the last batch's sentence, so it offers its report beside it.
    pub(crate) status_report: bool,
}

/// The catalog's model while Select is shown; `status` is what the status bar says.
pub(crate) fn derive(
    state: &SelectState,
    selection: &SelectionModel,
    status: &str,
) -> CatalogModel {
    let over = state.over_catalog();
    let catalog = &state.catalog;
    let home = state.home.as_deref();
    let said = catalog
        .batch
        .as_ref()
        .and_then(|batch| batch.sentence(home))
        .is_some_and(|sentence| sentence == status);
    CatalogModel {
        shown: over,
        sources: sources(state),
        filter: over.then(|| filter_bar(state)),
        metadata: (over && catalog.metadata).then(|| metadata(state)),
        info: over.then(|| info(state, selection)).flatten(),
        sheet: over.then(|| sheet(state, selection)).flatten(),
        status_report: said && report_of(catalog).is_some(),
    }
}

/// Whether the view shows Removed.
pub(crate) fn over_removed(state: &SelectState) -> bool {
    matches!(viewed(state), Some(ViewSource::Removed))
}

/// The last batch's report, once it ended with one.
fn report_of(catalog: &CatalogState) -> Option<(&BatchRun, &BatchReport)> {
    let batch = catalog.batch.as_ref()?;
    match &batch.end {
        Some(BatchEnd::Done(report)) => Some((batch, report)),
        _ => None,
    }
}

/// The sheet over the centre: the confirmation asked for, or the report opened.
fn sheet(state: &SelectState, selection: &SelectionModel) -> Option<CatalogSheet> {
    let catalog = &state.catalog;
    match catalog.confirm {
        Some(Confirm::Remove { count }) => {
            let title = match (count, selection.focus().and_then(|at| row_at(state, at))) {
                (1, Some(row)) => format!("Remove {} from the catalog?", row.file_name),
                _ => format!("Remove {} from the catalog?", photographs(count)),
            };
            return Some(CatalogSheet {
                kind: SheetKind::Remove,
                title,
                note: format!(
                    "{} to Removed with {} edits, history and collections. The files stay on \
                     disk, and the edits are kept until Removed is emptied. Undo with \u{2318}Z.",
                    if count == 1 { "It moves" } else { "They move" },
                    if count == 1 { "its" } else { "their" },
                ),
                sections: Vec::new(),
                confirm: Some(if count == 1 {
                    "Remove".to_owned()
                } else {
                    format!("Remove {}", thousands(count))
                }),
            });
        }
        Some(Confirm::Empty { count }) => {
            return Some(CatalogSheet {
                kind: SheetKind::Empty,
                title: "Empty Removed?".to_owned(),
                note: format!(
                    "{} in Removed {} deleted from the catalog with {} edits, history and \
                     versions. The files stay on disk. This cannot be undone.",
                    if count == 1 {
                        "The 1 photograph".to_owned()
                    } else {
                        format!("The {}", photographs(count))
                    },
                    if count == 1 { "is" } else { "are" },
                    if count == 1 { "its" } else { "their" },
                ),
                sections: Vec::new(),
                confirm: Some(format!("Delete {}", photographs(count))),
            });
        }
        None => {}
    }
    if !catalog.report {
        return None;
    }
    let (batch, report) = report_of(catalog)?;
    let home = state.home.as_deref();
    let section = |heading: &str, rows: Vec<(String, String)>| {
        let total = rows.len();
        let more = (total > REPORT_ROWS)
            .then(|| format!("and {} more", thousands((total - REPORT_ROWS) as u32)));
        SheetSection {
            heading: format!("{heading} \u{b7} {}", thousands(total as u32)),
            rows: rows.into_iter().take(REPORT_ROWS).collect(),
            more,
        }
    };
    let mut sections = Vec::new();
    if !report.skipped.is_empty() {
        sections.push(section(
            "Left out",
            report
                .skipped
                .iter()
                .map(|skip| {
                    (
                        batch.name(&skip.asset_id),
                        skip_text(&skip.code, &skip.reason),
                    )
                })
                .collect(),
        ));
    }
    if !report.settings_skipped.is_empty() {
        sections.push(section(
            "Without some settings",
            report
                .settings_skipped
                .iter()
                .map(|photo| {
                    let reasons: Vec<&str> = photo
                        .settings
                        .iter()
                        .map(|setting| setting.reason.as_str())
                        .collect();
                    (batch.name(&photo.asset_id), reasons.join("; "))
                })
                .collect(),
        ));
    }
    let (kind, title, note) = match &batch.kind {
        BatchKind::Preset { name } => (
            SheetKind::Preset,
            format!(
                "Applied {name} to {} of {}",
                thousands(report.done.len() as u32),
                photographs(batch.count)
            ),
            "Each photograph got its own history entry, which its history in Develop undoes. \
             Every one left out is listed with why."
                .to_owned(),
        ),
        BatchKind::Export { folder } => {
            let written: Vec<(String, String)> = report
                .written
                .iter()
                .map(|path| {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    (name, String::new())
                })
                .collect();
            if !written.is_empty() {
                sections.insert(0, section("Written", written));
            }
            (
                SheetKind::Export,
                format!(
                    "Exported {} of {}",
                    thousands(report.done.len() as u32),
                    photographs(batch.count)
                ),
                format!(
                    "Into {}, each named by the export's rule; no file was replaced. Every one \
                     left out is listed with why.",
                    shown_path(folder, home)
                ),
            )
        }
    };
    Some(CatalogSheet {
        kind,
        title,
        note,
        sections,
        confirm: None,
    })
}

/// Why a photograph was left out: the owner's words, with its code when they do not say it.
fn skip_text(code: &str, reason: &str) -> String {
    if reason.is_empty() {
        code.to_owned()
    } else {
        format!("{reason} ({code})")
    }
}

fn collection<'a>(state: &'a CatalogState, id: &CollectionId) -> Option<&'a Collection> {
    state
        .collections
        .as_ref()?
        .collections
        .iter()
        .find(|collection| &collection.id == id)
}

/// The folders by identity, and each folder's children in `folder.list`'s order (by name).
struct FolderTree<'a> {
    by_id: HashMap<&'a CatalogFolderId, &'a CatalogFolder>,
    children: HashMap<Option<&'a CatalogFolderId>, Vec<&'a CatalogFolder>>,
}

impl<'a> FolderTree<'a> {
    fn of(folders: &'a CatalogFolders) -> Self {
        let mut by_id = HashMap::with_capacity(folders.folders.len());
        let mut children: HashMap<Option<&CatalogFolderId>, Vec<&CatalogFolder>> = HashMap::new();
        for folder in &folders.folders {
            by_id.insert(&folder.id, folder);
        }
        for folder in &folders.folders {
            // A parent the list does not hold is read as the top level rather than hidden.
            let parent = folder
                .parent_id
                .as_ref()
                .filter(|parent| by_id.contains_key(parent));
            children.entry(parent).or_default().push(folder);
        }
        Self { by_id, children }
    }

    fn children(&self, parent: Option<&'a CatalogFolderId>) -> &[&'a CatalogFolder] {
        self.children.get(&parent).map_or(&[], Vec::as_slice)
    }

    /// Every folder in the tree's order: each parent before its children.
    fn ordered(&self) -> Vec<(&'a CatalogFolder, u8)> {
        let mut ordered = Vec::with_capacity(self.by_id.len());
        let mut stack: Vec<(&CatalogFolder, u8)> = self
            .children(None)
            .iter()
            .rev()
            .map(|folder| (*folder, 0))
            .collect();
        while let Some((folder, depth)) = stack.pop() {
            ordered.push((folder, depth));
            stack.extend(
                self.children(Some(&folder.id))
                    .iter()
                    .rev()
                    .map(|child| (*child, depth.saturating_add(1))),
            );
        }
        ordered
    }

    /// The photographs in `folder` and its subfolders, and the earliest capture year among them.
    fn subtree(&self, folder: &'a CatalogFolder) -> (u32, Option<i32>) {
        let (mut count, mut year) = (0u32, None::<i32>);
        let mut stack = vec![folder];
        while let Some(at) = stack.pop() {
            count = count.saturating_add(at.count);
            year = match (year, at.year) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            stack.extend(self.children(Some(&at.id)).iter().copied());
        }
        (count, year)
    }

    /// Whether `folder` is `ancestor` or inside it.
    fn within(&self, folder: &CatalogFolderId, ancestor: &CatalogFolderId) -> bool {
        let mut at = Some(folder);
        let mut steps = 0;
        while let Some(id) = at {
            if id == ancestor {
                return true;
            }
            steps += 1;
            if steps > self.by_id.len() {
                return false;
            }
            at = self
                .by_id
                .get(id)
                .and_then(|folder| folder.parent_id.as_ref());
        }
        false
    }

    /// A folder's name with its parents', as a menu that lists every folder says it.
    fn path(&self, folder: &'a CatalogFolder) -> String {
        let mut names = vec![folder.name.as_str()];
        let mut at = folder.parent_id.as_ref();
        while let Some(id) = at {
            let Some(parent) = self.by_id.get(id) else {
                break;
            };
            if names.len() > self.by_id.len() {
                break;
            }
            names.push(parent.name.as_str());
            at = parent.parent_id.as_ref();
        }
        names.reverse();
        names.join(" \u{203a} ")
    }
}

/// The year group of a top-level folder, from its subtree.
fn year_group(count: u32, year: Option<i32>) -> YearGroup {
    match (year, count) {
        (Some(year), _) => YearGroup::Year(year),
        (None, 0) => YearGroup::Empty,
        (None, _) => YearGroup::Undated,
    }
}

/// The source the view shows, or was last asked for.
fn viewed(state: &SelectState) -> Option<&ViewSource> {
    state.query.as_ref().map(|query| &query.source)
}

pub(crate) fn sources(state: &SelectState) -> CatalogSources {
    let catalog = &state.catalog;
    let viewing = viewed(state);
    let note = match (&catalog.folders, &catalog.lists_error) {
        (None, Some(_)) => Some("Folders and collections unavailable".to_owned()),
        (None, None) => Some("Reading folders\u{2026}".to_owned()),
        _ => None,
    };
    let mut folders = Vec::new();
    if let Some(list) = &catalog.folders {
        folder_rows(catalog, list, viewing, &mut folders);
    }
    let mut collections = Vec::new();
    if let Some(list) = &catalog.collections {
        collection_rows(catalog, list, viewing, &mut collections);
    }
    if let Some(Naming {
        target: NamingTarget::NewCollection { kind, parent: None },
        text,
    }) = &catalog.naming
    {
        let icon = if *kind == CollectionKind::Group {
            CatalogIcon::Group
        } else {
            CatalogIcon::Collection
        };
        collections.insert(
            0,
            CatalogRow {
                naming: Some(text.clone()),
                ..CatalogRow::new(icon, String::new(), 0)
            },
        );
    }
    CatalogSources {
        folders,
        collections,
        note,
        add_menu: (catalog.menu == Some(CatalogMenu::Add)).then(|| {
            vec![
                ActionChoice::new(
                    "New folder",
                    CatalogAction::Name(NamingTarget::NewFolder { parent: None }),
                ),
                ActionChoice::new(
                    "New collection",
                    CatalogAction::Name(NamingTarget::NewCollection {
                        kind: CollectionKind::Collection,
                        parent: None,
                    }),
                ),
                ActionChoice::new(
                    "New collection group",
                    CatalogAction::Name(NamingTarget::NewCollection {
                        kind: CollectionKind::Group,
                        parent: None,
                    }),
                ),
            ]
        }),
    }
}

/// The years, newest first and then Undated and Empty, each opening to its top-level folders and
/// each folder to its subfolders. A new folder's name is typed at the top of the list, or under the
/// folder it goes into.
fn folder_rows(
    catalog: &CatalogState,
    list: &CatalogFolders,
    viewing: Option<&ViewSource>,
    rows: &mut Vec<CatalogRow>,
) {
    let tree = FolderTree::of(list);
    rows.extend(new_folder_row(catalog, None, 0));
    let mut groups: BTreeMap<std::cmp::Reverse<YearGroupOrder>, Vec<(&CatalogFolder, u32)>> =
        BTreeMap::new();
    for folder in tree.children(None) {
        let (count, year) = tree.subtree(folder);
        groups
            .entry(std::cmp::Reverse(YearGroupOrder(year_group(count, year))))
            .or_default()
            .push((folder, count));
    }
    let newest = groups.keys().next().map(|key| key.0.0);
    for (key, top) in groups {
        let group = key.0.0;
        let open = catalog
            .years
            .get(&group)
            .copied()
            .unwrap_or(Some(group) == newest || group == YearGroup::Empty);
        rows.push(CatalogRow {
            open: Some(open),
            press: Some(CatalogAction::ToggleYear(group)),
            toggle: Some(CatalogAction::ToggleYear(group)),
            ..CatalogRow::new(CatalogIcon::Year, group.label(), 0)
        });
        if !open {
            continue;
        }
        for (folder, count) in top {
            push_folder(catalog, &tree, folder, count, 1, viewing, rows);
        }
    }
}

/// The order years are listed in, reversed: the newest year first, then Undated and Empty last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct YearGroupOrder(YearGroup);

impl PartialOrd for YearGroupOrder {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for YearGroupOrder {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |group: YearGroup| match group {
            YearGroup::Year(year) => (2, year),
            YearGroup::Undated => (1, 0),
            YearGroup::Empty => (0, 0),
        };
        rank(self.0).cmp(&rank(other.0))
    }
}

/// The row of a new folder whose name is being typed, when it goes into `parent`.
fn new_folder_row(
    catalog: &CatalogState,
    parent: Option<&CatalogFolderId>,
    indent: u8,
) -> Option<CatalogRow> {
    let naming = catalog.naming.as_ref()?;
    match &naming.target {
        NamingTarget::NewFolder { parent: into } if into.as_ref() == parent => Some(CatalogRow {
            naming: Some(naming.text.clone()),
            ..CatalogRow::new(CatalogIcon::Folder, String::new(), indent)
        }),
        _ => None,
    }
}

fn push_folder<'a>(
    catalog: &CatalogState,
    tree: &FolderTree<'a>,
    folder: &'a CatalogFolder,
    count: u32,
    indent: u8,
    viewing: Option<&ViewSource>,
    rows: &mut Vec<CatalogRow>,
) {
    let children = tree.children(Some(&folder.id));
    let open = catalog.open_folders.contains(&folder.id);
    let source = ViewSource::CatalogFolder {
        folder_id: folder.id.clone(),
        subfolders: true,
    };
    let renaming = catalog.naming.as_ref().and_then(|naming| {
        (naming.target == NamingTarget::RenameFolder(folder.id.clone()))
            .then(|| naming.text.clone())
    });
    rows.push(CatalogRow {
        count: (count > 0).then(|| thousands(count)),
        open: (!children.is_empty()).then_some(open),
        selected: viewing.is_some_and(|viewed| {
            matches!(viewed, ViewSource::CatalogFolder { folder_id, .. } if folder_id == &folder.id)
        }),
        press: Some(CatalogAction::View(source)),
        toggle: (!children.is_empty()).then(|| CatalogAction::ToggleFolder(folder.id.clone())),
        context: Some(CatalogAction::Menu(Some(CatalogMenu::Folder(folder.id.clone())))),
        menu: folder_menu(catalog, tree, folder, count, children.len()),
        naming: renaming,
        ..CatalogRow::new(CatalogIcon::Folder, folder.name.clone(), indent)
    });
    // A new folder's name is typed first among the folder's own, which open for it.
    let naming_inside = matches!(
        catalog.naming.as_ref().map(|naming| &naming.target),
        Some(NamingTarget::NewFolder { parent: Some(parent) }) if parent == &folder.id
    );
    if naming_inside {
        rows.extend(new_folder_row(
            catalog,
            Some(&folder.id),
            indent.saturating_add(1),
        ));
    }
    if open || naming_inside {
        for child in children {
            let (count, _) = tree.subtree(child);
            push_folder(
                catalog,
                tree,
                child,
                count,
                indent.saturating_add(1),
                viewing,
                rows,
            );
        }
    }
}

/// A folder row's menu while it is open: Rename…, New folder inside…, Move to…, Merge into… and
/// Delete, which only an empty folder offers; or one of its two pickers.
fn folder_menu<'a>(
    catalog: &CatalogState,
    tree: &FolderTree<'a>,
    folder: &'a CatalogFolder,
    count: u32,
    children: usize,
) -> Option<Vec<ActionChoice>> {
    let id = &folder.id;
    match catalog.menu.as_ref()? {
        CatalogMenu::Folder(open) if open == id => {
            let delete = if count > 0 {
                ActionChoice::refused(
                    "Delete folder",
                    format!("It holds {}; move them out first", photographs(count)),
                )
            } else if children > 0 {
                ActionChoice::refused(
                    "Delete folder",
                    "It holds folders; move or merge them first",
                )
            } else {
                ActionChoice::new("Delete folder", CatalogAction::DeleteFolder(id.clone()))
            };
            Some(vec![
                ActionChoice::new(
                    "Rename\u{2026}",
                    CatalogAction::Name(NamingTarget::RenameFolder(id.clone())),
                ),
                ActionChoice::new(
                    "New folder inside\u{2026}",
                    CatalogAction::Name(NamingTarget::NewFolder {
                        parent: Some(id.clone()),
                    }),
                ),
                ActionChoice::new(
                    "Move to\u{2026}",
                    CatalogAction::Menu(Some(CatalogMenu::FolderMove(id.clone()))),
                ),
                ActionChoice::new(
                    "Merge into\u{2026}",
                    CatalogAction::Menu(Some(CatalogMenu::FolderMerge(id.clone()))),
                ),
                delete.separated(),
            ])
        }
        CatalogMenu::FolderMove(open) if open == id => {
            let mut choices = vec![ActionChoice {
                checked: folder.parent_id.is_none(),
                ..if folder.parent_id.is_none() {
                    ActionChoice::refused("Top level", "It is at the top level")
                } else {
                    ActionChoice::new(
                        "Top level",
                        CatalogAction::MoveFolder {
                            folder: id.clone(),
                            parent: None,
                        },
                    )
                }
            }];
            for (other, _) in tree.ordered() {
                if tree.within(&other.id, id) {
                    continue;
                }
                let label = tree.path(other);
                let here = folder.parent_id.as_ref() == Some(&other.id);
                choices.push(ActionChoice {
                    checked: here,
                    ..if here {
                        ActionChoice::refused(label, "It is already in this folder")
                    } else {
                        ActionChoice::new(
                            label,
                            CatalogAction::MoveFolder {
                                folder: id.clone(),
                                parent: Some(other.id.clone()),
                            },
                        )
                    }
                });
            }
            Some(choices)
        }
        CatalogMenu::FolderMerge(open) if open == id => Some(
            tree.ordered()
                .into_iter()
                .filter(|(other, _)| !tree.within(&other.id, id))
                .map(|(other, _)| {
                    ActionChoice::new(
                        tree.path(other),
                        CatalogAction::MergeFolder {
                            folder: id.clone(),
                            into: other.id.clone(),
                        },
                    )
                })
                .collect(),
        ),
        _ => None,
    }
}

/// The collections, groups first-come as `collection.list` orders them, each group opening to what
/// it holds.
fn collection_rows(
    catalog: &CatalogState,
    list: &Collections,
    viewing: Option<&ViewSource>,
    rows: &mut Vec<CatalogRow>,
) {
    let mut children: HashMap<Option<&CollectionId>, Vec<&Collection>> = HashMap::new();
    let known: BTreeSet<&CollectionId> = list.collections.iter().map(|c| &c.id).collect();
    for collection in &list.collections {
        let parent = collection
            .parent_id
            .as_ref()
            .filter(|parent| known.contains(parent));
        children.entry(parent).or_default().push(collection);
    }
    let mut stack: Vec<(&Collection, u8)> = children
        .get(&None)
        .map(|top| {
            top.iter()
                .rev()
                .map(|collection| (*collection, 0))
                .collect()
        })
        .unwrap_or_default();
    let mut seen = 0usize;
    while let Some((collection, indent)) = stack.pop() {
        seen += 1;
        if seen > list.collections.len() {
            break;
        }
        let id = &collection.id;
        let inside = children.get(&Some(id)).map_or(0, Vec::len);
        let group = collection.kind == CollectionKind::Group;
        let open = group && !catalog.closed_groups.contains(id);
        let source = ViewSource::Collection {
            collection_id: id.clone(),
        };
        let renaming = catalog.naming.as_ref().and_then(|naming| {
            (naming.target == NamingTarget::RenameCollection(id.clone()))
                .then(|| naming.text.clone())
        });
        rows.push(CatalogRow {
            count: collection.count.map(thousands),
            open: group.then_some(open),
            selected: !group && viewing == Some(&source),
            press: Some(if group {
                CatalogAction::ToggleGroup(id.clone())
            } else {
                CatalogAction::View(source)
            }),
            toggle: group.then(|| CatalogAction::ToggleGroup(id.clone())),
            context: Some(CatalogAction::Menu(Some(CatalogMenu::Collection(
                id.clone(),
            )))),
            menu: (catalog.menu == Some(CatalogMenu::Collection(id.clone())))
                .then(|| collection_menu(collection, inside)),
            naming: renaming,
            ..CatalogRow::new(
                match collection.kind {
                    CollectionKind::Collection => CatalogIcon::Collection,
                    CollectionKind::Smart => CatalogIcon::Smart,
                    CollectionKind::Group => CatalogIcon::Group,
                },
                collection.name.clone(),
                indent,
            )
        });
        if let Some(Naming {
            target:
                NamingTarget::NewCollection {
                    kind,
                    parent: Some(parent),
                },
            text,
        }) = &catalog.naming
            && parent == id
        {
            rows.push(CatalogRow {
                naming: Some(text.clone()),
                ..CatalogRow::new(
                    if *kind == CollectionKind::Group {
                        CatalogIcon::Group
                    } else {
                        CatalogIcon::Collection
                    },
                    String::new(),
                    indent.saturating_add(1),
                )
            });
        }
        if open && let Some(inner) = children.get(&Some(id)) {
            stack.extend(
                inner
                    .iter()
                    .rev()
                    .map(|child| (*child, indent.saturating_add(1))),
            );
        }
    }
}

/// A collection's menu: Rename…, a group's New collection inside…, and Delete, which a group
/// offers only when empty.
fn collection_menu(collection: &Collection, inside: usize) -> Vec<ActionChoice> {
    let id = &collection.id;
    let noun = match collection.kind {
        CollectionKind::Collection => "collection",
        CollectionKind::Smart => "smart collection",
        CollectionKind::Group => "group",
    };
    let mut choices = vec![ActionChoice::new(
        "Rename\u{2026}",
        CatalogAction::Name(NamingTarget::RenameCollection(id.clone())),
    )];
    if collection.kind == CollectionKind::Group {
        choices.push(ActionChoice::new(
            "New collection inside\u{2026}",
            CatalogAction::Name(NamingTarget::NewCollection {
                kind: CollectionKind::Collection,
                parent: Some(id.clone()),
            }),
        ));
    }
    let label = format!("Delete {noun}");
    choices.push(
        if collection.kind == CollectionKind::Group && inside > 0 {
            ActionChoice::refused(label, "It holds collections; move or delete them first")
        } else {
            ActionChoice::new(label, CatalogAction::DeleteCollection(id.clone()))
        }
        .separated(),
    );
    choices
}

pub(crate) fn filter_bar(state: &SelectState) -> CatalogFilterBar {
    let catalog = &state.catalog;
    let Some(query) = &state.query else {
        return CatalogFilterBar::default();
    };
    let filter = &query.filter;
    let edited_open = catalog.menu == Some(CatalogMenu::Edited);
    let edited = EditedChip {
        label: match filter.edited {
            Some(false) => "Not edited",
            _ => "Edited",
        }
        .to_owned(),
        set: filter.edited.is_some(),
        menu: edited_open.then(|| {
            [
                ("Edited or not", None),
                ("Edited", Some(true)),
                ("Not edited", Some(false)),
            ]
            .map(|(label, edited)| ActionChoice {
                checked: filter.edited == edited,
                ..ActionChoice::new(label, CatalogAction::Change(CatalogChange::Edited(edited)))
            })
            .to_vec()
        }),
    };
    let facets = state.facets.as_ref();
    let label_of = |facet: Facet, value: &str| {
        facets
            .and_then(|facets| facets.counts.get(&facet))
            .and_then(|values| {
                values
                    .iter()
                    .find(|listed| listed.value.as_deref() == Some(value))
            })
            .and_then(|listed| listed.label.clone())
            .unwrap_or_else(|| value.to_owned())
    };
    let many = |values: usize, noun: &str| format!("{values} {noun}");
    let mut conditions = Vec::new();
    if let Some(range) = filter.dates {
        conditions.push(ConditionChip {
            glyph: ConditionGlyph::Date,
            label: dates_label(range),
            clear: CatalogAction::Change(CatalogChange::Dates(None)),
        });
    }
    if !filter.places.is_empty() {
        conditions.push(ConditionChip {
            glyph: ConditionGlyph::Place,
            label: match filter.places.as_slice() {
                [one] => one.clone(),
                several => many(several.len(), "places"),
            },
            clear: CatalogAction::Change(CatalogChange::Place(None)),
        });
    }
    if !filter.cameras.is_empty() {
        conditions.push(ConditionChip {
            glyph: ConditionGlyph::Camera,
            label: match filter.cameras.as_slice() {
                [one] => label_of(Facet::Camera, &one.0),
                several => many(several.len(), "cameras"),
            },
            clear: CatalogAction::Change(CatalogChange::Camera(None)),
        });
    }
    if !filter.lenses.is_empty() {
        conditions.push(ConditionChip {
            glyph: ConditionGlyph::Lens,
            label: match filter.lenses.as_slice() {
                [one] => one.clone(),
                several => many(several.len(), "lenses"),
            },
            clear: CatalogAction::Change(CatalogChange::Lens(None)),
        });
    }
    let summary = state
        .summary
        .as_ref()
        .filter(|summary| summary.query.source == query.source);
    let save_refused = match (&query.source, summary) {
        (_, None) => Some("Waiting for the view".to_owned()),
        (ViewSource::Collection { collection_id }, _)
            if collection(catalog, collection_id)
                .is_some_and(|collection| collection.kind == CollectionKind::Smart) =>
        {
            Some("A smart collection cannot be saved from another smart collection".to_owned())
        }
        _ => None,
    };
    let count = match summary {
        None if state.loading => "Reading\u{2026}".to_owned(),
        None => String::new(),
        Some(summary) if filtered(&summary.query.filter) => {
            match catalog
                .total
                .as_ref()
                .filter(|total| total.source == summary.query.source)
            {
                Some(total) => {
                    format!("{} of {}", thousands(summary.count), thousands(total.count))
                }
                None => photographs(summary.count),
            }
        }
        Some(summary) => photographs(summary.count),
    };
    CatalogFilterBar {
        search: catalog.search.clone(),
        metadata_open: catalog.metadata,
        edited,
        conditions,
        save_refused,
        naming: catalog.naming.as_ref().and_then(|naming| {
            (naming.target == NamingTarget::SmartCollection).then(|| naming.text.clone())
        }),
        empty: over_removed(state).then(|| EmptyButton {
            refused: empty_refused(state),
        }),
        count,
    }
}

/// Why Empty Removed… cannot empty now: it is emptying, the counts are not read, or Removed holds
/// nothing.
fn empty_refused(state: &SelectState) -> Option<String> {
    if state.catalog.emptying.is_some() {
        return Some("Emptying Removed\u{2026}".to_owned());
    }
    match state.counts.as_ref().map(|counts| counts.removed) {
        None => Some("Reading the catalog's counts\u{2026}".to_owned()),
        Some(0) => Some("Removed is empty".to_owned()),
        Some(_) => None,
    }
}

/// How many photographs Empty Removed… would delete: `catalog.info`'s count of Removed.
pub(crate) fn removed_count(state: &SelectState) -> Option<u32> {
    state
        .counts
        .as_ref()
        .map(|counts| u32::try_from(counts.removed).unwrap_or(u32::MAX))
        .filter(|count| *count > 0)
}

/// The Metadata browser's four columns — Date folded into years and months, Place, Camera and Lens
/// — from `browse.facets` over the view's source and filter. Each column's counts are the owner's,
/// counted without that column's own condition; "All" is their sum, the view with the column's
/// condition cleared. Pressing a value narrows the view to it, and pressing the chosen value again
/// clears it. A value the filter cannot name (no place, no lens, an unknown camera, undated) is
/// listed with its count and does nothing.
pub(crate) fn metadata(state: &SelectState) -> Vec<FacetColumnModel> {
    let values = |facet: Facet| {
        state
            .facets
            .as_ref()
            .and_then(|facets| facets.counts.get(&facet))
    };
    let note = if state.facets.is_some() {
        None
    } else if state.loading || state.query.is_some() {
        Some("Counting\u{2026}".to_owned())
    } else {
        None
    };
    let filter = state
        .query
        .as_ref()
        .map(|query| query.filter.clone())
        .unwrap_or_default();
    let column = |facet: Facet, title, rows: Vec<FacetRow>| FacetColumnModel {
        facet,
        title,
        rows,
        note: note.clone(),
    };
    vec![
        column(
            Facet::Date,
            "Date",
            values(Facet::Date).map_or_else(Vec::new, |values| date_rows(values, filter.dates)),
        ),
        column(
            Facet::Place,
            "Place",
            values(Facet::Place).map_or_else(Vec::new, |values| {
                named_rows(values, &filter.places, "No location", |value| {
                    CatalogChange::Place(value.map(str::to_owned))
                })
            }),
        ),
        column(
            Facet::Camera,
            "Camera",
            values(Facet::Camera).map_or_else(Vec::new, |values| {
                let chosen: Vec<String> = filter.cameras.iter().map(|key| key.0.clone()).collect();
                named_rows(values, &chosen, "Unknown camera", |value| {
                    CatalogChange::Camera(value.map(|key| BodyKey(key.to_owned())))
                })
            }),
        ),
        column(
            Facet::Lens,
            "Lens",
            values(Facet::Lens).map_or_else(Vec::new, |values| {
                named_rows(values, &filter.lenses, "Unknown", |value| {
                    CatalogChange::Lens(value.map(str::to_owned))
                })
            }),
        ),
    ]
}

/// Place, Camera or Lens: "All" (the column's condition cleared) and each value by its label, the
/// value absent last as `absent`.
fn named_rows(
    values: &[FacetValue],
    chosen: &[String],
    absent: &str,
    change: impl Fn(Option<&str>) -> CatalogChange,
) -> Vec<FacetRow> {
    let total: u32 = values.iter().map(|value| value.count).sum();
    let mut rows = vec![FacetRow {
        label: "All".to_owned(),
        count: thousands(total),
        indent: false,
        selected: chosen.is_empty(),
        change: Some(change(None)),
    }];
    let mut listed: Vec<&FacetValue> = values.iter().collect();
    listed.sort_by(|a, b| {
        a.value.is_none().cmp(&b.value.is_none()).then_with(|| {
            let label = |value: &FacetValue| {
                value
                    .label
                    .clone()
                    .or_else(|| value.value.clone())
                    .unwrap_or_default()
                    .to_lowercase()
            };
            label(a).cmp(&label(b))
        })
    });
    for value in listed {
        let Some(name) = &value.value else {
            rows.push(FacetRow {
                label: value.label.clone().unwrap_or_else(|| absent.to_owned()),
                count: thousands(value.count),
                indent: false,
                selected: false,
                change: None,
            });
            continue;
        };
        let selected = chosen.iter().any(|chosen| chosen == name);
        rows.push(FacetRow {
            label: value.label.clone().unwrap_or_else(|| name.clone()),
            count: thousands(value.count),
            indent: false,
            selected,
            // The value chosen alone clears the column; any other replaces its condition.
            change: Some(if selected && chosen.len() == 1 {
                change(None)
            } else {
                change(Some(name))
            }),
        });
    }
    rows
}

/// Date: each year, newest first, with its count; the year the condition is in (or the newest)
/// open to its months, newest first; and Undated last.
fn date_rows(values: &[FacetValue], chosen: Option<DateRange>) -> Vec<FacetRow> {
    let mut months: BTreeMap<Month, u32> = BTreeMap::new();
    let mut undated = 0u32;
    for value in values {
        match value.value.as_deref().and_then(month_of) {
            Some(month) => *months.entry(month).or_default() += value.count,
            None => undated = undated.saturating_add(value.count),
        }
    }
    let mut years: BTreeMap<i32, u32> = BTreeMap::new();
    for (month, count) in &months {
        *years.entry(month.year).or_default() += count;
    }
    let open = chosen
        .map(|range| range.from.ymd().0)
        .filter(|year| years.contains_key(year))
        .or_else(|| years.keys().next_back().copied());
    let mut rows = Vec::new();
    for (&year, &count) in years.iter().rev() {
        let range = year_range(year);
        let selected = range.is_some() && chosen == range;
        rows.push(FacetRow {
            label: year.to_string(),
            count: thousands(count),
            indent: false,
            selected,
            change: range.map(|range| CatalogChange::Dates((!selected).then_some(range))),
        });
        if open != Some(year) {
            continue;
        }
        for (&month, &count) in months
            .range(..)
            .rev()
            .filter(|(month, _)| month.year == year)
        {
            let range = month_range(month);
            let selected = range.is_some() && chosen == range;
            rows.push(FacetRow {
                label: month_name(month.month).to_owned(),
                count: thousands(count),
                indent: true,
                selected,
                change: range.map(|range| CatalogChange::Dates((!selected).then_some(range))),
            });
        }
    }
    if undated > 0 {
        rows.push(FacetRow {
            label: "Undated".to_owned(),
            count: thousands(undated),
            indent: false,
            selected: false,
            change: None,
        });
    }
    rows
}

fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[(month.clamp(1, 12) - 1) as usize]
}

// -- The Info panel over photographs ---------------------------------------------------------------

/// The row at `position` of the view on screen: from the rows near the screen, or those read for
/// the selection.
fn row_at(state: &SelectState, position: u32) -> Option<&ViewRow> {
    state.rows.row(position).or_else(|| {
        let read = state.catalog.selection_rows.as_ref()?;
        (Some(read.revision) == state.revision())
            .then(|| read.rows.get(&position))
            .flatten()
    })
}

/// The selection's positions, ascending.
fn positions(selection: &SelectionModel) -> impl Iterator<Item = u32> + '_ {
    selection.ranges.iter().flat_map(|&(start, end)| start..end)
}

/// The selected rows, when the desktop holds every one; `None` otherwise.
fn selected_rows<'a>(
    state: &'a SelectState,
    selection: &SelectionModel,
) -> Option<Vec<&'a ViewRow>> {
    if selection.count > MAX_SELECTION_ROWS {
        return None;
    }
    positions(selection)
        .map(|position| row_at(state, position))
        .collect()
}

/// The ranges of the selection whose rows the desktop does not hold: what the Info panel reads
/// (`browse.rows`) to show a batch's folders and collections. Empty when it holds them all, when
/// nothing or one photograph is selected, and past [`MAX_SELECTION_ROWS`].
pub(crate) fn missing_rows(state: &SelectState, selection: &SelectionModel) -> Vec<(u32, u32)> {
    if !state.over_catalog() || selection.count < 2 || selection.count > MAX_SELECTION_ROWS {
        return Vec::new();
    }
    let mut missing: Vec<(u32, u32)> = Vec::new();
    for position in positions(selection) {
        if row_at(state, position).is_some() {
            continue;
        }
        match missing.last_mut() {
            Some((_, end)) if *end == position => *end += 1,
            _ => missing.push((position, position + 1)),
        }
    }
    missing
}

fn folder_name(state: &CatalogState, id: &CatalogFolderId) -> String {
    state
        .folders
        .as_ref()
        .and_then(|folders| folders.folders.iter().find(|folder| &folder.id == id))
        .map_or_else(
            || "A catalog folder".to_owned(),
            |folder| folder.name.clone(),
        )
}

fn collection_path(state: &CatalogState, collection: &Collection) -> String {
    let mut names = vec![collection.name.clone()];
    let mut at = collection.parent_id.as_ref();
    let mut steps = 0;
    while let Some(id) = at
        && let Some(parent) = self::collection(state, id)
        && steps < 64
    {
        names.push(parent.name.clone());
        at = parent.parent_id.as_ref();
        steps += 1;
    }
    names.reverse();
    names.join(" \u{203a} ")
}

/// The Info panel over the catalog: one photograph, or the batch form for several; nothing when
/// nothing is selected.
pub(crate) fn info(state: &SelectState, selection: &SelectionModel) -> Option<PhotoInfo> {
    let catalog = &state.catalog;
    let count = selection.count;
    let focus = selection.focus()?;
    let rows = selected_rows(state, selection);
    let active = row_at(state, focus);
    let mut info = PhotoInfo {
        count,
        ..PhotoInfo::default()
    };
    if count <= 1 {
        let row = active?;
        let item = item_info(row, state.summary.as_ref(), state.home.as_deref());
        info.title = row.file_name.clone();
        info.aspect = item.aspect;
        info.previews = vec![focus];
        info.metadata = item
            .metadata
            .into_iter()
            .filter(|(label, _)| label != "Edited")
            .collect();
        info.edited = Some(if row.edited { "Yes" } else { "No" }.to_owned());
    } else {
        info.title = format!("{} selected", thousands(count));
        info.active = active.map(|row| row.file_name.clone());
        info.previews = positions(selection).take(BATCH_PREVIEWS).collect();
        if let Some(rows) = &rows {
            info.metadata = batch_metadata(rows);
            let edited = rows.iter().filter(|row| row.edited).count();
            info.edited = Some(format!(
                "{} of {}",
                thousands(edited as u32),
                thousands(count)
            ));
        }
    }
    let Some(rows) = rows else {
        info.organize_note = Some(if count > MAX_SELECTION_ROWS {
            format!(
                "Folders and collections are listed for up to {} selected photographs",
                thousands(MAX_SELECTION_ROWS)
            )
        } else {
            "Reading the selected photographs\u{2026}".to_owned()
        });
        info.move_menu =
            (catalog.menu == Some(CatalogMenu::MovePhotos)).then(|| move_choices(catalog, None));
        info.add_menu = (catalog.menu == Some(CatalogMenu::AddTo))
            .then(|| add_choices(catalog, &BTreeMap::new(), count));
        info.develop = develop_rows(&info);
        info.batch = batch_band(state, count);
        info.removal = Some(removal_button(state));
        return Some(info);
    };
    // Folders: how many of the selection each holds, most first.
    let mut in_folders: BTreeMap<&CatalogFolderId, u32> = BTreeMap::new();
    let mut in_collections: BTreeMap<&CollectionId, u32> = BTreeMap::new();
    for row in &rows {
        if let Some(folder) = &row.folder_id {
            *in_folders.entry(folder).or_default() += 1;
        }
        for collection in &row.collections {
            *in_collections.entry(collection).or_default() += 1;
        }
    }
    let partial =
        |held: u32| (held < count).then(|| format!("{} of {}", thousands(held), thousands(count)));
    let mut folders: Vec<(&CatalogFolderId, u32)> = in_folders.into_iter().collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1));
    info.folders = folders
        .iter()
        .map(|(folder, held)| OrganizeChip {
            collection: false,
            label: folder_name(catalog, folder),
            partial: partial(*held),
            press: None,
            menu: None,
        })
        .collect();
    let every = (folders.len() == 1 && folders[0].1 == count).then(|| folders[0].0.clone());
    info.move_menu = (catalog.menu == Some(CatalogMenu::MovePhotos))
        .then(|| move_choices(catalog, every.as_ref()));
    let mut members: Vec<(&CollectionId, u32)> = in_collections
        .iter()
        .map(|(id, held)| (*id, *held))
        .collect();
    members.sort_by(|a, b| b.1.cmp(&a.1));
    info.collections = members
        .iter()
        .map(|(id, held)| {
            let name = collection(catalog, id).map_or_else(
                || "A collection".to_owned(),
                |c| collection_path(catalog, c),
            );
            let menu = (catalog.menu == Some(CatalogMenu::Member((*id).clone()))).then(|| {
                let mut choices = vec![ActionChoice::new(
                    if count == 1 {
                        format!("Remove from {name}")
                    } else {
                        format!("Remove {} from {name}", thousands(*held))
                    },
                    CatalogAction::RemovePhotos((*id).clone()),
                )];
                if *held < count {
                    choices.push(ActionChoice::new(
                        format!("Add the other {} to {name}", thousands(count - held)),
                        CatalogAction::AddPhotos((*id).clone()),
                    ));
                }
                choices
            });
            OrganizeChip {
                collection: true,
                label: name,
                partial: partial(*held),
                press: Some(CatalogAction::Menu(Some(CatalogMenu::Member(
                    (*id).clone(),
                )))),
                menu,
            }
        })
        .collect();
    info.add_menu = (catalog.menu == Some(CatalogMenu::AddTo))
        .then(|| add_choices(catalog, &in_collections, count));
    info.develop = develop_rows(&info);
    info.batch = batch_band(state, count);
    info.removal = Some(removal_button(state));
    Some(info)
}

/// The Develop band's rows.
fn develop_rows(info: &PhotoInfo) -> Vec<(String, String)> {
    info.edited
        .iter()
        .map(|edited| ("Edited".to_owned(), edited.clone()))
        .collect()
}

/// The Develop band's Apply preset… and Export… for `count` selected photographs: refused while a
/// batch of this desktop's runs and over Removed, whose photographs a batch leaves out; the menu of
/// the library's presets while open; and the running batch's progress or the last one's sentence.
fn batch_band(state: &SelectState, count: u32) -> BatchBand {
    let catalog = &state.catalog;
    let refused = if let Some(batch) = catalog.running() {
        Some(format!("Waiting for the batch: {}", batch.running()))
    } else if over_removed(state) {
        Some("Put them back first: a batch leaves photographs in Removed out".to_owned())
    } else {
        None
    };
    let presets = (catalog.menu == Some(CatalogMenu::Presets)).then(|| match &catalog.presets {
        None => vec![ActionChoice::refused(
            "Reading presets\u{2026}",
            "The library's presets are being read",
        )],
        Some(Err(error)) => vec![ActionChoice::refused("Presets unavailable", error.clone())],
        Some(Ok(presets)) if presets.is_empty() => vec![ActionChoice::refused(
            "No presets",
            "Create one in Develop's Presets section",
        )],
        Some(Ok(presets)) => {
            let mut choices: Vec<ActionChoice> = Vec::with_capacity(presets.len());
            let mut group: Option<&str> = None;
            for preset in presets {
                let apply = CatalogAction::ApplyPreset {
                    id: preset.id.clone(),
                    name: preset.name.clone(),
                };
                let mut choice = match &preset.unavailable {
                    Some(reason) => ActionChoice::refused(preset.name.clone(), reason.clone()),
                    None => ActionChoice::new(preset.name.clone(), apply),
                };
                // Each run of one group's presets names its group on its first item, under a rule
                // after the first run.
                if group != Some(preset.group.as_str()) {
                    choice.trailing = Some(preset.group.clone());
                    choice.separated = group.is_some();
                    group = Some(&preset.group);
                }
                choices.push(choice);
            }
            choices
        }
    });
    let home = state.home.as_deref();
    let line = catalog.batch.as_ref().and_then(|batch| match batch.end {
        None => Some(batch.running()),
        Some(_) => batch.sentence(home),
    });
    BatchBand {
        refused,
        presets,
        export: if count == 1 {
            "Export\u{2026}".to_owned()
        } else {
            format!("Export {}\u{2026}", thousands(count))
        },
        line,
        report: report_of(catalog).is_some(),
        note: BATCH_NOTE,
    }
}

/// Remove from catalog…, or Put back over Removed.
fn removal_button(state: &SelectState) -> RemovalButton {
    if over_removed(state) {
        RemovalButton {
            label: "Put back".to_owned(),
            action: CatalogAction::Restore,
        }
    } else {
        RemovalButton {
            label: "Remove from catalog\u{2026}".to_owned(),
            action: CatalogAction::Remove,
        }
    }
}

/// Move to…: every catalog folder, as a path; the one that holds the whole selection checked and
/// refused.
fn move_choices(catalog: &CatalogState, every: Option<&CatalogFolderId>) -> Vec<ActionChoice> {
    let Some(list) = &catalog.folders else {
        return vec![ActionChoice::refused(
            "Reading folders\u{2026}",
            "The folders are being read",
        )];
    };
    let tree = FolderTree::of(list);
    let choices: Vec<ActionChoice> = tree
        .ordered()
        .into_iter()
        .map(|(folder, _)| {
            let label = tree.path(folder);
            if every == Some(&folder.id) {
                ActionChoice {
                    checked: true,
                    ..ActionChoice::refused(label, "They are all in this folder")
                }
            } else {
                ActionChoice::new(label, CatalogAction::MovePhotos(folder.id.clone()))
            }
        })
        .collect();
    if choices.is_empty() {
        vec![ActionChoice::refused(
            "No folders",
            "The catalog has no folders",
        )]
    } else {
        choices
    }
}

/// Add to…: every plain collection, as a path; one that holds the whole selection checked and
/// refused.
fn add_choices(
    catalog: &CatalogState,
    held: &BTreeMap<&CollectionId, u32>,
    count: u32,
) -> Vec<ActionChoice> {
    let Some(list) = &catalog.collections else {
        return vec![ActionChoice::refused(
            "Reading collections\u{2026}",
            "The collections are being read",
        )];
    };
    let choices: Vec<ActionChoice> = list
        .collections
        .iter()
        .filter(|collection| collection.kind == CollectionKind::Collection)
        .map(|collection| {
            let label = collection_path(catalog, collection);
            if held.get(&collection.id) == Some(&count) {
                ActionChoice {
                    checked: true,
                    ..ActionChoice::refused(label, "They are all in this collection")
                }
            } else {
                ActionChoice::new(label, CatalogAction::AddPhotos(collection.id.clone()))
            }
        })
        .collect();
    if choices.is_empty() {
        vec![ActionChoice::refused(
            "No collections",
            "Make one with the + in the Catalog heading",
        )]
    } else {
        choices
    }
}

/// The batch form's Metadata: when they were captured, where, with how many cameras, their kinds
/// and whether their originals are where they were.
fn batch_metadata(rows: &[&ViewRow]) -> Vec<(String, String)> {
    let mut metadata = Vec::new();
    let days: Vec<LocalDay> = rows
        .iter()
        .filter_map(|row| {
            let text = row.capture.as_deref()?;
            let (year, month, day) = (
                text.get(0..4)?.parse().ok()?,
                text.get(5..7)?.parse().ok()?,
                text.get(8..10)?.parse().ok()?,
            );
            LocalDay::from_ymd(year, month, day)
        })
        .collect();
    if let (Some(first), Some(last)) = (days.iter().min(), days.iter().max())
        && let Some(captured) = dates(Some(*first), Some(*last), true)
    {
        metadata.push(("Captured".to_owned(), captured));
    }
    let mut places: Vec<&str> = Vec::new();
    for row in rows {
        if let Some(place) = row.place.as_deref()
            && !places.contains(&place)
        {
            places.push(place);
        }
    }
    if !places.is_empty() {
        let shown = places.len().min(3);
        let mut text = places[..shown].join(", ");
        if places.len() > shown {
            text = format!("{text} and {} more", places.len() - shown);
        }
        metadata.push(("Places".to_owned(), text));
    }
    let cameras: BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.camera.as_deref())
        .collect();
    match cameras.len() {
        0 => {}
        1 => metadata.push((
            "Cameras".to_owned(),
            cameras
                .iter()
                .next()
                .copied()
                .unwrap_or_default()
                .to_owned(),
        )),
        several => metadata.push(("Cameras".to_owned(), format!("{several} cameras"))),
    }
    let raw = rows.iter().filter(|row| row.kind == SourceTag::Raw).count();
    let jpeg = rows.len() - raw;
    let kinds: Vec<String> = [(raw, "RAW"), (jpeg, "JPEG")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, kind)| format!("{count} {kind}"))
        .collect();
    metadata.push(("Kinds".to_owned(), kinds.join(" \u{b7} ")));
    let mut away: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        let state = match row.availability {
            FileAvailability::Available => continue,
            FileAvailability::Offline => "offline",
            FileAvailability::Missing => "missing",
            FileAvailability::Changed => "changed",
        };
        *away.entry(state).or_default() += 1;
    }
    metadata.push((
        "Originals".to_owned(),
        if away.is_empty() {
            "All available".to_owned()
        } else {
            away.iter()
                .map(|(state, count)| format!("{count} {state}"))
                .collect::<Vec<_>>()
                .join(" \u{b7} ")
        },
    ));
    metadata
}
